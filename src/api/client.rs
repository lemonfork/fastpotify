//! Authenticated OpenSubsonic 1.16.1 client.
//!
//! Metadata requests have a bounded total timeout. Streaming uses a separate
//! client without a total timeout, and redirects are followed only within the
//! configured origin so authentication query parameters cannot cross origins.

use std::collections::HashSet;
use std::fmt;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use reqwest::header::CONTENT_TYPE;
use reqwest::redirect::Policy;
use reqwest::{Response, Url};
use thiserror::Error;
use tokio::sync::Semaphore;

use crate::auth::{API_VERSION, CLIENT_NAME, Credentials, ProfileId, request_authentication};

use super::models::*;
use super::wire::{self, ConversionError, Envelope};

const MAX_IN_FLIGHT: usize = 6;
const METADATA_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Metadata may contain large playlists, but must never grow process memory
/// without a bound when a server is broken or hostile.
const MAX_METADATA_DOCUMENT_BYTES: usize = 32 * 1024 * 1024;
const MAX_PROTOCOL_DOCUMENT_BYTES: usize = 64 * 1024;
const MAX_COVER_ART_BYTES: usize = 8 * 1024 * 1024;
/// Conservative ceiling for a baseline GET request before authentication is
/// appended. Servers advertising `formPost` receive an unbounded form body
/// instead.
const MAX_MUTATION_QUERY_BYTES: usize = 6 * 1024;

pub type Result<T> = std::result::Result<T, ApiError>;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("unable to configure the OpenSubsonic client")]
    ClientConfiguration,
    #[error("network request failed: {0}")]
    Network(String),
    #[error("OpenSubsonic returned HTTP {status}")]
    Http { status: u16 },
    #[error("OpenSubsonic error {code}: {message}")]
    Protocol { code: u32, message: &'static str },
    #[error("OpenSubsonic returned malformed JSON")]
    Decode,
    #[error("OpenSubsonic response did not contain {0}")]
    MissingPayload(&'static str),
    #[error("OpenSubsonic response omitted {0}")]
    Conversion(&'static str),
    #[error("server returned an unexpected content type")]
    UnexpectedContentType,
    #[error("server returned an empty audio stream")]
    EmptyAudioStream,
    #[error("media reference belongs to a different server profile")]
    WrongProfile,
    #[error("expected a {expected} reference, got {actual}")]
    WrongMediaKind {
        expected: MediaKind,
        actual: MediaKind,
    },
    #[error("unsupported media kind for this operation")]
    UnsupportedMediaKind,
    #[error("malformed artwork reference")]
    InvalidArtworkReference,
    #[error("media reference has an empty server id")]
    InvalidMediaReference,
    #[error("request is too large for a server without the formPost extension")]
    RequestTooLarge,
}

impl ApiError {
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Http { status } => Some(*status),
            _ => None,
        }
    }
}

impl From<ConversionError> for ApiError {
    fn from(error: ConversionError) -> Self {
        let ConversionError::Missing(field) = error;
        Self::Conversion(field)
    }
}

/// Live network activity for the UI's non-blocking progress indicator.
pub struct NetActivity {
    started_at: Instant,
    in_flight: AtomicUsize,
    busy_since_ms: AtomicU64,
}

impl Default for NetActivity {
    fn default() -> Self {
        Self {
            started_at: Instant::now(),
            in_flight: AtomicUsize::new(0),
            busy_since_ms: AtomicU64::new(0),
        }
    }
}

impl NetActivity {
    fn now_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }

    fn begin(&self) {
        if self.in_flight.fetch_add(1, Ordering::SeqCst) == 0 {
            self.busy_since_ms.store(self.now_ms(), Ordering::SeqCst);
        }
    }

    fn end(&self) {
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }

    pub fn busy(&self, for_at_least: Duration) -> bool {
        self.in_flight.load(Ordering::SeqCst) > 0
            && self
                .now_ms()
                .saturating_sub(self.busy_since_ms.load(Ordering::SeqCst))
                >= for_at_least.as_millis() as u64
    }
}

struct ActivityGuard<'a>(&'a NetActivity);

impl Drop for ActivityGuard<'_> {
    fn drop(&mut self) {
        self.0.end();
    }
}

#[derive(Clone)]
pub struct OpenSubsonicClient {
    credentials: Credentials,
    profile: ProfileId,
    metadata: reqwest::Client,
    streaming: reqwest::Client,
    activity: Arc<NetActivity>,
    in_flight: Arc<Semaphore>,
    capabilities: Arc<RwLock<ServerCapabilities>>,
}

/// Compatibility name for callers that do not need the protocol in the type.
pub type ApiClient = OpenSubsonicClient;

/// A validated audio response with its first body chunk already available.
///
/// Peeking before the decoder starts lets the client distinguish an empty
/// server transcode from a real, but unsupported, media format.
pub struct AudioStream {
    response: Response,
    first_chunk: Option<Vec<u8>>,
}

impl AudioStream {
    async fn from_response(mut response: Response) -> Result<Option<Self>> {
        if response.content_length() == Some(0) {
            return Ok(None);
        }
        while let Some(chunk) = response.chunk().await.map_err(sanitize_reqwest_error)? {
            if !chunk.is_empty() {
                return Ok(Some(Self {
                    response,
                    first_chunk: Some(chunk.to_vec()),
                }));
            }
        }
        Ok(None)
    }

    pub async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>> {
        if let Some(chunk) = self.first_chunk.take() {
            return Ok(Some(chunk));
        }
        self.response
            .chunk()
            .await
            .map(|chunk| chunk.map(|chunk| chunk.to_vec()))
            .map_err(sanitize_reqwest_error)
    }
}

impl fmt::Debug for OpenSubsonicClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenSubsonicClient")
            .field("credentials", &self.credentials)
            .field("profile", &self.profile)
            .finish_non_exhaustive()
    }
}

impl OpenSubsonicClient {
    pub fn new(credentials: Credentials, activity: Arc<NetActivity>) -> Result<Self> {
        let origin = Url::parse(credentials.server()).map_err(|_| ApiError::ClientConfiguration)?;
        let metadata = build_http_client(&origin, Some(METADATA_TIMEOUT))?;
        let streaming = build_http_client(&origin, None)?;
        let profile = credentials.profile_id();
        Ok(Self {
            credentials,
            profile,
            metadata,
            streaming,
            activity,
            in_flight: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
            capabilities: Arc::new(RwLock::new(ServerCapabilities::default())),
        })
    }

