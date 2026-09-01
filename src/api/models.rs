//! Provider-neutral music domain models.
//!
//! OpenSubsonic JSON is decoded into private wire DTOs first. These models
//! contain server-scoped identifiers and secret-free artwork references only.

use std::fmt;
use std::str::FromStr;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use crate::auth::ProfileId;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MediaKind {
    Artist,
    Album,
    #[default]
    Song,
    Playlist,
    MusicFolder,
}

impl MediaKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Artist => "artist",
            Self::Album => "album",
            Self::Song => "song",
            Self::Playlist => "playlist",
            Self::MusicFolder => "music-folder",
        }
    }
}

impl fmt::Display for MediaKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for MediaKind {
    type Err = MediaIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "artist" => Ok(Self::Artist),
            "album" => Ok(Self::Album),
            "song" => Ok(Self::Song),
            "playlist" => Ok(Self::Playlist),
            "music-folder" => Ok(Self::MusicFolder),
            _ => Err(MediaIdError::UnknownKind),
        }
    }
}

/// An OpenSubsonic identifier scoped to one server/user profile and entity kind.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MediaId {
    pub profile: ProfileId,
    pub kind: MediaKind,
    /// The server's ID verbatim. OpenSubsonic IDs are strings, never numbers.
    #[serde(deserialize_with = "deserialize_nonempty_string")]
    pub id: String,
}

impl MediaId {
    pub fn new(profile: ProfileId, kind: MediaKind, id: impl Into<String>) -> Self {
        Self {
            profile,
            kind,
            id: id.into(),
        }
    }

    pub fn uri(&self) -> String {
        format!(
            "fastpotify:{}:{}:{}",
            self.kind,
            self.profile,
            URL_SAFE_NO_PAD.encode(self.id.as_bytes())
        )
    }

    pub fn raw(&self) -> &str {
        &self.id
    }
}

impl fmt::Display for MediaId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.uri())
    }
}

impl FromStr for MediaId {
    type Err = MediaIdError;

    fn from_str(uri: &str) -> Result<Self, Self::Err> {
        let mut parts = uri.splitn(4, ':');
        if parts.next() != Some("fastpotify") {
            return Err(MediaIdError::InvalidScheme);
        }
        let kind = parts
            .next()
            .ok_or(MediaIdError::Malformed)?
            .parse::<MediaKind>()?;
        let profile = parts
            .next()
            .ok_or(MediaIdError::Malformed)?
            .parse::<ProfileId>()
            .map_err(|_| MediaIdError::Malformed)?;
        let encoded = parts.next().ok_or(MediaIdError::Malformed)?;
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| MediaIdError::Malformed)?;
        let id = String::from_utf8(bytes).map_err(|_| MediaIdError::Malformed)?;
        if id.is_empty() {
            return Err(MediaIdError::Malformed);
        }
        Ok(Self::new(profile, kind, id))
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MediaIdError {
    #[error("not a Fastpotify media URI")]
    InvalidScheme,
    #[error("unknown media kind")]
    UnknownKind,
    #[error("malformed media URI")]
    Malformed,
}

/// Secret-free reference resolved by the active API client when artwork loads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ArtworkRef {
    pub profile: ProfileId,
    #[serde(deserialize_with = "deserialize_nonempty_string")]
    pub id: String,
}

impl ArtworkRef {
    pub fn new(profile: ProfileId, id: impl Into<String>) -> Self {
        Self {
            profile,
            id: id.into(),
        }
    }

    pub fn uri(&self) -> String {
        format!(
            "fastpotify-art:{}:{}",
            self.profile,
            URL_SAFE_NO_PAD.encode(self.id.as_bytes())
        )
    }
}

impl fmt::Display for ArtworkRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.uri())
    }
}

impl FromStr for ArtworkRef {
    type Err = ArtworkRefError;

