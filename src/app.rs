//! Application state, backend events, and actions emitted by the views.
//!
//! Drawing only appends [`Action`] values. They are applied after the view
//! releases its borrows. Playback and the queue are always projected from the
//! local player's authoritative [`PlaybackSnapshot`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::Color32;

use crate::api::{
    Album, AlbumListRequest, AlbumListType, Artist, Favorites, MediaId, MediaKind, PlayHistory,
    PlayableItem, Playlist, PlaylistItem, ProfileId, RandomSongsRequest, SearchOptions,
    SearchResults, Song, User, UserRef,
};
use crate::auth::Credentials;
use crate::backend::{
    ApiPayload, ApiRequest, ApiResponse, AuthStatus, Backend, Command, Event, RequestId,
    SessionEpoch, Waker,
};
use crate::media::{MediaCommand, MediaState, MediaTrack};
use crate::media_controls::MediaService;
use crate::model::*;
use crate::paths::AppDirs;
use crate::player::{
    EngineConfig, LoadContext, Playback, PlaybackSnapshot, PlayerCommand, QueueEntry, RepeatMode,
};
use crate::settings::{SessionState, Settings, ThemeChoice};
use crate::single_instance::ControlCommand;
use crate::theme::{self, Palette};
use crate::tray::{TrayCommand, TrayService};

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(280);
const SAVE_DELAY: Duration = Duration::from_secs(2);
const TOAST_LIFETIME: Duration = Duration::from_millis(3_200);
const ART_EVICTION_INTERVAL: Duration = Duration::from_secs(20);
const PLAYER_REPAINT_INTERVAL: Duration = Duration::from_millis(250);
const DAILY_MIX_CHECK_INTERVAL: Duration = Duration::from_secs(60);
const ALBUM_PAGE_SIZE: u32 = 50;
const RANDOM_MIX_REFILL_THRESHOLD: usize = 3;