    pub fn with_default_activity(credentials: Credentials) -> Result<Self> {
        Self::new(credentials, Arc::new(NetActivity::default()))
    }

    pub fn profile_id(&self) -> &ProfileId {
        &self.profile
    }

    pub fn activity(&self) -> &Arc<NetActivity> {
        &self.activity
    }

    pub fn supports_extension(&self, name: &str, version: u32) -> bool {
        self.capabilities
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .supports(name, version)
    }

    pub async fn ping(&self) -> Result<ServerCapabilities> {
        let response = self.request("ping", Vec::new()).await?;
        Ok(response.capabilities())
    }

    pub async fn user(&self) -> Result<User> {
        let response = self
            .request(
                "getUser",
                vec![("username", self.credentials.username().to_owned())],
            )
            .await?;
        required_payload(response.user, "user")?
            .into_domain()
            .map_err(Into::into)
    }

    pub async fn get_user(&self) -> Result<User> {
        self.user().await
    }

    pub async fn music_folders(&self) -> Result<Vec<MusicFolder>> {
        let response = self.request("getMusicFolders", Vec::new()).await?;
        required_payload(response.music_folders, "musicFolders")?
            .music_folder
            .into_iter()
            .map(|folder| folder.into_domain().map_err(Into::into))
            .collect()
    }

    pub async fn get_music_folders(&self) -> Result<Vec<MusicFolder>> {
        self.music_folders().await
    }

    pub async fn extensions(&self) -> Result<ServerCapabilities> {
        let response = self
            .request("getOpenSubsonicExtensions", Vec::new())
            .await?;
        Ok(response.capabilities())
    }

    pub async fn get_open_subsonic_extensions(&self) -> Result<ServerCapabilities> {
        self.extensions().await
    }

    /// Verifies authentication and gathers the identity, library roots and
    /// server-advertised OpenSubsonic capabilities as one coherent profile.
    pub async fn verify(&self) -> Result<VerifiedServer> {
        let ping = self.ping().await?;
        let user = self.user().await?;
        let music_folders = self.music_folders().await?;
        let extensions = self.extensions().await?;
        let capabilities = ServerCapabilities {
            protocol_version: if extensions.protocol_version.is_empty() {
                ping.protocol_version
            } else {
                extensions.protocol_version
            },
            server_type: extensions.server_type.or(ping.server_type),
            server_version: extensions.server_version.or(ping.server_version),
            open_subsonic: extensions.open_subsonic || ping.open_subsonic,
            extensions: extensions.extensions,
        };
        *self
            .capabilities
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = capabilities.clone();
        Ok(VerifiedServer {
            profile: self.profile.clone(),
            user,
            music_folders,
            capabilities,
        })
    }

    pub async fn artists(&self, music_folder_id: Option<&str>) -> Result<Vec<Artist>> {
        let mut params = Vec::new();
        push_optional(&mut params, "musicFolderId", music_folder_id);
        let response = self.request("getArtists", params).await?;
        required_payload(response.artists, "artists")?
            .index
            .into_iter()
            .flat_map(|index| index.artist)
            .map(|artist| artist.into_domain(&self.profile).map_err(Into::into))
            .collect()
    }

    pub async fn get_artists(&self, music_folder_id: Option<&str>) -> Result<Vec<Artist>> {
        self.artists(music_folder_id).await
    }

    pub async fn artist(&self, media: &MediaId) -> Result<Artist> {
        let id = self.checked_id(media, MediaKind::Artist)?;
        let response = self
            .request("getArtist", vec![("id", id.to_owned())])
            .await?;
        required_payload(response.artist, "artist")?
            .into_domain(&self.profile)
            .map_err(Into::into)
    }

    pub async fn get_artist(&self, media: &MediaId) -> Result<Artist> {
        self.artist(media).await
    }

    pub async fn album_list2(&self, request: &AlbumListRequest) -> Result<Page<Album>> {
        let limit = request.limit.clamp(1, 500);
        let mut params = vec![
            ("type", request.kind.as_str().to_owned()),
            ("size", limit.to_string()),
            ("offset", request.offset.to_string()),
        ];
        push_optional_value(&mut params, "fromYear", request.from_year);
        push_optional_value(&mut params, "toYear", request.to_year);
        push_optional_owned(&mut params, "genre", request.genre.as_deref());
        push_optional_owned(
            &mut params,
            "musicFolderId",
            request.music_folder_id.as_deref(),
        );
        let response = self.request("getAlbumList2", params).await?;
        let albums = required_payload(response.album_list2, "albumList2")?
            .album
            .into_iter()
            .map(|album| {
                album
                    .into_domain(&self.profile, false)
                    .map_err(ApiError::from)
            })
            .collect::<Result<Vec<_>>>()?;
        let has_more = albums.len() as u32 == limit;
        Ok(Page::from_slice(albums, request.offset, limit, has_more))
    }

    pub async fn get_album_list2(&self, request: &AlbumListRequest) -> Result<Page<Album>> {
        self.album_list2(request).await
    }

    /// Synthesizes pagination client-side until the server returns a short page.
    pub async fn all_albums(
        &self,
        kind: AlbumListType,
        music_folder_id: Option<&str>,
    ) -> Result<Vec<Album>> {
        let mut request = AlbumListRequest {
            kind,
            limit: 500,
            music_folder_id: music_folder_id.map(str::to_owned),
            ..AlbumListRequest::default()
        };
        let mut albums = Vec::new();
        let mut seen = HashSet::new();
        loop {
            let page = self.album_list2(&request).await?;
            let next = page.next_offset();
            let previous_count = seen.len();
            albums.extend(
                page.items
                    .into_iter()
                    .filter(|album| seen.insert(album.id.clone())),
            );
            // A few older servers ignore `offset`. Avoid looping forever on
            // the same full page while still synthesizing pagination here.
            if seen.len() == previous_count {
                break;
            }
            let Some(next) = next else { break };
            if next <= request.offset {
                break;
            }
            request.offset = next;
        }
        Ok(albums)
    }

    pub async fn album(&self, media: &MediaId) -> Result<Album> {
        let id = self.checked_id(media, MediaKind::Album)?;
        let response = self
            .request("getAlbum", vec![("id", id.to_owned())])
            .await?;
        required_payload(response.album, "album")?
            .into_domain(&self.profile, true)
            .map_err(Into::into)
    }

