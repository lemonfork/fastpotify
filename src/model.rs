//! UI state, loaded data, and pending actions.

use std::time::Instant;

use crate::api::models::*;

/// Every screen the central panel can show.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Page {
    Home,
    Search,
    Favorites,
    Albums,
    Artists,
    Playlist(MediaId),
    Album(MediaId),
    Artist(MediaId),
    Queue,
    Settings,
}

impl Page {
    pub fn encode(&self) -> String {
        match self {
            Page::Home => "home".into(),
            Page::Search => "search".into(),
            Page::Favorites => "favorites".into(),
            Page::Albums => "albums".into(),
            Page::Artists => "artists".into(),
            Page::Playlist(id) => format!("playlist|{}", id.uri()),
            Page::Album(id) => format!("album|{}", id.uri()),
            Page::Artist(id) => format!("artist|{}", id.uri()),
            Page::Queue => "queue".into(),
            Page::Settings => "settings".into(),
        }
    }

    pub fn decode(text: &str) -> Option<Self> {
        Some(match text {
            "home" => Page::Home,
            "search" => Page::Search,
            // The old label is accepted as a harmless navigation preference;
            // old provider media references still fail strict parsing below.
            "favorites" | "liked" => Page::Favorites,
            "albums" => Page::Albums,
            "artists" => Page::Artists,
            "queue" => Page::Queue,
            "settings" => Page::Settings,
            other => {
                let (kind, encoded) = other.split_once('|')?;
                let id = encoded.parse::<MediaId>().ok()?;
                match kind {
                    "playlist" if id.kind == MediaKind::Playlist => Page::Playlist(id),
                    "album" if id.kind == MediaKind::Album => Page::Album(id),
                    "artist" if id.kind == MediaKind::Artist => Page::Artist(id),
                    _ => return None,
                }
            }
        })
    }