    fn from_str(uri: &str) -> Result<Self, Self::Err> {
        let mut parts = uri.splitn(3, ':');
        if parts.next() != Some("fastpotify-art") {
            return Err(ArtworkRefError);
        }
        let profile = parts
            .next()
            .ok_or(ArtworkRefError)?
            .parse::<ProfileId>()
            .map_err(|_| ArtworkRefError)?;
        let encoded = parts.next().ok_or(ArtworkRefError)?;
        let id = URL_SAFE_NO_PAD
            .decode(encoded)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .ok_or(ArtworkRefError)?;
        if id.is_empty() {
            return Err(ArtworkRefError);
        }
        Ok(Self::new(profile, id))
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("malformed Fastpotify artwork reference")]
pub struct ArtworkRefError;

fn deserialize_nonempty_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if value.is_empty() {
        Err(serde::de::Error::custom("server id must not be empty"))
    } else {
        Ok(value)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    /// Always `fastpotify-art:...`; never an authenticated HTTP URL.
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl Image {
    pub fn from_cover_art(profile: ProfileId, id: impl Into<String>) -> Self {
        Self {
            url: ArtworkRef::new(profile, id).uri(),
            width: None,
            height: None,
        }
    }
}

pub fn pick_image(images: &[Image], target: u32) -> Option<&str> {
    let mut best: Option<&Image> = None;
    for image in images.iter().filter(|image| !image.url.is_empty()) {
        let width = image.width.unwrap_or(u32::MAX);
        match best {
            None => best = Some(image),
            Some(current) => {
                let current_width = current.width.unwrap_or(u32::MAX);
                let better = match (current_width >= target, width >= target) {
                    (true, true) => width < current_width,
                    (false, true) => true,
                    (true, false) => false,
                    (false, false) => width > current_width,
                };
                if better {
                    best = Some(image);
                }
            }
        }
    }
    best.map(|image| image.url.as_str())
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// Exact total only when the final page has been reached. OpenSubsonic
    /// list/search responses do not otherwise expose a total count.
    pub total: Option<u32>,
    pub limit: u32,
    pub offset: u32,
    /// The next local offset. OpenSubsonic list responses have no URL cursor.
    pub next: Option<u32>,
}

impl<T> Page<T> {
    pub fn from_slice(items: Vec<T>, offset: u32, limit: u32, has_more: bool) -> Self {
        let count = items.len() as u32;
        Self {
            items,
            total: (!has_more).then_some(offset.saturating_add(count)),
            limit,
            offset,
            next: has_more.then_some(offset.saturating_add(count)),
        }
    }

    pub fn next_offset(&self) -> Option<u32> {
        self.next
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtistRef {
    pub id: Option<MediaId>,
    pub name: String,
    pub uri: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artist {
    pub id: MediaId,
    pub name: String,
    pub uri: String,
    pub images: Vec<Image>,
    pub genres: Vec<String>,
    pub album_count: u32,
    pub albums: Vec<Album>,
    pub starred: bool,
    pub starred_at: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Album {
    pub id: MediaId,
    pub name: String,
    pub uri: String,
    pub images: Vec<Image>,
    pub artists: Vec<ArtistRef>,
    pub release_date: Option<String>,
    pub year: Option<u32>,
    pub genres: Vec<String>,
    pub total_tracks: Option<u32>,
    pub duration_ms: u32,
    pub tracks: Option<Page<Song>>,
    pub starred: bool,
    pub starred_at: Option<String>,
}

impl Album {
    pub fn year_label(&self) -> Option<String> {
        self.year.map(|year| year.to_string()).or_else(|| {
            self.release_date
                .as_deref()
                .map(|date| date.chars().take(4).collect())
        })
    }

    pub fn year(&self) -> Option<&str> {
        self.release_date.as_deref().map(|date| {
            let end = date
                .char_indices()
                .nth(4)
                .map_or(date.len(), |(index, _)| index);
            &date[..end]
        })
    }

    pub fn kind_label(&self) -> &'static str {
        "Album"
    }

    pub fn track_total(&self) -> u32 {
        self.total_tracks
            .or_else(|| self.tracks.as_ref().and_then(|tracks| tracks.total))
            .unwrap_or(0)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Song {
    pub id: MediaId,
    pub name: String,
    pub uri: String,
    pub duration_ms: u32,
    pub artists: Vec<ArtistRef>,
    pub album: Option<Album>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub year: Option<u32>,
    pub genres: Vec<String>,
    pub content_type: Option<String>,
    pub suffix: Option<String>,
    pub bit_rate: Option<u32>,
    pub size: Option<u64>,
    pub starred: bool,
    pub starred_at: Option<String>,
}

/// Compatibility name used by the existing UI during the migration.
pub type Track = Song;

impl Song {
    pub fn artist_names(&self) -> String {
        join_names(self.artists.iter().map(|artist| artist.name.as_str()))
    }

    pub fn image(&self, target: u32) -> Option<&str> {
        self.album
            .as_ref()
            .and_then(|album| pick_image(&album.images, target))
    }
}

/// Music-only compatibility wrapper used by queue and track-table code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PlayableItem {
    Track(Song),
}

impl Default for PlayableItem {
    fn default() -> Self {
        Self::Track(Song::default())
    }
}

impl From<Song> for PlayableItem {
    fn from(song: Song) -> Self {
        Self::Track(song)
    }
}

impl PlayableItem {
    pub fn as_track(&self) -> &Song {
        self.song()
    }

    pub fn song(&self) -> &Song {
        let Self::Track(song) = self;
        song
    }

    pub fn uri(&self) -> &str {
        &self.song().uri
    }

    pub fn id(&self) -> Option<&str> {
        Some(self.song().id.raw())
    }

    pub fn name(&self) -> &str {
        &self.song().name
    }

    pub fn duration_ms(&self) -> u32 {
        self.song().duration_ms
    }

    pub fn subtitle(&self) -> String {
        self.song().artist_names()
    }

    pub fn image(&self, target: u32) -> Option<&str> {
        self.song().image(target)
    }

    pub fn is_track(&self) -> bool {
        true
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistItem {
    /// Original zero-based position returned by `getPlaylist`. Mutations must
    /// use this value, never a filtered or sorted UI row index.
    pub index: u32,
    pub added_at: Option<String>,
    pub track: Song,
}

impl PlaylistItem {
    pub fn playable(&self) -> &Song {
        &self.track
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayHistory {
    pub track: Song,
    pub played_at: Option<String>,
    /// Opaque provider-neutral context URI when one is known.
    pub context: Option<String>,
}

pub fn join_names<'a>(names: impl Iterator<Item = &'a str>) -> String {
    names
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserRef {
    pub id: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Playlist {
    pub id: MediaId,
    pub name: String,
    pub uri: String,
    pub description: Option<String>,
    pub images: Vec<Image>,
    pub owner: UserRef,
    pub public: Option<bool>,
    /// True for smart or server-managed playlists that must not be mutated.
    #[serde(default)]
    pub readonly: bool,
    pub track_count: u32,
    pub duration_ms: u32,
    pub created: Option<String>,
    pub changed: Option<String>,
    pub entries: Vec<PlaylistItem>,
}

impl Playlist {
    pub fn track_total(&self) -> u32 {
        self.track_count.max(self.entries.len() as u32)
    }

    pub fn owner_name(&self) -> &str {
        self.owner
            .display_name
            .as_deref()
            .or(self.owner.id.as_deref())
            .unwrap_or("")
    }

    pub fn owned_by(&self, user_id: &str) -> bool {
        !self.readonly && self.owner.id.as_deref() == Some(user_id)
    }

    pub fn editable_by(&self, user: &User) -> bool {
        user.roles.playlist && self.owned_by(&user.id)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Favorites {
    pub artists: Vec<Artist>,
    pub albums: Vec<Album>,
    pub songs: Vec<Song>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResults {
    pub tracks: Option<Page<Song>>,
    pub artists: Option<Page<Artist>>,
    pub albums: Option<Page<Album>>,
    /// `search3` has no playlists; the client fills this by filtering the
    /// user's playlist metadata locally.
    pub playlists: Option<Page<Playlist>>,
}

impl SearchResults {
    pub fn is_empty(&self) -> bool {
        self.tracks
            .as_ref()
            .is_none_or(|page| page.items.is_empty())
            && self
                .artists
                .as_ref()
                .is_none_or(|page| page.items.is_empty())
            && self
                .albums
                .as_ref()
                .is_none_or(|page| page.items.is_empty())
            && self
                .playlists
                .as_ref()
                .is_none_or(|page| page.items.is_empty())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub display_name: Option<String>,
    pub scrobbling_enabled: bool,
    pub max_bit_rate: Option<u32>,
    pub roles: UserRoles,
}

impl User {
    pub fn name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.id)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserRoles {
    pub admin: bool,
    pub settings: bool,
    pub download: bool,
    pub upload: bool,
    pub playlist: bool,
    pub cover_art: bool,
    pub comment: bool,
    pub podcast: bool,
    pub stream: bool,
    pub jukebox: bool,
    pub share: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MusicFolder {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenSubsonicExtension {
    pub name: String,
    pub versions: Vec<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerCapabilities {
    pub protocol_version: String,
    pub server_type: Option<String>,
    pub server_version: Option<String>,
    pub open_subsonic: bool,
    pub extensions: Vec<OpenSubsonicExtension>,
}

impl ServerCapabilities {
    pub fn supports(&self, name: &str, version: u32) -> bool {
        self.extensions
            .iter()
            .any(|extension| extension.name == name && extension.versions.contains(&version))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedServer {
    pub profile: ProfileId,
    pub user: User,
    pub music_folders: Vec<MusicFolder>,
    pub capabilities: ServerCapabilities,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lyrics {
    pub artist: Option<String>,
    pub title: Option<String>,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlbumListType {
    Random,
    #[default]
    Newest,
    Highest,
    Frequent,
    Recent,
    AlphabeticalByName,
    AlphabeticalByArtist,
    Starred,
    ByYear,
    ByGenre,
}

impl AlbumListType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Random => "random",
            Self::Newest => "newest",
            Self::Highest => "highest",
            Self::Frequent => "frequent",
            Self::Recent => "recent",
            Self::AlphabeticalByName => "alphabeticalByName",
            Self::AlphabeticalByArtist => "alphabeticalByArtist",
            Self::Starred => "starred",
            Self::ByYear => "byYear",
            Self::ByGenre => "byGenre",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlbumListRequest {
    pub kind: AlbumListType,
    pub offset: u32,
    pub limit: u32,
    pub from_year: Option<i32>,
    pub to_year: Option<i32>,
    pub genre: Option<String>,
    pub music_folder_id: Option<String>,
}

impl Default for AlbumListRequest {
    fn default() -> Self {
        Self {
            kind: AlbumListType::Newest,
            offset: 0,
            limit: 50,
            from_year: None,
            to_year: None,
            genre: None,
            music_folder_id: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchOptions {
    pub artist_offset: u32,
    pub artist_count: u32,
    pub album_offset: u32,
    pub album_count: u32,
    pub song_offset: u32,
    pub song_count: u32,
    pub music_folder_id: Option<String>,
    pub include_playlists: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            artist_offset: 0,
            artist_count: 20,
            album_offset: 0,
            album_count: 20,
            song_offset: 0,
            song_count: 50,
            music_folder_id: None,
            include_playlists: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RandomSongsRequest {
    pub size: u32,
    pub genre: Option<String>,
    pub from_year: Option<u32>,
    pub to_year: Option<u32>,
    pub music_folder_id: Option<String>,
}

impl Default for RandomSongsRequest {
    fn default() -> Self {
        Self {
            size: 50,
            genre: None,
            from_year: None,
            to_year: None,
            music_folder_id: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaylistUpdate {
    pub name: Option<String>,
    pub description: Option<String>,
    pub public: Option<bool>,
    pub songs_to_add: Vec<MediaId>,
    pub song_indexes_to_remove: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scrobble {
    pub song: MediaId,
    /// Unix timestamp in milliseconds, as required by OpenSubsonic.
    pub time_ms: Option<u64>,
}

impl Scrobble {
    pub fn now(song: MediaId) -> Self {
        Self {
            song,
            time_ms: None,
        }
    }
}

pub(crate) fn seconds_to_millis(seconds: Option<u64>) -> u32 {
    seconds
        .unwrap_or_default()
        .saturating_mul(1_000)
        .min(u32::MAX as u64) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arbitrary_string_media_id_round_trips() {
        let original = MediaId::new(
            ProfileId::new("0123456789abcdef0123456789abcdef01234567"),
            MediaKind::Song,
            "slashes/colons: spaces 中文 \0 and ?&#",
        );
        let uri = original.uri();
        assert_eq!(uri.parse::<MediaId>().unwrap(), original);
        assert!(!uri.contains("slashes/colons"));
    }

    #[test]
    fn artwork_reference_is_opaque_and_secret_free() {
        let art = ArtworkRef::new(
            ProfileId::new("0123456789abcdef0123456789abcdef01234567"),
            "cover/id?x=1&t=secret",
        );
        let uri = art.uri();
        assert!(uri.starts_with("fastpotify-art:"));
        assert!(!uri.contains("t=secret"));
        assert_eq!(uri.parse::<ArtworkRef>().unwrap(), art);
    }

    #[test]
    fn seconds_convert_without_overflow() {
        assert_eq!(seconds_to_millis(Some(123)), 123_000);
        assert_eq!(seconds_to_millis(None), 0);
        assert_eq!(seconds_to_millis(Some(u64::MAX)), u32::MAX);
    }

    #[test]
    fn page_total_is_known_only_after_the_last_page() {
        let full = Page::from_slice(vec![1, 2], 0, 2, true);
        assert_eq!(full.total, None);
        assert_eq!(full.next, Some(2));

        let last = Page::from_slice(vec![3], 2, 2, false);
        assert_eq!(last.total, Some(3));
        assert_eq!(last.next, None);
    }

    #[test]
    fn malformed_unicode_release_date_never_panics() {
        let album = Album {
            release_date: Some("音楽年".into()),
            ..Album::default()
        };
        assert_eq!(album.year(), Some("音楽年"));
    }

    #[test]
    fn media_and_art_refs_reject_invalid_profile_fingerprints() {
        assert!("fastpotify:song:profile:c29uZw".parse::<MediaId>().is_err());
        assert!(
            "fastpotify-art:profile:Y292ZXI"
                .parse::<ArtworkRef>()
                .is_err()
        );
        let profile = "0123456789abcdef0123456789abcdef01234567";
        assert!(
            format!("fastpotify:song:{profile}:")
                .parse::<MediaId>()
                .is_err()
        );
        assert!(
            format!("fastpotify-art:{profile}:")
                .parse::<ArtworkRef>()
                .is_err()
        );
        assert!(
            serde_json::from_str::<MediaId>(&format!(
                r#"{{"profile":"{profile}","kind":"song","id":""}}"#
            ))
            .is_err()
        );
        assert!(
            serde_json::from_str::<ArtworkRef>(&format!(r#"{{"profile":"{profile}","id":""}}"#))
                .is_err()
        );
        let default_song = Song::default();
        assert_eq!(default_song.id.profile.as_str().len(), 40);
    }

    #[test]
    fn image_picker_prefers_smallest_sufficient_image() {
        let images = vec![
            Image {
                url: "large".into(),
                width: Some(640),
                height: Some(640),
            },
            Image {
                url: "small".into(),
                width: Some(64),
                height: Some(64),
            },
            Image {
                url: "medium".into(),
                width: Some(300),
                height: Some(300),
            },
        ];
        assert_eq!(pick_image(&images, 100), Some("medium"));
    }
}