    pub async fn get_album(&self, media: &MediaId) -> Result<Album> {
        self.album(media).await
    }

    /// Fetches a song by the server's raw string ID.
    pub async fn song(&self, raw_id: &str) -> Result<Song> {
        if raw_id.is_empty() {
            return Err(ApiError::InvalidMediaReference);
        }
        let response = self
            .request("getSong", vec![("id", raw_id.to_owned())])
            .await?;
        required_payload(response.song, "song")?
            .into_domain(&self.profile)
            .map_err(Into::into)
    }

    pub async fn get_song(&self, media: &MediaId) -> Result<Song> {
        let id = self.checked_id(media, MediaKind::Song)?;
        self.song(id).await
    }

    pub async fn playlists(&self, username: Option<&str>) -> Result<Vec<Playlist>> {
        let mut params = Vec::new();
        push_optional(&mut params, "username", username);
        let response = self.request("getPlaylists", params).await?;
        required_payload(response.playlists, "playlists")?
            .playlist
            .into_iter()
            .map(|playlist| playlist.into_domain(&self.profile).map_err(Into::into))
            .collect()
    }

    pub async fn get_playlists(&self, username: Option<&str>) -> Result<Vec<Playlist>> {
        self.playlists(username).await
    }

    pub async fn playlist(&self, media: &MediaId) -> Result<Playlist> {
        let id = self.checked_id(media, MediaKind::Playlist)?;
        let response = self
            .request("getPlaylist", vec![("id", id.to_owned())])
            .await?;
        required_payload(response.playlist, "playlist")?
            .into_domain(&self.profile)
            .map_err(Into::into)
    }

    pub async fn get_playlist(&self, media: &MediaId) -> Result<Playlist> {
        self.playlist(media).await
    }

    pub async fn create_playlist(&self, name: &str, songs: &[MediaId]) -> Result<Playlist> {
        let song_ids = songs
            .iter()
            .map(|song| self.checked_id(song, MediaKind::Song).map(str::to_owned))
            .collect::<Result<Vec<_>>>()?;
        let form_post = self.supports_extension("formPost", 1);
        let mut params = vec![("name", name.to_owned())];
        if form_post {
            params.extend(song_ids.iter().cloned().map(|id| ("songId", id)));
        } else {
            ensure_query_size(&params)?;
        }
        let response = self
            .mutation_request("createPlaylist", params, form_post)
            .await?;
        let created = required_payload(response.playlist, "playlist")?
            .into_domain(&self.profile)
            .map_err(ApiError::from)?;
        if form_post || songs.is_empty() {
            return Ok(created);
        }

        self.update_playlist(
            &created.id,
            &PlaylistUpdate {
                songs_to_add: songs.to_vec(),
                ..PlaylistUpdate::default()
            },
        )
        .await?;
        self.playlist(&created.id).await
    }

    pub async fn update_playlist(&self, playlist: &MediaId, update: &PlaylistUpdate) -> Result<()> {
        let id = self.checked_id(playlist, MediaKind::Playlist)?;
        let songs = update
            .songs_to_add
            .iter()
            .map(|song| self.checked_id(song, MediaKind::Song).map(str::to_owned))
            .collect::<Result<Vec<_>>>()?;
        let form_post = self.supports_extension("formPost", 1);
        if form_post {
            let mut params = vec![("playlistId", id.to_owned())];
            push_optional_owned(&mut params, "name", update.name.as_deref());
            push_optional_owned(&mut params, "comment", update.description.as_deref());
            push_optional_value(&mut params, "public", update.public);
            params.extend(songs.into_iter().map(|id| ("songIdToAdd", id)));
            params.extend(
                update
                    .song_indexes_to_remove
                    .iter()
                    .map(|index| ("songIndexToRemove", index.to_string())),
            );
            if params.len() > 1 {
                self.mutation_request("updatePlaylist", params, true)
                    .await?;
            }
            return Ok(());
        }

        // Baseline 1.16.1 only guarantees GET. Metadata is sent once, songs
        // are split below a conservative URL ceiling, and row removals happen
        // one-by-one from highest to lowest so earlier indices never shift.
        let mut metadata = vec![("playlistId", id.to_owned())];
        push_optional_owned(&mut metadata, "name", update.name.as_deref());
        push_optional_owned(&mut metadata, "comment", update.description.as_deref());
        push_optional_value(&mut metadata, "public", update.public);
        if metadata.len() > 1 {
            ensure_query_size(&metadata)?;
            self.request("updatePlaylist", metadata).await?;
        }

        for batch in mutation_batches("playlistId", id, "songIdToAdd", songs)? {
            self.request("updatePlaylist", batch).await?;
        }

        let mut removals = update.song_indexes_to_remove.clone();
        removals.sort_unstable_by(|left, right| right.cmp(left));
        removals.dedup();
        for index in removals {
            self.request(
                "updatePlaylist",
                vec![
                    ("playlistId", id.to_owned()),
                    ("songIndexToRemove", index.to_string()),
                ],
            )
            .await?;
        }
        Ok(())
    }

    /// Replaces the complete contents of a playlist in one request.
    ///
    /// This deliberately does not reuse the additive batching fallback from
    /// [`Self::update_playlist`]: splitting a replacement would expose a
    /// partial order and cannot preserve duplicate occurrences atomically.
    pub async fn replace_playlist_songs(
        &self,
        playlist: &MediaId,
        songs: &[MediaId],
    ) -> Result<Playlist> {
        let playlist_id = self.checked_id(playlist, MediaKind::Playlist)?;
        let song_ids = songs
            .iter()
            .map(|song| self.checked_id(song, MediaKind::Song).map(str::to_owned))
            .collect::<Result<Vec<_>>>()?;
        let params = playlist_replacement_params(playlist_id, song_ids);

        let form_post = self.supports_extension("formPost", 1);
        if !form_post {
            // Baseline OpenSubsonic 1.16.1 only guarantees GET. A replacement
            // must remain one request, so fail safely instead of batching.
            ensure_query_size(&params)?;
        }
        let response = self
            .mutation_request("createPlaylist", params, form_post)
            .await?;
        if let Some(playlist) = response.playlist {
            playlist.into_domain(&self.profile).map_err(Into::into)
        } else {
            // Some otherwise compatible servers apply createPlaylist but omit
            // its documented response body. Read back rather than reporting a
            // failure after the mutation has already committed.
            self.playlist(playlist).await
        }
    }

