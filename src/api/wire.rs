//! OpenSubsonic JSON wire shapes and conversion into domain models.

use serde::{Deserialize, Deserializer};
use thiserror::Error;

use super::models::*;

#[derive(Debug, Deserialize)]
pub(crate) struct Envelope {
    #[serde(rename = "subsonic-response")]
    pub response: Response,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Response {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub version: String,
    #[serde(default, rename = "type")]
    pub server_type: Option<String>,
    #[serde(default)]
    pub server_version: Option<String>,
    #[serde(default)]
    pub open_subsonic: Option<bool>,
    #[serde(default)]
    pub error: Option<WireError>,
    #[serde(default)]
    pub user: Option<WireUser>,
    #[serde(default)]
    pub music_folders: Option<WireMusicFolders>,
    #[serde(default)]
    pub open_subsonic_extensions: Vec<WireExtension>,
    #[serde(default)]
    pub artists: Option<WireArtists>,
    #[serde(default)]
    pub artist: Option<WireArtist>,
    #[serde(default)]
    pub album_list2: Option<WireAlbums>,
    #[serde(default)]
    pub album: Option<WireAlbum>,
    #[serde(default)]
    pub song: Option<WireSong>,
    #[serde(default)]
    pub playlists: Option<WirePlaylists>,
    #[serde(default)]
    pub playlist: Option<WirePlaylist>,
    #[serde(default)]
    pub starred2: Option<WireStarred>,
    #[serde(default)]
    pub search_result3: Option<WireSearchResult>,
    #[serde(default)]
    pub random_songs: Option<WireSongs>,
    #[serde(default)]
    pub lyrics: Option<WireLyrics>,
}

impl Response {
    pub fn ensure_success(&self) -> Result<(), ProtocolFailure> {
        if self.status.eq_ignore_ascii_case("ok") {
            return Ok(());
        }
        let error = self.error.as_ref();
        Err(ProtocolFailure {
            code: error.map_or(0, |error| error.code),
            message: error
                .map(|error| error.message.trim())
                .filter(|message| !message.is_empty())
                .unwrap_or("OpenSubsonic request failed")
                .to_owned(),
        })
    }

