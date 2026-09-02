//! Non-blocking bridge between egui, one Navidrome session, and the local
//! authoritative player.
//!
//! Authentication and metadata work run on a dedicated Tokio runtime. Player
//! commands reduce synchronously so the caller can display the authoritative
//! queue immediately. Every session-bound result carries an epoch; work from a
//! replaced account is discarded.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::api::{
    Album, AlbumListRequest, AlbumListType, ApiClient, ApiError, Artist, Credentials, Favorites,
    MediaId, MediaKind, NetActivity, Page, PlayHistory, Playlist, PlaylistUpdate,
    RandomSongsRequest, Scrobble, SearchOptions, SearchResults, Song, VerifiedServer,
};
use crate::history::History;
use crate::images::{ArtLoader, accent_color};
use crate::paths::AppDirs;
use crate::player::{
    CommandReceipt, Engine, EngineConfig, EngineEvent, Playback, PlaybackSnapshot, PlayerCommand,
};

pub const PLAYLIST_PAGE_SIZE: u32 = 50;
pub type SessionEpoch = u64;
pub type BackendResult<T> = Result<T, BackendError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RequestId(u64);

impl RequestId {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BackendError {
    #[error("not signed in")]
    SignedOut,
    #[error("server URL or username is invalid")]
    InvalidCredentials,
    #[error("unable to save Navidrome credentials")]
    CredentialStore,
    #[error("unable to save local play history")]
    HistoryStore,
    #[error("authentication failed")]
    Authentication,
    #[error("unable to reach the Navidrome server")]
    Network,
    #[error("the Navidrome server rejected the request")]
    Server,
    #[error("the Navidrome server returned an invalid response")]
    InvalidResponse,
    #[error("request is too large")]
    RequestTooLarge,
    #[error("media reference is invalid for the active server")]
    InvalidReference,
    #[error("local playback is unavailable")]
    PlaybackUnavailable,
    #[error("local playback failed")]
    Playback,
    #[error("lyrics request failed")]
    Lyrics,
    #[error("artwork request failed")]
    Artwork,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AuthStatus {
    Starting,
    SignedOut,
    Connecting,
    Connected(Box<VerifiedServer>),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HomeResponse {
    pub newest: Page<Album>,
    pub recent: Page<Album>,
    pub frequent: Page<Album>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScrobbleRequest {
    pub song: MediaId,
    pub time_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiRequest {
    Home {
        music_folder_id: Option<String>,
        album_limit: u32,
        generation: u64,
    },
    AlbumList {
        request: AlbumListRequest,
        generation: u64,
    },
    RandomSongs {
        request: RandomSongsRequest,
        generation: u64,
    },
    Artists {
        music_folder_id: Option<String>,
        generation: u64,
    },
    Artist {
        id: MediaId,
        generation: u64,
    },
    Album {
        id: MediaId,
        generation: u64,
    },
    Song {
        id: MediaId,
        generation: u64,
    },
    Playlists {
        generation: u64,
    },
    Playlist {
        id: MediaId,
        generation: u64,
    },
    CreatePlaylist {
        name: String,
        songs: Vec<MediaId>,
        generation: u64,
    },
    UpdatePlaylist {
        playlist: MediaId,
        name: Option<String>,
        description: Option<String>,
        public: Option<bool>,
        generation: u64,
    },
    AddToPlaylist {
        playlist: MediaId,
        songs: Vec<MediaId>,
        generation: u64,
    },
    /// Replaces the complete playlist order in one server operation. Songs
    /// remain typed so duplicates and their exact order are preserved.
    ReorderPlaylist {
        playlist: MediaId,
        songs: Vec<MediaId>,
        generation: u64,
    },
    RemoveFromPlaylist {
        playlist: MediaId,
        row_indices: Vec<u32>,
        generation: u64,
    },
    DeletePlaylist {
        playlist: MediaId,
        generation: u64,
    },
    Favorites {
        music_folder_id: Option<String>,
        generation: u64,
    },
    SetFavorite {
        media: MediaId,
        favorite: bool,
        generation: u64,
    },
    Search {
        query: String,
        options: SearchOptions,
        generation: u64,
    },
    Scrobble {
        entries: Vec<ScrobbleRequest>,
        submission: bool,
        generation: u64,
    },
}

impl ApiRequest {
    pub fn generation(&self) -> u64 {
        match self {
            Self::Home { generation, .. }
            | Self::AlbumList { generation, .. }
            | Self::RandomSongs { generation, .. }
            | Self::Artists { generation, .. }
            | Self::Artist { generation, .. }
            | Self::Album { generation, .. }
            | Self::Song { generation, .. }
            | Self::Playlists { generation }
            | Self::Playlist { generation, .. }
            | Self::CreatePlaylist { generation, .. }
            | Self::UpdatePlaylist { generation, .. }
            | Self::AddToPlaylist { generation, .. }
            | Self::ReorderPlaylist { generation, .. }
            | Self::RemoveFromPlaylist { generation, .. }
            | Self::DeletePlaylist { generation, .. }
            | Self::Favorites { generation, .. }
            | Self::SetFavorite { generation, .. }
            | Self::Search { generation, .. }
            | Self::Scrobble { generation, .. } => *generation,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiPayload {
    Home(HomeResponse),
    Albums(Page<Album>),
    RandomSongs(Vec<Song>),
    Artists(Vec<Artist>),
    Artist(Artist),
    Album(Album),
    Song(Box<Song>),
    Playlists(Vec<Playlist>),
    Playlist(Playlist),
    PlaylistCreated(Playlist),
    PlaylistChanged(MediaId),
    PlaylistDeleted(MediaId),
    Favorites(Favorites),
    FavoriteChanged { media: MediaId, favorite: bool },
    Search(SearchResults),
    Scrobbled { count: usize, submission: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiResponse {
    pub epoch: SessionEpoch,
    pub request_id: RequestId,
    pub generation: u64,
    pub result: BackendResult<ApiPayload>,
}

pub enum Command {
    SignIn {
        server: String,
        username: String,
        password: String,
    },
    SignOut,
    RestartEngine(EngineConfig),
    CheckForUpdates,
    Shutdown,
}

pub enum Event {
    Auth {
        epoch: SessionEpoch,
        status: AuthStatus,
    },
    Player {
        epoch: SessionEpoch,
        snapshot: Box<PlaybackSnapshot>,
    },
    Api(Box<ApiResponse>),
    LocalHistory {
        epoch: SessionEpoch,
        request_id: RequestId,
        generation: u64,
        plays: Vec<PlayHistory>,
    },
    Lyrics {
        epoch: SessionEpoch,
        request_id: RequestId,
        media: MediaId,
        result: BackendResult<Option<crate::lyrics::Lyrics>>,
    },
    Accent {
        epoch: SessionEpoch,
        request_id: RequestId,
        reference: String,
        result: BackendResult<Option<[u8; 3]>>,
    },
    Error {
        epoch: SessionEpoch,
        message: String,
    },
    UpdateAvailable {
        version: String,
        url: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerReceipt {
    pub epoch: SessionEpoch,
    pub command: CommandReceipt,
    pub snapshot: PlaybackSnapshot,
}

#[derive(Clone, Default)]
pub struct Waker(Arc<Mutex<Option<egui::Context>>>);

impl Waker {
    pub fn attach(&self, ctx: &egui::Context) {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(ctx.clone());
    }

    pub fn detach(&self) {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    pub fn wake(&self) {
        if let Some(ctx) = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
        {
            ctx.request_repaint();
        }
    }
}

type EngineSlot = Arc<Mutex<Option<(SessionEpoch, Arc<Engine>)>>>;

pub struct Backend {
    messages: mpsc::UnboundedSender<Message>,
    events: std::sync::mpsc::Receiver<Event>,
    art: ArtLoader,
    activity: Arc<NetActivity>,
    engine: EngineSlot,
    next_request: AtomicU64,
    thread: Option<std::thread::JoinHandle<()>>,
    offline: bool,
}

impl Backend {
    pub fn spawn(
        dirs: AppDirs,
        engine_config: EngineConfig,
        waker: Waker,
        restore_session: bool,
    ) -> Self {
        let (message_tx, message_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("fastpotify-runtime")
            .enable_all()
            .build()
            .expect("unable to start the async runtime");
        let http = reqwest::Client::builder()
            .user_agent(concat!("fastpotify/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()
            .expect("unable to build the HTTP client");
        let art = ArtLoader::new(runtime.handle().clone(), dirs.art_cache_dir());
        let activity = Arc::new(NetActivity::default());
        let engine = Arc::new(Mutex::new(None));
        let tracker = Arc::new(Mutex::new(ScrobbleTracker::default()));

        let worker_art = art.clone();
        let worker_activity = Arc::clone(&activity);
        let worker_engine = Arc::clone(&engine);
        let worker_tracker = Arc::clone(&tracker);
        let worker_messages = message_tx.clone();
        let thread = std::thread::Builder::new()
            .name("fastpotify-backend".to_owned())
            .spawn(move || {
                runtime.block_on(async move {
                    Worker::new(
                        dirs,
                        engine_config,
                        restore_session,
                        http,
                        worker_art,
                        worker_activity,
                        worker_engine,
                        worker_tracker,
                        event_tx,
                        worker_messages,
                        waker,
                    )
                    .run(message_rx)
                    .await;
                });
                runtime.shutdown_timeout(Duration::from_secs(2));
            })
            .expect("unable to start the backend thread");

        Self {
            messages: message_tx,
            events: event_rx,
            art,
            activity,
            engine,
            next_request: AtomicU64::new(1),
            thread: Some(thread),
            offline: false,
        }
    }

    pub fn activity(&self) -> &NetActivity {
        &self.activity
    }

    #[cfg_attr(not(any(test, feature = "demo")), allow(dead_code))]
    pub fn set_offline(&mut self, offline: bool) {
        self.offline = offline;
    }

    pub fn send(&self, command: Command) {
        if self.offline && !matches!(command, Command::Shutdown | Command::SignOut) {
            return;
        }
        let _ = self.messages.send(Message::Command(command));
    }

    /// Reduces a player command immediately and returns the same authoritative
    /// snapshot that the event stream will publish.
    pub fn player(&self, command: PlayerCommand) -> BackendResult<PlayerReceipt> {
        let (epoch, engine) = self
            .engine
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .cloned()
            .ok_or(BackendError::PlaybackUnavailable)?;
        let receipt = engine
            .command(command)
            .map_err(|_| BackendError::Playback)?;
        Ok(PlayerReceipt {
            epoch,
            command: receipt,
            snapshot: engine.snapshot(),
        })
    }

    pub fn api(&self, request: ApiRequest) -> RequestId {
        let request_id = self.request_id();
        if !self.offline {
            let _ = self.messages.send(Message::Api {
                request_id,
                request,
            });
        }
        request_id
    }

    pub fn history(&self, generation: u64) -> RequestId {
        let request_id = self.request_id();
        if !self.offline {
            let _ = self.messages.send(Message::History {
                request_id,
                generation,
            });
        }
        request_id
    }

    /// Clears only the active profile's local play history and publishes the
    /// resulting empty snapshot with the caller's generation.
    pub fn clear_history(&self, generation: u64) -> RequestId {
        let request_id = self.request_id();
        if !self.offline {
            let _ = self.messages.send(Message::ClearHistory {
                request_id,
                generation,
            });
        }
        request_id
    }

    pub fn lyrics(&self, query: crate::lyrics::Query) -> RequestId {
        let request_id = self.request_id();
        if !self.offline {
            let _ = self.messages.send(Message::Lyrics { request_id, query });
        }
        request_id
    }

    pub fn accent(&self, reference: String) -> RequestId {
        let request_id = self.request_id();
        let _ = self.messages.send(Message::Accent {
            request_id,
            reference,
        });
        request_id
    }

    fn request_id(&self) -> RequestId {
        RequestId(self.next_request.fetch_add(1, Ordering::Relaxed))
    }

    pub fn poll(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    pub fn art(&self) -> &ArtLoader {
        &self.art
    }

    pub fn shutdown(&mut self) {
        if self.thread.is_none() {
            return;
        }
        let _ = self.messages.send(Message::Command(Command::Shutdown));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.shutdown();
    }
}

enum Message {
    Command(Command),
    Api {
        request_id: RequestId,
        request: ApiRequest,
    },
    History {
        request_id: RequestId,
        generation: u64,
    },
    ClearHistory {
        request_id: RequestId,
        generation: u64,
    },
    Lyrics {
        request_id: RequestId,
        query: crate::lyrics::Query,
    },
    Accent {
        request_id: RequestId,
        reference: String,
    },
    LoginFinished {
        epoch: SessionEpoch,
        result: BackendResult<LoginReady>,
    },
    ApiFinished(ApiResponse),
    PlayerSnapshot {
        epoch: SessionEpoch,
        snapshot: Box<PlaybackSnapshot>,
    },
    ScrobbleTick {
        epoch: SessionEpoch,
        token: u64,
    },
    LyricsFinished {
        epoch: SessionEpoch,
        request_id: RequestId,
        media: MediaId,
        result: BackendResult<Option<crate::lyrics::Lyrics>>,
    },
    AccentFinished {
        epoch: SessionEpoch,
        request_id: RequestId,
        reference: String,
        result: BackendResult<Option<[u8; 3]>>,
    },
    UpdateFinished(Option<crate::updates::Release>),
}

struct LoginReady {
    client: Arc<ApiClient>,
    verified: VerifiedServer,
    credentials_to_save: Option<Credentials>,
}

#[derive(Clone, Default)]
struct SessionClock(Arc<AtomicU64>);

impl SessionClock {
    fn current(&self) -> SessionEpoch {
        self.0.load(Ordering::Acquire)
    }

    fn advance(&self) -> SessionEpoch {
        let current = self.current();
        if current == SessionEpoch::MAX {
            return current;
        }
        self.0.fetch_add(1, Ordering::AcqRel) + 1
    }

    fn accepts(&self, epoch: SessionEpoch) -> bool {
        self.current() == epoch
    }
}

struct Worker {
    dirs: AppDirs,
    engine_config: EngineConfig,
    restore_session: bool,
    http: reqwest::Client,
    activity: Arc<NetActivity>,
    art: ArtLoader,
    engine: EngineSlot,
    tracker: Arc<Mutex<ScrobbleTracker>>,
    client: Option<Arc<ApiClient>>,
    history: Option<History>,
    clock: SessionClock,
    events: std::sync::mpsc::Sender<Event>,
    messages: mpsc::UnboundedSender<Message>,
    waker: Waker,
}

impl Worker {
    #[allow(clippy::too_many_arguments)]
    fn new(
        dirs: AppDirs,
        engine_config: EngineConfig,
        restore_session: bool,
        http: reqwest::Client,
        art: ArtLoader,
        activity: Arc<NetActivity>,
        engine: EngineSlot,
        tracker: Arc<Mutex<ScrobbleTracker>>,
        events: std::sync::mpsc::Sender<Event>,
        messages: mpsc::UnboundedSender<Message>,
        waker: Waker,
    ) -> Self {
        Self {
            dirs,
            engine_config,
            restore_session,
            http,
            activity,
            art,
            engine,
            tracker,
            client: None,
            history: None,
            clock: SessionClock::default(),
            events,
            messages,
            waker,
        }
    }

    async fn run(mut self, mut incoming: mpsc::UnboundedReceiver<Message>) {
        self.emit(Event::Auth {
            epoch: self.clock.current(),
            status: AuthStatus::Starting,
        });
        if self.restore_session {
            self.restore_session();
        } else {
            self.emit_auth(AuthStatus::SignedOut);
        }

        while let Some(message) = incoming.recv().await {
            match message {
                Message::Command(Command::Shutdown) => break,
                Message::Command(Command::SignIn {
                    server,
                    username,
                    password,
                }) => self.sign_in(server, username, password),
                Message::Command(Command::SignOut) => self.sign_out(),
                Message::Command(Command::RestartEngine(config)) => {
                    self.engine_config = config;
                    self.start_engine();
                }
                Message::Command(Command::CheckForUpdates) => self.check_for_updates(),
                Message::Api {
                    request_id,
                    request,
                } => self.dispatch_api(request_id, request),
                Message::History {
                    request_id,
                    generation,
                } => self.emit_history(request_id, generation),
                Message::ClearHistory {
                    request_id,
                    generation,
                } => self.clear_history(request_id, generation),
                Message::Lyrics { request_id, query } => self.fetch_lyrics(request_id, query),
                Message::Accent {
                    request_id,
                    reference,
                } => self.fetch_accent(request_id, reference),
                Message::LoginFinished { epoch, result } => self.finish_login(epoch, result),
                Message::ApiFinished(response) => {
                    if self.clock.accepts(response.epoch) {
                        self.emit(Event::Api(Box::new(response)));
                    }
                }
                Message::PlayerSnapshot { epoch, snapshot } => {
                    if self.clock.accepts(epoch) {
                        self.handle_snapshot(epoch, *snapshot);
                    }
                }
                Message::ScrobbleTick { epoch, token } => {
                    if self.clock.accepts(epoch) {
                        self.handle_scrobble_tick(epoch, token);
                    }
                }
                Message::LyricsFinished {
                    epoch,
                    request_id,
                    media,
                    result,
                } => {
                    if self.clock.accepts(epoch) {
                        self.emit(Event::Lyrics {
                            epoch,
                            request_id,
                            media,
                            result,
                        });
                    }
                }
                Message::AccentFinished {
                    epoch,
                    request_id,
                    reference,
                    result,
                } => {
                    if self.clock.accepts(epoch) {
                        self.emit(Event::Accent {
                            epoch,
                            request_id,
                            reference,
                            result,
                        });
                    }
                }
                Message::UpdateFinished(Some(release)) => {
                    self.emit(Event::UpdateAvailable {
                        version: release.version,
                        url: release.url,
                    });
                }
                Message::UpdateFinished(None) => {}
            }
        }
        self.stop_session();
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
        self.waker.wake();
    }

    fn emit_auth(&self, status: AuthStatus) {
        self.emit(Event::Auth {
            epoch: self.clock.current(),
            status,
        });
    }

    fn restore_session(&mut self) {
        let volume = self.current_volume();
        let path = self.dirs.credentials_file();
        if !path.exists() {
            self.emit_auth(AuthStatus::SignedOut);
            return;
        }
        let epoch = self.clock.advance();
        self.stop_session();
        match Credentials::load(&path) {
            Ok(credentials) => {
                self.begin_login(epoch, credentials, false);
                self.emit_cleared_player(volume);
            }
            Err(_) => {
                self.emit_auth(AuthStatus::Failed(
                    BackendError::CredentialStore.to_string(),
                ));
                self.emit_cleared_player(volume);
            }
        }
    }

    fn sign_in(&mut self, server: String, username: String, password: String) {
        let volume = self.current_volume();
        let epoch = self.clock.advance();
        self.stop_session();
        match Credentials::new(server, username, password) {
            Ok(credentials) => {
                self.begin_login(epoch, credentials, true);
                self.emit_cleared_player(volume);
            }
            Err(_) => {
                self.emit_auth(AuthStatus::Failed(
                    BackendError::InvalidCredentials.to_string(),
                ));
                self.emit_cleared_player(volume);
            }
        }
    }

    fn begin_login(&self, epoch: SessionEpoch, credentials: Credentials, persist: bool) {
        self.emit_auth(AuthStatus::Connecting);
        let activity = Arc::clone(&self.activity);
        let clock = self.clock.clone();
        let messages = self.messages.clone();
        tokio::spawn(async move {
            if !clock.accepts(epoch) {
                return;
            }
            let result = async {
                let client =
                    Arc::new(ApiClient::new(credentials.clone(), activity).map_err(map_api_error)?);
                let verified = client.verify().await.map_err(map_login_error)?;
                Ok(LoginReady {
                    client,
                    verified,
                    credentials_to_save: persist.then_some(credentials),
                })
            }
            .await;
            if clock.accepts(epoch) {
                let _ = messages.send(Message::LoginFinished { epoch, result });
            }
        });
    }

    fn finish_login(&mut self, epoch: SessionEpoch, result: BackendResult<LoginReady>) {
        if !self.clock.accepts(epoch) {
            return;
        }
        match result {
            Ok(ready) => {
                if let Some(credentials) = ready.credentials_to_save.as_ref()
                    && let Err(error) =
                        persist_credentials(credentials, &self.dirs.credentials_file())
                {
                    self.emit_auth(AuthStatus::Failed(error.to_string()));
                    return;
                }
                self.history = Some(History::load(
                    &self.dirs.history_file(&ready.verified.profile),
                    &ready.verified.profile,
                ));
                self.art.set_client(Some(Arc::clone(&ready.client)));
                self.client = Some(ready.client);
                self.start_engine();
                self.emit_auth(AuthStatus::Connected(Box::new(ready.verified)));
            }
            Err(error) => {
                self.emit_auth(AuthStatus::Failed(error.to_string()));
            }
        }
    }

    fn sign_out(&mut self) {
        let volume = self.current_volume();
        self.clock.advance();
        self.stop_session();
        if let Err(error) = remove_credentials(&self.dirs.credentials_file()) {
            self.emit(Event::Error {
                epoch: self.clock.current(),
                message: error.to_string(),
            });
        }
        self.emit_auth(AuthStatus::SignedOut);
        self.emit_cleared_player(volume);
    }

    fn stop_session(&mut self) {
        self.stop_engine();
        if let (Some(client), Some(history)) = (self.client.as_ref(), self.history.as_mut()) {
            history.save(&self.dirs.history_file(client.profile_id()));
        }
        self.history = None;
        self.client = None;
        self.art.set_client(None);
        *self
            .tracker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = ScrobbleTracker::default();
    }

    fn stop_engine(&self) {
        if let Some((_, engine)) = self
            .engine
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let _ = engine.command(PlayerCommand::Stop);
            engine.shutdown();
        }
    }

    fn start_engine(&mut self) {
        self.stop_engine();
        let Some(client) = self.client.as_ref().cloned() else {
            return;
        };
        let epoch = self.clock.current();
        let messages = self.messages.clone();
        let notify = Arc::new(move |event| {
            let EngineEvent::Snapshot(snapshot) = event;
            let _ = messages.send(Message::PlayerSnapshot {
                epoch,
                snapshot: Box::new(snapshot),
            });
        });
        match Engine::new(
            self.engine_config.clone(),
            client,
            tokio::runtime::Handle::current(),
            notify,
        ) {
            Ok(engine) => {
                *self
                    .engine
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                    Some((epoch, Arc::new(engine)));
            }
            Err(_) => self.emit(Event::Error {
                epoch,
                message: BackendError::PlaybackUnavailable.to_string(),
            }),
        }
    }

    fn dispatch_api(&self, request_id: RequestId, request: ApiRequest) {
        let epoch = self.clock.current();
        let generation = request.generation();
        let Some(client) = self.client.as_ref().cloned() else {
            self.emit(Event::Api(Box::new(ApiResponse {
                epoch,
                request_id,
                generation,
                result: Err(BackendError::SignedOut),
            })));
            return;
        };
        let clock = self.clock.clone();
        let messages = self.messages.clone();
        tokio::spawn(async move {
            if !clock.accepts(epoch) {
                return;
            }
            let result = perform_api(&client, request).await;
            if clock.accepts(epoch) {
                let _ = messages.send(Message::ApiFinished(ApiResponse {
                    epoch,
                    request_id,
                    generation,
                    result,
                }));
            }
        });
    }

    fn emit_history(&self, request_id: RequestId, generation: u64) {
        let plays = self
            .history
            .as_ref()
            .map(|history| history.plays().to_vec())
            .unwrap_or_default();
        self.emit(Event::LocalHistory {
            epoch: self.clock.current(),
            request_id,
            generation,
            plays,
        });
    }

    fn clear_history(&mut self, request_id: RequestId, generation: u64) {
        let epoch = self.clock.current();
        let mut plays = Vec::new();
        if let (Some(client), Some(history)) = (self.client.as_ref(), self.history.as_mut()) {
            let path = self.dirs.history_file(client.profile_id());
            if let Err(error) = persist_cleared_history(history, &path) {
                // Keep the in-memory list unchanged and publish that durable
                // truth rather than letting a failed clear reappear at restart.
                plays = history.plays().to_vec();
                self.emit(Event::Error {
                    epoch,
                    message: error.to_string(),
                });
            }
        }
        self.emit(Event::LocalHistory {
            epoch,
            request_id,
            generation,
            plays,
        });
    }

    fn fetch_lyrics(&self, request_id: RequestId, query: crate::lyrics::Query) {
        let epoch = self.clock.current();
        let media = query.media.clone();
        let Some(client) = self.client.as_ref().cloned() else {
            self.emit(Event::Lyrics {
                epoch,
                request_id,
                media,
                result: Err(BackendError::SignedOut),
            });
            return;
        };
        let http = self.http.clone();
        let cache_dir = self.dirs.lyrics_cache_dir();
        let clock = self.clock.clone();
        let messages = self.messages.clone();
        tokio::spawn(async move {
            if !clock.accepts(epoch) {
                return;
            }
            let result = crate::lyrics::fetch(&client, &http, &cache_dir, &query)
                .await
                .map_err(|_| BackendError::Lyrics);
            if clock.accepts(epoch) {
                let _ = messages.send(Message::LyricsFinished {
                    epoch,
                    request_id,
                    media,
                    result,
                });
            }
        });
    }

    fn fetch_accent(&self, request_id: RequestId, reference: String) {
        let epoch = self.clock.current();
        let art = self.art.clone();
        let clock = self.clock.clone();
        let messages = self.messages.clone();
        tokio::spawn(async move {
            if !clock.accepts(epoch) {
                return;
            }
            let result = art
                .fetch(&reference)
                .await
                .map(|bytes| accent_color(&bytes))
                .map_err(|_| BackendError::Artwork);
            if clock.accepts(epoch) {
                let _ = messages.send(Message::AccentFinished {
                    epoch,
                    request_id,
                    reference,
                    result,
                });
            }
        });
    }

    fn check_for_updates(&self) {
        let http = self.http.clone();
        let messages = self.messages.clone();
        tokio::spawn(async move {
            let release = crate::updates::newer_release(&http).await.ok().flatten();
            let _ = messages.send(Message::UpdateFinished(release));
        });
    }

    fn handle_snapshot(&mut self, epoch: SessionEpoch, snapshot: PlaybackSnapshot) {
        let observation = Observation::from_snapshot(&snapshot);
        let output = self
            .tracker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .observe_at(observation, Instant::now());
        self.handle_scrobble_actions(epoch, output.actions);
        self.schedule_scrobble_tick(epoch, output.timer);
        self.emit(Event::Player {
            epoch,
            snapshot: Box::new(snapshot),
        });
    }

    fn handle_scrobble_tick(&mut self, epoch: SessionEpoch, token: u64) {
        let output = self
            .tracker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .tick_at(token, Instant::now());
        self.handle_scrobble_actions(epoch, output.actions);
        self.schedule_scrobble_tick(epoch, output.timer);
    }

    fn schedule_scrobble_tick(&self, epoch: SessionEpoch, timer: Option<ScrobbleTimer>) {
        let Some(timer) = timer else { return };
        let clock = self.clock.clone();
        let messages = self.messages.clone();
        tokio::spawn(async move {
            tokio::time::sleep(timer.after).await;
            if clock.accepts(epoch) {
                let _ = messages.send(Message::ScrobbleTick {
                    epoch,
                    token: timer.token,
                });
            }
        });
    }

    fn handle_scrobble_actions(&mut self, epoch: SessionEpoch, actions: Vec<ScrobbleAction>) {
        if actions.is_empty() {
            return;
        }
        let Some(client) = self.client.as_ref().cloned() else {
            return;
        };
        let mut requests = Vec::with_capacity(actions.len());
        for action in actions {
            if action.submission
                && let Some(history) = self.history.as_mut()
            {
                history.record(action.song.clone(), jiff::Timestamp::now(), None);
                history.save(&self.dirs.history_file(client.profile_id()));
            }
            requests.push((Scrobble::now(action.song.id), action.submission));
        }
        let clock = self.clock.clone();
        tokio::spawn(async move {
            for (request, submission) in requests {
                if !clock.accepts(epoch) {
                    break;
                }
                let _ = client.scrobble(&[request], submission).await;
            }
        });
    }

    fn current_volume(&self) -> u16 {
        self.engine
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|(_, engine)| engine.snapshot().volume)
            .unwrap_or(self.engine_config.initial_volume)
    }

    fn emit_cleared_player(&self, volume: u16) {
        self.emit(Event::Player {
            epoch: self.clock.current(),
            snapshot: Box::new(PlaybackSnapshot {
                volume,
                ..PlaybackSnapshot::default()
            }),
        });
    }
}

async fn perform_api(client: &ApiClient, request: ApiRequest) -> BackendResult<ApiPayload> {
    match request {
        ApiRequest::Home {
            music_folder_id,
            album_limit,
            ..
        } => {
            let album_request = |kind| AlbumListRequest {
                kind,
                limit: album_limit.clamp(1, 500),
                music_folder_id: music_folder_id.clone(),
                ..AlbumListRequest::default()
            };
            let newest_request = album_request(AlbumListType::Newest);
            let recent_request = album_request(AlbumListType::Recent);
            let frequent_request = album_request(AlbumListType::Frequent);
            let (newest, recent, frequent) = tokio::try_join!(
                client.album_list2(&newest_request),
                client.album_list2(&recent_request),
                client.album_list2(&frequent_request),
            )
            .map_err(map_api_error)?;
            Ok(ApiPayload::Home(HomeResponse {
                newest,
                recent,
                frequent,
            }))
        }
        ApiRequest::AlbumList { request, .. } => client
            .album_list2(&request)
            .await
            .map(ApiPayload::Albums)
            .map_err(map_api_error),
        ApiRequest::RandomSongs { request, .. } => client
            .random_songs(&request)
            .await
            .map(ApiPayload::RandomSongs)
            .map_err(map_api_error),
        ApiRequest::Artists {
            music_folder_id, ..
        } => client
            .artists(music_folder_id.as_deref())
            .await
            .map(ApiPayload::Artists)
            .map_err(map_api_error),
        ApiRequest::Artist { id, .. } => {
            checked_media(client, &id, MediaKind::Artist)?;
            client
                .artist(&id)
                .await
                .map(ApiPayload::Artist)
                .map_err(map_api_error)
        }
        ApiRequest::Album { id, .. } => {
            checked_media(client, &id, MediaKind::Album)?;
            client
                .album(&id)
                .await
                .map(ApiPayload::Album)
                .map_err(map_api_error)
        }
        ApiRequest::Song { id, .. } => {
            checked_media(client, &id, MediaKind::Song)?;
            client
                .get_song(&id)
                .await
                .map(Box::new)
                .map(ApiPayload::Song)
                .map_err(map_api_error)
        }
        ApiRequest::Playlists { .. } => client
            .playlists(None)
            .await
            .map(ApiPayload::Playlists)
            .map_err(map_api_error),
        ApiRequest::Playlist { id, .. } => {
            checked_media(client, &id, MediaKind::Playlist)?;
            client
                .playlist(&id)
                .await
                .map(ApiPayload::Playlist)
                .map_err(map_api_error)
        }
        ApiRequest::CreatePlaylist { name, songs, .. } => {
            let songs = checked_songs(client, &songs)?;
            client
                .create_playlist(name.trim(), &songs)
                .await
                .map(ApiPayload::PlaylistCreated)
                .map_err(map_api_error)
        }
        ApiRequest::UpdatePlaylist {
            playlist,
            name,
            description,
            public,
            ..
        } => {
            checked_media(client, &playlist, MediaKind::Playlist)?;
            client
                .update_playlist(
                    &playlist,
                    &PlaylistUpdate {
                        name,
                        description,
                        public,
                        ..PlaylistUpdate::default()
                    },
                )
                .await
                .map_err(map_api_error)?;
            Ok(ApiPayload::PlaylistChanged(playlist))
        }
        ApiRequest::AddToPlaylist {
            playlist, songs, ..
        } => {
            checked_media(client, &playlist, MediaKind::Playlist)?;
            let songs = checked_songs(client, &songs)?;
            client
                .update_playlist(
                    &playlist,
                    &PlaylistUpdate {
                        songs_to_add: songs,
                        ..PlaylistUpdate::default()
                    },
                )
                .await
                .map_err(map_api_error)?;
            Ok(ApiPayload::PlaylistChanged(playlist))
        }
        ApiRequest::ReorderPlaylist {
            playlist, songs, ..
        } => {
            checked_media(client, &playlist, MediaKind::Playlist)?;
            let songs = checked_songs(client, &songs)?;
            client
                .replace_playlist_songs(&playlist, &songs)
                .await
                .map_err(map_api_error)?;
            Ok(ApiPayload::PlaylistChanged(playlist))
        }
        ApiRequest::RemoveFromPlaylist {
            playlist,
            row_indices,
            ..
        } => {
            checked_media(client, &playlist, MediaKind::Playlist)?;
            client
                .update_playlist(
                    &playlist,
                    &PlaylistUpdate {
                        song_indexes_to_remove: row_indices,
                        ..PlaylistUpdate::default()
                    },
                )
                .await
                .map_err(map_api_error)?;
            Ok(ApiPayload::PlaylistChanged(playlist))
        }
        ApiRequest::DeletePlaylist { playlist, .. } => {
            checked_media(client, &playlist, MediaKind::Playlist)?;
            client
                .delete_playlist(&playlist)
                .await
                .map_err(map_api_error)?;
            Ok(ApiPayload::PlaylistDeleted(playlist))
        }
        ApiRequest::Favorites {
            music_folder_id, ..
        } => client
            .favorites(music_folder_id.as_deref())
            .await
            .map(ApiPayload::Favorites)
            .map_err(map_api_error),
        ApiRequest::SetFavorite {
            media, favorite, ..
        } => {
            checked_favorite(client, &media)?;
            if favorite {
                client.star(&media).await.map_err(map_api_error)?;
            } else {
                client.unstar(&media).await.map_err(map_api_error)?;
            }
            Ok(ApiPayload::FavoriteChanged { media, favorite })
        }
        ApiRequest::Search { query, options, .. } => client
            .search3(query.trim(), &options)
            .await
            .map(ApiPayload::Search)
            .map_err(map_api_error),
        ApiRequest::Scrobble {
            entries,
            submission,
            ..
        } => {
            let mut wire = Vec::with_capacity(entries.len());
            for entry in &entries {
                checked_media(client, &entry.song, MediaKind::Song)?;
                wire.push(Scrobble {
                    song: entry.song.clone(),
                    time_ms: entry.time_ms,
                });
            }
            client
                .scrobble(&wire, submission)
                .await
                .map_err(map_api_error)?;
            Ok(ApiPayload::Scrobbled {
                count: wire.len(),
                submission,
            })
        }
    }
}

fn checked_media(client: &ApiClient, media: &MediaId, kind: MediaKind) -> BackendResult<()> {
    if media.profile != *client.profile_id()
        || media.kind != kind
        || media.id.is_empty()
        || !crate::media::is_media_ref(&media.uri())
    {
        return Err(BackendError::InvalidReference);
    }
    Ok(())
}

fn checked_songs(client: &ApiClient, songs: &[MediaId]) -> BackendResult<Vec<MediaId>> {
    songs
        .iter()
        .map(|song| {
            checked_media(client, song, MediaKind::Song)?;
            Ok(song.clone())
        })
        .collect()
}

fn checked_favorite(client: &ApiClient, media: &MediaId) -> BackendResult<()> {
    if !matches!(
        media.kind,
        MediaKind::Song | MediaKind::Album | MediaKind::Artist
    ) {
        return Err(BackendError::InvalidReference);
    }
    checked_media(client, media, media.kind)
}

fn map_api_error(error: ApiError) -> BackendError {
    match error {
        ApiError::ClientConfiguration => BackendError::InvalidCredentials,
        ApiError::RequestTooLarge => BackendError::RequestTooLarge,
        ApiError::Network(_) => BackendError::Network,
        ApiError::Http { .. } | ApiError::Protocol { .. } => BackendError::Server,
        ApiError::Decode
        | ApiError::MissingPayload(_)
        | ApiError::Conversion(_)
        | ApiError::UnexpectedContentType
        | ApiError::EmptyAudioStream => BackendError::InvalidResponse,
        ApiError::WrongProfile
        | ApiError::WrongMediaKind { .. }
        | ApiError::UnsupportedMediaKind
        | ApiError::InvalidArtworkReference
        | ApiError::InvalidMediaReference => BackendError::InvalidReference,
    }
}

fn map_login_error(error: ApiError) -> BackendError {
    match error {
        ApiError::Network(_) => BackendError::Network,
        ApiError::ClientConfiguration => BackendError::InvalidCredentials,
        ApiError::RequestTooLarge => BackendError::InvalidResponse,
        ApiError::Decode
        | ApiError::MissingPayload(_)
        | ApiError::Conversion(_)
        | ApiError::UnexpectedContentType
        | ApiError::EmptyAudioStream => BackendError::InvalidResponse,
        ApiError::Http { .. } | ApiError::Protocol { .. } => BackendError::Authentication,
        ApiError::WrongProfile
        | ApiError::WrongMediaKind { .. }
        | ApiError::UnsupportedMediaKind
        | ApiError::InvalidArtworkReference
        | ApiError::InvalidMediaReference => BackendError::InvalidResponse,
    }
}

fn persist_credentials(credentials: &Credentials, path: &Path) -> BackendResult<()> {
    credentials
        .save(path)
        .map_err(|_| BackendError::CredentialStore)
}

fn remove_credentials(path: &Path) -> BackendResult<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(BackendError::CredentialStore),
    }
}

fn persist_cleared_history(history: &mut History, path: &Path) -> BackendResult<()> {
    crate::paths::atomic_write(path, b"[]").map_err(|_| BackendError::HistoryStore)?;
    *history = History::default();
    Ok(())
}

#[derive(Clone)]
struct Observation {
    occurrence_id: u64,
    play_instance_id: u64,
    song: Song,
    playback: Playback,
    observed_at: Option<Instant>,
}

impl Observation {
    fn from_snapshot(snapshot: &PlaybackSnapshot) -> Option<Self> {
        let current = snapshot.current.as_ref()?;
        Some(Self {
            occurrence_id: current.occurrence_id.get(),
            play_instance_id: snapshot.play_instance_id,
            song: current.song.clone(),
            playback: snapshot.position.playback,
            observed_at: snapshot.position.observed_at,
        })
    }
}

struct TrackedPlay {
    occurrence_id: u64,
    play_instance_id: u64,
    song: Song,
    started: bool,
    listened: Duration,
    playing_since: Option<Instant>,
    submitted: bool,
}

impl TrackedPlay {
    fn settle(&mut self, now: Instant) -> bool {
        let Some(started) = self.playing_since.take() else {
            return false;
        };
        let elapsed = now.checked_duration_since(started).unwrap_or_default();
        self.listened = self.listened.saturating_add(elapsed);
        true
    }

    fn listened_at(&self, now: Instant) -> Duration {
        let current = self
            .playing_since
            .and_then(|started| now.checked_duration_since(started))
            .unwrap_or_default();
        self.listened.saturating_add(current)
    }

    fn threshold(&self) -> Duration {
        crate::history::counts_after(self.song.duration_ms)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ScrobbleAction {
    song: Song,
    submission: bool,
}

#[derive(Default)]
struct TrackerOutput {
    actions: Vec<ScrobbleAction>,
    timer: Option<ScrobbleTimer>,
}

struct ScrobbleTimer {
    token: u64,
    after: Duration,
}

struct ScheduledScrobbleTimer {
    token: u64,
    key: (u64, u64),
    deadline: Instant,
}

#[derive(Default)]
struct ScrobbleTracker {
    current: Option<TrackedPlay>,
    next_timer_token: u64,
    scheduled: Option<ScheduledScrobbleTimer>,
}

impl ScrobbleTracker {
    fn observe_at(&mut self, next: Option<Observation>, now: Instant) -> TrackerOutput {
        let mut actions = Vec::new();
        let Some(next) = next else {
            if let Some(mut current) = self.current.take() {
                Self::finish(&mut current, now, &mut actions);
            }
            return self.output(actions, now);
        };

        let replace = self.current.as_ref().is_some_and(|current| {
            current.occurrence_id != next.occurrence_id
                || current.play_instance_id != next.play_instance_id
        });
        if replace {
            if let Some(mut current) = self.current.take() {
                Self::finish(&mut current, now, &mut actions);
            }
            self.begin(next, now, &mut actions);
            return self.output(actions, now);
        }

        if next.playback == Playback::Stopped {
            // Stop resets position but the reducer deliberately keeps the
            // same occurrence/play-instance identity. Retain its tombstone so
            // a later Play cannot announce or submit that instance twice.
            if let Some(current) = self.current.as_mut() {
                Self::finish(current, now, &mut actions);
            }
            return self.output(actions, now);
        } else if let Some(current) = self.current.as_mut() {
            let was_playing = current.settle(now);
            Self::submit_if_ready(current, &mut actions);
            if next.playback == Playback::Playing {
                Self::start_playing(current, next.observed_at, now, was_playing, &mut actions);
            }
        } else {
            self.begin(next, now, &mut actions);
        }
        self.output(actions, now)
    }

    fn begin(&mut self, observation: Observation, now: Instant, actions: &mut Vec<ScrobbleAction>) {
        if observation.playback == Playback::Stopped {
            return;
        }
        let playback = observation.playback;
        let observed_at = observation.observed_at;
        let mut current = TrackedPlay {
            occurrence_id: observation.occurrence_id,
            play_instance_id: observation.play_instance_id,
            song: observation.song,
            started: false,
            listened: Duration::ZERO,
            playing_since: None,
            submitted: false,
        };
        if playback == Playback::Playing {
            Self::start_playing(&mut current, observed_at, now, false, actions);
        }
        self.current = Some(current);
    }

    fn start_playing(
        current: &mut TrackedPlay,
        observed_at: Option<Instant>,
        now: Instant,
        was_playing: bool,
        actions: &mut Vec<ScrobbleAction>,
    ) {
        if !current.started {
            current.started = true;
            actions.push(ScrobbleAction {
                song: current.song.clone(),
                submission: false,
            });
        }
        current.playing_since = Some(if was_playing {
            now
        } else {
            observed_at
                .filter(|observed_at| *observed_at <= now)
                .unwrap_or(now)
        });
    }

    fn finish(current: &mut TrackedPlay, now: Instant, actions: &mut Vec<ScrobbleAction>) {
        current.settle(now);
        Self::submit_if_ready(current, actions);
    }

    fn submit_if_ready(current: &mut TrackedPlay, actions: &mut Vec<ScrobbleAction>) {
        if current.started && !current.submitted && current.listened >= current.threshold() {
            current.submitted = true;
            actions.push(ScrobbleAction {
                song: current.song.clone(),
                submission: true,
            });
        }
    }

    fn tick_at(&mut self, token: u64, now: Instant) -> TrackerOutput {
        if self.scheduled.as_ref().map(|timer| timer.token) != Some(token) {
            return TrackerOutput::default();
        }
        self.scheduled = None;
        let mut actions = Vec::new();
        if let Some(current) = self.current.as_mut() {
            let was_playing = current.settle(now);
            Self::submit_if_ready(current, &mut actions);
            if was_playing {
                current.playing_since = Some(now);
            }
        }
        self.output(actions, now)
    }

    fn output(&mut self, actions: Vec<ScrobbleAction>, now: Instant) -> TrackerOutput {
        let desired = self.current.as_ref().and_then(|current| {
            if current.submitted || current.playing_since.is_none() {
                return None;
            }
            let remaining = current
                .threshold()
                .saturating_sub(current.listened_at(now))
                .max(Duration::from_millis(1));
            Some((
                (current.occurrence_id, current.play_instance_id),
                now + remaining,
            ))
        });
        let Some((key, deadline)) = desired else {
            self.scheduled = None;
            return TrackerOutput {
                actions,
                timer: None,
            };
        };
        if self
            .scheduled
            .as_ref()
            .is_some_and(|scheduled| scheduled.key == key && scheduled.deadline == deadline)
        {
            return TrackerOutput {
                actions,
                timer: None,
            };
        }

        self.next_timer_token = self.next_timer_token.saturating_add(1);
        let token = self.next_timer_token;
        self.scheduled = Some(ScheduledScrobbleTimer {
            token,
            key,
            deadline,
        });
        TrackerOutput {
            actions,
            timer: Some(ScrobbleTimer {
                token,
                after: deadline.saturating_duration_since(now),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{BufRead as _, BufReader, Write as _};
    use std::net::{Ipv4Addr, TcpListener};
    use std::thread;

    const PROFILE: &str = "0123456789abcdef0123456789abcdef01234567";

    fn song(id: &str, duration_ms: u32) -> Song {
        let id = MediaId::new(crate::auth::ProfileId::new(PROFILE), MediaKind::Song, id);
        Song {
            uri: id.uri(),
            id,
            duration_ms,
            ..Song::default()
        }
    }

    fn observation(
        occurrence_id: u64,
        play_instance_id: u64,
        playback: Playback,
        observed_at: Option<Instant>,
    ) -> Observation {
        Observation {
            occurrence_id,
            play_instance_id,
            song: song("song", 240_000),
            playback,
            observed_at,
        }
    }

    #[tokio::test]
    async fn home_succeeds_when_only_the_album_list_endpoint_exists() {
        let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, 0)) {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("unable to bind loopback test server: {error}"),
        };
        let server_url = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            const BODY: &str = concat!(
                r#"{"subsonic-response":{"status":"ok","version":"1.16.1","#,
                r#""albumList2":{"album":[]}}}"#,
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{BODY}",
                BODY.len()
            );
            let mut requests = Vec::with_capacity(3);
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if matches!(line.as_str(), "\r\n" | "\n" | "") {
                        break;
                    }
                }
                requests.push(first);
                stream.write_all(response.as_bytes()).unwrap();
            }
            requests
        });
        let client = ApiClient::with_default_activity(
            Credentials::new(server_url, "alice", "secret").unwrap(),
        )
        .unwrap();

        let payload = perform_api(
            &client,
            ApiRequest::Home {
                music_folder_id: Some("library".to_owned()),
                album_limit: 7,
                generation: 1,
            },
        )
        .await
        .unwrap();
        let ApiPayload::Home(home) = payload else {
            panic!("home request returned a different payload");
        };
        assert!(home.newest.items.is_empty());
        assert!(home.recent.items.is_empty());
        assert!(home.frequent.items.is_empty());

        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(
            requests
                .iter()
                .all(|request| request.contains("/rest/getAlbumList2.view?"))
        );
        for kind in ["newest", "recent", "frequent"] {
            assert_eq!(
                requests
                    .iter()
                    .filter(|request| request.contains(&format!("type={kind}")))
                    .count(),
                1
            );
        }
        assert!(requests.iter().all(|request| !request.contains("random")));
    }

    #[test]
    fn advancing_a_session_epoch_rejects_old_work() {
        let clock = SessionClock::default();
        let first = clock.advance();
        assert!(clock.accepts(first));
        let second = clock.advance();
        assert_eq!(second, first + 1);
        assert!(!clock.accepts(first));
        assert!(clock.accepts(second));
    }

    #[test]
    fn credential_save_failure_is_fatal_and_redacted() {
        let directory = std::env::temp_dir().join(format!(
            "fastpotify-backend-credentials-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let credentials =
            Credentials::new("https://music.example.test", "alice", "never-print-this").unwrap();
        let error = persist_credentials(&credentials, &directory).unwrap_err();
        assert_eq!(error, BackendError::CredentialStore);
        assert!(!error.to_string().contains("never-print-this"));
        assert!(!error.to_string().contains("music.example"));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn api_errors_never_expose_authenticated_urls() {
        let error = map_api_error(ApiError::Network(
            "https://music.example/rest/getSong?u=alice&t=secret&s=salt".to_owned(),
        ));
        let shown = error.to_string();
        assert_eq!(shown, "unable to reach the Navidrome server");
        assert!(!shown.contains("secret"));
        assert!(!shown.contains("music.example"));
    }

    #[test]
    fn invalid_media_references_have_request_and_response_error_semantics() {
        assert_eq!(
            map_api_error(ApiError::InvalidMediaReference),
            BackendError::InvalidReference
        );
        assert_eq!(
            map_login_error(ApiError::InvalidMediaReference),
            BackendError::InvalidResponse
        );
    }

    #[test]
    fn clearing_history_atomically_persists_an_empty_list() {
        let directory = std::env::temp_dir().join(format!(
            "fastpotify-backend-history-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let path = directory.join("history.json");
        let mut history = History::default();
        history.record(song("played", 240_000), jiff::Timestamp::now(), None);
        persist_cleared_history(&mut history, &path).unwrap();
        assert!(history.is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), b"[]");
        assert!(
            !std::fs::read_dir(&directory)
                .unwrap()
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_history_clear_keeps_the_authoritative_in_memory_list() {
        let directory = std::env::temp_dir().join(format!(
            "fastpotify-backend-history-failure-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let mut history = History::default();
        history.record(song("still-played", 240_000), jiff::Timestamp::now(), None);
        assert_eq!(
            persist_cleared_history(&mut history, &directory),
            Err(BackendError::HistoryStore)
        );
        assert_eq!(history.plays().len(), 1);
        assert_eq!(history.plays()[0].track.id.id, "still-played");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn loading_or_a_failed_stream_does_not_announce_a_play() {
        let mut tracker = ScrobbleTracker::default();
        let now = Instant::now();
        let loading = tracker.observe_at(Some(observation(7, 10, Playback::Loading, None)), now);
        assert!(loading.actions.is_empty());
        assert!(loading.timer.is_none());
        let failed = tracker.observe_at(
            Some(observation(7, 10, Playback::Paused, None)),
            now + Duration::from_secs(2),
        );
        assert!(failed.actions.is_empty());
        assert!(failed.timer.is_none());
    }

    #[test]
    fn first_playing_snapshot_announces_exactly_once() {
        let mut tracker = ScrobbleTracker::default();
        let now = Instant::now();
        assert!(
            tracker
                .observe_at(Some(observation(7, 10, Playback::Loading, None)), now,)
                .actions
                .is_empty()
        );
        let playing =
            tracker.observe_at(Some(observation(7, 10, Playback::Playing, Some(now))), now);
        assert_eq!(playing.actions.len(), 1);
        assert!(!playing.actions[0].submission);
        let duplicate = tracker.observe_at(
            Some(observation(7, 10, Playback::Playing, Some(now))),
            now + Duration::from_secs(1),
        );
        assert!(duplicate.actions.is_empty());
    }

    #[test]
    fn frequent_playing_snapshots_keep_one_scrobble_timer() {
        let mut tracker = ScrobbleTracker::default();
        let now = Instant::now();
        let first = tracker.observe_at(Some(observation(7, 10, Playback::Playing, Some(now))), now);
        let token = first.timer.unwrap().token;
        for step in 1_u64..100 {
            let update = tracker.observe_at(
                Some(observation(7, 10, Playback::Playing, Some(now))),
                now + Duration::from_nanos(step * 16_666_667),
            );
            assert!(update.actions.is_empty());
            assert!(update.timer.is_none(), "step {step} spawned another timer");
            assert_eq!(tracker.scheduled.as_ref().unwrap().token, token);
        }
    }

    #[test]
    fn seeking_to_the_end_does_not_count_reported_position_as_listening() {
        let mut tracker = ScrobbleTracker::default();
        let now = Instant::now();
        tracker.observe_at(Some(observation(7, 10, Playback::Playing, Some(now))), now);
        let seek = tracker.observe_at(
            Some(observation(7, 10, Playback::Loading, None)),
            now + Duration::from_secs(1),
        );
        assert!(seek.actions.is_empty());
        let resumed = tracker.observe_at(
            Some(observation(
                7,
                10,
                Playback::Playing,
                Some(now + Duration::from_secs(2)),
            )),
            now + Duration::from_secs(2),
        );
        assert!(resumed.actions.is_empty());
        assert!(resumed.timer.unwrap().after >= Duration::from_secs(28));
    }

    #[test]
    fn thirty_one_seconds_then_seek_submits_only_once() {
        let mut tracker = ScrobbleTracker::default();
        let now = Instant::now();
        let started =
            tracker.observe_at(Some(observation(7, 10, Playback::Playing, Some(now))), now);
        let timer = started.timer.unwrap();
        let counted = tracker.tick_at(timer.token, now + Duration::from_secs(31));
        assert_eq!(counted.actions.len(), 1);
        assert!(counted.actions[0].submission);
        assert!(counted.timer.is_none());

        let seek = tracker.observe_at(
            Some(observation(7, 10, Playback::Loading, None)),
            now + Duration::from_secs(32),
        );
        assert!(seek.actions.is_empty());
        let resumed = tracker.observe_at(
            Some(observation(
                7,
                10,
                Playback::Playing,
                Some(now + Duration::from_secs(33)),
            )),
            now + Duration::from_secs(33),
        );
        assert!(resumed.actions.is_empty());
        assert!(resumed.timer.is_none());
    }

    #[test]
    fn repeat_after_seek_starts_a_new_now_playing() {
        let mut tracker = ScrobbleTracker::default();
        let now = Instant::now();
        tracker.observe_at(Some(observation(7, 10, Playback::Playing, Some(now))), now);
        tracker.observe_at(
            Some(observation(7, 10, Playback::Loading, None)),
            now + Duration::from_secs(1),
        );
        tracker.observe_at(
            Some(observation(
                7,
                10,
                Playback::Playing,
                Some(now + Duration::from_secs(2)),
            )),
            now + Duration::from_secs(2),
        );
        let repeat_loading = tracker.observe_at(
            Some(observation(7, 11, Playback::Loading, None)),
            now + Duration::from_secs(3),
        );
        assert!(repeat_loading.actions.is_empty());
        let repeat_playing = tracker.observe_at(
            Some(observation(
                7,
                11,
                Playback::Playing,
                Some(now + Duration::from_secs(4)),
            )),
            now + Duration::from_secs(4),
        );
        assert_eq!(repeat_playing.actions.len(), 1);
        assert!(!repeat_playing.actions[0].submission);
    }

    #[test]
    fn stopping_preserves_the_same_instance_tombstone() {
        let mut tracker = ScrobbleTracker::default();
        let now = Instant::now();
        let started =
            tracker.observe_at(Some(observation(7, 10, Playback::Playing, Some(now))), now);
        assert!(started.timer.is_some());

        let stopped = tracker.observe_at(
            Some(observation(7, 10, Playback::Stopped, None)),
            now + Duration::from_secs(10),
        );
        assert!(stopped.actions.is_empty());
        assert!(stopped.timer.is_none());

        let restarted = tracker.observe_at(
            Some(observation(
                7,
                10,
                Playback::Playing,
                Some(now + Duration::from_secs(11)),
            )),
            now + Duration::from_secs(11),
        );
        assert!(restarted.actions.is_empty());
        let remaining = restarted.timer.unwrap().after;
        assert!(remaining >= Duration::from_secs(19));
        assert!(remaining <= Duration::from_secs(20));
    }

    #[test]
    fn playing_after_stop_with_a_new_instance_starts_a_new_scrobble() {
        let mut tracker = ScrobbleTracker::default();
        let now = Instant::now();
        tracker.observe_at(Some(observation(7, 10, Playback::Playing, Some(now))), now);
        tracker.observe_at(
            Some(observation(7, 10, Playback::Stopped, None)),
            now + Duration::from_secs(10),
        );
        let loading = tracker.observe_at(
            Some(observation(7, 11, Playback::Loading, None)),
            now + Duration::from_secs(11),
        );
        assert!(loading.actions.is_empty());
        let playing = tracker.observe_at(
            Some(observation(
                7,
                11,
                Playback::Playing,
                Some(now + Duration::from_secs(12)),
            )),
            now + Duration::from_secs(12),
        );
        assert_eq!(playing.actions.len(), 1);
        assert!(!playing.actions[0].submission);
        assert!(playing.timer.unwrap().after >= Duration::from_secs(29));
    }

    #[test]
    fn stop_and_replay_cannot_submit_the_same_instance_twice() {
        let mut tracker = ScrobbleTracker::default();
        let now = Instant::now();
        let started =
            tracker.observe_at(Some(observation(7, 10, Playback::Playing, Some(now))), now);
        let counted = tracker.tick_at(started.timer.unwrap().token, now + Duration::from_secs(31));
        assert_eq!(
            counted
                .actions
                .iter()
                .filter(|action| action.submission)
                .count(),
            1
        );
        assert!(
            tracker
                .observe_at(
                    Some(observation(7, 10, Playback::Stopped, None)),
                    now + Duration::from_secs(32),
                )
                .actions
                .is_empty()
        );
        tracker.observe_at(
            Some(observation(7, 10, Playback::Loading, None)),
            now + Duration::from_secs(33),
        );
        let replayed = tracker.observe_at(
            Some(observation(
                7,
                10,
                Playback::Playing,
                Some(now + Duration::from_secs(34)),
            )),
            now + Duration::from_secs(34),
        );
        assert!(replayed.actions.is_empty());
        assert!(replayed.timer.is_none());
    }

    #[test]
    fn typed_scrobbles_cannot_cross_profiles() {
        let credentials =
            Credentials::new("https://music.example.test", "alice", "secret").unwrap();
        let client = ApiClient::with_default_activity(credentials).unwrap();
        let foreign = MediaId::new(
            crate::auth::ProfileId::new("fedcba9876543210fedcba9876543210fedcba98"),
            MediaKind::Song,
            "song",
        );
        assert_eq!(
            checked_media(&client, &foreign, MediaKind::Song),
            Err(BackendError::InvalidReference)
        );
    }

    #[test]
    fn playlist_reorder_validation_preserves_order_and_duplicates() {
        let credentials =
            Credentials::new("https://music.example.test", "alice", "secret").unwrap();
        let client = ApiClient::with_default_activity(credentials).unwrap();
        let first = MediaId::new(client.profile_id().clone(), MediaKind::Song, "first");
        let second = MediaId::new(client.profile_id().clone(), MediaKind::Song, "second");
        let requested = vec![first.clone(), second, first];
        assert_eq!(checked_songs(&client, &requested).unwrap(), requested);
    }
}