    pub async fn delete_playlist(&self, playlist: &MediaId) -> Result<()> {
        let id = self.checked_id(playlist, MediaKind::Playlist)?;
        self.request("deletePlaylist", vec![("id", id.to_owned())])
            .await?;
        Ok(())
    }

    pub async fn favorites(&self, music_folder_id: Option<&str>) -> Result<Favorites> {
        let mut params = Vec::new();
        push_optional(&mut params, "musicFolderId", music_folder_id);
        let response = self.request("getStarred2", params).await?;
        required_payload(response.starred2, "starred2")?
            .into_domain(&self.profile)
            .map_err(Into::into)
    }

    pub async fn get_starred2(&self, music_folder_id: Option<&str>) -> Result<Favorites> {
        self.favorites(music_folder_id).await
    }

    pub async fn star(&self, media: &MediaId) -> Result<()> {
        self.annotate("star", media).await
    }

    pub async fn unstar(&self, media: &MediaId) -> Result<()> {
        self.annotate("unstar", media).await
    }

    async fn annotate(&self, endpoint: &'static str, media: &MediaId) -> Result<()> {
        self.ensure_profile(media)?;
        let key = match media.kind {
            MediaKind::Song => "id",
            MediaKind::Album => "albumId",
            MediaKind::Artist => "artistId",
            MediaKind::Playlist | MediaKind::MusicFolder => {
                return Err(ApiError::UnsupportedMediaKind);
            }
        };
        self.request(endpoint, vec![(key, media.id.clone())])
            .await?;
        Ok(())
    }

    pub async fn search3(&self, query: &str, options: &SearchOptions) -> Result<SearchResults> {
        let mut params = vec![
            ("query", query.to_owned()),
            ("artistCount", options.artist_count.to_string()),
            ("artistOffset", options.artist_offset.to_string()),
            ("albumCount", options.album_count.to_string()),
            ("albumOffset", options.album_offset.to_string()),
            ("songCount", options.song_count.to_string()),
            ("songOffset", options.song_offset.to_string()),
        ];
        push_optional_owned(
            &mut params,
            "musicFolderId",
            options.music_folder_id.as_deref(),
        );
        let response = self.request("search3", params).await?;
        let result = required_payload(response.search_result3, "searchResult3")?;
        let artists = result
            .artist
            .into_iter()
            .map(|artist| artist.into_domain(&self.profile).map_err(ApiError::from))
            .collect::<Result<Vec<_>>>()?;
        let albums = result
            .album
            .into_iter()
            .map(|album| {
                album
                    .into_domain(&self.profile, false)
                    .map_err(ApiError::from)
            })
            .collect::<Result<Vec<_>>>()?;
        let songs = result
            .song
            .into_iter()
            .map(|song| song.into_domain(&self.profile).map_err(ApiError::from))
            .collect::<Result<Vec<_>>>()?;

        let playlists = if options.include_playlists {
            let needle = query.to_lowercase();
            let matching =
                self.playlists(None)
                    .await?
                    .into_iter()
                    .filter(|playlist| {
                        playlist.name.to_lowercase().contains(&needle)
                            || playlist.description.as_deref().is_some_and(|description| {
                                description.to_lowercase().contains(&needle)
                            })
                    })
                    .collect::<Vec<_>>();
            Some(Page {
                total: Some(matching.len() as u32),
                limit: matching.len() as u32,
                items: matching,
                ..Page::default()
            })
        } else {
            None
        };

        let songs_have_more = options.song_count > 0 && songs.len() as u32 == options.song_count;
        let artists_have_more =
            options.artist_count > 0 && artists.len() as u32 == options.artist_count;
        let albums_have_more =
            options.album_count > 0 && albums.len() as u32 == options.album_count;

        Ok(SearchResults {
            tracks: Some(Page::from_slice(
                songs,
                options.song_offset,
                options.song_count,
                songs_have_more,
            )),
            artists: Some(Page::from_slice(
                artists,
                options.artist_offset,
                options.artist_count,
                artists_have_more,
            )),
            albums: Some(Page::from_slice(
                albums,
                options.album_offset,
                options.album_count,
                albums_have_more,
            )),
            playlists,
        })
    }

    pub async fn search(&self, query: &str, options: &SearchOptions) -> Result<SearchResults> {
        self.search3(query, options).await
    }

    pub async fn random_songs(&self, request: &RandomSongsRequest) -> Result<Vec<Song>> {
        let mut params = vec![("size", request.size.clamp(1, 500).to_string())];
        push_optional_owned(&mut params, "genre", request.genre.as_deref());
        push_optional_value(&mut params, "fromYear", request.from_year);
        push_optional_value(&mut params, "toYear", request.to_year);
        push_optional_owned(
            &mut params,
            "musicFolderId",
            request.music_folder_id.as_deref(),
        );
        let response = self.request("getRandomSongs", params).await?;
        required_payload(response.random_songs, "randomSongs")?
            .song
            .into_iter()
            .map(|song| song.into_domain(&self.profile).map_err(Into::into))
            .collect()
    }

    pub async fn get_random_songs(&self, request: &RandomSongsRequest) -> Result<Vec<Song>> {
        self.random_songs(request).await
    }