    pub fn capabilities(&self) -> ServerCapabilities {
        ServerCapabilities {
            protocol_version: self.version.clone(),
            server_type: self.server_type.clone(),
            server_version: self.server_version.clone(),
            open_subsonic: self.open_subsonic.unwrap_or(false),
            extensions: self
                .open_subsonic_extensions
                .iter()
                .map(|extension| OpenSubsonicExtension {
                    name: extension.name.clone(),
                    versions: extension.versions.clone(),
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("OpenSubsonic error {code}: {message}")]
pub struct ProtocolFailure {
    pub code: u32,
    pub message: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WireError {
    #[serde(default)]
    pub code: u32,
    #[serde(default)]
    pub message: String,
    #[allow(dead_code)]
    #[serde(default)]
    pub help_url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct WireExtension {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub versions: Vec<u32>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct WireMusicFolders {
    #[serde(default, rename = "musicFolder")]
    pub music_folder: Vec<WireMusicFolder>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct WireMusicFolder {
    #[serde(default, deserialize_with = "deserialize_string_or_integer")]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

fn deserialize_string_or_integer<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrInteger {
        String(String),
        Signed(i64),
        Unsigned(u64),
    }

    Ok(match StringOrInteger::deserialize(deserializer)? {
        StringOrInteger::String(value) => value,
        StringOrInteger::Signed(value) => value.to_string(),
        StringOrInteger::Unsigned(value) => value.to_string(),
    })
}

impl WireMusicFolder {
    pub fn into_domain(self) -> Result<MusicFolder, ConversionError> {
        Ok(MusicFolder {
            id: required(self.id, "music folder id")?,
            name: self.name,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WireUser {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub scrobbling_enabled: bool,
    #[serde(default)]
    pub max_bit_rate: Option<u32>,
    #[serde(default)]
    pub admin_role: bool,
    #[serde(default)]
    pub settings_role: bool,
    #[serde(default)]
    pub download_role: bool,
    #[serde(default)]
    pub upload_role: bool,
    #[serde(default)]
    pub playlist_role: bool,
    #[serde(default)]
    pub cover_art_role: bool,
    #[serde(default)]
    pub comment_role: bool,
    #[serde(default)]
    pub podcast_role: bool,
    #[serde(default)]
    pub stream_role: bool,
    #[serde(default)]
    pub jukebox_role: bool,
    #[serde(default)]
    pub share_role: bool,
}

impl WireUser {
    pub fn into_domain(self) -> Result<User, ConversionError> {
        let id = required(self.username, "username")?;
        Ok(User {
            display_name: Some(id.clone()),
            id,
            scrobbling_enabled: self.scrobbling_enabled,
            max_bit_rate: self.max_bit_rate,
            roles: UserRoles {
                admin: self.admin_role,
                settings: self.settings_role,
                download: self.download_role,
                upload: self.upload_role,
                playlist: self.playlist_role,
                cover_art: self.cover_art_role,
                comment: self.comment_role,
                podcast: self.podcast_role,
                stream: self.stream_role,
                jukebox: self.jukebox_role,
                share: self.share_role,
            },
        })
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct WireArtists {
    #[serde(default)]
    pub index: Vec<WireArtistIndex>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct WireArtistIndex {
    #[serde(default)]
    pub artist: Vec<WireArtist>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WireArtist {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub cover_art: Option<String>,
    #[serde(default)]
    pub album_count: Option<u32>,
    #[serde(default)]
    pub starred: Option<String>,
    #[serde(default)]
    pub album: Vec<WireAlbum>,
}

impl WireArtist {
    pub fn into_domain(self, profile: &ProfileId) -> Result<Artist, ConversionError> {
        let id = required(self.id, "artist id")?;
        let media_id = MediaId::new(profile.clone(), MediaKind::Artist, id);
        let albums = self
            .album
            .into_iter()
            .map(|album| album.into_domain(profile, false))
            .collect::<Result<Vec<_>, _>>()?;
        let starred = self.starred;
        Ok(Artist {
            uri: media_id.uri(),
            id: media_id,
            name: self.name,
            images: images(profile, self.cover_art),
            genres: Vec::new(),
            album_count: self.album_count.unwrap_or(albums.len() as u32),
            albums,
            starred: starred.is_some(),
            starred_at: starred,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct WireAlbums {
    #[serde(default)]
    pub album: Vec<WireAlbum>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WireAlbum {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub cover_art: Option<String>,
    #[serde(default)]
    pub song_count: Option<u32>,
    #[serde(default)]
    pub duration: Option<u64>,
    #[serde(default)]
    pub artist_id: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub artists: Vec<WireArtistRef>,
    #[serde(default)]
    pub year: Option<u32>,
    #[serde(default)]
    pub genre: Option<String>,
    #[serde(default)]
    pub starred: Option<String>,
    #[serde(default)]
    pub song: Vec<WireSong>,
}

impl WireAlbum {
    pub fn into_domain(
        self,
        profile: &ProfileId,
        include_songs: bool,
    ) -> Result<Album, ConversionError> {
        let id = required(self.id, "album id")?;
        let media_id = MediaId::new(profile.clone(), MediaKind::Album, id);
        let name = first_nonempty([self.name, self.title, self.album]);
        let artists = artist_refs(profile, self.artists, self.artist_id, self.artist);
        let song_count = self.song_count.unwrap_or(self.song.len() as u32);
        let songs = if include_songs {
            let converted = self
                .song
                .into_iter()
                .map(|song| song.into_domain(profile))
                .collect::<Result<Vec<_>, _>>()?;
            Some(Page {
                total: Some(song_count.max(converted.len() as u32)),
                limit: converted.len() as u32,
                offset: 0,
                next: None,
                items: converted,
            })
        } else {
            None
        };
        let starred = self.starred;
        Ok(Album {
            uri: media_id.uri(),
            id: media_id,
            name,
            images: images(profile, self.cover_art),
            artists,
            // `created` is the server's library-ingestion timestamp, not the
            // album release date. Keep the ID3 year as the user-facing date.
            release_date: self.year.map(|year| year.to_string()),
            year: self.year,
            genres: self.genre.into_iter().collect(),
            total_tracks: Some(song_count),
            duration_ms: seconds_to_millis(self.duration),
            tracks: songs,
            starred: starred.is_some(),
            starred_at: starred,
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WireSong {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub album_id: Option<String>,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub artist_id: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub artists: Vec<WireArtistRef>,
    #[serde(default)]
    pub cover_art: Option<String>,
    #[serde(default)]
    pub duration: Option<u64>,
    #[serde(default)]
    pub track: Option<u32>,
    #[serde(default)]
    pub disc_number: Option<u32>,
    #[serde(default)]
    pub year: Option<u32>,
    #[serde(default)]
    pub genre: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub suffix: Option<String>,
    #[serde(default)]
    pub bit_rate: Option<u32>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub starred: Option<String>,
}

impl WireSong {
    pub fn into_domain(self, profile: &ProfileId) -> Result<Song, ConversionError> {
        let id = required(self.id, "song id")?;
        let media_id = MediaId::new(profile.clone(), MediaKind::Song, id);
        let artists = artist_refs(profile, self.artists, self.artist_id, self.artist);
        let album = self.album_id.clone().map(|album_id| {
            let album_media_id = MediaId::new(profile.clone(), MediaKind::Album, album_id);
            Album {
                uri: album_media_id.uri(),
                id: album_media_id,
                name: self.album.clone().unwrap_or_default(),
                images: images(profile, self.cover_art.clone()),
                artists: artists.clone(),
                year: self.year,
                release_date: self.year.map(|year| year.to_string()),
                genres: self.genre.clone().into_iter().collect(),
                ..Album::default()
            }
        });
        let starred = self.starred;
        Ok(Song {
            uri: media_id.uri(),
            id: media_id,
            name: self.title,
            duration_ms: seconds_to_millis(self.duration),
            artists,
            album,
            track_number: self.track,
            disc_number: self.disc_number,
            year: self.year,
            genres: self.genre.into_iter().collect(),
            content_type: self.content_type,
            suffix: self.suffix,
            bit_rate: self.bit_rate,
            size: self.size,
            starred: starred.is_some(),
            starred_at: starred,
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct WireArtistRef {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct WireSongs {
    #[serde(default)]
    pub song: Vec<WireSong>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct WirePlaylists {
    #[serde(default)]
    pub playlist: Vec<WirePlaylist>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WirePlaylist {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub public: Option<bool>,
    #[serde(default, rename = "readonly")]
    pub readonly: bool,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub cover_art: Option<String>,
    #[serde(default)]
    pub song_count: Option<u32>,
    #[serde(default)]
    pub duration: Option<u64>,
    #[serde(default)]
    pub created: Option<String>,
    #[serde(default)]
    pub changed: Option<String>,
    #[serde(default)]
    pub entry: Vec<WireSong>,
}

impl WirePlaylist {
    pub fn into_domain(self, profile: &ProfileId) -> Result<Playlist, ConversionError> {
        let id = required(self.id, "playlist id")?;
        let media_id = MediaId::new(profile.clone(), MediaKind::Playlist, id);
        let entries = self
            .entry
            .into_iter()
            .enumerate()
            .map(|(index, song)| {
                Ok(PlaylistItem {
                    index: index.min(u32::MAX as usize) as u32,
                    added_at: None,
                    track: song.into_domain(profile)?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Playlist {
            uri: media_id.uri(),
            id: media_id,
            name: self.name,
            description: self.comment,
            images: images(profile, self.cover_art),
            owner: UserRef {
                display_name: self.owner.clone(),
                id: self.owner,
            },
            public: self.public,
            readonly: self.readonly,
            track_count: self.song_count.unwrap_or(entries.len() as u32),
            duration_ms: seconds_to_millis(self.duration),
            created: self.created,
            changed: self.changed,
            entries,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct WireStarred {
    #[serde(default)]
    pub artist: Vec<WireArtist>,
    #[serde(default)]
    pub album: Vec<WireAlbum>,
    #[serde(default)]
    pub song: Vec<WireSong>,
}

impl WireStarred {
    pub fn into_domain(self, profile: &ProfileId) -> Result<Favorites, ConversionError> {
        Ok(Favorites {
            artists: self
                .artist
                .into_iter()
                .map(|artist| artist.into_domain(profile))
                .collect::<Result<_, _>>()?,
            albums: self
                .album
                .into_iter()
                .map(|album| album.into_domain(profile, false))
                .collect::<Result<_, _>>()?,
            songs: self
                .song
                .into_iter()
                .map(|song| song.into_domain(profile))
                .collect::<Result<_, _>>()?,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct WireSearchResult {
    #[serde(default)]
    pub artist: Vec<WireArtist>,
    #[serde(default)]
    pub album: Vec<WireAlbum>,
    #[serde(default)]
    pub song: Vec<WireSong>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct WireLyrics {
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub value: String,
}

impl WireLyrics {
    pub fn into_domain(self) -> Lyrics {
        Lyrics {
            artist: self.artist,
            title: self.title,
            text: self.value,
        }
    }
}

fn images(profile: &ProfileId, cover_art: Option<String>) -> Vec<Image> {
    cover_art
        .filter(|id| !id.is_empty())
        .map(|id| Image::from_cover_art(profile.clone(), id))
        .into_iter()
        .collect()
}

fn artist_ref(profile: &ProfileId, id: Option<String>, name: Option<String>) -> Option<ArtistRef> {
    if id.as_deref().is_none_or(str::is_empty) && name.as_deref().is_none_or(str::is_empty) {
        return None;
    }
    let media_id = id
        .filter(|id| !id.is_empty())
        .map(|id| MediaId::new(profile.clone(), MediaKind::Artist, id));
    Some(ArtistRef {
        uri: media_id.as_ref().map(MediaId::uri),
        id: media_id,
        name: name.unwrap_or_default(),
    })
}

fn artist_refs(
    profile: &ProfileId,
    artists: Vec<WireArtistRef>,
    legacy_id: Option<String>,
    legacy_name: Option<String>,
) -> Vec<ArtistRef> {
    if artists.is_empty() {
        return artist_ref(profile, legacy_id, legacy_name)
            .into_iter()
            .collect();
    }

    artists
        .into_iter()
        .filter_map(|artist| artist_ref(profile, artist.id, artist.name))
        .collect()
}

fn required(value: String, field: &'static str) -> Result<String, ConversionError> {
    (!value.is_empty())
        .then_some(value)
        .ok_or(ConversionError::Missing(field))
}

fn first_nonempty(values: [String; 3]) -> String {
    values
        .into_iter()
        .find(|value| !value.is_empty())
        .unwrap_or_default()
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ConversionError {
    #[error("OpenSubsonic response omitted {0}")]
    Missing(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROFILE: &str = "1111111111111111111111111111111111111111";

    fn profile() -> ProfileId {
        ProfileId::new(PROFILE)
    }

    #[test]
    fn failed_envelope_is_an_error_even_at_http_200() {
        let envelope: Envelope = serde_json::from_str(
            r#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":40,"message":"Wrong username or password"}}}"#,
        )
        .unwrap();
        assert_eq!(
            envelope.response.ensure_success().unwrap_err(),
            ProtocolFailure {
                code: 40,
                message: "Wrong username or password".into(),
            }
        );
    }

    #[test]
    fn music_folder_ids_accept_navidrome_integers_and_compatible_strings() {
        let envelope: Envelope = serde_json::from_str(
            r#"{"subsonic-response":{"status":"ok","version":"1.16.1","type":"navidrome","musicFolders":{"musicFolder":[{"id":1,"name":"Music"},{"id":"archive","name":"Archive"}]}}}"#,
        )
        .unwrap();
        let folders = envelope
            .response
            .music_folders
            .unwrap()
            .music_folder
            .into_iter()
            .map(WireMusicFolder::into_domain)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(
            folders,
            vec![
                MusicFolder {
                    id: "1".into(),
                    name: "Music".into(),
                },
                MusicFolder {
                    id: "archive".into(),
                    name: "Archive".into(),
                },
            ]
        );
    }

    #[test]
    fn wire_song_uses_string_id_seconds_and_opaque_art() {
        let envelope: Envelope = serde_json::from_str(
            r#"{"subsonic-response":{"status":"ok","version":"1.16.1","song":{"id":"song:奇怪/id","title":"One","albumId":"album-1","album":"Record","artistId":"artist-1","artist":"Artist","coverArt":"cover?not=a-query","duration":4294968,"track":7,"unknownFutureField":{"x":1}}}}"#,
        )
        .unwrap();
        let song = envelope
            .response
            .song
            .unwrap()
            .into_domain(&profile())
            .unwrap();
        assert_eq!(song.id.id, "song:奇怪/id");
        assert_eq!(song.duration_ms, u32::MAX);
        assert_eq!(song.track_number, Some(7));
        let art = &song.album.unwrap().images[0].url;
        assert!(art.starts_with("fastpotify-art:"));
        assert!(!art.contains("not=a-query"));
    }

    #[test]
    fn song_prefers_opensubsonic_artists_array() {
        let envelope: Envelope = serde_json::from_str(
            r#"{"subsonic-response":{"status":"ok","version":"1.16.1","song":{"id":"song","title":"Duet","artistId":"legacy-id","artist":"Legacy Artist","albumId":"album","album":"Record","artists":[{"id":"artist:first/\u5947","name":"First","futureField":true},{"id":"artist-second","name":"Second"}]}}}"#,
        )
        .unwrap();
        let song = envelope
            .response
            .song
            .unwrap()
            .into_domain(&profile())
            .unwrap();

        assert_eq!(
            song.artists
                .iter()
                .map(|artist| artist.name.as_str())
                .collect::<Vec<_>>(),
            ["First", "Second"]
        );
        assert_eq!(
            song.artists[0].id.as_ref().unwrap().id,
            "artist:first/\u{5947}"
        );
        assert_eq!(song.artists[1].id.as_ref().unwrap().id, "artist-second");
        assert_eq!(
            song.artists[0].id.as_ref().unwrap().profile.as_str(),
            PROFILE
        );
        assert_eq!(song.album.unwrap().artists, song.artists);
    }

    #[test]
    fn album_prefers_opensubsonic_artists_array() {
        let envelope: Envelope = serde_json::from_str(
            r#"{"subsonic-response":{"status":"ok","version":"1.16.1","album":{"id":"album","name":"Collaboration","artistId":"legacy-id","artist":"Legacy Artist","artists":[{"id":"artist-1","name":"One"},{"id":"artist:2/opaque","name":"Two","unknown":{"nested":true}}]}}}"#,
        )
        .unwrap();
        let album = envelope
            .response
            .album
            .unwrap()
            .into_domain(&profile(), false)
            .unwrap();

        assert_eq!(
            album
                .artists
                .iter()
                .map(|artist| artist.name.as_str())
                .collect::<Vec<_>>(),
            ["One", "Two"]
        );
        assert_eq!(album.artists[1].id.as_ref().unwrap().id, "artist:2/opaque");
    }

    #[test]
    fn empty_or_missing_artists_array_falls_back_to_legacy_artist_fields() {
        let envelope: Envelope = serde_json::from_str(
            r#"{"subsonic-response":{"status":"ok","version":"1.16.1","album":{"id":"album","name":"Record","artistId":"album-artist","artist":"Album Artist","artists":[]},"song":{"id":"song","title":"Track","artistId":"song-artist","artist":"Song Artist"}}}"#,
        )
        .unwrap();
        let profile = profile();
        let album = envelope
            .response
            .album
            .unwrap()
            .into_domain(&profile, false)
            .unwrap();
        let song = envelope
            .response
            .song
            .unwrap()
            .into_domain(&profile)
            .unwrap();

        assert_eq!(album.artists[0].name, "Album Artist");
        assert_eq!(album.artists[0].id.as_ref().unwrap().id, "album-artist");
        assert_eq!(song.artists[0].name, "Song Artist");
        assert_eq!(song.artists[0].id.as_ref().unwrap().id, "song-artist");
    }

    #[test]
    fn unknown_optional_fields_do_not_break_conversion() {
        let envelope: Envelope = serde_json::from_str(
            r#"{"subsonic-response":{"status":"ok","version":"1.99.0","openSubsonic":true,"newCapability":42,"artist":{"id":"ar","name":"Artist","future":true,"album":[{"id":"al","name":"Album","duration":1}]}}}"#,
        )
        .unwrap();
        let artist = envelope
            .response
            .artist
            .unwrap()
            .into_domain(&profile())
            .unwrap();
        assert_eq!(artist.albums[0].duration_ms, 1_000);
    }

    #[test]
    fn playlist_items_keep_server_indexes_after_ui_sorting() {
        let envelope: Envelope = serde_json::from_str(
            r#"{"subsonic-response":{"status":"ok","version":"1.16.1","playlist":{"id":"playlist","name":"List","entry":[{"id":"song-a","title":"Zulu"},{"id":"song-b","title":"Alpha"}]}}}"#,
        )
        .unwrap();
        let mut playlist = envelope
            .response
            .playlist
            .unwrap()
            .into_domain(&profile())
            .unwrap();
        playlist
            .entries
            .sort_by(|left, right| left.track.name.cmp(&right.track.name));

        assert_eq!(playlist.entries[0].track.name, "Alpha");
        assert_eq!(playlist.entries[0].index, 1);
        assert_eq!(playlist.entries[1].index, 0);
    }

    #[test]
    fn readonly_playlist_is_not_editable_even_by_its_owner() {
        let envelope: Envelope = serde_json::from_str(
            r#"{"subsonic-response":{"status":"ok","version":"1.16.1","playlist":{"id":"smart","name":"Smart mix","owner":"alice","readonly":true}}}"#,
        )
        .unwrap();
        let playlist = envelope
            .response
            .playlist
            .unwrap()
            .into_domain(&profile())
            .unwrap();
        let user = User {
            id: "alice".into(),
            roles: UserRoles {
                playlist: true,
                ..UserRoles::default()
            },
            ..User::default()
        };

        assert!(playlist.readonly);
        assert!(!playlist.owned_by(&user.id));
        assert!(!playlist.editable_by(&user));
    }
}