    /// Opens a canonical, server-scoped media reference.
    pub fn from_uri(uri: &str) -> Option<Self> {
        let id = uri.parse::<MediaId>().ok()?;
        Some(match id.kind {
            MediaKind::Playlist => Page::Playlist(id),
            MediaKind::Album => Page::Album(id),
            MediaKind::Artist => Page::Artist(id),
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum QueueTab {
    #[default]
    Queue,
    Recents,
}

impl QueueTab {
    pub fn encode(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Recents => "recents",
        }
    }

    pub fn decode(text: &str) -> Option<Self> {
        match text {
            "queue" => Some(Self::Queue),
            "recents" => Some(Self::Recents),
            // Backward compatibility with the old tab name.
            "recently_played" => Some(Self::Recents),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Loadable<T> {
    #[default]
    NotLoaded,
    Loading,
    Loaded(T),
    Failed(String),
}

impl<T> Loadable<T> {
    pub fn get(&self) -> Option<&T> {
        match self {
            Loadable::Loaded(value) => Some(value),
            _ => None,
        }
    }

    pub fn get_mut(&mut self) -> Option<&mut T> {
        match self {
            Loadable::Loaded(value) => Some(value),
            _ => None,
        }
    }

    pub fn is_loading(&self) -> bool {
        matches!(self, Loadable::Loading)
    }

    pub fn needs_load(&self) -> bool {
        matches!(self, Loadable::NotLoaded | Loadable::Failed(_))
    }

    pub fn from_result<E: std::fmt::Display>(result: Result<T, E>) -> Self {
        match result {
            Ok(value) => Loadable::Loaded(value),
            Err(error) => Loadable::Failed(error.to_string()),
        }
    }

    /// Keeps an already loaded value when a refresh fails.
    pub fn refresh<E: std::fmt::Display>(&mut self, result: Result<T, E>) {
        if result.is_ok() || self.get().is_none() {
            *self = Self::from_result(result);
        }
    }
}

/// An offset-paginated list that loads on demand as the user scrolls.
#[derive(Clone, Debug)]
pub struct PagedList<T> {
    pub items: Vec<T>,
    pub total: Option<u32>,
    pub next_offset: Option<u32>,
    pub loading: bool,
    pub error: Option<String>,
    pub loaded_once: bool,
    pub revision: u64,
}

impl<T> Default for PagedList<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            total: None,
            next_offset: Some(0),
            loading: false,
            error: None,
            loaded_once: false,
            revision: 0,
        }
    }
}

impl<T> PagedList<T> {
    pub fn reset(&mut self) {
        *self = Self {
            revision: self.revision.wrapping_add(1),
            ..Default::default()
        };
    }

    pub fn can_load_more(&self) -> bool {
        !self.loading && self.next_offset.is_some()
    }

    pub fn is_complete(&self) -> bool {
        self.loaded_once && self.next_offset.is_none()
    }

    pub fn absorb(&mut self, offset: u32, page: Page_<T>) {
        if offset == 0 {
            self.items.clear();
        }
        if (offset as usize) < self.items.len() {
            self.items.truncate(offset as usize);
        }
        let next_offset = page.next_offset();
        self.items.extend(page.items);
        self.total = page.total;
        self.next_offset = next_offset;
        self.loading = false;
        self.error = None;
        self.loaded_once = true;
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn retain<F>(&mut self, f: F)
    where
        F: FnMut(&T) -> bool,
    {
        self.items.retain(f);
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn reorder(&mut self, from: usize, to: usize) {
        if from < self.items.len() && to <= self.items.len() {
            let item = self.items.remove(from);
            let insert_at = if to > from { to - 1 } else { to };
            self.items.insert(insert_at.min(self.items.len()), item);
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub fn set_cached(&mut self, items: Vec<T>) {
        self.total = Some(items.len() as u32);
        self.items = items;
        self.next_offset = None;
        self.loading = false;
        self.loaded_once = true;
        self.error = None;
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn fail(&mut self, error: String) {
        self.loading = false;
        self.error = Some(error);
        self.loaded_once = true;
    }
}

type Page_<T> = crate::api::models::Page<T>;

/// Selected track-table rows for batch actions.
///
/// Selection belongs to one page and clears when sorting, filtering, or paging
/// changes the row order.
#[derive(Clone, Debug, Default)]
pub struct RowSelection {
    pub rows: std::collections::BTreeSet<usize>,
    /// Row used as the anchor for shift-click ranges.
    pub anchor: Option<usize>,
}

/// Selection behavior for a row click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowPick {
    /// Select only this row.
    Only,
    /// Toggle this row.
    Toggle,
    /// Everything from the anchor to here.
    Range,
}

#[derive(Default)]
pub struct Library {
    pub playlists: Loadable<Vec<Playlist>>,
    pub favorite_songs: PagedList<Song>,
    pub albums: PagedList<Album>,
    pub artists: PagedList<Artist>,
    pub filter: String,
}

#[derive(Default)]
pub struct HomeData {
    pub recently_added: Loadable<Vec<Album>>,
    pub recently_played: Loadable<Vec<PlayHistory>>,
    pub frequent_albums: Loadable<Vec<Album>>,
    pub random_songs: Loadable<Vec<Song>>,
    pub generation: u64,
    pub requested: bool,
    pub loaded_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchFilter {
    #[default]
    All,
    Songs,
    Artists,
    Albums,
    Playlists,
}

impl SearchFilter {
    pub const ALL: [SearchFilter; 5] = [
        Self::All,
        Self::Songs,
        Self::Artists,
        Self::Albums,
        Self::Playlists,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Songs => "Songs",
            Self::Artists => "Artists",
            Self::Albums => "Albums",
            Self::Playlists => "Playlists",
        }
    }
}

#[derive(Default)]
pub struct SearchState {
    pub query: String,
    pub committed: String,
    pub serial: u64,
    pub results: Loadable<SearchResults>,
    pub filter: SearchFilter,
    pub typed_at: Option<Instant>,
    pub focus_requested: bool,
}

#[derive(Default)]
pub struct PlaylistPage {
    pub generation: u64,
    pub playlist: Loadable<Playlist>,
    pub items: PagedList<PlaylistItem>,
    pub filter: String,
}

#[derive(Default)]
pub struct AlbumPage {
    pub album: Loadable<Album>,
    pub tracks: PagedList<Track>,
}

#[derive(Default)]
pub struct ArtistPage {
    pub artist: Loadable<Artist>,
    pub albums: PagedList<Album>,
}

/// A table's sort, chosen by clicking a column heading.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TableSort {
    pub column: SortColumn,
    pub ascending: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SortColumn {
    Title,
    Album,
    Added,
    Duration,
    /// The list's own order, for playing it reversed from the # heading.
    Index,
}

/// Playback and action context for a track row.
#[derive(Clone, Debug, PartialEq)]
pub enum RowContext {
    /// An album or playlist played from the selected row.
    Context {
        context: MediaId,
        /// The playlist id when the user owns it, enabling removal.
        editable_playlist: Option<MediaId>,
    },
    /// A loose list of songs shown in their exact play order.
    Songs(Vec<Song>),
    /// A Next up occurrence. Identity, rather than song ID, distinguishes
    /// duplicate rows and lets the player consume exactly through this row.
    Queue(crate::player::OccurrenceId),
    /// A sorted or filtered context view that plays the displayed rows.
    View { songs: Vec<Song>, context: MediaId },
}

/// Track data held during a drag.
#[derive(Clone, Debug)]
pub struct DragTrack {
    pub song: Song,
    /// Source playlist ID and row index for moves within an editable playlist.
    pub from: Option<(MediaId, u32)>,
}

#[derive(Clone, Debug)]
pub enum Dialog {
    CreatePlaylist {
        name: String,
        public: bool,
        songs: Vec<Song>,
    },
    EditPlaylist {
        id: MediaId,
        name: String,
        description: String,
        public: bool,
    },
    ConfirmDeletePlaylist {
        id: MediaId,
        name: String,
    },
    Shortcuts,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Error,
}

#[derive(Clone, Debug)]
pub struct Toast {
    pub message: String,
    pub kind: ToastKind,
    pub created: Instant,
}

/// Actions emitted while drawing and applied afterward to avoid borrow conflicts.
#[derive(Clone, Debug)]
pub enum Action {
    Open(Page),
    /// A media reference entering through CLI, MPRIS, or loopback IPC. App
    /// must parse and profile-check it before turning it into a typed action.
    OpenUri(String),
    Back,
    Forward,
    PlayContext {
        context: MediaId,
        offset: Option<MediaId>,
        offset_index: Option<u32>,
    },
    PlaySongs {
        songs: Vec<Song>,
        index: u32,
    },
    PlayFromRow {
        context: RowContext,
        song: Song,
        index: u32,
    },
    /// Play one exact row from the authoritative local queue.
    PlayQueueOccurrence(crate::player::OccurrenceId),
    ShufflePlay(MediaId),
    TogglePlay,
    Next,
    Previous,
    Seek(u32),
    SeekBy(i64),
    SetVolume(u8),
    /// Preview volume locally during a drag.
    PreviewVolume(u8),
    VolumeBy(i8),
    ToggleMute,
    ToggleShuffle,
    CycleRepeat,
    SetShuffle(bool),
    SetRepeat(crate::player::RepeatMode),
    AddToQueue {
        song: Song,
        label: String,
    },
    ToggleFavorite(MediaId),
    /// Queue several songs in order and show one notification.
    QueueMany {
        songs: Vec<Song>,
    },
    /// Set favorite state for several entities explicitly.
    SetFavoriteMany {
        ids: Vec<MediaId>,
        favorite: bool,
    },
    AddToPlaylist {
        playlist_id: MediaId,
        playlist_name: String,
        songs: Vec<Song>,
    },
    RemoveFromPlaylist {
        playlist_id: MediaId,
        row_indices: Vec<u32>,
    },
    /// Replace an editable playlist with this permutation of its original
    /// server row indices. Row identity, rather than song ID, preserves
    /// duplicate occurrences while a drag is in flight.
    ReorderPlaylist {
        playlist_id: MediaId,
        ordered_row_indices: Vec<u32>,
    },
    ShowDialog(Dialog),
    CloseDialog,
    CreatePlaylist {
        name: String,
        public: bool,
        songs: Vec<Song>,
    },
    UpdatePlaylist {
        id: MediaId,
        name: String,
        description: String,
        public: bool,
    },
    DeletePlaylist(MediaId),
    /// Empty Next up of its queued songs, keeping the context's own.
    ClearQueue,
    /// Save the current and upcoming queue as a playlist.
    SaveQueueAsPlaylist,
    CopyLink(MediaId),
    /// Open a web page in the browser.
    OpenUrl(String),
    Search(String),
    SetSearchFilter(SearchFilter),
    FocusSearch,
    LoadMore(Page),
    LoadMoreRecents,
    ReloadRecents,
    SetQueueTab(QueueTab),
    Reload(Page),
    SignIn,
    SignOut,
    ToggleSidebar,
    ToggleQueuePanel,
    ToggleLyricsPanel,
    SettingsChanged,
    RestartEngine,
    ShowWindow,
    HideWindow,
    ClearArtCache,
    /// Clear local play history.
    ClearPlayHistory,
    /// Open or close the Winamp window.
    ToggleWinampWindow,
    /// Select a skin, or the built-in skin for `None`.
    SetSkin(Option<String>),
    /// Install and select a skin file.
    InstallSkin(std::path::PathBuf),
    /// Screen pixels per skin pixel in the Winamp window.
    SetSkinScale(u8),
    ToggleWinampOnTop,
    OpenSkinsFolder,
    /// Cycle bars, scope, and off.
    CycleVisualiser,
    /// Set the visualizer mode directly.
    SetVisualiser(crate::settings::VisMode),
    /// Open or close the playlist window under the mini player.
    ToggleWinampPlaylist,
    /// The playlist window's height, in skin pixels.
    SetPlaylistHeight(u32),
    /// Open or close the equalizer window under the mini player.
    ToggleWinampEq,
    /// Switch the equalizer's effect on the sound on or off.
    ToggleEq,
    SetEqBand(usize, f32),
    SetEqPreamp(f32),
    /// One of Winamp's presets, by its place in the list.
    ApplyEqPreset(usize),
    /// The balance, -1 all left to 1 all right.
    SetBalance(f32),
    ToggleMono,
    /// Roll the playlist window up to its title bar, or down again.
    ToggleWinampPlaylistShade,
    /// Roll the equalizer window up to its title bar, or down again.
    ToggleWinampEqShade,
    /// Close the window the way its close button does: into the tray when
    /// that is on, out of the app otherwise.
    CloseWindow,
    /// Roll the main window up to its title bar, or down again.
    ToggleWinampShade,
    Quit,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media(kind: MediaKind, raw: &str) -> MediaId {
        MediaId::new(
            ProfileId::new("0123456789abcdef0123456789abcdef01234567"),
            kind,
            raw,
        )
    }

    #[test]
    fn typed_pages_round_trip_arbitrary_server_ids() {
        for page in [
            Page::Playlist(media(MediaKind::Playlist, "list:/ 中文")),
            Page::Album(media(MediaKind::Album, "album:?&")),
            Page::Artist(media(MediaKind::Artist, "artist:with:colons")),
        ] {
            assert_eq!(Page::decode(&page.encode()), Some(page));
        }
    }

    #[test]
    fn page_kind_and_legacy_provider_refs_cannot_cross_the_boundary() {
        let song = media(MediaKind::Song, "song");
        assert_eq!(Page::from_uri(&song.uri()), None);
        assert_eq!(Page::decode(&format!("album|{}", song.uri())), None);
        assert_eq!(Page::from_uri("legacy:track:old-id"), None);
        assert_eq!(Page::decode("playlist:legacy"), None);
    }
}