    pub async fn scrobble(&self, entries: &[Scrobble], submission: bool) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        for entry in entries {
            let id = self.checked_id(&entry.song, MediaKind::Song)?;
            let mut params = vec![("id", id.to_owned())];
            if let Some(time) = entry.time_ms {
                params.push(("time", time.to_string()));
            }
            params.push(("submission", submission.to_string()));
            self.request("scrobble", params).await?;
        }
        Ok(())
    }

    pub async fn lyrics(&self, artist: Option<&str>, title: Option<&str>) -> Result<Lyrics> {
        let mut params = Vec::new();
        push_optional(&mut params, "artist", artist);
        push_optional(&mut params, "title", title);
        let response = self.request("getLyrics", params).await?;
        Ok(required_payload(response.lyrics, "lyrics")?.into_domain())
    }

    pub async fn get_lyrics(&self, artist: Option<&str>, title: Option<&str>) -> Result<Lyrics> {
        self.lyrics(artist, title).await
    }

    pub async fn cover_art(&self, artwork: &ArtworkRef, size: Option<u32>) -> Result<Vec<u8>> {
        if artwork.profile != self.profile {
            return Err(ApiError::WrongProfile);
        }
        if artwork.id.is_empty() {
            return Err(ApiError::InvalidArtworkReference);
        }
        self.get_cover_art(&artwork.id, size).await
    }

    pub async fn cover_art_ref(&self, reference: &str, size: Option<u32>) -> Result<Vec<u8>> {
        let artwork = reference
            .parse::<ArtworkRef>()
            .map_err(|_| ApiError::InvalidArtworkReference)?;
        self.cover_art(&artwork, size).await
    }

    pub async fn get_cover_art(&self, raw_id: &str, size: Option<u32>) -> Result<Vec<u8>> {
        if raw_id.is_empty() {
            return Err(ApiError::InvalidArtworkReference);
        }
        let mut params = vec![("id", raw_id.to_owned())];
        push_optional_value(&mut params, "size", size);
        let response = self
            .binary_response(&self.metadata, "getCoverArt", params, BinaryKind::Image)
            .await?;
        bounded_response_body(response, MAX_COVER_ART_BYTES).await
    }

    /// Opens an authenticated audio stream after checking status, content
    /// type, and the first body chunk. MP3 is preferred; an empty transcode is
    /// retried once as the original format. The response has no total timeout.
    pub async fn open_stream(
        &self,
        media: &MediaId,
        max_bitrate: Option<u32>,
        time_offset_secs: Option<u32>,
    ) -> Result<AudioStream> {
        let id = self.checked_id(media, MediaKind::Song)?;
        let response = self
            .stream_response(id, "mp3", max_bitrate, time_offset_secs)
            .await?;
        if let Some(stream) = AudioStream::from_response(response).await? {
            return Ok(stream);
        }

        // A server-side transcoder can fail after the response headers have
        // already advertised a successful MP3 stream. Navidrome versions in
        // the wild then return a zero-length 200 response instead of an API
        // error. The original remains playable by the local decoder, so keep
        // the bitrate preference for the first request and retry only this
        // unambiguously empty response without transcoding.
        log::debug!("server returned an empty transcoded stream; retrying the original audio");
        let response = self.stream_response(id, "raw", None, None).await?;
        AudioStream::from_response(response)
            .await?
            .ok_or(ApiError::EmptyAudioStream)
    }

    async fn stream_response(
        &self,
        id: &str,
        format: &str,
        max_bitrate: Option<u32>,
        time_offset_secs: Option<u32>,
    ) -> Result<Response> {
        let mut params = vec![("id", id.to_owned()), ("format", format.to_owned())];
        push_optional_value(&mut params, "maxBitRate", max_bitrate);
        if self.supports_extension("transcodeOffset", 1) {
            push_optional_value(&mut params, "timeOffset", time_offset_secs);
        }
        self.binary_response(&self.streaming, "stream", params, BinaryKind::Audio)
            .await
    }

    /// Convenience for non-streaming consumers and tests.
    pub async fn stream(
        &self,
        media: &MediaId,
        max_bitrate: Option<u32>,
        time_offset_secs: Option<u32>,
    ) -> Result<Vec<u8>> {
        let mut stream = self
            .open_stream(media, max_bitrate, time_offset_secs)
            .await?;
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next_chunk().await? {
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    fn ensure_profile(&self, media: &MediaId) -> Result<()> {
        if media.profile != self.profile {
            return Err(ApiError::WrongProfile);
        }
        Ok(())
    }

    fn checked_id<'a>(&self, media: &'a MediaId, expected: MediaKind) -> Result<&'a str> {
        self.ensure_profile(media)?;
        if media.kind != expected {
            return Err(ApiError::WrongMediaKind {
                expected,
                actual: media.kind,
            });
        }
        if media.id.is_empty() {
            return Err(ApiError::InvalidMediaReference);
        }
        Ok(&media.id)
    }

    async fn request(
        &self,
        endpoint: &'static str,
        params: Vec<(&'static str, String)>,
    ) -> Result<wire::Response> {
        self.request_document(endpoint, params, false).await
    }

    async fn mutation_request(
        &self,
        endpoint: &'static str,
        params: Vec<(&'static str, String)>,
        form_post: bool,
    ) -> Result<wire::Response> {
        self.request_document(endpoint, params, form_post).await
    }

    async fn request_document(
        &self,
        endpoint: &'static str,
        params: Vec<(&'static str, String)>,
        form_post: bool,
    ) -> Result<wire::Response> {
        let _permit = self
            .in_flight
            .acquire()
            .await
            .expect("semaphore is never closed");
        self.activity.begin();
        let _activity = ActivityGuard(&self.activity);
        let url = self.endpoint(endpoint)?;
        let authenticated = self.authenticated_params(params);
        let request = if form_post {
            self.metadata.post(url).form(&authenticated)
        } else {
            self.metadata.get(url).query(&authenticated)
        };
        let response = request.send().await.map_err(sanitize_reqwest_error)?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let bytes = bounded_response_body(response, MAX_METADATA_DOCUMENT_BYTES).await?;
        if !status.is_success() {
            if let Some(error) = protocol_document_error(&bytes) {
                return Err(error);
            }
            return Err(ApiError::Http {
                status: status.as_u16(),
            });
        }
        if !content_type.is_empty() && !is_json_content_type(&content_type) {
            return Err(ApiError::UnexpectedContentType);
        }
        let envelope: Envelope = serde_json::from_slice(&bytes).map_err(|_| ApiError::Decode)?;
        envelope
            .response
            .ensure_success()
            .map_err(|failure| protocol_error(failure.code))?;
        Ok(envelope.response)
    }

    async fn binary_response(
        &self,
        client: &reqwest::Client,
        endpoint: &'static str,
        params: Vec<(&'static str, String)>,
        kind: BinaryKind,
    ) -> Result<Response> {
        let _permit = self
            .in_flight
            .acquire()
            .await
            .expect("semaphore is never closed");
        self.activity.begin();
        let _activity = ActivityGuard(&self.activity);
        let url = self.endpoint(endpoint)?;
        let query = self.authenticated_params(params);
        let response = client
            .get(url)
            .query(&query)
            .send()
            .await
            .map_err(sanitize_reqwest_error)?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !status.is_success() || is_protocol_document(&content_type) {
            let bytes = bounded_response_body(response, MAX_PROTOCOL_DOCUMENT_BYTES).await?;
            if let Some(error) = protocol_document_error(&bytes) {
                return Err(error);
            }
            return if status.is_success() {
                Err(ApiError::UnexpectedContentType)
            } else {
                Err(ApiError::Http {
                    status: status.as_u16(),
                })
            };
        }
        if !kind.accepts(&content_type) {
            return Err(ApiError::UnexpectedContentType);
        }
        Ok(response)
    }

    fn endpoint(&self, endpoint: &'static str) -> Result<Url> {
        Url::parse(&format!(
            "{}/rest/{endpoint}.view",
            self.credentials.server()
        ))
        .map_err(|_| ApiError::ClientConfiguration)
    }

    fn authenticated_params(&self, params: Vec<(&'static str, String)>) -> Vec<(String, String)> {
        let authentication = request_authentication(self.credentials.password());
        let mut query = Vec::with_capacity(params.len() + 6);
        query.push(("u".to_owned(), self.credentials.username().to_owned()));
        query.push(("t".to_owned(), authentication.token));
        query.push(("s".to_owned(), authentication.salt));
        query.push(("v".to_owned(), API_VERSION.to_owned()));
        query.push(("c".to_owned(), CLIENT_NAME.to_owned()));
        query.push(("f".to_owned(), "json".to_owned()));
        query.extend(
            params
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value)),
        );
        query
    }
}

fn build_http_client(origin: &Url, timeout: Option<Duration>) -> Result<reqwest::Client> {
    let redirect_origin = origin.clone();
    let policy = Policy::custom(move |attempt| {
        if attempt.previous().len() >= 10 {
            attempt.stop()
        } else if same_origin(&redirect_origin, attempt.url()) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    });
    let mut builder = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(policy)
        .user_agent(format!("Fastpotify/{}", env!("CARGO_PKG_VERSION")));
    if let Some(timeout) = timeout {
        builder = builder.timeout(timeout);
    }
    builder.build().map_err(|_| ApiError::ClientConfiguration)
}

fn same_origin(expected: &Url, actual: &Url) -> bool {
    expected.scheme() == actual.scheme()
        && expected.host_str() == actual.host_str()
        && expected.port_or_known_default() == actual.port_or_known_default()
}

fn sanitize_reqwest_error(error: reqwest::Error) -> ApiError {
    ApiError::Network(error.without_url().to_string())
}

fn required_payload<T>(payload: Option<T>, name: &'static str) -> Result<T> {
    payload.ok_or(ApiError::MissingPayload(name))
}

fn encoded_parameter_cost(key: &str, value: &str) -> usize {
    // Every UTF-8 byte may become `%XX`. This deliberately overestimates
    // ASCII IDs so no proxy-specific encoder can make the real URL larger.
    key.len()
        .saturating_add(1)
        .saturating_add(value.len().saturating_mul(3))
        .saturating_add(1)
}

fn ensure_query_size(params: &[(&'static str, String)]) -> Result<()> {
    let estimated = params.iter().fold(0usize, |total, (key, value)| {
        total.saturating_add(encoded_parameter_cost(key, value))
    });
    if estimated > MAX_MUTATION_QUERY_BYTES {
        Err(ApiError::RequestTooLarge)
    } else {
        Ok(())
    }
}

fn playlist_replacement_params(
    playlist_id: &str,
    song_ids: Vec<String>,
) -> Vec<(&'static str, String)> {
    let mut params = vec![("playlistId", playlist_id.to_owned())];
    params.extend(song_ids.into_iter().map(|id| ("songId", id)));
    params
}

fn mutation_batches(
    identity_key: &'static str,
    identity: &str,
    value_key: &'static str,
    values: Vec<String>,
) -> Result<Vec<Vec<(&'static str, String)>>> {
    let base = encoded_parameter_cost(identity_key, identity);
    if base > MAX_MUTATION_QUERY_BYTES {
        return Err(ApiError::RequestTooLarge);
    }
    let mut batches = Vec::new();
    let mut current = vec![(identity_key, identity.to_owned())];
    let mut size = base;
    for value in values {
        let cost = encoded_parameter_cost(value_key, &value);
        if base.saturating_add(cost) > MAX_MUTATION_QUERY_BYTES {
            return Err(ApiError::RequestTooLarge);
        }
        if size.saturating_add(cost) > MAX_MUTATION_QUERY_BYTES {
            batches.push(current);
            current = vec![(identity_key, identity.to_owned())];
            size = base;
        }
        current.push((value_key, value));
        size = size.saturating_add(cost);
    }
    if current.len() > 1 {
        batches.push(current);
    }
    Ok(batches)
}

fn protocol_error(code: u32) -> ApiError {
    let message = match code {
        10 => "required parameter is missing",
        20 => "client protocol version is incompatible with the server",
        30 => "server protocol version is incompatible with the client",
        40 => "authentication failed",
        41 => "token authentication is not supported for this account",
        42 => "authentication mechanism is not supported",
        43 => "conflicting authentication mechanisms were provided",
        44 => "API key is invalid",
        50 => "this account is not authorized for the operation",
        60 => "server trial period has expired",
        70 => "requested item was not found",
        _ => "server rejected the request",
    };
    ApiError::Protocol { code, message }
}

fn protocol_document_error(bytes: &[u8]) -> Option<ApiError> {
    let json = serde_json::from_slice::<Envelope>(bytes)
        .ok()
        .and_then(|envelope| envelope.response.ensure_success().err())
        .map(|failure| protocol_error(failure.code));
    json.or_else(|| xml_error_code(bytes).map(protocol_error))
}

fn is_json_content_type(content_type: &str) -> bool {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    matches!(mime.as_str(), "application/json" | "text/json")
        || mime
            .split_once('/')
            .is_some_and(|(_, subtype)| subtype.ends_with("+json"))
}

fn is_protocol_document(content_type: &str) -> bool {
    is_json_content_type(content_type)
        || content_type.starts_with("text/")
        || content_type.contains("xml")
        || content_type.contains("html")
}

async fn bounded_response_body(mut response: Response, limit: usize) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(ApiError::UnexpectedContentType);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(sanitize_reqwest_error)? {
        append_bounded(&mut body, &chunk, limit)?;
    }
    Ok(body)
}

fn append_bounded(body: &mut Vec<u8>, chunk: &[u8], limit: usize) -> Result<()> {
    if body.len().saturating_add(chunk.len()) > limit {
        return Err(ApiError::UnexpectedContentType);
    }
    body.extend_from_slice(chunk);
    Ok(())
}

fn xml_error_code(bytes: &[u8]) -> Option<u32> {
    let document = std::str::from_utf8(bytes).ok()?;
    let start = document.find("<error")?;
    let tag = document.get(start..)?.split_once('>')?.0;
    let mut offset = 0;
    while let Some(found) = tag.get(offset..)?.find("code") {
        let code = offset + found;
        let before = tag[..code].chars().next_back();
        let boundary = before.is_some_and(|character| character.is_ascii_whitespace());
        let remainder = tag.get(code + "code".len()..)?.trim_start();
        if boundary && let Some(remainder) = remainder.strip_prefix('=').map(str::trim_start) {
            let quote = remainder.chars().next()?;
            if matches!(quote, '\'' | '"') {
                let value = remainder.get(quote.len_utf8()..)?.split(quote).next()?;
                return value.parse().ok();
            }
        }
        offset = code + "code".len();
    }
    None
}

#[derive(Clone, Copy)]
enum BinaryKind {
    Image,
    Audio,
}

impl BinaryKind {
    fn accepts(self, content_type: &str) -> bool {
        if content_type.is_empty() {
            return false;
        }
        match self {
            Self::Image => {
                content_type.starts_with("image/")
                    || content_type.starts_with("application/octet-stream")
            }
            Self::Audio => {
                content_type.starts_with("audio/")
                    || content_type.starts_with("application/octet-stream")
                    || content_type.starts_with("application/ogg")
                    || content_type.starts_with("application/flac")
                    || content_type.starts_with("application/x-flac")
            }
        }
    }
}

fn push_optional(params: &mut Vec<(&'static str, String)>, key: &'static str, value: Option<&str>) {
    if let Some(value) = value {
        params.push((key, value.to_owned()));
    }
}

fn push_optional_owned(
    params: &mut Vec<(&'static str, String)>,
    key: &'static str,
    value: Option<&str>,
) {
    push_optional(params, key, value);
}

fn push_optional_value<T: ToString>(
    params: &mut Vec<(&'static str, String)>,
    key: &'static str,
    value: Option<T>,
) {
    if let Some(value) = value {
        params.push((key, value.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{BufRead as _, BufReader, Write as _};
    use std::net::{Ipv4Addr, TcpListener, TcpStream};
    use std::thread;

    fn loopback_listener() -> Option<TcpListener> {
        match TcpListener::bind((Ipv4Addr::LOCALHOST, 0)) {
            Ok(listener) => Some(listener),
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => None,
            Err(error) => panic!("unable to bind loopback test server: {error}"),
        }
    }

    fn request_line(stream: &TcpStream) -> String {
        let mut reader = BufReader::new(stream);
        let mut first = String::new();
        reader.read_line(&mut first).unwrap();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if matches!(line.as_str(), "\r\n" | "\n" | "") {
                break;
            }
        }
        first
    }

    fn stream_client(listener: &TcpListener) -> (OpenSubsonicClient, MediaId) {
        let credentials = Credentials::new(
            format!("http://{}", listener.local_addr().unwrap()),
            "alice",
            "secret",
        )
        .unwrap();
        let media = MediaId::new(credentials.profile_id(), MediaKind::Song, "track");
        let client = OpenSubsonicClient::with_default_activity(credentials).unwrap();
        (client, media)
    }

    fn client() -> OpenSubsonicClient {
        OpenSubsonicClient::with_default_activity(
            Credentials::new("https://music.example.test/subsonic/", "alice", "secret").unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn every_request_uses_a_new_salt_and_token_shape() {
        let client = client();
        let first = client.authenticated_params(Vec::new());
        let second = client.authenticated_params(Vec::new());
        let get = |params: &[(String, String)], key: &str| {
            params
                .iter()
                .find(|(name, _)| name == key)
                .unwrap()
                .1
                .clone()
        };
        assert_ne!(get(&first, "s"), get(&second, "s"));
        assert_ne!(get(&first, "t"), get(&second, "t"));
        assert_eq!(get(&first, "t").len(), 32);
        assert_eq!(get(&first, "v"), "1.16.1");
        assert_eq!(get(&first, "c"), "Fastpotify");
        assert_eq!(get(&first, "f"), "json");
    }

    #[test]
    fn endpoint_stays_under_configured_base_path() {
        assert_eq!(
            client().endpoint("getSong").unwrap().as_str(),
            "https://music.example.test/subsonic/rest/getSong.view"
        );
    }

    #[test]
    fn endpoint_preserves_percent_encoded_base_path_bytes() {
        let client = OpenSubsonicClient::with_default_activity(
            Credentials::new(
                "https://music.example.test/Music%20Server/%E9%9F%B3%E4%B9%90/%2F/",
                "alice",
                "secret",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            client.endpoint("getSong").unwrap().as_str(),
            "https://music.example.test/Music%20Server/%E9%9F%B3%E4%B9%90/%2F/rest/getSong.view"
        );
    }

    #[test]
    fn redirect_origin_includes_scheme_host_and_effective_port() {
        let https = Url::parse("https://example.test/music").unwrap();
        assert!(same_origin(
            &https,
            &Url::parse("https://example.test:443/elsewhere").unwrap()
        ));
        assert!(!same_origin(
            &https,
            &Url::parse("http://example.test/elsewhere").unwrap()
        ));
        assert!(!same_origin(
            &https,
            &Url::parse("https://cdn.example.test/elsewhere").unwrap()
        ));
    }

    #[test]
    fn binary_content_type_rejects_protocol_documents() {
        assert!(BinaryKind::Audio.accepts("audio/mpeg"));
        assert!(BinaryKind::Image.accepts("image/jpeg"));
        assert!(!BinaryKind::Audio.accepts("application/json"));
        assert!(is_protocol_document("text/xml; charset=utf-8"));
        assert!(is_json_content_type(
            "application/vnd.opensubsonic.response+json; charset=utf-8"
        ));
        assert!(!is_json_content_type("application/jsonp"));
    }

    #[test]
    fn empty_transcode_retries_the_original_stream() {
        let Some(listener) = loopback_listener() else {
            return;
        };
        let (client, media) = stream_client(&listener);
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for response in [
                "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 200 OK\r\nContent-Type: audio/flac\r\nContent-Length: 4\r\nConnection: close\r\n\r\nfLaC",
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                requests.push(request_line(&stream));
                stream.write_all(response.as_bytes()).unwrap();
            }
            requests
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let body = runtime
            .block_on(client.stream(&media, Some(320), None))
            .unwrap();
        let requests = server.join().unwrap();

        assert_eq!(body, b"fLaC");
        assert_eq!(requests.len(), 2);
        assert!(requests[0].contains("format=mp3"));
        assert!(requests[0].contains("maxBitRate=320"));
        assert!(requests[1].contains("format=raw"));
        assert!(!requests[1].contains("maxBitRate"));
    }

    #[test]
    fn eof_without_a_content_length_also_retries_the_original_stream() {
        let Some(listener) = loopback_listener() else {
            return;
        };
        let (client, media) = stream_client(&listener);
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for response in [
                "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 200 OK\r\nContent-Type: audio/flac\r\nContent-Length: 4\r\nConnection: close\r\n\r\nfLaC",
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                requests.push(request_line(&stream));
                stream.write_all(response.as_bytes()).unwrap();
            }
            requests
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let body = runtime
            .block_on(client.stream(&media, Some(320), None))
            .unwrap();
        let requests = server.join().unwrap();

        assert_eq!(body, b"fLaC");
        assert_eq!(requests.len(), 2);
        assert!(requests[0].contains("format=mp3"));
        assert!(requests[1].contains("format=raw"));
    }

    #[test]
    fn empty_original_stream_is_reported_as_empty_audio() {
        let Some(listener) = loopback_listener() else {
            return;
        };
        let (client, media) = stream_client(&listener);
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let _ = request_line(&stream);
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .unwrap();
            }
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = runtime.block_on(client.stream(&media, Some(320), None));
        server.join().unwrap();

        assert!(matches!(result, Err(ApiError::EmptyAudioStream)));
    }

    #[test]
    fn nonempty_transcode_does_not_request_the_original() {
        let Some(listener) = loopback_listener() else {
            return;
        };
        let (client, media) = stream_client(&listener);
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = request_line(&stream);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: 3\r\nConnection: close\r\n\r\nID3",
                )
                .unwrap();
            request
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let body = runtime
            .block_on(client.stream(&media, Some(320), None))
            .unwrap();
        let request = server.join().unwrap();

        assert_eq!(body, b"ID3");
        assert!(request.contains("format=mp3"));
        assert!(request.contains("maxBitRate=320"));
    }

    #[test]
    fn json_and_xml_protocol_failures_map_codes_without_server_messages() {
        let xml = br#"<subsonic-response status="failed"><error message="private code word" code="30"/></subsonic-response>"#;
        assert!(matches!(
            protocol_document_error(xml),
            Some(ApiError::Protocol {
                code: 30,
                message: "server protocol version is incompatible with the client"
            })
        ));
        let json =
            br#"{"subsonic-response":{"status":"failed","error":{"code":60,"message":"secret"}}}"#;
        assert!(matches!(
            protocol_document_error(json),
            Some(ApiError::Protocol {
                code: 60,
                message: "server trial period has expired"
            })
        ));
    }

    #[test]
    fn baseline_playlist_batches_preserve_duplicates_and_bound_urls() {
        let values = (0..600)
            .map(|index| format!("song-{index:04}-{}", "x".repeat(24)))
            .collect::<Vec<_>>();
        let batches =
            mutation_batches("playlistId", "playlist", "songIdToAdd", values.clone()).unwrap();
        assert!(batches.len() > 1);
        assert!(batches.iter().all(|batch| ensure_query_size(batch).is_ok()));
        let rebuilt = batches
            .iter()
            .flat_map(|batch| batch.iter().skip(1).map(|(_, value)| value.clone()))
            .collect::<Vec<_>>();
        assert_eq!(rebuilt, values);

        let duplicates = mutation_batches(
            "playlistId",
            "playlist",
            "songIdToAdd",
            vec!["same".into(), "same".into()],
        )
        .unwrap();
        assert_eq!(duplicates[0].len(), 3);
        assert!(matches!(
            mutation_batches(
                "playlistId",
                "playlist",
                "songIdToAdd",
                vec!["x".repeat(MAX_MUTATION_QUERY_BYTES)]
            ),
            Err(ApiError::RequestTooLarge)
        ));
    }

    #[test]
    fn playlist_replacement_is_one_ordered_duplicate_preserving_request() {
        let params = playlist_replacement_params(
            "playlist",
            vec!["first".into(), "same".into(), "same".into(), "last".into()],
        );
        assert_eq!(
            params,
            vec![
                ("playlistId", "playlist".into()),
                ("songId", "first".into()),
                ("songId", "same".into()),
                ("songId", "same".into()),
                ("songId", "last".into()),
            ]
        );
        assert!(ensure_query_size(&params).is_ok());

        let oversized =
            playlist_replacement_params("playlist", vec!["x".repeat(MAX_MUTATION_QUERY_BYTES)]);
        assert!(matches!(
            ensure_query_size(&oversized),
            Err(ApiError::RequestTooLarge)
        ));
    }

    #[test]
    fn client_debug_is_credential_safe() {
        let debug = format!("{:?}", client());
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("t="));
    }

    #[test]
    fn media_ids_cannot_cross_profiles_or_kinds() {
        let client = client();
        let wrong_profile = MediaId::new(
            ProfileId::new("fedcba9876543210fedcba9876543210fedcba98"),
            MediaKind::Song,
            "id",
        );
        assert!(matches!(
            client.checked_id(&wrong_profile, MediaKind::Song),
            Err(ApiError::WrongProfile)
        ));
        let artist = MediaId::new(client.profile.clone(), MediaKind::Artist, "id");
        assert!(matches!(
            client.checked_id(&artist, MediaKind::Song),
            Err(ApiError::WrongMediaKind { .. })
        ));
        let empty = MediaId::new(client.profile.clone(), MediaKind::Song, "");
        assert!(matches!(
            client.checked_id(&empty, MediaKind::Song),
            Err(ApiError::InvalidMediaReference)
        ));
    }

    #[test]
    fn response_chunks_cannot_grow_past_the_configured_limit() {
        let mut body = vec![1, 2, 3];
        append_bounded(&mut body, &[4], 4).unwrap();
        assert_eq!(body, vec![1, 2, 3, 4]);
        assert!(matches!(
            append_bounded(&mut body, &[5], 4),
            Err(ApiError::UnexpectedContentType)
        ));
        assert_eq!(body, vec![1, 2, 3, 4]);
    }
}