#[derive(Clone, Debug, PartialEq)]
pub struct NowPlaying {
    pub song: Song,
    pub position_ms: u32,
    pub playing: bool,
    pub loading: bool,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub volume_percent: u8,
    pub can_control: bool,
    pub resuming: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QueueView {
    pub current: Option<QueueEntry>,
    pub rows: Vec<QueueEntry>,
    pub manual_len: usize,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct AppOptions {
    pub media_controls: bool,
    pub tray: bool,
    /// Prevents credential restore and every network request. Demo mode sets
    /// this before the backend starts, so there is no sign-in race.
    pub offline: bool,
}

impl Default for AppOptions {
    fn default() -> Self {
        Self {
            media_controls: true,
            tray: true,
            offline: false,
        }
    }
}

#[derive(Clone)]
struct FavoriteBefore {
    value: bool,
    song: Option<Song>,
}

#[derive(Clone)]
enum PlaylistBefore {
    Create(MediaId),
    Update(Playlist),
    Entries(MediaId, Vec<PlaylistItem>),
    Delete(Playlist),
}

enum RequestPurpose {
    Home,
    RandomMix,
    RandomMixContinuation,
    LibraryAlbums(u32),
    LibraryArtists,
    Playlists,
    Favorites,
    Search(u64),
    Playlist(MediaId),
    Album(MediaId),
    Artist(MediaId),
    Favorite(MediaId, Box<FavoriteBefore>),
    PlaylistMutation(Box<PlaylistBefore>),
    PlaySong,
    PlayContext {
        context: MediaId,
        offset: Option<MediaId>,
        offset_index: Option<u32>,
        shuffle: bool,
    },
    PlayArtist {
        context: MediaId,
        offset: Option<MediaId>,
        offset_index: Option<u32>,
        shuffle: bool,
    },
    ResumeSong(u32),
    ResumeContext(MediaId, u32),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum RequestKey {
    Home,
    RandomMix,
    RandomMixContinuation,
    LibraryAlbums,
    LibraryArtists,
    Playlists,
    Favorites,
    Search,
    Playlist(MediaId),
    Album(MediaId),
    Artist(MediaId),
    Favorite(MediaId),
    PlaylistMutation(MediaId),
    Play,
}

impl PlaylistBefore {
    fn target(&self) -> MediaId {
        match self {
            Self::Create(id) | Self::Entries(id, _) => id.clone(),
            Self::Update(playlist) | Self::Delete(playlist) => playlist.id.clone(),
        }
    }
}

impl RequestPurpose {
    fn key(&self) -> RequestKey {
        match self {
            Self::Home => RequestKey::Home,
            Self::RandomMix => RequestKey::RandomMix,
            Self::RandomMixContinuation => RequestKey::RandomMixContinuation,
            Self::LibraryAlbums(_) => RequestKey::LibraryAlbums,
            Self::LibraryArtists => RequestKey::LibraryArtists,
            Self::Playlists => RequestKey::Playlists,
            Self::Favorites => RequestKey::Favorites,
            Self::Search(_) => RequestKey::Search,
            Self::Playlist(id) => RequestKey::Playlist(id.clone()),
            Self::Album(id) => RequestKey::Album(id.clone()),
            Self::Artist(id) => RequestKey::Artist(id.clone()),
            Self::Favorite(id, _) => RequestKey::Favorite(id.clone()),
            Self::PlaylistMutation(before) => RequestKey::PlaylistMutation(before.target()),
            Self::PlaySong
            | Self::PlayContext { .. }
            | Self::PlayArtist { .. }
            | Self::ResumeSong(_)
            | Self::ResumeContext(_, _) => RequestKey::Play,
        }
    }
}

#[derive(Default)]
struct ResumeState {
    context: Option<MediaId>,
    track: Option<MediaId>,
    position_ms: u32,
    manual: Vec<Song>,
    requested: bool,
    applied: bool,
}

#[derive(Clone, Copy, Default)]
struct RandomMixPlayback {
    last_refill_play_instance: Option<u64>,
    pause_requested: bool,
}

pub struct App {
    pub dirs: AppDirs,
    pub settings: Settings,
    pub backend: Backend,
    pub palette: Palette,
    pub auth: AuthStatus,
    pub user: Option<User>,
    pub login_server: String,
    pub login_username: String,
    pub login_password: String,

    pub home: HomeData,
    pub library: Library,
    pub search: SearchState,
    pub playlist_pages: HashMap<MediaId, PlaylistPage>,
    pub album_pages: HashMap<MediaId, AlbumPage>,
    pub artist_pages: HashMap<MediaId, ArtistPage>,
    pub table_sorts: HashMap<Page, TableSort>,
    pub recents: PagedList<PlayHistory>,
    pub recents_view: Vec<PlayHistory>,
    pub queue_tab: QueueTab,

    pub dialog: Option<Dialog>,
    pub dialog_rect: Option<egui::Rect>,
    pub playlist_busy: bool,
    pub show_queue_panel: bool,
    pub show_lyrics_panel: bool,
    pub lyrics: Loadable<Option<crate::lyrics::Lyrics>>,
    pub lyrics_following: bool,
    pub lyrics_line_shown: Option<Option<usize>>,
    pub seek_preview: Option<u32>,
    pub volume_preview: Option<u8>,
    pub toasts: Vec<Toast>,
    pub actions: Vec<Action>,
    pub update: Option<crate::updates::Release>,
    pub winamp: crate::winamp::WinampState,

    pub window_hidden: bool,
    pub hide_intent: bool,
    pub switch_intent: bool,
    pub quit_requested: bool,
    pub offline: bool,

    media_controls: Option<MediaService>,
    tray: Option<TrayService>,
    control_commands: Option<Arc<std::sync::Mutex<Vec<ControlCommand>>>>,
    control_now_playing: Option<Arc<std::sync::Mutex<String>>>,
    active_profile: Option<ProfileId>,
    active_epoch: SessionEpoch,
    playback: PlaybackSnapshot,
    player_revision_floor: u64,
    playing_context: Option<MediaId>,
    random_mix_playback: Option<RandomMixPlayback>,
    restored_preview: bool,
    pending_play: HashSet<MediaId>,
    volume_before_mute: Option<u8>,

    navigation: Vec<Page>,
    navigation_index: usize,
    load_generation: u64,
    requests: HashMap<RequestId, RequestPurpose>,
    latest_generations: HashMap<RequestKey, u64>,
    saved: HashMap<MediaId, bool>,
    pending_playlist_ops: usize,
    selection: Option<(Page, String, RowSelection)>,

    resume: ResumeState,
    lyrics_media: Option<MediaId>,
    lyrics_request: Option<RequestId>,
    accents: HashMap<String, Color32>,
    accent_requests: HashMap<RequestId, String>,

    settings_dirty: bool,
    session_dirty: bool,
    last_settings_save: Instant,
    last_session_save: Instant,
    applied_dark: Option<bool>,
    zoom_applied: bool,
    last_eviction: Instant,
    last_update_check: Option<Instant>,
    last_history_refresh: Instant,
    history_generation_floor: u64,
    daily_mix_day: Option<String>,
    last_daily_mix_check: Instant,
    window_title: String,
    session_window_size: Option<[f32; 2]>,
    session_window_pos: Option<[f32; 2]>,
    last_window_size: Option<[f32; 2]>,
    last_window_pos: Option<[f32; 2]>,
}

impl App {
    pub fn new(waker: &Waker, dirs: AppDirs, mut settings: Settings, options: AppOptions) -> Self {
        let credentials = (!options.offline)
            .then(|| Credentials::load(&dirs.credentials_file()).ok())
            .flatten();
        let profile = credentials.as_ref().map(Credentials::profile_id);
        let session = profile
            .as_ref()
            .map(|profile| SessionState::load(&dirs.session_file(profile)))
            .unwrap_or_default();
        let history_store = profile
            .as_ref()
            .map(|profile| crate::history::History::load(&dirs.history_file(profile), profile))
            .unwrap_or_default();
        let today = crate::mixes::local_day_key();
        let cached_daily_mix = profile.as_ref().and_then(|profile| {
            crate::mixes::DailyMixCache::load(
                &dirs.daily_mix_file(profile),
                &today,
                profile,
                crate::mixes::MIX_SIZE,
            )
        });
        let daily_mix_day = cached_daily_mix.as_ref().map(|_| today);
        let daily_mix = match (profile.is_some(), cached_daily_mix) {
            (_, Some(songs)) => Loadable::Loaded(songs),
            (true, None) => Loadable::Loading,
            (false, None) => Loadable::NotLoaded,
        };

        let first_page = session
            .last_page
            .as_deref()
            .and_then(Page::decode)
            .filter(|page| page_has_profile(page, profile.as_ref()))
            .filter(|page| !matches!(page, Page::Queue | Page::Settings))
            .unwrap_or(Page::Home);
        let table_sorts = session
            .sorts
            .iter()
            .filter_map(|(encoded, sort)| {
                let page = Page::decode(encoded)?;
                page_has_profile(&page, profile.as_ref()).then_some((page, *sort))
            })
            .collect();
        let manual = session
            .last_queue_rows
            .iter()
            .map(PlayableItem::as_track)
            .filter(|song| profile.as_ref() == Some(&song.id.profile))
            .cloned()
            .collect();
        let resume = ResumeState {
            context: parse_profile_ref(session.last_context.as_deref(), profile.as_ref()),
            track: parse_profile_ref(session.last_track.as_deref(), profile.as_ref()),
            position_ms: session.last_position_ms,
            manual,
            requested: false,
            applied: false,
        };
        settings
            .pinned_contexts
            .retain(|reference| parse_profile_ref(Some(reference), profile.as_ref()).is_some());

        let tap = crate::vis::AudioTap::new();
        let eq = crate::eq::shared();
        if let Ok(mut shared) = eq.lock() {
            *shared = eq_settings(&settings);
        }
        let config = engine_config(&settings, Arc::clone(&tap), Arc::clone(&eq));
        let mut backend = Backend::spawn(dirs.clone(), config, waker.clone(), !options.offline);
        backend.set_offline(options.offline);
        let media_controls = options.media_controls.then(|| {
            let waker = waker.clone();
            MediaService::spawn(move || waker.wake())
        });
        let tray = options
            .tray
            .then(|| {
                let waker = waker.clone();
                TrayService::spawn(move || waker.wake())
            })
            .flatten();
        let (login_server, login_username) = credentials
            .as_ref()
            .map(|credentials| {
                (
                    credentials.server().to_owned(),
                    credentials.username().to_owned(),
                )
            })
            .unwrap_or_default();

        let mut recents = PagedList::default();
        recents.set_cached(history_store.plays().to_vec());
        let recents_view = history_store.plays().to_vec();
        let playback = PlaybackSnapshot {
            volume: settings.volume,
            shuffle: session.shuffle_on,
            ..PlaybackSnapshot::default()
        };

        Self {
            dirs,
            settings,
            backend,
            palette: Palette::dark(),
            // An offline instance has no authentication work to wait for.
            // Its backend still boots on a worker and may publish the normal
            // Starting/SignedOut pair later; `receive_auth` rejects those
            // same-epoch bootstrap events so demo state cannot be undone.
            auth: if options.offline {
                AuthStatus::SignedOut
            } else {
                AuthStatus::Starting
            },
            user: None,
            login_server,
            login_username,
            login_password: String::new(),
            home: HomeData {
                daily_mix,
                recently_played: Loadable::Loaded(history_store.plays().to_vec()),
                ..HomeData::default()
            },
            library: Library::default(),
            search: SearchState::default(),
            playlist_pages: HashMap::new(),
            album_pages: HashMap::new(),
            artist_pages: HashMap::new(),
            table_sorts,
            recents,
            recents_view,
            queue_tab: session
                .queue_tab
                .as_deref()
                .and_then(QueueTab::decode)
                .unwrap_or_default(),
            dialog: None,
            dialog_rect: None,
            playlist_busy: false,
            show_queue_panel: session.queue_open.unwrap_or(false),
            show_lyrics_panel: false,
            lyrics: Loadable::NotLoaded,
            lyrics_following: true,
            lyrics_line_shown: None,
            seek_preview: None,
            volume_preview: None,
            toasts: Vec::new(),
            actions: Vec::new(),
            update: None,
            winamp: crate::winamp::WinampState::new(session.winamp_pos, tap, eq),
            window_hidden: false,
            hide_intent: false,
            switch_intent: false,
            quit_requested: false,
            offline: options.offline,
            media_controls,
            tray,
            control_commands: None,
            control_now_playing: None,
            active_profile: profile,
            active_epoch: 0,
            playback,
            player_revision_floor: 0,
            playing_context: None,
            random_mix_playback: None,
            restored_preview: false,
            pending_play: HashSet::new(),
            volume_before_mute: None,
            navigation: vec![first_page],
            navigation_index: 0,
            load_generation: 0,
            requests: HashMap::new(),
            latest_generations: HashMap::new(),
            saved: HashMap::new(),
            pending_playlist_ops: 0,
            selection: None,
            resume,
            lyrics_media: None,
            lyrics_request: None,
            accents: HashMap::new(),
            accent_requests: HashMap::new(),
            settings_dirty: false,
            session_dirty: false,
            last_settings_save: Instant::now(),
            last_session_save: Instant::now(),
            applied_dark: None,
            zoom_applied: false,
            last_eviction: Instant::now(),
            last_update_check: None,
            last_history_refresh: Instant::now(),
            history_generation_floor: 0,
            daily_mix_day,
            last_daily_mix_check: Instant::now(),
            window_title: String::new(),
            session_window_size: session.window_size,
            session_window_pos: session.window_pos,
            last_window_size: None,
            last_window_pos: None,
        }
    }

    pub fn set_remote_control(&mut self, guard: &crate::single_instance::Guard) {
        self.control_commands = Some(guard.commands());
        self.control_now_playing = Some(guard.now_playing_slot());
    }

    pub fn attach(&mut self, ctx: &egui::Context) {
        theme::install(ctx);
        ctx.add_bytes_loader(Arc::new(self.backend.art().clone()));
        ctx.set_theme(match self.settings.theme {
            ThemeChoice::Dark => egui::ThemePreference::Dark,
            ThemeChoice::Light => egui::ThemePreference::Light,
            ThemeChoice::System => egui::ThemePreference::System,
        });
        self.applied_dark = None;
        self.winamp.forget_textures();
        self.window_hidden = false;
        self.hide_intent = false;
        self.switch_intent = false;
        #[cfg(target_os = "macos")]
        if let Some(tray) = &mut self.tray {
            tray.attach();
        }
        #[cfg(target_os = "macos")]
        if let Some(media_controls) = &mut self.media_controls {
            media_controls.attach();
        }
        if self.settings.winamp_window {
            return;
        }
        if let Some(size) = self.session_window_size.take()
            && (400.0..=3_000.0).contains(&size[0])
            && (300.0..=2_000.0).contains(&size[1])
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                size[0], size[1],
            )));
        }
        if let Some(position) = self.session_window_pos.take()
            && (-1_000.0..=5_000.0).contains(&position[0])
            && (-1_000.0..=5_000.0).contains(&position[1])
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                position[0],
                position[1],
            )));
        }
        ctx.options_mut(|options| options.input_options.line_scroll_speed = 120.0);
    }

    pub fn window_gone(&mut self) {
        self.winamp.remember_position();
        self.winamp.forget_textures();
        self.hide_intent = false;
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        self.window_hidden = false;
        #[cfg(target_os = "macos")]
        {
            // Keep AppKit's event loop alive while the window is hidden. The
            // status item depends on that loop to receive clicks.
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn hide_window(&mut self, ctx: &egui::Context, close_requested: bool) {
        self.window_hidden = true;
        #[cfg(target_os = "macos")]
        {
            if close_requested {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
            self.save_state();
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.hide_intent = true;
            if !close_requested {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    pub fn hides_to_tray(&self) -> bool {
        self.tray.is_some() && self.settings.keep_playing_in_background
    }

    pub fn page(&self) -> &Page {
        &self.navigation[self.navigation_index]
    }

    pub fn can_go_back(&self) -> bool {
        self.navigation_index > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.navigation_index + 1 < self.navigation.len()
    }

    pub fn open(&mut self, page: Page) {
        if self.page() == &page {
            self.ensure_loaded(page);
            return;
        }
        self.navigation.truncate(self.navigation_index + 1);
        self.navigation.push(page.clone());
        self.navigation_index = self.navigation.len() - 1;
        self.selection = None;
        self.note_session_change();
        self.ensure_loaded(page);
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.auth, AuthStatus::Connected(_))
    }

    pub fn login_security_warning(&self) -> Option<&'static str> {
        self.login_server
            .trim()
            .to_ascii_lowercase()
            .starts_with("http://")
            .then_some(
                "HTTP is not encrypted. Other people on this network may observe your music and sign-in token.",
            )
    }

    pub fn now_playing(&self) -> Option<NowPlaying> {
        let song = self.playback.current_song()?.clone();
        let state = self.playback.playback();
        Some(NowPlaying {
            song,
            position_ms: self.playback.position_now(),
            playing: state == Playback::Playing,
            loading: state == Playback::Loading,
            shuffle: self.playback.shuffle,
            repeat: self.playback.repeat,
            volume_percent: volume_to_percent(self.playback.volume),
            can_control: self.is_connected() || self.offline,
            resuming: self.restored_preview && state == Playback::Paused,
        })
    }

    pub fn now_playing_item(&self) -> Option<PlayableItem> {
        self.playback
            .current_song()
            .cloned()
            .map(PlayableItem::from)
    }

    pub fn current_song_id(&self) -> Option<&MediaId> {
        self.playback.current_song().map(|song| &song.id)
    }

    pub fn believed_playing(&self) -> bool {
        matches!(
            self.playback.playback(),
            Playback::Playing | Playback::Loading
        )
    }

    pub fn playing_context_id(&self) -> Option<&MediaId> {
        self.playing_context.as_ref()
    }

    pub fn playing_context_shuffle(&self) -> bool {
        self.playback.shuffle
    }

    pub fn play_pending(&self, media: &MediaId) -> bool {
        self.pending_play.contains(media)
            || (self.playback.playback() == Playback::Loading
                && (self.current_song_id() == Some(media)
                    || self.playing_context.as_ref() == Some(media)))
    }

    pub fn any_play_pending(&self) -> bool {
        !self.pending_play.is_empty() || self.playback.playback() == Playback::Loading
    }

    pub fn queue_view(&self) -> QueueView {
        QueueView {
            current: self.playback.current.clone(),
            rows: self.playback.queue.entries().cloned().collect(),
            manual_len: self.playback.queue.manual.len(),
            revision: self.playback.revision,
        }
    }

    pub fn can_clear_queue(&self) -> bool {
        !self.playback.queue.manual.is_empty()
    }

    pub fn queue_playlist_songs(&self) -> Vec<Song> {
        self.playback
            .current_song()
            .into_iter()
            .chain(self.playback.queue.entries().map(|entry| &entry.song))
            .cloned()
            .collect()
    }

    pub fn queue_playlist_name(&self) -> String {
        let playlist = self.playing_context.as_ref().and_then(|id| {
            self.library
                .playlists
                .get()
                .and_then(|rows| rows.iter().find(|playlist| playlist.id == *id))
        });
        playlist
            .map(|playlist| format!("Queue from {}", playlist.name))
            .or_else(|| {
                self.playback
                    .current_song()
                    .map(|song| format!("Queue from {}", song.name))
            })
            .unwrap_or_else(|| "Fastpotify queue".to_owned())
    }

    pub fn is_saved(&self, media: &MediaId) -> Option<bool> {
        self.saved.get(media).copied()
    }

    pub fn editable_playlists(&self) -> Vec<(MediaId, String)> {
        self.library
            .playlists
            .get()
            .into_iter()
            .flatten()
            .filter(|playlist| {
                self.user
                    .as_ref()
                    .is_some_and(|user| playlist.editable_by(user))
            })
            .map(|playlist| (playlist.id.clone(), playlist.name.clone()))
            .collect()
    }

    pub fn now_playing_tint(&self) -> Option<Color32> {
        let reference = self.playback.current_song()?.image(300)?;
        self.accents.get(reference).copied()
    }

    pub fn tint_for(&mut self, reference: Option<&str>) -> Option<Color32> {
        if !self.settings.accent_from_art {
            return None;
        }
        let reference = reference?;
        if let Some(color) = self.accents.get(reference) {
            return Some(*color);
        }
        if self.offline
            || !crate::media::is_artwork_ref(reference)
            || self
                .accent_requests
                .values()
                .any(|pending| pending == reference)
        {
            return None;
        }
        let request = self.backend.accent(reference.to_owned());
        self.accent_requests.insert(request, reference.to_owned());
        None
    }

    pub fn mark_settings_dirty(&mut self) {
        self.settings_dirty = true;
    }

    pub fn note_session_change(&mut self) {
        self.session_dirty = true;
    }

    pub fn toast(&mut self, message: impl Into<String>) {
        self.toasts.push(Toast {
            message: message.into(),
            kind: ToastKind::Info,
            created: Instant::now(),
        });
        self.toasts.truncate(4);
    }

    pub fn toast_error(&mut self, message: impl Into<String>) {
        let message = message.into();
        log::warn!("{message}");
        self.toasts.push(Toast {
            message,
            kind: ToastKind::Error,
            created: Instant::now(),
        });
        self.toasts.truncate(4);
    }

    pub fn picked_rows(&self, page: &Page) -> Option<&std::collections::BTreeSet<usize>> {
        self.selection
            .as_ref()
            .filter(|(owner, _, _)| owner == page)
            .map(|(_, _, selection)| &selection.rows)
            .filter(|rows| !rows.is_empty())
    }

    pub fn keep_picked_rows_for(&mut self, page: &Page, view: &str) {
        if self
            .selection
            .as_ref()
            .is_some_and(|(owner, seen, _)| owner == page && seen != view)
        {
            self.selection = None;
        }
    }

    pub fn pick_row(&mut self, page: &Page, view: &str, row: usize, pick: RowPick, len: usize) {
        let mut selection = match self.selection.take() {
            Some((owner, seen, selection)) if owner == *page && seen == view => selection,
            _ => RowSelection::default(),
        };
        match pick {
            RowPick::Only => {
                let only = selection.rows.len() == 1 && selection.rows.contains(&row);
                selection.rows.clear();
                selection.anchor = (!only).then_some(row);
                if !only && row < len {
                    selection.rows.insert(row);
                }
            }
            RowPick::Toggle => {
                if row < len && !selection.rows.remove(&row) {
                    selection.rows.insert(row);
                }
                selection.anchor = Some(row);
            }
            RowPick::Range => {
                let anchor = selection.anchor.unwrap_or(row);
                let (from, to) = if anchor <= row {
                    (anchor, row)
                } else {
                    (row, anchor)
                };
                selection.rows.clear();
                selection
                    .rows
                    .extend((from..=to).filter(|index| *index < len));
                selection.anchor = Some(anchor);
            }
        }
        self.selection =
            (!selection.rows.is_empty()).then(|| (page.clone(), view.to_owned(), selection));
    }

    pub fn clear_picked_rows(&mut self) {
        self.selection = None;
    }
}

impl App {
    fn next_generation(&mut self) -> u64 {
        self.load_generation = self.load_generation.saturating_add(1);
        self.load_generation
    }

    fn request(&mut self, request: ApiRequest, purpose: RequestPurpose) -> RequestId {
        let generation = request.generation();
        self.latest_generations.insert(purpose.key(), generation);
        let id = self.backend.api(request);
        self.requests.insert(id, purpose);
        id
    }

    pub fn ensure_loaded(&mut self, page: Page) {
        if self.offline || !self.is_connected() {
            return;
        }
        match page {
            Page::Home => self.load_home(false),
            Page::Favorites => {
                if !self.library.favorite_songs.loaded_once && !self.library.favorite_songs.loading
                {
                    self.library.favorite_songs.loading = true;
                    let generation = self.next_generation();
                    self.request(
                        ApiRequest::Favorites {
                            music_folder_id: None,
                            generation,
                        },
                        RequestPurpose::Favorites,
                    );
                }
            }
            Page::DailyMix => {
                self.ensure_loaded(Page::Favorites);
                self.load_random_mix(false);
                self.refresh_daily_mix_if_needed();
            }
            Page::RandomMix => self.load_random_mix(false),
            Page::Albums => {
                if !self.library.albums.loaded_once && !self.library.albums.loading {
                    self.load_album_page(0);
                }
            }
            Page::Artists => {
                if !self.library.artists.loaded_once && !self.library.artists.loading {
                    self.library.artists.loading = true;
                    let generation = self.next_generation();
                    self.request(
                        ApiRequest::Artists {
                            music_folder_id: None,
                            generation,
                        },
                        RequestPurpose::LibraryArtists,
                    );
                }
            }
            Page::Playlist(id) => {
                let needs = self
                    .playlist_pages
                    .entry(id.clone())
                    .or_default()
                    .playlist
                    .needs_load();
                if needs {
                    let generation = self.next_generation();
                    let page = self.playlist_pages.entry(id.clone()).or_default();
                    page.generation = generation;
                    page.playlist = Loadable::Loading;
                    page.items.loading = true;
                    self.request(
                        ApiRequest::Playlist {
                            id: id.clone(),
                            generation,
                        },
                        RequestPurpose::Playlist(id),
                    );
                }
            }
            Page::Album(id) => {
                let needs = self
                    .album_pages
                    .entry(id.clone())
                    .or_default()
                    .album
                    .needs_load();
                if needs {
                    let generation = self.next_generation();
                    let page = self.album_pages.entry(id.clone()).or_default();
                    page.album = Loadable::Loading;
                    page.tracks.loading = true;
                    self.request(
                        ApiRequest::Album {
                            id: id.clone(),
                            generation,
                        },
                        RequestPurpose::Album(id),
                    );
                }
            }
            Page::Artist(id) => {
                let needs = self
                    .artist_pages
                    .entry(id.clone())
                    .or_default()
                    .artist
                    .needs_load();
                if needs {
                    let generation = self.next_generation();
                    let page = self.artist_pages.entry(id.clone()).or_default();
                    page.artist = Loadable::Loading;
                    page.albums.loading = true;
                    self.request(
                        ApiRequest::Artist {
                            id: id.clone(),
                            generation,
                        },
                        RequestPurpose::Artist(id),
                    );
                }
            }
            Page::Search | Page::Queue | Page::Settings => {}
        }
    }

    fn load_home(&mut self, force: bool) {
        if self.home.requested && !force {
            return;
        }
        self.home.requested = true;
        self.home.generation = self.next_generation();
        self.home.recently_added = Loadable::Loading;
        self.home.frequent_albums = Loadable::Loading;
        self.request(
            ApiRequest::Home {
                music_folder_id: None,
                album_limit: 20,
                generation: self.home.generation,
            },
            RequestPurpose::Home,
        );
    }

    fn load_random_mix(&mut self, force: bool) {
        if self.home.random_refreshing || (!force && !self.home.random_songs.needs_load()) {
            return;
        }
        if self.home.random_songs.get().is_none() {
            self.home.random_songs = Loadable::Loading;
        }
        self.home.random_refreshing = true;
        let generation = self.next_generation();
        self.request(
            ApiRequest::RandomSongs {
                request: RandomSongsRequest {
                    size: crate::mixes::MIX_SIZE as u32,
                    ..RandomSongsRequest::default()
                },
                generation,
            },
            RequestPurpose::RandomMix,
        );
    }

    fn refresh_daily_mix_if_needed(&mut self) {
        let Some(profile) = self.active_profile.clone() else {
            self.home.daily_mix = Loadable::NotLoaded;
            self.daily_mix_day = None;
            return;
        };
        let today = crate::mixes::local_day_key();
        if self.daily_mix_day.as_deref() == Some(today.as_str()) {
            return;
        }

        let random_settled = matches!(
            self.home.random_songs,
            Loadable::Loaded(_) | Loadable::Failed(_)
        ) && !self.home.random_refreshing;
        if !self.library.favorite_songs.loaded_once || !random_settled {
            self.home.daily_mix = Loadable::Loading;
            return;
        }

        let history = self.home.recently_played.get().cloned().unwrap_or_default();
        let favorites = self.library.favorite_songs.items.clone();
        let discovery = self.home.random_songs.get().cloned().unwrap_or_default();
        let songs = crate::mixes::generate_daily_mix(
            &history,
            &favorites,
            &discovery,
            &today,
            crate::mixes::MIX_SIZE,
        );
        // Historical snapshots can predate later Favorites or local optimistic
        // toggles. Seed unknown rows without letting the mix overwrite any
        // favorite state the current session already knows.
        self.seed_songs_preserving_saved(&songs);
        if songs.is_empty() {
            // Keep an empty mix eligible for another attempt after the first
            // qualified play or Favorite arrives later today.
            self.home.daily_mix = Loadable::Loaded(songs);
            self.home.daily_mix_revision = self.home.daily_mix_revision.wrapping_add(1);
            self.daily_mix_day = None;
            return;
        }
        if let Err(error) =
            crate::mixes::DailyMixCache::save(&self.dirs.daily_mix_file(&profile), &today, &songs)
        {
            log::warn!("could not save the Daily mix: {error}");
        }
        self.home.daily_mix = Loadable::Loaded(songs);
        self.home.daily_mix_revision = self.home.daily_mix_revision.wrapping_add(1);
        self.daily_mix_day = Some(today);
    }

    fn load_library(&mut self) {
        if self.library.playlists.needs_load() {
            self.library.playlists = Loadable::Loading;
            let generation = self.next_generation();
            self.request(
                ApiRequest::Playlists { generation },
                RequestPurpose::Playlists,
            );
        }
        self.ensure_loaded(Page::Favorites);
        self.ensure_loaded(Page::Albums);
        self.ensure_loaded(Page::Artists);
        self.load_random_mix(false);
        self.load_home(false);
    }

    fn load_album_page(&mut self, offset: u32) {
        self.library.albums.loading = true;
        let generation = self.next_generation();
        self.request(
            ApiRequest::AlbumList {
                request: AlbumListRequest {
                    kind: AlbumListType::AlphabeticalByName,
                    offset,
                    limit: ALBUM_PAGE_SIZE,
                    ..AlbumListRequest::default()
                },
                generation,
            },
            RequestPurpose::LibraryAlbums(offset),
        );
    }

    pub fn load_more(&mut self, page: Page) {
        match page {
            Page::Albums => {
                if let Some(offset) = self.library.albums.next_offset
                    && self.library.albums.can_load_more()
                {
                    self.load_album_page(offset);
                }
            }
            Page::Favorites
            | Page::DailyMix
            | Page::RandomMix
            | Page::Artists
            | Page::Playlist(_)
            | Page::Album(_)
            | Page::Artist(_)
            | Page::Home
            | Page::Search
            | Page::Queue
            | Page::Settings => {}
        }
    }

    fn reload(&mut self, page: Page) {
        match &page {
            Page::Home => {
                self.home.requested = false;
                self.load_home(true);
                return;
            }
            Page::Favorites => {
                self.library.favorite_songs.reset();
            }
            Page::DailyMix => {
                self.ensure_loaded(Page::DailyMix);
                return;
            }
            Page::RandomMix => {
                self.load_random_mix(true);
                return;
            }
            Page::Albums => self.library.albums.reset(),
            Page::Artists => self.library.artists.reset(),
            Page::Playlist(id) => {
                self.playlist_pages.remove(id);
            }
            Page::Album(id) => {
                self.album_pages.remove(id);
            }
            Page::Artist(id) => {
                self.artist_pages.remove(id);
            }
            Page::Search => {
                let query = self.search.committed.clone();
                self.run_search(query);
                return;
            }
            Page::Queue | Page::Settings => return,
        }
        self.ensure_loaded(page);
    }

    fn run_search(&mut self, query: String) {
        let query = query.trim().to_owned();
        self.search.query = query.clone();
        self.search.committed = query.clone();
        self.search.serial = self.search.serial.saturating_add(1);
        self.search.typed_at = None;
        if query.is_empty() {
            self.search.results = Loadable::Loaded(SearchResults::default());
            return;
        }
        self.settings.remember_search(&query);
        self.settings_dirty = true;
        self.search.results = Loadable::Loading;
        let serial = self.search.serial;
        self.request(
            ApiRequest::Search {
                query,
                options: SearchOptions::default(),
                generation: serial,
            },
            RequestPurpose::Search(serial),
        );
    }

    pub fn request_lyrics(&mut self) {
        let Some(song) = self.playback.current_song().cloned() else {
            return;
        };
        if self.lyrics_media.as_ref() == Some(&song.id)
            && !matches!(self.lyrics, Loadable::NotLoaded | Loadable::Failed(_))
        {
            return;
        }
        let album = song
            .album
            .as_ref()
            .map(|album| album.name.clone())
            .unwrap_or_default();
        self.lyrics_media = Some(song.id.clone());
        self.lyrics = Loadable::Loading;
        self.lyrics_following = true;
        self.lyrics_line_shown = None;
        self.lyrics_request = Some(self.backend.lyrics(crate::lyrics::Query {
            artist: song.artist_names(),
            media: song.id,
            title: song.name,
            album,
            duration_ms: song.duration_ms,
        }));
    }

    fn start_resume(&mut self) {
        if self.resume.requested || self.resume.applied || !self.is_connected() {
            return;
        }
        let Some(track) = self.resume.track.clone() else {
            self.resume.applied = true;
            self.restore_manual_queue();
            return;
        };
        self.resume.requested = true;
        let generation = self.next_generation();
        match self.resume.context.clone() {
            Some(context) if context.kind == MediaKind::Album => {
                self.request(
                    ApiRequest::Album {
                        id: context,
                        generation,
                    },
                    RequestPurpose::ResumeContext(track, self.resume.position_ms),
                );
            }
            Some(context) if context.kind == MediaKind::Playlist => {
                self.request(
                    ApiRequest::Playlist {
                        id: context,
                        generation,
                    },
                    RequestPurpose::ResumeContext(track, self.resume.position_ms),
                );
            }
            _ => {
                self.request(
                    ApiRequest::Song {
                        id: track,
                        generation,
                    },
                    RequestPurpose::ResumeSong(self.resume.position_ms),
                );
            }
        }
    }

    fn restore_manual_queue(&mut self) {
        let songs = std::mem::take(&mut self.resume.manual);
        for song in songs {
            self.player(PlayerCommand::AddManual(Box::new(song)));
        }
    }

    fn load_context(
        &mut self,
        songs: Vec<Song>,
        start: usize,
        context: Option<MediaId>,
        position_ms: u32,
        play: bool,
    ) -> bool {
        if songs.is_empty() {
            return false;
        }
        self.random_mix_playback = None;
        self.latest_generations
            .remove(&RequestKey::RandomMixContinuation);
        self.playing_context = context;
        self.restored_preview = !play;
        let accepted = self.player(PlayerCommand::LoadContext(LoadContext {
            songs,
            start_index: start,
            position_ms,
            play,
        }));
        self.pending_play.clear();
        self.note_session_change();
        accepted
    }

    fn load_song_list(&mut self, songs: Vec<Song>, start: usize, mode: SongListMode) {
        if self.load_context(songs, start, None, 0, true) && mode == SongListMode::RandomMix {
            self.random_mix_playback = Some(RandomMixPlayback::default());
            self.maybe_refill_random_mix();
        }
    }

    fn maybe_refill_random_mix(&mut self) {
        let Some(state) = self.random_mix_playback else {
            return;
        };
        if self.offline
            || !self.is_connected()
            || self
                .latest_generations
                .contains_key(&RequestKey::RandomMixContinuation)
            || state.pause_requested
            || (self.playback.playback() == Playback::Stopped && self.playback.current.is_some())
            || self.playback.queue.context.len() > RANDOM_MIX_REFILL_THRESHOLD
            || state.last_refill_play_instance == Some(self.playback.play_instance_id)
        {
            return;
        }

        let play_instance = self.playback.play_instance_id;
        let Some(state) = &mut self.random_mix_playback else {
            return;
        };
        state.last_refill_play_instance = Some(play_instance);
        let generation = self.next_generation();
        self.request(
            ApiRequest::RandomSongs {
                request: RandomSongsRequest {
                    size: crate::mixes::MIX_SIZE as u32,
                    ..RandomSongsRequest::default()
                },
                generation,
            },
            RequestPurpose::RandomMixContinuation,
        );
    }

    fn finish_random_mix_continuation(&mut self, songs: Vec<Song>) {
        let Some(state) = self.random_mix_playback else {
            return;
        };

        if songs.is_empty() {
            // Playback may have advanced while this request was in flight.
            // Re-check once against that newer play instance; the recorded
            // instance prevents an empty response from spinning requests.
            self.maybe_refill_random_mix();
            return;
        }
        self.seed_songs_preserving_saved(&songs);
        let appended = self.player(PlayerCommand::AppendContext(songs));
        // The player can exhaust the old context after the API event was
        // queued but before this append reaches its authoritative reducer.
        // Decide from the append receipt, not the App's earlier snapshot.
        if appended
            && !state.pause_requested
            && self.playback.current.is_none()
            && self.playback.playback() == Playback::Stopped
            && !self.playback.queue.context.is_empty()
        {
            self.player(PlayerCommand::Play);
        }
    }

    fn player(&mut self, command: PlayerCommand) -> bool {
        let play_intent = match &command {
            PlayerCommand::Play
            | PlayerCommand::Next
            | PlayerCommand::Previous
            | PlayerCommand::SkipTo(_) => Some(true),
            PlayerCommand::Pause => Some(false),
            PlayerCommand::Toggle => Some(!self.believed_playing()),
            _ => None,
        };
        if let (Some(should_play), Some(state)) = (play_intent, &mut self.random_mix_playback) {
            if should_play && state.pause_requested {
                state.last_refill_play_instance = None;
            }
            state.pause_requested = !should_play;
        }
        let mut accepted = false;
        match self.backend.player(command) {
            Ok(receipt) if receipt.epoch == self.active_epoch => {
                self.player_revision_floor =
                    receipt.command.revision.max(receipt.snapshot.revision);
                self.playback = receipt.snapshot;
                self.settings.volume = self.playback.volume;
                self.settings_dirty = true;
                accepted = true;
            }
            Ok(_) => {}
            Err(error) => self.toast_error(error.to_string()),
        }
        if accepted {
            self.maybe_refill_random_mix();
        }
        accepted
    }

    #[cfg(any(test, feature = "demo"))]
    pub(crate) fn demo_connect(&mut self, verified: crate::api::VerifiedServer) {
        self.active_profile = Some(verified.profile.clone());
        self.user = Some(verified.user.clone());
        self.auth = AuthStatus::Connected(Box::new(verified));
    }

    #[cfg(test)]
    pub(crate) fn demo_receive_bootstrap_auth(&mut self, status: AuthStatus) {
        self.receive_auth(self.active_epoch, status);
    }

    #[cfg(any(test, feature = "demo"))]
    pub(crate) fn demo_set_playback(
        &mut self,
        snapshot: crate::player::PlaybackSnapshot,
        playing_context: Option<MediaId>,
    ) {
        self.player_revision_floor = snapshot.revision;
        self.settings.volume = snapshot.volume;
        self.playback = snapshot;
        self.playing_context = playing_context;
        self.random_mix_playback = None;
        self.restored_preview = self.playback.position.playback == Playback::Paused;
        self.pending_play.clear();
    }

    #[cfg(any(test, feature = "demo"))]
    pub(crate) fn demo_rebuild_saved_state(&mut self) {
        self.saved.clear();
        if let Some(songs) = self.home.daily_mix.get().cloned() {
            self.seed_songs(&songs);
        }
        if let Some(albums) = self.home.recently_added.get().cloned() {
            self.seed_albums(&albums);
        }
        if let Some(plays) = self.home.recently_played.get().cloned() {
            let songs: Vec<_> = plays.into_iter().map(|play| play.track).collect();
            self.seed_songs(&songs);
        }
        if let Some(albums) = self.home.frequent_albums.get().cloned() {
            self.seed_albums(&albums);
        }
        if let Some(songs) = self.home.random_songs.get().cloned() {
            self.seed_songs(&songs);
        }
        if let Some(playlists) = self.library.playlists.get().cloned() {
            for playlist in playlists {
                let songs: Vec<_> = playlist
                    .entries
                    .into_iter()
                    .map(|entry| entry.track)
                    .collect();
                self.seed_songs(&songs);
            }
        }
        self.seed_songs(&self.library.favorite_songs.items.clone());
        self.seed_albums(&self.library.albums.items.clone());
        self.seed_artists(&self.library.artists.items.clone());
        if let Some(results) = self.search.results.get().cloned() {
            self.seed_search(&results);
        }
        let playlist_pages: Vec<_> = self
            .playlist_pages
            .values()
            .map(|page| page.items.items.clone())
            .collect();
        for page in playlist_pages {
            let songs: Vec<_> = page.into_iter().map(|entry| entry.track).collect();
            self.seed_songs(&songs);
        }
        let album_pages: Vec<_> = self
            .album_pages
            .values()
            .map(|page| (page.album.get().cloned(), page.tracks.items.clone()))
            .collect();
        for (album, songs) in album_pages {
            if let Some(album) = album {
                self.seed_albums(&[album]);
            }
            self.seed_songs(&songs);
        }
        let artist_pages: Vec<_> = self
            .artist_pages
            .values()
            .map(|page| (page.artist.get().cloned(), page.albums.items.clone()))
            .collect();
        for (artist, albums) in artist_pages {
            if let Some(artist) = artist {
                self.seed_artists(&[artist]);
            }
            self.seed_albums(&albums);
        }
        if let Some(song) = self.playback.current_song().cloned() {
            self.seed_songs(std::slice::from_ref(&song));
        }
        let queued: Vec<_> = self
            .playback
            .queue
            .entries()
            .map(|entry| entry.song.clone())
            .collect();
        self.seed_songs(&queued);
    }

    fn start_song_list(
        &mut self,
        songs: Vec<Song>,
        context: MediaId,
        offset: Option<MediaId>,
        offset_index: Option<u32>,
        shuffle: bool,
    ) {
        let start = offset
            .as_ref()
            .and_then(|wanted| songs.iter().position(|song| song.id == *wanted))
            .or_else(|| offset_index.map(|index| index as usize))
            .unwrap_or_default()
            .min(songs.len().saturating_sub(1));
        self.player(PlayerCommand::Shuffle(shuffle));
        self.load_context(songs, start, Some(context), 0, true);
    }

    fn request_playing_album(
        &mut self,
        album: MediaId,
        context: MediaId,
        offset: Option<MediaId>,
        offset_index: Option<u32>,
        shuffle: bool,
    ) {
        let generation = self.next_generation();
        self.request(
            ApiRequest::Album {
                id: album,
                generation,
            },
            RequestPurpose::PlayContext {
                context,
                offset,
                offset_index,
                shuffle,
            },
        );
    }

    fn play_context(
        &mut self,
        context: MediaId,
        offset: Option<MediaId>,
        offset_index: Option<u32>,
        shuffle: bool,
    ) {
        if !self.accepts_media(&context) {
            self.toast_error("That item belongs to another Navidrome profile");
            return;
        }
        if let Some(songs) = self.songs_for_context(&context) {
            self.start_song_list(songs, context, offset, offset_index, shuffle);
            return;
        }
        self.pending_play.insert(context.clone());
        let generation = self.next_generation();
        match context.kind {
            MediaKind::Song => {
                self.request(
                    ApiRequest::Song {
                        id: context,
                        generation,
                    },
                    RequestPurpose::PlaySong,
                );
            }
            MediaKind::Album => {
                self.request(
                    ApiRequest::Album {
                        id: context.clone(),
                        generation,
                    },
                    RequestPurpose::PlayContext {
                        context,
                        offset,
                        offset_index,
                        shuffle,
                    },
                );
            }
            MediaKind::Playlist => {
                self.request(
                    ApiRequest::Playlist {
                        id: context.clone(),
                        generation,
                    },
                    RequestPurpose::PlayContext {
                        context,
                        offset,
                        offset_index,
                        shuffle,
                    },
                );
            }
            MediaKind::Artist => {
                self.request(
                    ApiRequest::Artist {
                        id: context.clone(),
                        generation,
                    },
                    RequestPurpose::PlayArtist {
                        context,
                        offset,
                        offset_index,
                        shuffle,
                    },
                );
            }
            MediaKind::MusicFolder => {
                self.pending_play.remove(&context);
                self.toast_error("Music folders are not playable items");
            }
        }
    }

    fn songs_for_context(&self, context: &MediaId) -> Option<Vec<Song>> {
        match context.kind {
            MediaKind::Album => self
                .album_pages
                .get(context)
                .filter(|page| page.tracks.loaded_once)
                .map(|page| page.tracks.items.clone()),
            MediaKind::Playlist => self
                .playlist_pages
                .get(context)
                .and_then(|page| page.playlist.get())
                .map(|playlist| {
                    playlist
                        .entries
                        .iter()
                        .map(|entry| entry.track.clone())
                        .collect()
                }),
            MediaKind::Artist => self
                .artist_pages
                .get(context)
                .and_then(|page| page.albums.items.first())
                .and_then(|album| self.songs_for_context(&album.id)),
            MediaKind::Song => self.find_song(context).map(|song| vec![song]),
            MediaKind::MusicFolder => None,
        }
    }

    fn accepts_media(&self, media: &MediaId) -> bool {
        !media.id.is_empty() && self.active_profile.as_ref() == Some(&media.profile)
    }

    fn find_song(&self, media: &MediaId) -> Option<Song> {
        self.playback
            .current_song()
            .filter(|song| song.id == *media)
            .cloned()
            .or_else(|| {
                self.playback
                    .queue
                    .entries()
                    .find(|entry| entry.song.id == *media)
                    .map(|entry| entry.song.clone())
            })
            .or_else(|| {
                self.library
                    .favorite_songs
                    .items
                    .iter()
                    .find(|song| song.id == *media)
                    .cloned()
            })
            .or_else(|| {
                self.home
                    .daily_mix
                    .get()
                    .into_iter()
                    .flatten()
                    .find(|song| song.id == *media)
                    .cloned()
            })
            .or_else(|| {
                self.home
                    .random_songs
                    .get()
                    .into_iter()
                    .flatten()
                    .find(|song| song.id == *media)
                    .cloned()
            })
            .or_else(|| {
                self.search
                    .results
                    .get()
                    .and_then(|results| results.tracks.as_ref())
                    .and_then(|page| page.items.iter().find(|song| song.id == *media))
                    .cloned()
            })
            .or_else(|| {
                self.album_pages
                    .values()
                    .flat_map(|page| &page.tracks.items)
                    .find(|song| song.id == *media)
                    .cloned()
            })
            .or_else(|| {
                self.playlist_pages
                    .values()
                    .flat_map(|page| &page.items.items)
                    .find(|entry| entry.track.id == *media)
                    .map(|entry| entry.track.clone())
            })
    }

    fn find_album(&self, media: &MediaId) -> Option<Album> {
        self.album_pages
            .get(media)
            .and_then(|page| page.album.get())
            .cloned()
            .or_else(|| {
                self.library
                    .albums
                    .items
                    .iter()
                    .find(|album| album.id == *media)
                    .cloned()
            })
            .or_else(|| {
                self.home
                    .recently_added
                    .get()
                    .into_iter()
                    .flatten()
                    .find(|album| album.id == *media)
                    .cloned()
            })
            .or_else(|| {
                self.search
                    .results
                    .get()
                    .and_then(|results| results.albums.as_ref())
                    .and_then(|page| page.items.iter().find(|album| album.id == *media))
                    .cloned()
            })
    }

    fn find_artist(&self, media: &MediaId) -> Option<Artist> {
        self.artist_pages
            .get(media)
            .and_then(|page| page.artist.get())
            .cloned()
            .or_else(|| {
                self.library
                    .artists
                    .items
                    .iter()
                    .find(|artist| artist.id == *media)
                    .cloned()
            })
            .or_else(|| {
                self.search
                    .results
                    .get()
                    .and_then(|results| results.artists.as_ref())
                    .and_then(|page| page.items.iter().find(|artist| artist.id == *media))
                    .cloned()
            })
    }

    fn favorite_before(&self, media: &MediaId) -> FavoriteBefore {
        let song = (media.kind == MediaKind::Song)
            .then(|| self.find_song(media))
            .flatten();
        let album = (media.kind == MediaKind::Album)
            .then(|| self.find_album(media))
            .flatten();
        let artist = (media.kind == MediaKind::Artist)
            .then(|| self.find_artist(media))
            .flatten();
        let value = self.saved.get(media).copied().unwrap_or_else(|| {
            song.as_ref().is_some_and(|song| song.starred)
                || album.as_ref().is_some_and(|album| album.starred)
                || artist.as_ref().is_some_and(|artist| artist.starred)
        });
        FavoriteBefore { value, song }
    }

    fn set_favorite(&mut self, media: MediaId, favorite: bool) {
        if !self.accepts_media(&media)
            || matches!(media.kind, MediaKind::Playlist | MediaKind::MusicFolder)
        {
            self.toast_error("That item cannot be added to favorites");
            return;
        }
        let before = self.favorite_before(&media);
        self.saved.insert(media.clone(), favorite);
        match media.kind {
            MediaKind::Song => optimistic_member(
                &mut self.library.favorite_songs.items,
                before.song.clone(),
                favorite,
                |song| &song.id,
            ),
            MediaKind::Album | MediaKind::Artist | MediaKind::Playlist | MediaKind::MusicFolder => {
            }
        }
        self.library.favorite_songs.revision = self.library.favorite_songs.revision.wrapping_add(1);
        let generation = self.next_generation();
        self.request(
            ApiRequest::SetFavorite {
                media: media.clone(),
                favorite,
                generation,
            },
            RequestPurpose::Favorite(media, Box::new(before)),
        );
    }

    fn playlist_snapshot(&self, id: &MediaId) -> Option<Playlist> {
        self.playlist_pages
            .get(id)
            .and_then(|page| page.playlist.get())
            .cloned()
            .or_else(|| {
                self.library
                    .playlists
                    .get()
                    .and_then(|rows| rows.iter().find(|playlist| playlist.id == *id))
                    .cloned()
            })
    }

    fn playlist_is_editable(&self, id: &MediaId) -> bool {
        self.user.as_ref().is_some_and(|user| {
            self.playlist_snapshot(id)
                .is_some_and(|playlist| playlist.editable_by(user))
        })
    }

    fn begin_playlist_mutation(&mut self, request: ApiRequest, before: PlaylistBefore) {
        // A read-back from an earlier successful mutation only describes the
        // state before this optimistic operation. Invalidate it now so it
        // cannot flash the old order or membership back into the UI.
        self.latest_generations
            .remove(&RequestKey::Playlist(before.target()));
        self.pending_playlist_ops = self.pending_playlist_ops.saturating_add(1);
        self.playlist_busy = true;
        self.request(request, RequestPurpose::PlaylistMutation(Box::new(before)));
    }
}

impl App {
    fn apply_actions(&mut self, ctx: &egui::Context) {
        let mut actions = std::mem::take(&mut self.actions);
        while !actions.is_empty() {
            for action in actions.drain(..) {
                self.apply(action, ctx);
            }
            actions = std::mem::take(&mut self.actions);
        }
    }

    fn apply(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::Open(page) => self.open(page),
            Action::OpenUri(reference) => {
                if !crate::media::is_media_ref(&reference) {
                    self.toast_error("Invalid Fastpotify media reference");
                    return;
                }
                let Ok(media) = reference.parse::<MediaId>() else {
                    self.toast_error("Invalid Fastpotify media reference");
                    return;
                };
                if !self.accepts_media(&media) {
                    self.toast_error("That item belongs to another Navidrome profile");
                } else {
                    self.play_context(media, None, None, false);
                }
            }
            Action::Back => {
                if self.can_go_back() {
                    self.navigation_index -= 1;
                    self.selection = None;
                    let page = self.page().clone();
                    self.ensure_loaded(page);
                    self.note_session_change();
                }
            }
            Action::Forward => {
                if self.can_go_forward() {
                    self.navigation_index += 1;
                    self.selection = None;
                    let page = self.page().clone();
                    self.ensure_loaded(page);
                    self.note_session_change();
                }
            }
            Action::PlayContext {
                context,
                offset,
                offset_index,
            } => self.play_context(context, offset, offset_index, false),
            Action::ShufflePlay(context) => self.play_context(context, None, None, true),
            Action::PlaySongs { songs, index, mode } => {
                if !songs.is_empty() && songs.iter().all(|song| self.accepts_media(&song.id)) {
                    self.load_song_list(songs, index as usize, mode);
                }
            }
            Action::PlayFromRow {
                context,
                song,
                index,
            } => match context {
                RowContext::Context { context, .. } => {
                    self.play_context(context, Some(song.id), Some(index), false)
                }
                RowContext::Songs { songs, mode } => {
                    self.load_song_list(songs, index as usize, mode)
                }
                RowContext::Queue(occurrence) => {
                    self.player(PlayerCommand::SkipTo(occurrence));
                }
                RowContext::View { songs, context } => {
                    self.load_context(songs, index as usize, Some(context), 0, true);
                }
            },
            Action::PlayQueueOccurrence(occurrence) => {
                self.player(PlayerCommand::SkipTo(occurrence));
            }
            Action::SetPlaying(playing) => {
                self.player(if playing {
                    PlayerCommand::Play
                } else {
                    PlayerCommand::Pause
                });
            }
            Action::TogglePlay => {
                self.player(PlayerCommand::Toggle);
            }
            Action::Next => {
                self.player(PlayerCommand::Next);
            }
            Action::Previous => {
                self.player(PlayerCommand::Previous);
            }
            Action::Seek(position) => {
                self.player(PlayerCommand::Seek(position));
            }
            Action::SeekBy(delta) => {
                let position = i64::from(self.playback.position_now())
                    .saturating_add(delta)
                    .clamp(0, i64::from(self.playback.duration_ms().max(1)))
                    as u32;
                self.player(PlayerCommand::Seek(position));
            }
            Action::SetVolume(percent) => {
                self.volume_preview = None;
                self.player(PlayerCommand::Volume(percent_to_volume(percent)));
            }
            Action::PreviewVolume(percent) => {
                self.volume_preview = Some(percent.min(100));
                self.player(PlayerCommand::VolumePreview(percent_to_volume(percent)));
            }
            Action::VolumeBy(delta) => {
                let current = i16::from(volume_to_percent(self.playback.volume));
                let percent = current.saturating_add(i16::from(delta)).clamp(0, 100) as u8;
                self.player(PlayerCommand::Volume(percent_to_volume(percent)));
            }
            Action::ToggleMute => {
                let current = volume_to_percent(self.playback.volume);
                if current == 0 {
                    let restore = self.volume_before_mute.take().unwrap_or(70);
                    self.player(PlayerCommand::Volume(percent_to_volume(restore)));
                } else {
                    self.volume_before_mute = Some(current);
                    self.player(PlayerCommand::Volume(0));
                }
            }
            Action::ToggleShuffle => {
                self.player(PlayerCommand::Shuffle(!self.playback.shuffle));
            }
            Action::CycleRepeat => {
                self.player(PlayerCommand::Repeat(self.playback.repeat.next()));
            }
            Action::SetShuffle(shuffle) => {
                self.player(PlayerCommand::Shuffle(shuffle));
            }
            Action::SetRepeat(repeat) => {
                self.player(PlayerCommand::Repeat(repeat));
            }
            Action::AddToQueue { song, label } => {
                if self.accepts_media(&song.id) {
                    self.player(PlayerCommand::AddManual(Box::new(song)));
                    self.toast(format!("Added {label} to Next up"));
                }
            }
            Action::QueueMany { songs } => {
                let count = songs.len();
                for song in songs {
                    if self.accepts_media(&song.id) {
                        self.player(PlayerCommand::AddManual(Box::new(song)));
                    }
                }
                if count != 0 {
                    self.toast(format!("Added {count} songs to Next up"));
                }
            }
            Action::ClearQueue => {
                self.player(PlayerCommand::ClearManual);
            }
            Action::ToggleFavorite(media) => {
                let favorite = !self.saved.get(&media).copied().unwrap_or(false);
                self.set_favorite(media, favorite);
            }
            Action::SetFavoriteMany { ids, favorite } => {
                for id in ids {
                    if self.saved.get(&id).copied() != Some(favorite) {
                        self.set_favorite(id, favorite);
                    }
                }
            }
            Action::AddToPlaylist {
                playlist_id,
                playlist_name,
                songs,
            } => {
                if songs.is_empty()
                    || !self.accepts_media(&playlist_id)
                    || !self.playlist_is_editable(&playlist_id)
                {
                    return;
                }
                let before = self
                    .playlist_pages
                    .get(&playlist_id)
                    .map(|page| page.items.items.clone())
                    .unwrap_or_default();
                if let Some(page) = self.playlist_pages.get_mut(&playlist_id) {
                    let mut next = page
                        .items
                        .items
                        .iter()
                        .map(|entry| entry.index)
                        .max()
                        .map_or(0, |index| index.saturating_add(1));
                    for song in &songs {
                        page.items.items.push(PlaylistItem {
                            index: next,
                            added_at: None,
                            track: song.clone(),
                        });
                        next = next.saturating_add(1);
                    }
                    page.items.revision = page.items.revision.wrapping_add(1);
                    if let Some(playlist) = page.playlist.get_mut() {
                        playlist.entries = page.items.items.clone();
                        playlist.track_count = playlist.entries.len() as u32;
                    }
                }
                if let Some(playlists) = self.library.playlists.get_mut()
                    && let Some(playlist) = playlists.iter_mut().find(|row| row.id == playlist_id)
                {
                    playlist.track_count = playlist.track_count.saturating_add(songs.len() as u32);
                }
                let generation = self.next_generation();
                self.begin_playlist_mutation(
                    ApiRequest::AddToPlaylist {
                        playlist: playlist_id.clone(),
                        songs: songs.into_iter().map(|song| song.id).collect(),
                        generation,
                    },
                    PlaylistBefore::Entries(playlist_id, before),
                );
                self.toast(format!("Added to {playlist_name}"));
            }
            Action::RemoveFromPlaylist {
                playlist_id,
                row_indices,
            } => {
                if row_indices.is_empty()
                    || !self.accepts_media(&playlist_id)
                    || !self.playlist_is_editable(&playlist_id)
                {
                    return;
                }
                let before = self
                    .playlist_pages
                    .get(&playlist_id)
                    .map(|page| page.items.items.clone())
                    .unwrap_or_default();
                let remove: HashSet<u32> = row_indices.iter().copied().collect();
                if let Some(page) = self.playlist_pages.get_mut(&playlist_id) {
                    page.items
                        .items
                        .retain(|entry| !remove.contains(&entry.index));
                    page.items.revision = page.items.revision.wrapping_add(1);
                    if let Some(playlist) = page.playlist.get_mut() {
                        playlist.entries = page.items.items.clone();
                        playlist.track_count = playlist.entries.len() as u32;
                    }
                }
                if let Some(playlists) = self.library.playlists.get_mut()
                    && let Some(playlist) = playlists.iter_mut().find(|row| row.id == playlist_id)
                {
                    playlist.track_count = playlist.track_count.saturating_sub(remove.len() as u32);
                }
                let generation = self.next_generation();
                self.begin_playlist_mutation(
                    ApiRequest::RemoveFromPlaylist {
                        playlist: playlist_id.clone(),
                        row_indices,
                        generation,
                    },
                    PlaylistBefore::Entries(playlist_id, before),
                );
            }
            Action::ReorderPlaylist {
                playlist_id,
                ordered_row_indices,
            } => {
                if !self.accepts_media(&playlist_id) || !self.playlist_is_editable(&playlist_id) {
                    return;
                }
                let Some(before) = self
                    .playlist_pages
                    .get(&playlist_id)
                    .map(|page| page.items.items.clone())
                else {
                    return;
                };
                if before.len() != ordered_row_indices.len() {
                    self.toast_error("Playlist changed before it could be reordered");
                    return;
                }
                let mut entries_by_index = before
                    .iter()
                    .cloned()
                    .map(|entry| (entry.index, entry))
                    .collect::<HashMap<_, _>>();
                if entries_by_index.len() != before.len() {
                    self.toast_error("Playlist rows could not be identified safely");
                    return;
                }
                let mut reordered = Vec::with_capacity(before.len());
                for index in ordered_row_indices {
                    let Some(entry) = entries_by_index.remove(&index) else {
                        self.toast_error("Playlist changed before it could be reordered");
                        return;
                    };
                    reordered.push(entry);
                }
                if !entries_by_index.is_empty() {
                    self.toast_error("Playlist changed before it could be reordered");
                    return;
                }
                if reordered
                    .iter()
                    .zip(&before)
                    .all(|(left, right)| left.index == right.index)
                {
                    return;
                }
                for (index, entry) in reordered.iter_mut().enumerate() {
                    entry.index = index.min(u32::MAX as usize) as u32;
                }
                let songs = reordered
                    .iter()
                    .map(|entry| entry.track.id.clone())
                    .collect();
                if let Some(page) = self.playlist_pages.get_mut(&playlist_id) {
                    page.items.items = reordered.clone();
                    page.items.revision = page.items.revision.wrapping_add(1);
                    if let Some(playlist) = page.playlist.get_mut() {
                        playlist.entries = reordered;
                        playlist.track_count = playlist.entries.len() as u32;
                    }
                }
                let generation = self.next_generation();
                self.begin_playlist_mutation(
                    ApiRequest::ReorderPlaylist {
                        playlist: playlist_id.clone(),
                        songs,
                        generation,
                    },
                    PlaylistBefore::Entries(playlist_id, before),
                );
            }
            Action::ShowDialog(dialog) => self.dialog = Some(dialog),
            Action::CloseDialog => self.dialog = None,
            Action::CreatePlaylist {
                name,
                public,
                songs,
            } => {
                let name = name.trim().to_owned();
                let Some(profile) = self.active_profile.clone() else {
                    return;
                };
                if name.is_empty() {
                    self.toast_error("Playlist name cannot be empty");
                    return;
                }
                let generation = self.next_generation();
                let temporary = MediaId::new(
                    profile,
                    MediaKind::Playlist,
                    format!("pending-{generation}"),
                );
                let entries = songs
                    .iter()
                    .enumerate()
                    .map(|(index, song)| PlaylistItem {
                        index: index as u32,
                        added_at: None,
                        track: song.clone(),
                    })
                    .collect::<Vec<_>>();
                let playlist = Playlist {
                    id: temporary.clone(),
                    uri: temporary.uri(),
                    name: name.clone(),
                    owner: UserRef {
                        id: self.user.as_ref().map(|user| user.id.clone()),
                        display_name: self.user.as_ref().map(|user| user.name().to_owned()),
                    },
                    public: Some(public),
                    track_count: entries.len() as u32,
                    entries,
                    ..Playlist::default()
                };
                if let Some(playlists) = self.library.playlists.get_mut() {
                    playlists.push(playlist.clone());
                } else {
                    self.library.playlists = Loadable::Loaded(vec![playlist.clone()]);
                }
                self.set_playlist_page(playlist);
                self.begin_playlist_mutation(
                    ApiRequest::CreatePlaylist {
                        name,
                        songs: songs.into_iter().map(|song| song.id).collect(),
                        generation,
                    },
                    PlaylistBefore::Create(temporary),
                );
                self.dialog = None;
            }
            Action::UpdatePlaylist {
                id,
                name,
                description,
                public,
            } => {
                if !self.playlist_is_editable(&id) {
                    return;
                }
                let Some(before) = self.playlist_snapshot(&id) else {
                    self.toast_error("Playlist must be loaded before it can be edited");
                    return;
                };
                if let Some(playlists) = self.library.playlists.get_mut()
                    && let Some(playlist) = playlists.iter_mut().find(|row| row.id == id)
                {
                    playlist.name = name.clone();
                    playlist.description = (!description.is_empty()).then(|| description.clone());
                    playlist.public = Some(public);
                }
                if let Some(page) = self.playlist_pages.get_mut(&id)
                    && let Some(playlist) = page.playlist.get_mut()
                {
                    playlist.name = name.clone();
                    playlist.description = (!description.is_empty()).then(|| description.clone());
                    playlist.public = Some(public);
                }
                let generation = self.next_generation();
                self.begin_playlist_mutation(
                    ApiRequest::UpdatePlaylist {
                        playlist: id,
                        name: Some(name),
                        description: Some(description),
                        public: Some(public),
                        generation,
                    },
                    PlaylistBefore::Update(before),
                );
                self.dialog = None;
            }
            Action::DeletePlaylist(id) => {
                if !self.playlist_is_editable(&id) {
                    return;
                }
                let Some(before) = self.playlist_snapshot(&id) else {
                    return;
                };
                if let Some(playlists) = self.library.playlists.get_mut() {
                    playlists.retain(|playlist| playlist.id != id);
                }
                self.playlist_pages.remove(&id);
                let generation = self.next_generation();
                self.begin_playlist_mutation(
                    ApiRequest::DeletePlaylist {
                        playlist: id,
                        generation,
                    },
                    PlaylistBefore::Delete(before),
                );
                self.dialog = None;
            }
            Action::SaveQueueAsPlaylist => {
                self.dialog = Some(Dialog::CreatePlaylist {
                    name: self.queue_playlist_name(),
                    public: false,
                    songs: self.queue_playlist_songs(),
                });
            }
            Action::CopyLink(media) => ctx.copy_text(media.uri()),
            Action::OpenUrl(url) => ctx.open_url(egui::OpenUrl::new_tab(url)),
            Action::Search(query) => self.run_search(query),
            Action::SetSearchFilter(filter) => self.search.filter = filter,
            Action::FocusSearch => {
                self.search.focus_requested = true;
                self.open(Page::Search);
            }
            Action::LoadMore(page) => self.load_more(page),
            Action::LoadMoreRecents | Action::ReloadRecents => {
                let generation = self.next_generation();
                self.history_generation_floor = generation;
                self.backend.history(generation);
            }
            Action::RefreshRandomMix => self.load_random_mix(true),
            Action::SetQueueTab(tab) => {
                self.queue_tab = tab;
                self.note_session_change();
            }
            Action::Reload(page) => self.reload(page),
            Action::SignIn => {
                let password = std::mem::take(&mut self.login_password);
                self.auth = AuthStatus::Connecting;
                self.backend.send(Command::SignIn {
                    server: self.login_server.trim().to_owned(),
                    username: self.login_username.trim().to_owned(),
                    password,
                });
            }
            Action::SignOut => {
                self.save_session();
                self.backend.send(Command::SignOut);
            }
            Action::ToggleSidebar => {
                self.settings.sidebar_visible = !self.settings.sidebar_visible;
                self.mark_settings_dirty();
            }
            Action::ToggleQueuePanel => {
                self.show_queue_panel = !self.show_queue_panel;
                if self.show_queue_panel {
                    self.show_lyrics_panel = false;
                }
                self.note_session_change();
            }
            Action::ToggleLyricsPanel => {
                self.show_lyrics_panel = !self.show_lyrics_panel;
                if self.show_lyrics_panel {
                    self.show_queue_panel = false;
                    self.request_lyrics();
                }
            }
            Action::SettingsChanged => {
                ctx.set_theme(match self.settings.theme {
                    ThemeChoice::Dark => egui::ThemePreference::Dark,
                    ThemeChoice::Light => egui::ThemePreference::Light,
                    ThemeChoice::System => egui::ThemePreference::System,
                });
                self.mark_settings_dirty();
            }
            Action::RestartEngine => self.backend.send(Command::RestartEngine(engine_config(
                &self.settings,
                Arc::clone(&self.winamp.tap),
                Arc::clone(&self.winamp.eq),
            ))),
            Action::ShowWindow => self.show_window(ctx),
            Action::HideWindow => self.hide_window(ctx, false),
            Action::ClearArtCache => match self.backend.art().clear_disk_cache() {
                Ok(bytes) => self.toast(format!("Cleared {} MiB of artwork", bytes / 1_048_576)),
                Err(error) => self.toast_error(format!("Couldn't clear artwork: {error}")),
            },
            Action::ClearPlayHistory => {
                self.recents.set_cached(Vec::new());
                self.recents_view.clear();
                self.home.recently_played = Loadable::Loaded(Vec::new());
                self.home.daily_mix = Loadable::Loading;
                self.daily_mix_day = None;
                if let Some(profile) = self.active_profile.as_ref() {
                    let path = self.dirs.daily_mix_file(profile);
                    if let Err(error) = std::fs::remove_file(path)
                        && error.kind() != std::io::ErrorKind::NotFound
                    {
                        log::warn!("could not remove the cached Daily mix: {error}");
                    }
                }
                self.refresh_daily_mix_if_needed();
                let generation = self.next_generation();
                self.history_generation_floor = generation;
                self.backend.clear_history(generation);
            }
            Action::ToggleWinampWindow => {
                self.settings.winamp_window = !self.settings.winamp_window;
                self.settings_dirty = true;
                self.switch_intent = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Action::SetSkin(skin) => {
                self.settings.skin = skin;
                self.mark_settings_dirty();
            }
            Action::InstallSkin(path) => {
                self.winamp.install(path, &self.dirs.skins_dir(), ctx);
            }
            Action::SetSkinScale(scale) => {
                self.settings.skin_scale = Some(scale.clamp(1, crate::winamp::MAX_SCALE as u8));
                self.mark_settings_dirty();
            }
            Action::ToggleWinampOnTop => {
                self.settings.winamp_on_top = !self.settings.winamp_on_top;
                self.mark_settings_dirty();
            }
            Action::OpenSkinsFolder => self.open_folder(self.dirs.skins_dir()),
            Action::CycleVisualiser => {
                self.settings.vis = self.settings.vis.next();
                self.mark_settings_dirty();
            }
            Action::SetVisualiser(mode) => {
                self.settings.vis = mode;
                self.mark_settings_dirty();
            }
            Action::ToggleWinampPlaylist => {
                self.settings.playlist_open = !self.settings.playlist_open;
                self.mark_settings_dirty();
            }
            Action::SetPlaylistHeight(height) => {
                self.settings.playlist_height = height.clamp(29, 1_000);
                self.mark_settings_dirty();
            }
            Action::ToggleWinampEq => {
                self.settings.eq_open = !self.settings.eq_open;
                self.mark_settings_dirty();
            }
            Action::ToggleEq => {
                self.settings.eq_on = !self.settings.eq_on;
                self.push_eq();
            }
            Action::SetEqBand(index, value) => {
                if let Some(band) = self.settings.eq_bands_db.get_mut(index) {
                    *band = value.clamp(-crate::eq::RANGE_DB, crate::eq::RANGE_DB);
                    self.push_eq();
                }
            }
            Action::SetEqPreamp(value) => {
                self.settings.eq_preamp_db = value.clamp(-crate::eq::RANGE_DB, crate::eq::RANGE_DB);
                self.push_eq();
            }
            Action::ApplyEqPreset(index) => {
                if let Some(preset) = crate::eq::PRESETS.get(index) {
                    self.settings.eq_bands_db = preset.bands_db;
                    self.push_eq();
                }
            }
            Action::SetBalance(balance) => {
                self.settings.balance = balance.clamp(-1.0, 1.0);
                self.winamp.balance_preview = None;
                self.push_eq();
            }
            Action::ToggleMono => {
                self.settings.mono = !self.settings.mono;
                self.push_eq();
            }
            Action::ToggleWinampPlaylistShade => {
                self.settings.playlist_shaded = !self.settings.playlist_shaded;
                self.mark_settings_dirty();
            }
            Action::ToggleWinampEqShade => {
                self.settings.eq_shaded = !self.settings.eq_shaded;
                self.mark_settings_dirty();
            }
            Action::CloseWindow => {
                if self.hides_to_tray() {
                    self.hide_window(ctx, false);
                } else {
                    self.quit_requested = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Action::ToggleWinampShade => {
                self.settings.winamp_shaded = !self.settings.winamp_shaded;
                self.mark_settings_dirty();
            }
            Action::Quit => {
                self.quit_requested = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn open_folder(&mut self, folder: std::path::PathBuf) {
        let result = std::fs::create_dir_all(&folder).and_then(|()| crate::opener::open(&folder));
        if let Err(error) = result {
            self.toast_error(format!("Couldn't open {}: {error}", folder.display()));
        }
    }

    fn push_eq(&mut self) {
        if let Ok(mut eq) = self.winamp.eq.lock() {
            *eq = eq_settings(&self.settings);
        }
        self.mark_settings_dirty();
    }

    fn handle_control_commands(&mut self) {
        let Some(queue) = &self.control_commands else {
            return;
        };
        let commands = std::mem::take(
            &mut *queue
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        for command in commands {
            let playing = self.believed_playing();
            let action = match command {
                ControlCommand::Show => Some(Action::ShowWindow),
                ControlCommand::PlayPause => Some(Action::SetPlaying(!playing)),
                ControlCommand::Play => Some(Action::SetPlaying(true)),
                ControlCommand::Pause => Some(Action::SetPlaying(false)),
                ControlCommand::Next => Some(Action::Next),
                ControlCommand::Previous => Some(Action::Previous),
                ControlCommand::SeekBy(offset) => Some(Action::SeekBy(offset)),
                ControlCommand::VolumeBy(delta) => Some(Action::VolumeBy(delta)),
                ControlCommand::SetVolume(volume) => Some(Action::SetVolume(volume.min(100))),
                ControlCommand::ToggleMute => Some(Action::ToggleMute),
                ControlCommand::ToggleShuffle => Some(Action::ToggleShuffle),
                ControlCommand::CycleRepeat => Some(Action::CycleRepeat),
                ControlCommand::SetShuffle(shuffle) => Some(Action::SetShuffle(shuffle)),
                ControlCommand::SetRepeat(repeat) => Some(Action::SetRepeat(repeat)),
                ControlCommand::SeekTo(position) => Some(Action::Seek(position)),
                ControlCommand::ToggleSaved => {
                    self.current_song_id().cloned().map(Action::ToggleFavorite)
                }
                ControlCommand::PlayRef(reference) => Some(Action::OpenUri(reference)),
            };
            if let Some(action) = action {
                self.actions.push(action);
            }
        }
    }

    fn handle_media_commands(&mut self) {
        let Some(commands) = self
            .media_controls
            .as_ref()
            .map(MediaService::drain_commands)
        else {
            return;
        };
        for command in commands {
            let playing = self.believed_playing();
            let action = match command {
                MediaCommand::Play => Some(Action::SetPlaying(true)),
                MediaCommand::Pause | MediaCommand::Stop => Some(Action::SetPlaying(false)),
                MediaCommand::PlayPause => Some(Action::SetPlaying(!playing)),
                MediaCommand::Next => Some(Action::Next),
                MediaCommand::Previous => Some(Action::Previous),
                MediaCommand::SeekBy(offset) => Some(Action::SeekBy(offset)),
                MediaCommand::SetPosition {
                    track_uri,
                    position_ms,
                } => (self.current_song_id().map(MediaId::uri).as_deref()
                    == Some(track_uri.as_str()))
                .then_some(Action::Seek(position_ms)),
                MediaCommand::SetVolume(volume) => Some(Action::SetVolume(
                    (volume.clamp(0.0, 1.0) * 100.0).round() as u8,
                )),
                MediaCommand::SetShuffle(shuffle) => Some(Action::SetShuffle(shuffle)),
                MediaCommand::SetRepeat(repeat) => Some(Action::SetRepeat(repeat)),
                MediaCommand::OpenUri(reference) => Some(Action::OpenUri(reference)),
                MediaCommand::Raise => Some(Action::ShowWindow),
                MediaCommand::Quit => Some(Action::Quit),
            };
            if let Some(action) = action {
                self.actions.push(action);
            }
        }
    }

    fn handle_tray(&mut self) {
        let Some(commands) = self.tray.as_ref().map(TrayService::drain_commands) else {
            return;
        };
        for command in commands {
            self.handle_tray_command(command);
        }
    }

    fn handle_tray_command(&mut self, command: TrayCommand) {
        self.actions.push(match command {
            TrayCommand::Show if self.settings.winamp_window => Action::ToggleWinampWindow,
            TrayCommand::Show => Action::ShowWindow,
            TrayCommand::ShowHide => {
                if self.window_hidden {
                    Action::ShowWindow
                } else {
                    Action::HideWindow
                }
            }
            TrayCommand::PlayPause => Action::TogglePlay,
            TrayCommand::Next => Action::Next,
            TrayCommand::Previous => Action::Previous,
            TrayCommand::Quit => Action::Quit,
        });
    }

    fn sync_media_controls(&mut self) {
        let state = if let Some(now) = self.now_playing() {
            let album = now
                .song
                .album
                .as_ref()
                .map(|album| album.name.clone())
                .unwrap_or_default();
            MediaState {
                playback: if now.playing {
                    Playback::Playing
                } else if now.loading {
                    Playback::Loading
                } else {
                    Playback::Paused
                },
                track: Some(MediaTrack {
                    uri: now.song.id.uri(),
                    title: now.song.name.clone(),
                    artists: now
                        .song
                        .artists
                        .iter()
                        .map(|artist| artist.name.clone())
                        .collect(),
                    album,
                    art_url: now.song.image(300).map(str::to_owned),
                    duration_ms: now.song.duration_ms,
                }),
                position_ms: now.position_ms,
                volume: f64::from(now.volume_percent) / 100.0,
                shuffle: now.shuffle,
                repeat: now.repeat,
                can_control: now.can_control,
            }
        } else {
            MediaState {
                volume: f64::from(volume_to_percent(self.settings.volume)) / 100.0,
                ..MediaState::default()
            }
        };
        if let Some(controls) = &mut self.media_controls {
            controls.update(state);
        }
        let playing = self.believed_playing();
        if let Some(tray) = &mut self.tray {
            tray.set_playing(playing);
        }
        if let Some(slot) = &self.control_now_playing {
            *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = self.control_snapshot();
        }
    }

    fn control_snapshot(&self) -> String {
        let Some(now) = self.now_playing() else {
            return crate::single_instance::NOTHING_PLAYING.to_owned();
        };
        fn clean(text: &str) -> std::borrow::Cow<'_, str> {
            if text.contains(['\t', '\n', '\r']) {
                std::borrow::Cow::Owned(text.replace(['\t', '\n', '\r'], " "))
            } else {
                std::borrow::Cow::Borrowed(text)
            }
        }
        let album = now
            .song
            .album
            .as_ref()
            .map(|album| album.name.as_str())
            .unwrap_or_default();
        let artists = now.song.artist_names();
        let art = now.song.image(300).unwrap_or_default();
        let saved = match self.is_saved(&now.song.id) {
            Some(true) => "yes",
            Some(false) => "no",
            None => "unknown",
        };
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\tlocal",
            if now.playing { "playing" } else { "paused" },
            clean(&now.song.name),
            clean(&artists),
            clean(album),
            now.position_ms,
            now.song.duration_ms,
            now.volume_percent,
            if now.shuffle { "on" } else { "off" },
            now.repeat.api_name(),
            clean(art),
            saved,
        )
    }

    fn apply_theme(&mut self, ctx: &egui::Context) {
        let dark = ctx.theme() == egui::Theme::Dark;
        if self.applied_dark != Some(dark) {
            self.palette = if dark {
                Palette::dark()
            } else {
                Palette::light()
            };
            theme::apply(ctx, &self.palette);
            self.applied_dark = Some(dark);
            self.accents.clear();
            self.accent_requests.clear();
        }
    }

    fn sync_skin(&mut self, ctx: &egui::Context) {
        if self.settings.winamp_window
            && !self.winamp.is_loading()
            && self.winamp.worn != self.settings.skin
        {
            match self.settings.skin.clone() {
                None => self.winamp.wear(None, crate::skin::Skin::builtin()),
                Some(name) => self.winamp.load(name, &self.dirs.skins_dir(), ctx),
            }
        }
        if let Some(loaded) = self.winamp.poll() {
            match loaded.result {
                Ok(skin) => {
                    self.winamp.wear(Some(loaded.name.clone()), Arc::new(skin));
                    if loaded.installed {
                        self.toast(format!("Added {} skin", crate::winamp::label(&loaded.name)));
                        self.winamp.list_choices(&self.dirs.skins_dir());
                        self.settings.skin = Some(loaded.name);
                        self.mark_settings_dirty();
                    }
                }
                Err(error) => {
                    self.toast_error(format!("{}: {error}", crate::winamp::label(&loaded.name)));
                    if !loaded.installed {
                        self.settings.skin = self.winamp.worn.clone();
                        self.mark_settings_dirty();
                    }
                }
            }
        }
    }

    fn tick(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        if !self.zoom_applied {
            self.zoom_applied = true;
            ctx.set_zoom_factor(self.settings.zoom.clamp(0.5, 2.5));
        } else {
            let zoom = ctx.zoom_factor();
            if (zoom - self.settings.zoom).abs() > 0.001 {
                self.settings.zoom = zoom;
                self.mark_settings_dirty();
            }
        }
        self.toasts
            .retain(|toast| toast.created.elapsed() < TOAST_LIFETIME);
        if let Some(typed) = self.search.typed_at {
            if typed.elapsed() >= SEARCH_DEBOUNCE {
                self.search.typed_at = None;
                self.run_search(self.search.query.clone());
            } else {
                ctx.request_repaint_after(SEARCH_DEBOUNCE - typed.elapsed());
            }
        }
        if self.settings.check_for_updates
            && !self.offline
            && self
                .last_update_check
                .is_none_or(|at| at.elapsed() >= crate::updates::CHECK_INTERVAL)
        {
            self.last_update_check = Some(now);
            self.backend.send(Command::CheckForUpdates);
        }
        if self.is_connected() && self.last_history_refresh.elapsed() >= Duration::from_secs(5) {
            self.last_history_refresh = now;
            let generation = self.next_generation();
            self.history_generation_floor = generation;
            self.backend.history(generation);
        }
        if self.last_daily_mix_check.elapsed() >= DAILY_MIX_CHECK_INTERVAL {
            self.last_daily_mix_check = now;
            self.refresh_daily_mix_if_needed();
        }
        if self.last_eviction.elapsed() >= ART_EVICTION_INTERVAL {
            self.last_eviction = now;
            self.backend.art().evict(ctx);
        }
        self.sync_skin(ctx);
        if self.settings_dirty && self.last_settings_save.elapsed() >= SAVE_DELAY {
            self.save_settings();
        }
        if self.session_dirty && self.last_session_save.elapsed() >= SAVE_DELAY {
            self.save_session();
        }
    }

    fn sync_window_title(&mut self, ctx: &egui::Context) {
        let title = self
            .now_playing()
            .filter(|now| now.playing)
            .map(|now| {
                let artists = now.song.artist_names();
                if artists.is_empty() {
                    format!("{} - Fastpotify", now.song.name)
                } else {
                    format!("{artists} - {}", now.song.name)
                }
            })
            .unwrap_or_else(|| "Fastpotify".to_owned());
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }

    pub fn background_frame(&mut self, ctx: &egui::Context) {
        self.handle_control_commands();
        self.handle_events();
        self.handle_media_commands();
        self.handle_tray();
        self.tick(ctx);
        self.apply_actions(ctx);
        self.sync_media_controls();
        self.sync_window_title(ctx);
        if ctx.input(|input| input.viewport().close_requested())
            && !self.quit_requested
            && !self.switch_intent
            && self.hides_to_tray()
        {
            self.hide_window(ctx, true);
        }
        if self.believed_playing() {
            ctx.request_repaint_after(PLAYER_REPAINT_INTERVAL);
        }
    }

    pub fn frame_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.apply_theme(&ctx);
        let needs_sign_in = !(self.is_connected() && self.user.is_some())
            && !matches!(self.auth, AuthStatus::Connecting | AuthStatus::Starting);
        if self.settings.winamp_window && needs_sign_in && !self.switch_intent {
            self.actions.push(Action::ToggleWinampWindow);
        }
        if self.settings.winamp_window {
            crate::ui::winamp::show(self, ui);
        } else {
            crate::ui::show(self, ui);
        }
        self.apply_actions(&ctx);
        self.sync_media_controls();
        if !self.settings.winamp_window {
            if let Some(rect) = ctx.input(|input| input.viewport().inner_rect) {
                self.last_window_size = Some([rect.width(), rect.height()]);
            }
            if let Some(rect) = ctx.input(|input| input.viewport().outer_rect) {
                self.last_window_pos = Some([rect.min.x, rect.min.y]);
            }
        }
        if self.believed_playing() {
            ctx.request_repaint_after(PLAYER_REPAINT_INTERVAL);
        }
        if !self.toasts.is_empty() || self.any_play_pending() {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
    }

    fn save_settings(&mut self) {
        self.settings_dirty = false;
        self.last_settings_save = Instant::now();
        if !self.offline {
            self.settings.save(&self.dirs.settings_file());
        }
    }

    fn save_session(&mut self) {
        self.session_dirty = false;
        self.last_session_save = Instant::now();
        let Some(profile) = self.active_profile.as_ref() else {
            return;
        };
        if self.offline {
            return;
        }
        let manual = self
            .playback
            .queue
            .manual
            .iter()
            .map(|entry| PlayableItem::from(entry.song.clone()))
            .collect::<Vec<_>>();
        SessionState {
            last_page: Some(self.page().encode()),
            recent_contexts: Vec::new(),
            last_context: self.playing_context.as_ref().map(MediaId::uri),
            last_track: self.playback.current_song().map(|song| song.id.uri()),
            last_position_ms: self.playback.position_now(),
            last_added_queue: self
                .playback
                .queue
                .manual
                .iter()
                .map(|entry| entry.song.id.uri())
                .collect(),
            last_queue_rows: manual,
            shuffle_on: self.playback.shuffle,
            sorts: self
                .table_sorts
                .iter()
                .filter(|(page, _)| page_has_profile(page, Some(profile)))
                .map(|(page, sort)| (page.encode(), *sort))
                .collect(),
            window_size: self.last_window_size.or(self.session_window_size),
            window_pos: self.last_window_pos.or(self.session_window_pos),
            queue_open: Some(self.show_queue_panel),
            queue_tab: Some(self.queue_tab.encode().to_owned()),
            winamp_pos: self.winamp.last_pos.or(self.winamp.restore_pos),
        }
        .save(&self.dirs.session_file(profile));
    }

    pub fn save_state(&mut self) {
        self.save_settings();
        self.save_session();
    }

    pub fn shutdown(&mut self) {
        self.save_state();
        self.backend.shutdown();
    }

    fn handle_events(&mut self) {
        for event in self.backend.poll() {
            match event {
                Event::Auth { epoch, status } => self.receive_auth(epoch, status),
                Event::Player { epoch, snapshot } => self.accept_player(epoch, *snapshot),
                Event::Api(response) => self.handle_api(*response),
                Event::LocalHistory {
                    epoch,
                    generation,
                    plays,
                    ..
                } if epoch == self.active_epoch && generation >= self.history_generation_floor => {
                    self.history_generation_floor = generation;
                    self.recents.set_cached(plays.clone());
                    self.recents_view = plays.clone();
                    self.home.recently_played = Loadable::Loaded(plays);
                    self.refresh_daily_mix_if_needed();
                }
                Event::Lyrics {
                    epoch,
                    request_id,
                    media,
                    result,
                } if epoch == self.active_epoch
                    && self.lyrics_request == Some(request_id)
                    && self.lyrics_media.as_ref() == Some(&media) =>
                {
                    self.lyrics_request = None;
                    self.lyrics = Loadable::from_result(result);
                }
                Event::Accent {
                    epoch,
                    request_id,
                    reference,
                    result,
                } if epoch == self.active_epoch => {
                    self.accent_requests.remove(&request_id);
                    if let Ok(Some(rgb)) = result {
                        self.accents
                            .insert(reference, self.palette.tint_from_art(rgb));
                    }
                }
                Event::Error { epoch, message } if epoch == self.active_epoch => {
                    self.toast_error(message);
                }
                Event::UpdateAvailable { version, url } => {
                    self.update = Some(crate::updates::Release { version, url });
                }
                Event::LocalHistory { .. }
                | Event::Lyrics { .. }
                | Event::Accent { .. }
                | Event::Error { .. } => {}
            }
        }
    }

    fn receive_auth(&mut self, epoch: SessionEpoch, status: AuthStatus) {
        // Backend startup is asynchronous. Offline/demo state is installed
        // synchronously after `App::new`, so its late epoch-zero
        // Starting/SignedOut events describe the bootstrap state from before
        // that installation. A genuinely newer epoch (for example SignOut)
        // is still accepted. Online authentication is unchanged.
        if self.offline && epoch <= self.active_epoch {
            return;
        }
        self.handle_auth(epoch, status);
    }

    fn handle_auth(&mut self, epoch: SessionEpoch, status: AuthStatus) {
        if epoch < self.active_epoch {
            return;
        }
        let epoch_changed = epoch != self.active_epoch;
        self.active_epoch = epoch;
        if epoch_changed {
            self.requests.clear();
            self.latest_generations.clear();
            self.pending_play.clear();
            self.player_revision_floor = 0;
            self.user = None;
            self.active_profile = None;
            self.reset_server_data();
        }
        match status {
            AuthStatus::Connected(server) => {
                if self.active_profile.as_ref() != Some(&server.profile) {
                    self.activate_profile(server.profile.clone());
                }
                self.user = Some(server.user.clone());
                self.auth = AuthStatus::Connected(server);
                self.load_library();
                self.start_resume();
                let generation = self.next_generation();
                self.history_generation_floor = generation;
                self.backend.history(generation);
            }
            AuthStatus::SignedOut => {
                self.auth = AuthStatus::SignedOut;
                self.user = None;
                self.active_profile = None;
                self.reset_server_data();
            }
            AuthStatus::Failed(message) => {
                self.user = None;
                self.auth = AuthStatus::Failed(message.clone());
                self.toast_error(message);
            }
            AuthStatus::Starting => self.auth = AuthStatus::Starting,
            AuthStatus::Connecting => self.auth = AuthStatus::Connecting,
        }
    }

    fn activate_profile(&mut self, profile: ProfileId) {
        self.active_profile = Some(profile.clone());
        self.reset_server_data();
        let session = SessionState::load(&self.dirs.session_file(&profile));
        self.navigation = vec![
            session
                .last_page
                .as_deref()
                .and_then(Page::decode)
                .filter(|page| page_has_profile(page, Some(&profile)))
                .filter(|page| !matches!(page, Page::Queue | Page::Settings))
                .unwrap_or(Page::Home),
        ];
        self.navigation_index = 0;
        self.table_sorts = session
            .sorts
            .iter()
            .filter_map(|(encoded, sort)| {
                let page = Page::decode(encoded)?;
                page_has_profile(&page, Some(&profile)).then_some((page, *sort))
            })
            .collect();
        self.show_queue_panel = session.queue_open.unwrap_or(false);
        self.queue_tab = session
            .queue_tab
            .as_deref()
            .and_then(QueueTab::decode)
            .unwrap_or_default();
        self.resume = ResumeState {
            context: parse_profile_ref(session.last_context.as_deref(), Some(&profile)),
            track: parse_profile_ref(session.last_track.as_deref(), Some(&profile)),
            position_ms: session.last_position_ms,
            manual: session
                .last_queue_rows
                .iter()
                .map(PlayableItem::as_track)
                .filter(|song| song.id.profile == profile)
                .cloned()
                .collect(),
            requested: false,
            applied: false,
        };
        let plays = crate::history::History::load(&self.dirs.history_file(&profile), &profile)
            .plays()
            .to_vec();
        self.recents.set_cached(plays.clone());
        self.recents_view = plays.clone();
        self.home.recently_played = Loadable::Loaded(plays);
        let today = crate::mixes::local_day_key();
        match crate::mixes::DailyMixCache::load(
            &self.dirs.daily_mix_file(&profile),
            &today,
            &profile,
            crate::mixes::MIX_SIZE,
        ) {
            Some(songs) => {
                self.seed_songs(&songs);
                self.home.daily_mix = Loadable::Loaded(songs);
                self.home.daily_mix_revision = self.home.daily_mix_revision.wrapping_add(1);
                self.daily_mix_day = Some(today);
            }
            None => {
                self.home.daily_mix = Loadable::Loading;
                self.daily_mix_day = None;
            }
        }
    }

    fn reset_server_data(&mut self) {
        let daily_mix_revision = self.home.daily_mix_revision.wrapping_add(1);
        let random_mix_revision = self.home.random_mix_revision.wrapping_add(1);
        self.home = HomeData {
            daily_mix_revision,
            random_mix_revision,
            ..HomeData::default()
        };
        self.daily_mix_day = None;
        self.library = Library::default();
        self.search = SearchState::default();
        self.playlist_pages.clear();
        self.album_pages.clear();
        self.artist_pages.clear();
        self.saved.clear();
        self.accents.clear();
        self.accent_requests.clear();
        self.playlist_busy = false;
        self.pending_playlist_ops = 0;
        self.latest_generations.clear();
        self.playing_context = None;
        self.random_mix_playback = None;
        self.playback = PlaybackSnapshot {
            volume: self.settings.volume,
            ..PlaybackSnapshot::default()
        };
        self.lyrics = Loadable::NotLoaded;
        self.lyrics_media = None;
        self.lyrics_request = None;
    }

    fn accept_player(&mut self, epoch: SessionEpoch, snapshot: PlaybackSnapshot) {
        if epoch != self.active_epoch || snapshot.revision < self.player_revision_floor {
            return;
        }
        let previous = self.current_song_id().cloned();
        let previous_error = (
            self.playback.current_occurrence(),
            self.playback.error.clone(),
        );
        self.player_revision_floor = self.player_revision_floor.max(snapshot.revision);
        self.settings.volume = snapshot.volume;
        self.playback = snapshot;
        self.pending_play.clear();
        if previous.as_ref() != self.current_song_id() {
            self.restored_preview = false;
            self.lyrics = Loadable::NotLoaded;
            self.lyrics_media = None;
            self.lyrics_request = None;
            self.lyrics_following = true;
            self.lyrics_line_shown = None;
        }
        let current_error = (
            self.playback.current_occurrence(),
            self.playback.error.clone(),
        );
        if current_error != previous_error
            && let Some(error) = current_error.1
        {
            self.toast_error(error);
        }
        self.note_session_change();
        self.maybe_refill_random_mix();
    }

    fn handle_api(&mut self, response: ApiResponse) {
        if response.epoch != self.active_epoch {
            return;
        }
        let Some(purpose) = self.requests.remove(&response.request_id) else {
            return;
        };
        let key = purpose.key();
        if self.latest_generations.get(&key) != Some(&response.generation) {
            if matches!(purpose, RequestPurpose::PlaylistMutation(_)) {
                self.pending_playlist_ops = self.pending_playlist_ops.saturating_sub(1);
                self.playlist_busy = self.pending_playlist_ops != 0;
            }
            return;
        }
        self.latest_generations.remove(&key);
        match response.result {
            Ok(payload) => self.apply_api_success(purpose, payload),
            Err(error) => self.apply_api_failure(purpose, error.to_string()),
        }
    }

    fn apply_api_success(&mut self, purpose: RequestPurpose, payload: ApiPayload) {
        match (purpose, payload) {
            (RequestPurpose::Home, ApiPayload::Home(home)) => {
                self.seed_albums(&home.newest.items);
                self.seed_albums(&home.frequent.items);
                self.home.recently_added = Loadable::Loaded(home.newest.items);
                self.home.frequent_albums = Loadable::Loaded(home.frequent.items);
                self.home.loaded_at = Some(Instant::now());
            }
            (RequestPurpose::LibraryAlbums(offset), ApiPayload::Albums(albums)) => {
                self.seed_albums(&albums.items);
                self.library.albums.absorb(offset, albums);
            }
            (RequestPurpose::LibraryArtists, ApiPayload::Artists(artists)) => {
                self.seed_artists(&artists);
                self.library.artists.set_cached(artists);
            }
            (RequestPurpose::Playlists, ApiPayload::Playlists(playlists)) => {
                self.library.playlists = Loadable::Loaded(playlists);
            }
            (RequestPurpose::Favorites, ApiPayload::Favorites(favorites)) => {
                self.apply_favorites(favorites);
                self.refresh_daily_mix_if_needed();
            }
            (RequestPurpose::Search(serial), ApiPayload::Search(results)) => {
                if serial == self.search.serial {
                    self.seed_search(&results);
                    self.search.results = Loadable::Loaded(results);
                }
            }
            (RequestPurpose::Playlist(id), ApiPayload::Playlist(playlist)) => {
                if id == playlist.id {
                    self.set_playlist_page(playlist);
                }
            }
            (RequestPurpose::Album(id), ApiPayload::Album(album)) => {
                if id == album.id {
                    self.set_album_page(album);
                }
            }
            (RequestPurpose::Artist(id), ApiPayload::Artist(artist)) => {
                if id == artist.id {
                    self.set_artist_page(artist);
                }
            }
            (RequestPurpose::PlaySong, ApiPayload::Song(song)) => {
                self.load_context(vec![*song], 0, None, 0, true);
            }
            (
                RequestPurpose::PlayContext {
                    context,
                    offset,
                    offset_index,
                    shuffle,
                },
                ApiPayload::Album(album),
            ) => {
                self.set_album_page(album.clone());
                let songs = album.tracks.map(|page| page.items).unwrap_or_default();
                self.start_song_list(songs, context, offset, offset_index, shuffle);
            }
            (
                RequestPurpose::PlayContext {
                    context,
                    offset,
                    offset_index,
                    shuffle,
                },
                ApiPayload::Playlist(playlist),
            ) => {
                self.set_playlist_page(playlist.clone());
                let songs = playlist
                    .entries
                    .into_iter()
                    .map(|entry| entry.track)
                    .collect();
                self.start_song_list(songs, context, offset, offset_index, shuffle);
            }
            (
                RequestPurpose::PlayArtist {
                    context,
                    offset,
                    offset_index,
                    shuffle,
                },
                ApiPayload::Artist(artist),
            ) => {
                self.set_artist_page(artist.clone());
                if let Some(album) = artist.albums.first() {
                    self.request_playing_album(
                        album.id.clone(),
                        context,
                        offset,
                        offset_index,
                        shuffle,
                    );
                } else {
                    self.pending_play.remove(&context);
                    self.toast_error("This artist has no albums to play");
                }
            }
            (RequestPurpose::ResumeSong(position), ApiPayload::Song(song)) => {
                self.load_context(vec![*song], 0, None, position, false);
                self.resume.applied = true;
                self.restore_manual_queue();
            }
            (RequestPurpose::ResumeContext(track, position), ApiPayload::Album(album)) => {
                let context = album.id.clone();
                let songs = album
                    .tracks
                    .as_ref()
                    .map(|page| page.items.clone())
                    .unwrap_or_default();
                let start = songs.iter().position(|song| song.id == track).unwrap_or(0);
                self.load_context(songs, start, Some(context), position, false);
                self.resume.applied = true;
                self.restore_manual_queue();
            }
            (RequestPurpose::ResumeContext(track, position), ApiPayload::Playlist(playlist)) => {
                let context = playlist.id.clone();
                let songs = playlist
                    .entries
                    .iter()
                    .map(|entry| entry.track.clone())
                    .collect::<Vec<_>>();
                let start = songs.iter().position(|song| song.id == track).unwrap_or(0);
                self.load_context(songs, start, Some(context), position, false);
                self.resume.applied = true;
                self.restore_manual_queue();
            }
            (RequestPurpose::Favorite(media, _), ApiPayload::FavoriteChanged { favorite, .. }) => {
                self.saved.insert(media, favorite);
                self.refresh_daily_mix_if_needed();
            }
            (RequestPurpose::PlaylistMutation(before), payload) => {
                self.finish_playlist_mutation(*before, payload);
            }
            (RequestPurpose::RandomMix, ApiPayload::RandomSongs(songs)) => {
                self.seed_songs_preserving_saved(&songs);
                self.home.random_songs = Loadable::Loaded(songs);
                self.home.random_mix_revision = self.home.random_mix_revision.wrapping_add(1);
                self.home.random_refreshing = false;
                self.refresh_daily_mix_if_needed();
            }
            (RequestPurpose::RandomMixContinuation, ApiPayload::RandomSongs(songs)) => {
                self.finish_random_mix_continuation(songs)
            }
            _ => {}
        }
    }

    fn apply_api_failure(&mut self, purpose: RequestPurpose, message: String) {
        match purpose {
            RequestPurpose::Home => {
                self.home.recently_added = Loadable::Failed(message.clone());
                self.home.frequent_albums = Loadable::Failed(message.clone());
                self.home.requested = false;
            }
            RequestPurpose::RandomMix => {
                self.home.random_refreshing = false;
                if self.home.random_songs.get().is_none() {
                    self.home.random_songs = Loadable::Failed(message.clone());
                }
                self.refresh_daily_mix_if_needed();
            }
            RequestPurpose::RandomMixContinuation => {
                if self.random_mix_playback.is_none() {
                    return;
                }
                // A request can fail after one or more tracks have advanced.
                // Give the latest play instance its own attempt without
                // retrying repeatedly on an unchanged snapshot.
                self.maybe_refill_random_mix();
            }
            RequestPurpose::LibraryAlbums(_) => self.library.albums.fail(message.clone()),
            RequestPurpose::LibraryArtists => self.library.artists.fail(message.clone()),
            RequestPurpose::Playlists => self.library.playlists = Loadable::Failed(message.clone()),
            RequestPurpose::Favorites => {
                self.library.favorite_songs.fail(message.clone());
                self.refresh_daily_mix_if_needed();
            }
            RequestPurpose::Search(serial) if serial == self.search.serial => {
                self.search.results = Loadable::Failed(message.clone())
            }
            RequestPurpose::Playlist(id) => {
                let page = self.playlist_pages.entry(id).or_default();
                page.playlist = Loadable::Failed(message.clone());
                page.items.fail(message.clone());
            }
            RequestPurpose::Album(id) => {
                let page = self.album_pages.entry(id).or_default();
                page.album = Loadable::Failed(message.clone());
                page.tracks.fail(message.clone());
            }
            RequestPurpose::Artist(id) => {
                let page = self.artist_pages.entry(id).or_default();
                page.artist = Loadable::Failed(message.clone());
                page.albums.fail(message.clone());
            }
            RequestPurpose::Favorite(media, before) => {
                self.restore_favorite(&media, *before);
                self.refresh_daily_mix_if_needed();
            }
            RequestPurpose::PlaylistMutation(before) => self.rollback_playlist(*before),
            RequestPurpose::PlaySong
            | RequestPurpose::PlayContext { .. }
            | RequestPurpose::PlayArtist { .. }
            | RequestPurpose::ResumeSong(_)
            | RequestPurpose::ResumeContext(_, _) => {
                self.pending_play.clear();
                self.resume.requested = false;
            }
            RequestPurpose::Search(_) => {}
        }
        self.toast_error(message);
    }

    fn seed_songs(&mut self, songs: &[Song]) {
        for song in songs {
            self.saved.insert(song.id.clone(), song.starred);
            if let Some(album) = &song.album {
                self.saved.insert(album.id.clone(), album.starred);
                for artist in &album.artists {
                    if let Some(id) = &artist.id {
                        self.saved.entry(id.clone()).or_insert(false);
                    }
                }
            }
            for artist in &song.artists {
                if let Some(id) = &artist.id {
                    self.saved.entry(id.clone()).or_insert(false);
                }
            }
        }
    }

    fn seed_songs_preserving_saved(&mut self, songs: &[Song]) {
        let saved = self.saved.clone();
        self.seed_songs(songs);
        self.saved.extend(saved);
    }

    fn seed_albums(&mut self, albums: &[Album]) {
        for album in albums {
            self.saved.insert(album.id.clone(), album.starred);
            for artist in &album.artists {
                if let Some(id) = &artist.id {
                    self.saved.entry(id.clone()).or_insert(false);
                }
            }
            if let Some(tracks) = &album.tracks {
                self.seed_songs(&tracks.items);
            }
        }
    }

    fn seed_artists(&mut self, artists: &[Artist]) {
        for artist in artists {
            self.saved.insert(artist.id.clone(), artist.starred);
            self.seed_albums(&artist.albums);
        }
    }

    fn seed_search(&mut self, results: &SearchResults) {
        if let Some(page) = &results.tracks {
            self.seed_songs(&page.items);
        }
        if let Some(page) = &results.albums {
            self.seed_albums(&page.items);
        }
        if let Some(page) = &results.artists {
            self.seed_artists(&page.items);
        }
    }

    fn apply_favorites(&mut self, favorites: Favorites) {
        self.seed_songs(&favorites.songs);
        self.seed_albums(&favorites.albums);
        self.seed_artists(&favorites.artists);
        for song in &favorites.songs {
            self.saved.insert(song.id.clone(), true);
        }
        for album in &favorites.albums {
            self.saved.insert(album.id.clone(), true);
        }
        for artist in &favorites.artists {
            self.saved.insert(artist.id.clone(), true);
        }
        self.library.favorite_songs.set_cached(favorites.songs);
    }

    fn set_playlist_page(&mut self, playlist: Playlist) {
        self.seed_songs(
            &playlist
                .entries
                .iter()
                .map(|entry| entry.track.clone())
                .collect::<Vec<_>>(),
        );
        let id = playlist.id.clone();
        let entries = playlist.entries.clone();
        let page = self.playlist_pages.entry(id).or_default();
        page.items.set_cached(entries);
        page.playlist = Loadable::Loaded(playlist);
    }

    fn set_album_page(&mut self, album: Album) {
        self.seed_albums(std::slice::from_ref(&album));
        let id = album.id.clone();
        let tracks = album
            .tracks
            .as_ref()
            .map(|page| page.items.clone())
            .unwrap_or_default();
        let page = self.album_pages.entry(id).or_default();
        page.tracks.set_cached(tracks);
        page.album = Loadable::Loaded(album);
    }

    fn set_artist_page(&mut self, artist: Artist) {
        self.seed_artists(std::slice::from_ref(&artist));
        let id = artist.id.clone();
        let albums = artist.albums.clone();
        let page = self.artist_pages.entry(id).or_default();
        page.albums.set_cached(albums);
        page.artist = Loadable::Loaded(artist);
    }

    fn restore_favorite(&mut self, media: &MediaId, before: FavoriteBefore) {
        self.saved.insert(media.clone(), before.value);
        match media.kind {
            MediaKind::Song => restore_member(
                &mut self.library.favorite_songs.items,
                before.song,
                before.value,
                |song| &song.id,
            ),
            MediaKind::Album | MediaKind::Artist | MediaKind::Playlist | MediaKind::MusicFolder => {
            }
        }
    }

    fn finish_playlist_mutation(&mut self, before: PlaylistBefore, payload: ApiPayload) {
        self.pending_playlist_ops = self.pending_playlist_ops.saturating_sub(1);
        self.playlist_busy = self.pending_playlist_ops != 0;
        match payload {
            ApiPayload::PlaylistCreated(playlist) => {
                if let PlaylistBefore::Create(temporary) = before {
                    if let Some(playlists) = self.library.playlists.get_mut() {
                        playlists.retain(|playlist| playlist.id != temporary);
                        playlists.push(playlist.clone());
                    }
                    self.playlist_pages.remove(&temporary);
                    self.set_playlist_page(playlist);
                }
            }
            ApiPayload::PlaylistChanged(id) => {
                let generation = self.next_generation();
                self.request(
                    ApiRequest::Playlist {
                        id: id.clone(),
                        generation,
                    },
                    RequestPurpose::Playlist(id),
                );
            }
            ApiPayload::PlaylistDeleted(id) => {
                self.playlist_pages.remove(&id);
                if self.page() == &Page::Playlist(id) {
                    self.open(Page::Home);
                }
            }
            ApiPayload::Playlist(playlist) => self.set_playlist_page(playlist),
            _ => {
                self.restore_playlist(before);
                self.toast_error("The server returned an unexpected playlist response");
            }
        }
    }

    fn rollback_playlist(&mut self, before: PlaylistBefore) {
        self.pending_playlist_ops = self.pending_playlist_ops.saturating_sub(1);
        self.playlist_busy = self.pending_playlist_ops != 0;
        self.restore_playlist(before);
    }

    fn restore_playlist(&mut self, before: PlaylistBefore) {
        match before {
            PlaylistBefore::Create(id) => {
                if let Some(playlists) = self.library.playlists.get_mut() {
                    playlists.retain(|playlist| playlist.id != id);
                }
                self.playlist_pages.remove(&id);
            }
            PlaylistBefore::Update(playlist) | PlaylistBefore::Delete(playlist) => {
                if let Some(playlists) = self.library.playlists.get_mut() {
                    if let Some(current) = playlists.iter_mut().find(|row| row.id == playlist.id) {
                        *current = playlist.clone();
                    } else {
                        playlists.push(playlist.clone());
                    }
                }
                self.set_playlist_page(playlist);
            }
            PlaylistBefore::Entries(id, entries) => {
                if let Some(playlists) = self.library.playlists.get_mut()
                    && let Some(playlist) = playlists.iter_mut().find(|playlist| playlist.id == id)
                {
                    playlist.track_count = entries.len() as u32;
                }
                let page = self.playlist_pages.entry(id).or_default();
                if let Some(playlist) = page.playlist.get_mut() {
                    playlist.entries = entries.clone();
                    playlist.track_count = entries.len() as u32;
                }
                page.items.set_cached(entries);
            }
        }
    }
}

fn restore_member<T>(
    rows: &mut Vec<T>,
    before: Option<T>,
    present: bool,
    id: impl Fn(&T) -> &MediaId,
) {
    if present {
        if let Some(value) = before
            && !rows.iter().any(|row| id(row) == id(&value))
        {
            rows.insert(0, value);
        }
    } else if let Some(value) = before {
        rows.retain(|row| id(row) != id(&value));
    }
}

fn optimistic_member<T>(
    rows: &mut Vec<T>,
    value: Option<T>,
    present: bool,
    id: impl Fn(&T) -> &MediaId,
) {
    let Some(value) = value else {
        return;
    };
    if present {
        if !rows.iter().any(|row| id(row) == id(&value)) {
            rows.insert(0, value);
        }
    } else {
        rows.retain(|row| id(row) != id(&value));
    }
}

fn page_has_profile(page: &Page, profile: Option<&ProfileId>) -> bool {
    match page {
        Page::Playlist(id) | Page::Album(id) | Page::Artist(id) => profile == Some(&id.profile),
        Page::Home
        | Page::Search
        | Page::Favorites
        | Page::DailyMix
        | Page::RandomMix
        | Page::Albums
        | Page::Artists
        | Page::Queue
        | Page::Settings => true,
    }
}

fn parse_profile_ref(reference: Option<&str>, profile: Option<&ProfileId>) -> Option<MediaId> {
    let media = reference?.parse::<MediaId>().ok()?;
    (profile == Some(&media.profile)).then_some(media)
}

pub fn engine_config(
    settings: &Settings,
    tap: Arc<crate::vis::AudioTap>,
    eq: crate::eq::SharedEq,
) -> EngineConfig {
    EngineConfig {
        max_bitrate_kbps: (settings.bitrate != 0).then_some(u32::from(settings.bitrate)),
        audio_device: settings
            .audio_device
            .clone()
            .filter(|device| !device.trim().is_empty()),
        initial_volume: settings.volume,
        buffer_ms: settings.audio_buffer_ms,
        tap,
        eq,
    }
}

pub fn eq_settings(settings: &Settings) -> crate::eq::EqSettings {
    crate::eq::EqSettings {
        on: settings.eq_on,
        preamp_db: settings.eq_preamp_db,
        bands_db: settings.eq_bands_db,
        balance: settings.balance,
        mono: settings.mono,
    }
    .clamped()
}

pub fn volume_to_percent(volume: u16) -> u8 {
    ((u32::from(volume) * 100 + u32::from(u16::MAX) / 2) / u32::from(u16::MAX)) as u8
}

pub fn percent_to_volume(percent: u8) -> u16 {
    ((u32::from(percent.min(100)) * u32::from(u16::MAX)) / 100) as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Page as ApiPage;
    use crate::backend::HomeResponse;

    const PROFILE: &str = "0123456789abcdef0123456789abcdef01234567";
    const OTHER_PROFILE: &str = "fedcba9876543210fedcba9876543210fedcba98";

    struct Harness {
        app: App,
        root: std::path::PathBuf,
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            self.app.shutdown();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn harness() -> Harness {
        let root = std::env::temp_dir().join(format!(
            "fastpotify-app-test-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let app = App::new(
            &Waker::default(),
            dirs,
            Settings::default(),
            AppOptions {
                media_controls: false,
                tray: false,
                offline: true,
            },
        );
        Harness { app, root }
    }

    fn profile() -> ProfileId {
        ProfileId::new(PROFILE)
    }

    fn media(kind: MediaKind, raw: &str) -> MediaId {
        MediaId::new(profile(), kind, raw)
    }

    fn artist(raw: &str) -> Artist {
        let id = media(MediaKind::Artist, raw);
        Artist {
            uri: id.uri(),
            id,
            name: raw.to_owned(),
            ..Artist::default()
        }
    }

    fn album(raw: &str) -> Album {
        let id = media(MediaKind::Album, raw);
        Album {
            uri: id.uri(),
            id,
            name: raw.to_owned(),
            ..Album::default()
        }
    }

    fn song(raw: &str) -> Song {
        let id = media(MediaKind::Song, raw);
        Song {
            uri: id.uri(),
            id,
            name: raw.to_owned(),
            duration_ms: 180_000,
            ..Song::default()
        }
    }

    fn playlist(raw: &str, entries: Vec<PlaylistItem>) -> Playlist {
        let id = media(MediaKind::Playlist, raw);
        Playlist {
            uri: id.uri(),
            id,
            name: raw.to_owned(),
            owner: UserRef {
                id: Some("listener".into()),
                display_name: Some("Listener".into()),
            },
            track_count: entries.len() as u32,
            entries,
            ..Playlist::default()
        }
    }

    fn allow_playlist_edits(app: &mut App) {
        app.user = Some(User {
            id: "listener".into(),
            roles: crate::api::UserRoles {
                playlist: true,
                ..crate::api::UserRoles::default()
            },
            ..User::default()
        });
    }

    fn connect_for_background_requests(app: &mut App) {
        app.demo_connect(crate::api::VerifiedServer {
            profile: profile(),
            ..crate::api::VerifiedServer::default()
        });
        // The harness keeps the backend itself offline, so requests remain
        // deterministic while the App exercises its connected behavior.
        app.offline = false;
    }

    fn home(name: &str) -> HomeResponse {
        HomeResponse {
            newest: ApiPage::from_slice(vec![album(name)], 0, 20, false),
            recent: ApiPage::default(),
            frequent: ApiPage::default(),
        }
    }

    #[test]
    fn restored_media_must_match_the_active_profile() {
        let matching = media(MediaKind::Album, "album / arbitrary");
        let foreign = MediaId::new(
            ProfileId::new(OTHER_PROFILE),
            MediaKind::Album,
            "album / arbitrary",
        );
        assert_eq!(
            parse_profile_ref(Some(&matching.uri()), Some(&profile())),
            Some(matching.clone())
        );
        assert_eq!(
            parse_profile_ref(Some(&foreign.uri()), Some(&profile())),
            None
        );
        assert!(page_has_profile(&Page::Album(matching), Some(&profile())));
        assert!(!page_has_profile(&Page::Album(foreign), Some(&profile())));
    }

    #[test]
    fn favorites_response_never_replaces_all_albums_or_artists() {
        let mut harness = harness();
        harness
            .app
            .library
            .albums
            .set_cached(vec![album("all-album")]);
        harness
            .app
            .library
            .artists
            .set_cached(vec![artist("all-artist")]);
        harness.app.apply_favorites(Favorites {
            albums: vec![album("favorite-album")],
            artists: vec![artist("favorite-artist")],
            songs: vec![song("favorite-song")],
        });
        assert_eq!(harness.app.library.albums.items[0].name, "all-album");
        assert_eq!(harness.app.library.artists.items[0].name, "all-artist");
        assert_eq!(
            harness.app.library.favorite_songs.items[0].name,
            "favorite-song"
        );
    }

    #[test]
    fn failed_album_favorite_does_not_remove_the_library_row() {
        let mut harness = harness();
        harness.app.active_profile = Some(profile());
        let album = album("kept");
        harness.app.library.albums.set_cached(vec![album.clone()]);
        harness.app.saved.insert(album.id.clone(), false);
        let before = harness.app.favorite_before(&album.id);
        harness.app.saved.insert(album.id.clone(), true);
        harness.app.restore_favorite(&album.id, before);
        assert_eq!(harness.app.saved.get(&album.id), Some(&false));
        assert_eq!(harness.app.library.albums.items, vec![album]);
    }

    #[test]
    fn playlist_removal_uses_original_server_index_after_sorting() {
        let mut harness = harness();
        harness.app.active_profile = Some(profile());
        allow_playlist_edits(&mut harness.app);
        let playlist_id = media(MediaKind::Playlist, "playlist");
        let entries = vec![
            PlaylistItem {
                index: 9,
                track: song("visible-first"),
                ..PlaylistItem::default()
            },
            PlaylistItem {
                index: 2,
                track: song("visible-second"),
                ..PlaylistItem::default()
            },
            PlaylistItem {
                index: 7,
                track: song("selected"),
                ..PlaylistItem::default()
            },
        ];
        let original = playlist("playlist", entries.clone());
        harness.app.library.playlists = Loadable::Loaded(vec![original.clone()]);
        harness.app.set_playlist_page(original);
        harness.app.apply(
            Action::RemoveFromPlaylist {
                playlist_id: playlist_id.clone(),
                row_indices: vec![7],
            },
            &egui::Context::default(),
        );
        let kept = &harness.app.playlist_pages[&playlist_id].items.items;
        assert_eq!(
            kept.iter().map(|entry| entry.index).collect::<Vec<_>>(),
            vec![9, 2]
        );
        assert_eq!(
            harness.app.library.playlists.get().unwrap()[0].track_count,
            2
        );

        let request_id = *harness
            .app
            .requests
            .iter()
            .find_map(|(request_id, purpose)| {
                matches!(purpose, RequestPurpose::PlaylistMutation(_)).then_some(request_id)
            })
            .unwrap();
        let generation =
            harness.app.latest_generations[&RequestKey::PlaylistMutation(playlist_id.clone())];
        harness.app.handle_api(ApiResponse {
            epoch: harness.app.active_epoch,
            request_id,
            generation,
            result: Err(crate::backend::BackendError::Server),
        });
        assert_eq!(
            harness.app.library.playlists.get().unwrap()[0].track_count,
            3
        );
        assert_eq!(
            harness.app.playlist_pages[&playlist_id].items.items,
            entries
        );
    }

    #[test]
    fn playlist_reorder_preserves_duplicate_occurrences_and_rolls_back() {
        let mut harness = harness();
        harness.app.active_profile = Some(profile());
        allow_playlist_edits(&mut harness.app);
        let playlist_id = media(MediaKind::Playlist, "playlist");
        let entries = vec![
            PlaylistItem {
                index: 0,
                added_at: Some("first duplicate".into()),
                track: song("same-song"),
            },
            PlaylistItem {
                index: 1,
                added_at: Some("middle".into()),
                track: song("middle-song"),
            },
            PlaylistItem {
                index: 2,
                added_at: Some("second duplicate".into()),
                track: song("same-song"),
            },
        ];
        harness
            .app
            .set_playlist_page(playlist("playlist", entries.clone()));
        harness.app.apply(
            Action::ReorderPlaylist {
                playlist_id: playlist_id.clone(),
                ordered_row_indices: vec![2, 1, 0],
            },
            &egui::Context::default(),
        );

        let optimistic = &harness.app.playlist_pages[&playlist_id].items.items;
        assert_eq!(
            optimistic
                .iter()
                .map(|entry| entry.added_at.as_deref().unwrap())
                .collect::<Vec<_>>(),
            vec!["second duplicate", "middle", "first duplicate"]
        );
        assert_eq!(
            optimistic
                .iter()
                .map(|entry| entry.index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );

        let request_id = *harness
            .app
            .requests
            .iter()
            .find_map(|(request_id, purpose)| {
                matches!(purpose, RequestPurpose::PlaylistMutation(_)).then_some(request_id)
            })
            .unwrap();
        let generation =
            harness.app.latest_generations[&RequestKey::PlaylistMutation(playlist_id.clone())];
        harness.app.handle_api(ApiResponse {
            epoch: harness.app.active_epoch,
            request_id,
            generation,
            result: Err(crate::backend::BackendError::RequestTooLarge),
        });

        assert_eq!(
            harness.app.playlist_pages[&playlist_id].items.items,
            entries
        );
    }

    #[test]
    fn readonly_playlist_is_hidden_from_edit_actions() {
        let mut harness = harness();
        allow_playlist_edits(&mut harness.app);
        let editable = playlist("editable", Vec::new());
        let mut readonly = playlist("readonly", Vec::new());
        readonly.readonly = true;
        harness.app.library.playlists = Loadable::Loaded(vec![editable.clone(), readonly]);

        assert_eq!(
            harness.app.editable_playlists(),
            vec![(editable.id, editable.name)]
        );
    }

    #[test]
    fn new_playlist_mutation_invalidates_an_older_readback() {
        let mut harness = harness();
        harness.app.active_profile = Some(profile());
        allow_playlist_edits(&mut harness.app);
        let playlist_id = media(MediaKind::Playlist, "playlist");
        let entries = ["first", "second", "third"]
            .into_iter()
            .enumerate()
            .map(|(index, label)| PlaylistItem {
                index: index as u32,
                added_at: Some(label.into()),
                track: song(label),
            })
            .collect::<Vec<_>>();
        harness.app.set_playlist_page(playlist("playlist", entries));

        harness.app.apply(
            Action::ReorderPlaylist {
                playlist_id: playlist_id.clone(),
                ordered_row_indices: vec![2, 1, 0],
            },
            &egui::Context::default(),
        );
        let first_mutation = *harness
            .app
            .requests
            .iter()
            .find_map(|(request_id, purpose)| {
                matches!(purpose, RequestPurpose::PlaylistMutation(_)).then_some(request_id)
            })
            .unwrap();
        let first_generation =
            harness.app.latest_generations[&RequestKey::PlaylistMutation(playlist_id.clone())];
        harness.app.handle_api(ApiResponse {
            epoch: harness.app.active_epoch,
            request_id: first_mutation,
            generation: first_generation,
            result: Ok(ApiPayload::PlaylistChanged(playlist_id.clone())),
        });

        let old_readback = *harness
            .app
            .requests
            .iter()
            .find_map(|(request_id, purpose)| {
                matches!(purpose, RequestPurpose::Playlist(id) if id == &playlist_id)
                    .then_some(request_id)
            })
            .unwrap();
        let old_generation =
            harness.app.latest_generations[&RequestKey::Playlist(playlist_id.clone())];
        let old_server_state = harness.app.playlist_pages[&playlist_id]
            .playlist
            .get()
            .unwrap()
            .clone();

        harness.app.apply(
            Action::ReorderPlaylist {
                playlist_id: playlist_id.clone(),
                ordered_row_indices: vec![1, 0, 2],
            },
            &egui::Context::default(),
        );
        let optimistic_labels = harness.app.playlist_pages[&playlist_id]
            .items
            .items
            .iter()
            .map(|entry| entry.added_at.clone().unwrap())
            .collect::<Vec<_>>();
        harness.app.handle_api(ApiResponse {
            epoch: harness.app.active_epoch,
            request_id: old_readback,
            generation: old_generation,
            result: Ok(ApiPayload::Playlist(old_server_state)),
        });

        assert_eq!(
            harness.app.playlist_pages[&playlist_id]
                .items
                .items
                .iter()
                .map(|entry| entry.added_at.clone().unwrap())
                .collect::<Vec<_>>(),
            optimistic_labels
        );
    }

    #[test]
    fn offline_demo_connection_ignores_stale_signed_out_events() {
        let mut harness = harness();
        let profile = profile();
        let user = User {
            id: "demo".into(),
            display_name: Some("Demo Listener".into()),
            roles: crate::api::UserRoles {
                playlist: true,
                stream: true,
                cover_art: true,
                ..crate::api::UserRoles::default()
            },
            ..User::default()
        };
        let verified = crate::api::VerifiedServer {
            profile: profile.clone(),
            user: user.clone(),
            music_folders: Vec::new(),
            capabilities: crate::api::ServerCapabilities {
                protocol_version: "1.16.1".into(),
                server_type: Some("navidrome".into()),
                server_version: Some("0.54.0".into()),
                open_subsonic: true,
                extensions: vec![crate::api::OpenSubsonicExtension {
                    name: "formPost".into(),
                    versions: vec![1],
                }],
            },
        };
        harness.app.demo_connect(verified);

        harness.app.receive_auth(0, AuthStatus::SignedOut);

        assert!(matches!(harness.app.auth, AuthStatus::Connected(_)));
        assert_eq!(harness.app.active_profile, Some(profile));
        assert_eq!(harness.app.user, Some(user));
    }

    #[test]
    fn stale_player_epoch_and_revision_cannot_undo_visible_state() {
        let mut harness = harness();
        harness.app.active_epoch = 4;
        harness.app.player_revision_floor = 10;
        harness.app.playback.volume = 123;
        harness.app.accept_player(
            3,
            PlaybackSnapshot {
                revision: 99,
                volume: 999,
                ..PlaybackSnapshot::default()
            },
        );
        harness.app.accept_player(
            4,
            PlaybackSnapshot {
                revision: 9,
                volume: 888,
                ..PlaybackSnapshot::default()
            },
        );
        assert_eq!(harness.app.playback.volume, 123);
    }

    #[test]
    fn repeated_player_snapshot_does_not_repeat_the_same_error() {
        let mut harness = harness();
        harness.app.active_epoch = 4;
        for revision in [1, 2] {
            harness.app.accept_player(
                4,
                PlaybackSnapshot {
                    revision,
                    error: Some("The server returned an empty audio stream".into()),
                    ..PlaybackSnapshot::default()
                },
            );
        }
        assert_eq!(harness.app.toasts.len(), 1);

        harness.app.accept_player(
            4,
            PlaybackSnapshot {
                revision: 3,
                error: Some("The audio download failed".into()),
                ..PlaybackSnapshot::default()
            },
        );
        assert_eq!(harness.app.toasts.len(), 2);
    }

    #[test]
    fn stale_metadata_generation_cannot_replace_a_reload() {
        let mut harness = harness();
        let first_generation = harness.app.next_generation();
        let first = harness.app.request(
            ApiRequest::Home {
                music_folder_id: None,
                album_limit: 20,
                generation: first_generation,
            },
            RequestPurpose::Home,
        );
        let second_generation = harness.app.next_generation();
        let second = harness.app.request(
            ApiRequest::Home {
                music_folder_id: None,
                album_limit: 20,
                generation: second_generation,
            },
            RequestPurpose::Home,
        );
        harness.app.handle_api(ApiResponse {
            epoch: 0,
            request_id: first,
            generation: first_generation,
            result: Ok(ApiPayload::Home(home("old"))),
        });
        assert!(harness.app.home.recently_added.get().is_none());
        harness.app.handle_api(ApiResponse {
            epoch: 0,
            request_id: second,
            generation: second_generation,
            result: Ok(ApiPayload::Home(home("new"))),
        });
        assert_eq!(
            harness.app.home.recently_added.get().unwrap()[0].name,
            "new"
        );
    }

    #[test]
    fn home_and_random_mix_use_independent_requests() {
        let mut harness = harness();
        harness.app.load_home(false);

        assert!(harness.app.home.requested);
        assert!(!harness.app.home.random_refreshing);
        assert_eq!(
            harness
                .app
                .requests
                .values()
                .filter(|purpose| matches!(purpose, RequestPurpose::Home))
                .count(),
            1
        );
        assert_eq!(
            harness
                .app
                .requests
                .values()
                .filter(|purpose| matches!(purpose, RequestPurpose::RandomMix))
                .count(),
            0
        );

        harness.app.load_random_mix(false);
        assert!(harness.app.home.random_refreshing);
        assert_eq!(
            harness
                .app
                .requests
                .values()
                .filter(|purpose| matches!(purpose, RequestPurpose::RandomMix))
                .count(),
            1
        );
    }

    #[test]
    fn stale_random_mix_cannot_replace_a_newer_refresh() {
        let mut harness = harness();
        harness.app.home.random_songs = Loadable::Loaded(vec![song("original")]);
        harness.app.home.random_refreshing = true;

        let first_generation = harness.app.next_generation();
        let first = harness.app.request(
            ApiRequest::RandomSongs {
                request: RandomSongsRequest::default(),
                generation: first_generation,
            },
            RequestPurpose::RandomMix,
        );
        let second_generation = harness.app.next_generation();
        let second = harness.app.request(
            ApiRequest::RandomSongs {
                request: RandomSongsRequest::default(),
                generation: second_generation,
            },
            RequestPurpose::RandomMix,
        );

        harness.app.handle_api(ApiResponse {
            epoch: 0,
            request_id: first,
            generation: first_generation,
            result: Ok(ApiPayload::RandomSongs(vec![song("stale")])),
        });
        assert_eq!(
            harness.app.home.random_songs.get().unwrap()[0].name,
            "original"
        );
        assert_eq!(harness.app.home.random_mix_revision, 0);
        assert!(harness.app.home.random_refreshing);

        harness.app.handle_api(ApiResponse {
            epoch: 0,
            request_id: second,
            generation: second_generation,
            result: Ok(ApiPayload::RandomSongs(vec![song("fresh")])),
        });
        assert_eq!(
            harness.app.home.random_songs.get().unwrap()[0].name,
            "fresh"
        );
        assert_eq!(harness.app.home.random_mix_revision, 1);
        assert!(!harness.app.home.random_refreshing);
    }

    #[test]
    fn failed_random_mix_refresh_keeps_the_previous_songs() {
        let mut harness = harness();
        harness.app.home.random_songs = Loadable::Loaded(vec![song("still here")]);
        harness.app.home.random_refreshing = true;
        let generation = harness.app.next_generation();
        let request_id = harness.app.request(
            ApiRequest::RandomSongs {
                request: RandomSongsRequest::default(),
                generation,
            },
            RequestPurpose::RandomMix,
        );

        harness.app.handle_api(ApiResponse {
            epoch: 0,
            request_id,
            generation,
            result: Err(crate::backend::BackendError::Network),
        });

        assert_eq!(
            harness.app.home.random_songs.get().unwrap()[0].name,
            "still here"
        );
        assert!(!harness.app.home.random_refreshing);
        assert_eq!(harness.app.toasts.len(), 1);
    }

    #[test]
    fn random_mix_refill_starts_once_when_three_context_songs_remain() {
        let mut harness = harness();
        connect_for_background_requests(&mut harness.app);
        harness.app.demo_set_playback(
            crate::player::demo_snapshot(crate::player::DemoPlayback {
                current: Some(song("playing")),
                manual: (0..8)
                    .map(|index| song(&format!("manual-{index}")))
                    .collect(),
                context: vec![song("next-1"), song("next-2"), song("next-3")],
                position_ms: 12_345,
                playback: Playback::Playing,
                volume: harness.app.settings.volume,
                shuffle: false,
                repeat: RepeatMode::Off,
            }),
            None,
        );
        harness.app.random_mix_playback = Some(RandomMixPlayback::default());

        harness.app.maybe_refill_random_mix();
        harness.app.maybe_refill_random_mix();

        assert_eq!(
            harness
                .app
                .requests
                .values()
                .filter(|purpose| matches!(purpose, RequestPurpose::RandomMixContinuation))
                .count(),
            1
        );
    }

    #[test]
    fn random_mix_refill_waits_for_the_threshold_and_an_active_random_session() {
        let mut harness = harness();
        connect_for_background_requests(&mut harness.app);
        harness.app.demo_set_playback(
            crate::player::demo_snapshot(crate::player::DemoPlayback {
                current: Some(song("playing")),
                manual: Vec::new(),
                context: vec![song("one"), song("two"), song("three"), song("four")],
                position_ms: 0,
                playback: Playback::Playing,
                volume: harness.app.settings.volume,
                shuffle: false,
                repeat: RepeatMode::Off,
            }),
            None,
        );

        harness.app.maybe_refill_random_mix();
        assert!(harness.app.requests.is_empty());

        harness.app.random_mix_playback = Some(RandomMixPlayback::default());
        harness.app.maybe_refill_random_mix();
        assert!(harness.app.requests.is_empty());
    }

    #[test]
    fn failed_random_mix_refill_retries_only_after_playback_advances() {
        let mut harness = harness();
        connect_for_background_requests(&mut harness.app);
        harness.app.demo_set_playback(
            crate::player::demo_snapshot(crate::player::DemoPlayback {
                current: Some(song("playing")),
                manual: Vec::new(),
                context: vec![song("one"), song("two"), song("three")],
                position_ms: 0,
                playback: Playback::Playing,
                volume: harness.app.settings.volume,
                shuffle: false,
                repeat: RepeatMode::Off,
            }),
            None,
        );
        harness.app.random_mix_playback = Some(RandomMixPlayback::default());
        harness.app.maybe_refill_random_mix();
        let request_id = harness
            .app
            .requests
            .iter()
            .find_map(|(id, purpose)| {
                matches!(purpose, RequestPurpose::RandomMixContinuation).then_some(*id)
            })
            .unwrap();
        let generation = harness.app.latest_generations[&RequestKey::RandomMixContinuation];

        harness.app.handle_api(ApiResponse {
            epoch: harness.app.active_epoch,
            request_id,
            generation,
            result: Err(crate::backend::BackendError::Network),
        });
        harness.app.maybe_refill_random_mix();
        assert!(harness.app.requests.is_empty());

        harness.app.playback.play_instance_id =
            harness.app.playback.play_instance_id.saturating_add(1);
        harness.app.maybe_refill_random_mix();
        assert_eq!(
            harness
                .app
                .requests
                .values()
                .filter(|purpose| matches!(purpose, RequestPurpose::RandomMixContinuation))
                .count(),
            1
        );
    }

    #[test]
    fn failed_random_mix_refill_observes_progress_made_while_in_flight() {
        let mut harness = harness();
        connect_for_background_requests(&mut harness.app);
        harness.app.demo_set_playback(
            crate::player::demo_snapshot(crate::player::DemoPlayback {
                current: Some(song("playing")),
                manual: Vec::new(),
                context: vec![song("one"), song("two"), song("three")],
                position_ms: 0,
                playback: Playback::Playing,
                volume: harness.app.settings.volume,
                shuffle: false,
                repeat: RepeatMode::Off,
            }),
            None,
        );
        harness.app.random_mix_playback = Some(RandomMixPlayback::default());
        harness.app.maybe_refill_random_mix();
        let request_id = harness
            .app
            .requests
            .iter()
            .find_map(|(id, purpose)| {
                matches!(purpose, RequestPurpose::RandomMixContinuation).then_some(*id)
            })
            .unwrap();
        let generation = harness.app.latest_generations[&RequestKey::RandomMixContinuation];
        harness.app.playback.play_instance_id =
            harness.app.playback.play_instance_id.saturating_add(1);
        harness.app.playback.queue.context.remove(0);

        harness.app.handle_api(ApiResponse {
            epoch: harness.app.active_epoch,
            request_id,
            generation,
            result: Err(crate::backend::BackendError::Network),
        });

        assert!(
            harness
                .app
                .latest_generations
                .contains_key(&RequestKey::RandomMixContinuation)
        );
        assert_eq!(
            harness
                .app
                .requests
                .values()
                .filter(|purpose| matches!(purpose, RequestPurpose::RandomMixContinuation))
                .count(),
            1
        );
    }

    #[test]
    fn empty_random_mix_refill_retries_once_after_the_context_exhausts() {
        let mut harness = harness();
        connect_for_background_requests(&mut harness.app);
        harness.app.demo_set_playback(
            crate::player::demo_snapshot(crate::player::DemoPlayback {
                current: Some(song("playing")),
                manual: Vec::new(),
                context: vec![song("one"), song("two"), song("three")],
                position_ms: 0,
                playback: Playback::Playing,
                volume: harness.app.settings.volume,
                shuffle: false,
                repeat: RepeatMode::Off,
            }),
            None,
        );
        harness.app.random_mix_playback = Some(RandomMixPlayback::default());
        harness.app.maybe_refill_random_mix();
        let request_id = harness
            .app
            .requests
            .iter()
            .find_map(|(id, purpose)| {
                matches!(purpose, RequestPurpose::RandomMixContinuation).then_some(*id)
            })
            .unwrap();
        let generation = harness.app.latest_generations[&RequestKey::RandomMixContinuation];
        harness.app.playback.current = None;
        harness.app.playback.queue.context.clear();
        harness.app.playback.position.playback = Playback::Stopped;
        harness.app.playback.position.observed_at = None;
        harness.app.playback.play_instance_id =
            harness.app.playback.play_instance_id.saturating_add(1);

        harness.app.handle_api(ApiResponse {
            epoch: harness.app.active_epoch,
            request_id,
            generation,
            result: Ok(ApiPayload::RandomSongs(Vec::new())),
        });

        assert!(
            harness
                .app
                .latest_generations
                .contains_key(&RequestKey::RandomMixContinuation)
        );
        assert_eq!(harness.app.requests.len(), 1);
    }

    #[test]
    fn random_mix_continuation_resumes_when_backend_exhausts_ahead_of_app() {
        let mut harness = harness();
        harness.app.active_profile = Some(profile());
        harness
            .app
            .backend
            .install_test_player(harness.app.active_epoch, harness.app.settings.volume);
        harness.app.player(PlayerCommand::LoadContext(LoadContext {
            songs: vec![song("last")],
            start_index: 0,
            position_ms: 0,
            play: true,
        }));
        harness.app.random_mix_playback = Some(RandomMixPlayback::default());
        let stale_app_snapshot = harness.app.playback.clone();

        let exhausted = harness
            .app
            .backend
            .player(PlayerCommand::Next)
            .unwrap()
            .snapshot;
        assert!(exhausted.current.is_none());
        assert_eq!(exhausted.playback(), Playback::Stopped);
        assert_eq!(harness.app.playback, stale_app_snapshot);
        assert_eq!(
            harness
                .app
                .playback
                .current_song()
                .map(|song| song.name.as_str()),
            Some("last")
        );

        harness
            .app
            .finish_random_mix_continuation(vec![song("appended-first"), song("appended-next")]);

        assert_eq!(
            harness
                .app
                .playback
                .current_song()
                .map(|song| song.name.as_str()),
            Some("appended-first")
        );
        assert_eq!(harness.app.playback.playback(), Playback::Loading);
    }

    #[test]
    fn stopped_random_mix_does_not_start_a_background_refill() {
        let mut harness = harness();
        connect_for_background_requests(&mut harness.app);
        harness.app.demo_set_playback(
            crate::player::demo_snapshot(crate::player::DemoPlayback {
                current: Some(song("stopped")),
                manual: Vec::new(),
                context: vec![song("one"), song("two"), song("three")],
                position_ms: 0,
                playback: Playback::Stopped,
                volume: harness.app.settings.volume,
                shuffle: false,
                repeat: RepeatMode::Off,
            }),
            None,
        );
        harness.app.random_mix_playback = Some(RandomMixPlayback::default());

        harness.app.maybe_refill_random_mix();

        assert!(harness.app.requests.is_empty());
    }

    #[test]
    fn explicit_pause_in_a_refill_gap_prevents_resume_until_play_is_requested() {
        let mut harness = harness();
        connect_for_background_requests(&mut harness.app);
        harness.app.demo_set_playback(
            crate::player::demo_snapshot(crate::player::DemoPlayback {
                current: None,
                manual: Vec::new(),
                context: Vec::new(),
                position_ms: 0,
                playback: Playback::Stopped,
                volume: harness.app.settings.volume,
                shuffle: false,
                repeat: RepeatMode::Off,
            }),
            None,
        );
        harness.app.random_mix_playback = Some(RandomMixPlayback::default());

        harness
            .app
            .apply(Action::SetPlaying(false), &egui::Context::default());
        harness.app.maybe_refill_random_mix();
        assert!(harness.app.requests.is_empty());

        harness
            .app
            .apply(Action::SetPlaying(true), &egui::Context::default());
        harness.app.maybe_refill_random_mix();
        assert_eq!(harness.app.requests.len(), 1);
    }

    #[test]
    fn external_pause_keeps_its_intent_if_a_player_event_arrives_before_apply() {
        let mut harness = harness();
        harness.app.demo_set_playback(
            crate::player::demo_snapshot(crate::player::DemoPlayback {
                current: Some(song("playing")),
                manual: Vec::new(),
                context: Vec::new(),
                position_ms: 0,
                playback: Playback::Playing,
                volume: harness.app.settings.volume,
                shuffle: false,
                repeat: RepeatMode::Off,
            }),
            None,
        );
        harness.app.control_commands =
            Some(Arc::new(std::sync::Mutex::new(vec![ControlCommand::Pause])));

        harness.app.handle_control_commands();

        assert!(matches!(
            harness.app.actions.as_slice(),
            [Action::SetPlaying(false)]
        ));
    }

    #[test]
    fn finite_song_list_invalidates_a_pending_random_mix_refill() {
        let mut harness = harness();
        connect_for_background_requests(&mut harness.app);
        harness
            .app
            .backend
            .install_test_player(harness.app.active_epoch, harness.app.settings.volume);
        harness
            .app
            .load_song_list(vec![song("random")], 0, SongListMode::RandomMix);
        let request_id = harness
            .app
            .requests
            .iter()
            .find_map(|(id, purpose)| {
                matches!(purpose, RequestPurpose::RandomMixContinuation).then_some(*id)
            })
            .unwrap();
        let generation = harness.app.latest_generations[&RequestKey::RandomMixContinuation];
        harness
            .app
            .load_song_list(vec![song("finite")], 0, SongListMode::Finite);

        harness.app.handle_api(ApiResponse {
            epoch: harness.app.active_epoch,
            request_id,
            generation,
            result: Ok(ApiPayload::RandomSongs(vec![song("must-not-append")])),
        });

        assert_eq!(
            harness
                .app
                .playback
                .current_song()
                .map(|song| song.name.as_str()),
            Some("finite")
        );
        assert!(harness.app.playback.queue.context.is_empty());
        assert!(harness.app.random_mix_playback.is_none());
        assert!(harness.app.toasts.is_empty());
    }

    #[test]
    fn random_mix_page_refresh_and_playback_refill_use_independent_requests() {
        let mut harness = harness();
        connect_for_background_requests(&mut harness.app);
        harness.app.demo_set_playback(
            crate::player::demo_snapshot(crate::player::DemoPlayback {
                current: Some(song("playing")),
                manual: Vec::new(),
                context: vec![song("one"), song("two"), song("three")],
                position_ms: 0,
                playback: Playback::Playing,
                volume: harness.app.settings.volume,
                shuffle: false,
                repeat: RepeatMode::Off,
            }),
            None,
        );
        harness.app.random_mix_playback = Some(RandomMixPlayback::default());
        harness.app.maybe_refill_random_mix();
        harness.app.home.random_songs = Loadable::Loaded(vec![song("visible")]);
        harness.app.load_random_mix(true);

        assert!(
            harness
                .app
                .latest_generations
                .contains_key(&RequestKey::RandomMixContinuation)
        );
        assert!(
            harness
                .app
                .latest_generations
                .contains_key(&RequestKey::RandomMix)
        );
        assert_eq!(harness.app.requests.len(), 2);
    }

    #[test]
    fn random_mix_refresh_preserves_playback_and_current_favorite_state() {
        let mut harness = harness();
        harness.app.demo_set_playback(
            crate::player::demo_snapshot(crate::player::DemoPlayback {
                current: Some(song("playing")),
                manual: vec![song("queued")],
                context: vec![song("later")],
                position_ms: 12_345,
                playback: Playback::Playing,
                volume: harness.app.settings.volume,
                shuffle: true,
                repeat: RepeatMode::Context,
            }),
            None,
        );
        let before = harness.app.playback.clone();
        let server_says_unsaved = song("still saved");
        let mut server_says_saved = song("still unsaved");
        server_says_saved.starred = true;
        server_says_saved.starred_at = Some("2026-09-01T00:00:00Z".into());
        harness
            .app
            .saved
            .insert(server_says_unsaved.id.clone(), true);
        harness
            .app
            .saved
            .insert(server_says_saved.id.clone(), false);
        harness.app.home.random_refreshing = true;
        let generation = harness.app.next_generation();
        let request_id = harness.app.request(
            ApiRequest::RandomSongs {
                request: RandomSongsRequest::default(),
                generation,
            },
            RequestPurpose::RandomMix,
        );

        harness.app.handle_api(ApiResponse {
            epoch: 0,
            request_id,
            generation,
            result: Ok(ApiPayload::RandomSongs(vec![
                server_says_unsaved.clone(),
                server_says_saved.clone(),
            ])),
        });

        assert_eq!(harness.app.playback, before);
        assert_eq!(harness.app.is_saved(&server_says_unsaved.id), Some(true));
        assert_eq!(harness.app.is_saved(&server_says_saved.id), Some(false));
    }

    #[test]
    fn daily_mix_generation_preserves_current_favorite_state() {
        let mut harness = harness();
        let historical = song("favorite since it was played");
        let mut favorite = historical.clone();
        favorite.starred = true;
        favorite.starred_at = Some("2026-09-01T00:00:00Z".into());

        harness.app.active_profile = Some(profile());
        harness.app.home.recently_played = Loadable::Loaded(vec![PlayHistory {
            track: historical.clone(),
            played_at: Some("2026-08-31T00:00:00Z".into()),
            context: None,
        }]);
        harness
            .app
            .library
            .favorite_songs
            .set_cached(vec![favorite]);
        harness.app.home.random_songs = Loadable::Loaded(Vec::new());
        harness.app.home.random_refreshing = false;
        harness.app.daily_mix_day = None;
        harness.app.saved.insert(historical.id.clone(), true);

        harness.app.refresh_daily_mix_if_needed();

        assert_eq!(harness.app.is_saved(&historical.id), Some(true));
        assert_eq!(
            harness.app.home.daily_mix.get().unwrap()[0].id,
            historical.id
        );
        assert_eq!(harness.app.home.daily_mix_revision, 1);
        assert!(harness.app.dirs.daily_mix_file(&profile()).is_file());
    }

    #[test]
    fn daily_mix_generation_preserves_current_unfavorite_state() {
        let mut harness = harness();
        let mut historical = song("used to be favorite");
        historical.starred = true;
        historical.starred_at = Some("2026-09-01T00:00:00Z".into());

        harness.app.active_profile = Some(profile());
        harness.app.home.recently_played = Loadable::Loaded(vec![PlayHistory {
            track: historical.clone(),
            played_at: Some("2026-08-31T00:00:00Z".into()),
            context: None,
        }]);
        harness.app.library.favorite_songs.set_cached(Vec::new());
        harness.app.home.random_songs = Loadable::Loaded(Vec::new());
        harness.app.home.random_refreshing = false;
        harness.app.daily_mix_day = None;
        harness.app.saved.insert(historical.id.clone(), false);

        harness.app.refresh_daily_mix_if_needed();

        assert_eq!(harness.app.is_saved(&historical.id), Some(false));
        let mix = harness.app.home.daily_mix.get().unwrap();
        assert_eq!(mix[0].id, historical.id);
        assert!(!mix[0].starred);
    }

    #[test]
    fn an_empty_daily_mix_can_fill_when_history_arrives_later_that_day() {
        let mut harness = harness();
        harness.app.active_profile = Some(profile());
        harness.app.library.favorite_songs.set_cached(Vec::new());
        harness.app.home.random_songs = Loadable::Loaded(Vec::new());
        harness.app.home.random_refreshing = false;
        harness.app.home.recently_played = Loadable::Loaded(Vec::new());
        harness.app.daily_mix_day = None;

        harness.app.refresh_daily_mix_if_needed();

        assert!(harness.app.home.daily_mix.get().unwrap().is_empty());
        assert_eq!(harness.app.home.daily_mix_revision, 1);
        assert!(harness.app.daily_mix_day.is_none());
        assert!(!harness.app.dirs.daily_mix_file(&profile()).exists());

        let listened = song("qualified play");
        harness.app.home.recently_played = Loadable::Loaded(vec![PlayHistory {
            track: listened.clone(),
            played_at: Some("2026-09-02T00:00:00Z".into()),
            context: None,
        }]);
        harness.app.refresh_daily_mix_if_needed();

        assert_eq!(harness.app.home.daily_mix.get().unwrap()[0].id, listened.id);
        assert_eq!(harness.app.home.daily_mix_revision, 2);
        assert!(harness.app.daily_mix_day.is_some());
        assert!(harness.app.dirs.daily_mix_file(&profile()).is_file());
    }

    #[test]
    fn sign_in_immediately_releases_the_password_field() {
        let mut harness = harness();
        harness.app.login_server = "https://music.example".into();
        harness.app.login_username = "listener".into();
        harness.app.login_password = "top-secret".into();
        harness.app.apply(Action::SignIn, &egui::Context::default());
        assert!(harness.app.login_password.is_empty());
    }

    #[test]
    fn tray_show_switches_the_mini_player_back_to_the_main_window() {
        let mut harness = harness();
        harness.app.settings.winamp_window = true;

        harness.app.handle_tray_command(TrayCommand::Show);
        harness.app.apply_actions(&egui::Context::default());

        assert!(!harness.app.settings.winamp_window);
        assert!(harness.app.switch_intent);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn show_during_window_teardown_preserves_the_recreate_intent() {
        let mut harness = harness();
        let ctx = egui::Context::default();

        let hidden = ctx.run_logic(&egui::RawInput::default(), |ctx| {
            harness.app.hide_window(ctx, false);
        });
        assert!(
            hidden.viewport_commands[&egui::ViewportId::ROOT]
                .contains(&egui::ViewportCommand::Close)
        );
        assert!(harness.app.window_hidden);
        assert!(harness.app.hide_intent);

        ctx.run_logic(&egui::RawInput::default(), |ctx| {
            harness.app.show_window(ctx);
        });
        assert!(!harness.app.window_hidden);
        assert!(harness.app.hide_intent);

        harness.app.window_gone();
        assert!(!harness.app.window_hidden);
        assert!(!harness.app.hide_intent);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn close_request_hides_window_without_requeueing_close() {
        let mut harness = harness();
        let ctx = egui::Context::default();

        let hidden = ctx.run_logic(&egui::RawInput::default(), |ctx| {
            harness.app.hide_window(ctx, true);
        });
        assert!(
            !hidden.viewport_commands[&egui::ViewportId::ROOT]
                .contains(&egui::ViewportCommand::Close)
        );
        assert!(harness.app.window_hidden);
        assert!(harness.app.hide_intent);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn close_to_tray_keeps_event_loop_alive_and_tray_show_restores_window() {
        let mut harness = harness();
        harness.app.tray = TrayService::spawn(|| {});
        harness.app.settings.keep_playing_in_background = true;
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .events
            .push(egui::ViewportEvent::Close);

        let hidden = ctx.run_logic(&input, |ctx| {
            harness.app.background_frame(ctx);
        });
        let hide_commands = &hidden.viewport_commands[&egui::ViewportId::ROOT];
        assert!(hide_commands.contains(&egui::ViewportCommand::CancelClose));
        assert!(hide_commands.contains(&egui::ViewportCommand::Visible(false)));
        assert!(!hide_commands.contains(&egui::ViewportCommand::Close));
        assert!(harness.app.window_hidden);

        harness.app.handle_tray_command(TrayCommand::Show);
        let shown = ctx.run_logic(&egui::RawInput::default(), |ctx| {
            harness.app.background_frame(ctx);
        });
        let show_commands = &shown.viewport_commands[&egui::ViewportId::ROOT];
        assert!(show_commands.contains(&egui::ViewportCommand::Visible(true)));
        assert!(show_commands.contains(&egui::ViewportCommand::Minimized(false)));
        assert!(show_commands.contains(&egui::ViewportCommand::Focus));
        assert!(!harness.app.window_hidden);
    }

    #[test]
    fn queue_view_preserves_duplicate_occurrence_identity() {
        let mut harness = harness();
        harness.app.demo_set_playback(
            crate::player::demo_snapshot(crate::player::DemoPlayback {
                current: Some(song("same-song")),
                manual: vec![song("same-song")],
                context: Vec::new(),
                position_ms: 0,
                playback: Playback::Paused,
                volume: harness.app.settings.volume,
                shuffle: false,
                repeat: RepeatMode::Off,
            }),
            None,
        );
        let view = harness.app.queue_view();
        assert_eq!(view.current.as_ref().unwrap().song.id, view.rows[0].song.id);
        assert_ne!(
            view.current.unwrap().occurrence_id,
            view.rows[0].occurrence_id
        );
    }
}
