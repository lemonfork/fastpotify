//! Local OpenSubsonic playback and its authoritative queue.
//!
//! The reducer in this module is the only owner of playback order. Network
//! downloads and Symphonia decoders are disposable workers: every result is
//! tagged with a monotonically increasing decode generation and is ignored as
//! soon as a newer command wins. Audio is streamed through bounded channels;
//! no track or authenticated stream URL is retained in memory as a whole.

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use opus_pure::Error as OpusError;
use serde::{Deserialize, Serialize};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_OPUS, CodecParameters, Decoder, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, Packet};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::api::{ApiError, OpenSubsonicClient, Song};
use crate::opus::OggOpusDecoder;
use crate::resample::Resampler;
use crate::sink::{OutputError, OutputTransition, PCM_CHANNELS, PCM_SAMPLE_RATE, RodioSink};
use crate::vis::{AudioTap, PcmProcessor};

const STREAM_CHUNKS: usize = 8;
const PCM_MESSAGES: usize = 8;
const PCM_BLOCK_FRAMES: usize = 2_048;
const CANCEL_POLL: Duration = Duration::from_millis(5);
const SOURCE_POLL: Duration = Duration::from_millis(20);
const PREVIOUS_RESTART_MS: u32 = 3_000;

#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// Requested OpenSubsonic transcode ceiling. `Some(320)` is the default.
    pub max_bitrate_kbps: Option<u32>,
    pub audio_device: Option<String>,
    pub initial_volume: u16,
    pub buffer_ms: u32,
    pub tap: Arc<AudioTap>,
    pub eq: crate::eq::SharedEq,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Playback {
    #[default]
    Stopped,
    Loading,
    Playing,
    Paused,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepeatMode {
    #[default]
    Off,
    Context,
    Track,
}

impl RepeatMode {
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Context,
            Self::Context => Self::Track,
            Self::Track => Self::Off,
        }
    }

    pub fn api_name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Context => "context",
            Self::Track => "track",
        }
    }

    pub fn from_api(name: &str) -> Self {
        match name {
            "context" => Self::Context,
            "track" => Self::Track,
            _ => Self::Off,
        }
    }
}

/// Compatibility name for callers that display the currently playing song.
/// The player retains only provider-neutral, secret-free [`Song`] values.
pub type LocalTrack = Song;

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord,
)]
pub struct OccurrenceId(u64);

impl OccurrenceId {
    pub fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for OccurrenceId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueueSection {
    Manual,
    Context,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueEntry {
    pub occurrence_id: OccurrenceId,
    pub section: QueueSection,
    pub song: Song,
    /// Original position in the loaded context, used to undo shuffle.
    context_index: Option<usize>,
}

impl QueueEntry {
    pub fn id(&self) -> OccurrenceId {
        self.occurrence_id
    }

    pub fn song(&self) -> &Song {
        &self.song
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackQueue {
    pub manual: Vec<QueueEntry>,
    pub context: Vec<QueueEntry>,
}

impl PlaybackQueue {
    pub fn len(&self) -> usize {
        self.manual.len() + self.context.len()
    }

    pub fn is_empty(&self) -> bool {
        self.manual.is_empty() && self.context.is_empty()
    }

    /// Exact play order: manually queued occurrences, then context.
    pub fn entries(&self) -> impl Iterator<Item = &QueueEntry> {
        self.manual.iter().chain(&self.context)
    }

    pub fn get(&self, id: OccurrenceId) -> Option<&QueueEntry> {
        self.entries().find(|entry| entry.occurrence_id == id)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlaybackPosition {
    pub playback: Playback,
    pub elapsed_ms: u32,
    /// The clock behind interpolation; absent while not advancing.
    pub observed_at: Option<Instant>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlaybackSnapshot {
    pub revision: u64,
    pub play_instance_id: u64,
    pub current: Option<QueueEntry>,
    pub queue: PlaybackQueue,
    pub position: PlaybackPosition,
    pub volume: u16,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub error: Option<String>,
}

impl Default for PlaybackSnapshot {
    fn default() -> Self {
        Self {
            revision: 0,
            play_instance_id: 0,
            current: None,
            queue: PlaybackQueue::default(),
            position: PlaybackPosition::default(),
            volume: u16::MAX,
            shuffle: false,
            repeat: RepeatMode::Off,
            error: None,
        }
    }
}

impl PlaybackSnapshot {
    pub fn playback(&self) -> Playback {
        self.position.playback
    }

    pub fn is_paused(&self) -> bool {
        self.position.playback == Playback::Paused
    }

    pub fn current_song(&self) -> Option<&Song> {
        self.current.as_ref().map(|entry| &entry.song)
    }

    pub fn current_track(&self) -> Option<&LocalTrack> {
        self.current_song()
    }

    pub fn current_occurrence(&self) -> Option<OccurrenceId> {
        self.current.as_ref().map(|entry| entry.occurrence_id)
    }

    pub fn duration_ms(&self) -> u32 {
        self.current_song().map_or(0, |song| song.duration_ms)
    }

    /// The position now, interpolated from the most recent playing snapshot.
    pub fn position_now(&self) -> u32 {
        let base = self.position.elapsed_ms;
        let now = match (self.position.playback, self.position.observed_at) {
            (Playback::Playing, Some(at)) => {
                base.saturating_add(at.elapsed().as_millis().min(u128::from(u32::MAX)) as u32)
            }
            _ => base,
        };
        match self.duration_ms() {
            0 => now,
            duration => now.min(duration.max(base)),
        }
    }

    pub fn is_active(&self) -> bool {
        self.current.is_some() && self.position.playback != Playback::Stopped
    }
}

#[cfg(any(test, feature = "demo"))]
pub(crate) struct DemoPlayback {
    pub current: Option<Song>,
    pub manual: Vec<Song>,
    pub context: Vec<Song>,
    pub position_ms: u32,
    pub playback: Playback,
    pub volume: u16,
    pub shuffle: bool,
    pub repeat: RepeatMode,
}

#[cfg(any(test, feature = "demo"))]
pub(crate) fn demo_snapshot(state: DemoPlayback) -> PlaybackSnapshot {
    let mut next_occurrence = 1_u64;
    let mut entry = |song: Song, section: QueueSection, context_index: Option<usize>| {
        let current = QueueEntry {
            occurrence_id: OccurrenceId(next_occurrence),
            section,
            song,
            context_index,
        };
        next_occurrence = next_occurrence.saturating_add(1);
        current
    };
    let current_duration = state.current.as_ref().map_or(0, |song| song.duration_ms);
    let current = state
        .current
        .map(|song| entry(song, QueueSection::Context, Some(0)));
    let manual = state
        .manual
        .into_iter()
        .map(|song| entry(song, QueueSection::Manual, None))
        .collect();
    let start_index = usize::from(current.is_some());
    let context = state
        .context
        .into_iter()
        .enumerate()
        .map(|(index, song)| entry(song, QueueSection::Context, Some(start_index + index)))
        .collect();
    PlaybackSnapshot {
        revision: 1,
        play_instance_id: 1,
        current,
        queue: PlaybackQueue { manual, context },
        position: PlaybackPosition {
            playback: state.playback,
            elapsed_ms: clamp_position(state.position_ms, current_duration),
            observed_at: (state.playback == Playback::Playing).then(Instant::now),
        },
        volume: state.volume,
        shuffle: state.shuffle,
        repeat: state.repeat,
        error: None,
    }
}

/// Compatibility name while the UI migrates to the explicit snapshot type.
pub type LocalState = PlaybackSnapshot;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoadContext {
    pub songs: Vec<Song>,
    pub start_index: usize,
    pub position_ms: u32,
    pub play: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlayerCommand {
    Toggle,
    Play,
    Pause,
    Stop,
    LoadContext(LoadContext),
    /// Adds songs in the supplied order after the context without changing playback.
    AppendContext(Vec<Song>),
    AddManual(Box<Song>),
    ClearManual,
    Next,
    Previous,
    SkipTo(OccurrenceId),
    Seek(u32),
    Volume(u16),
    /// Same local behavior as Volume; kept so slider previews stay explicit.
    VolumePreview(u16),
    Shuffle(bool),
    Repeat(RepeatMode),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandReceipt {
    pub revision: u64,
    pub decode_generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DecodeToken {
    revision: u64,
    generation: u64,
    play_instance_id: u64,
    occurrence_id: OccurrenceId,
}

#[derive(Clone)]
struct DecodeRequest {
    token: DecodeToken,
    song: Song,
    position_ms: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputAction {
    None,
    Clear,
    /// An explicit change of song, including another skip while still loading.
    Replace,
}

struct Transition {
    receipt: CommandReceipt,
    snapshot: PlaybackSnapshot,
    decode: Option<DecodeRequest>,
    output: OutputAction,
}

/// Deterministic reducer for queue and playback state. It performs no I/O.
pub struct PlaybackReducer {
    snapshot: PlaybackSnapshot,
    history: Vec<QueueEntry>,
    context_source: Vec<Song>,
    next_occurrence_id: u64,
    next_play_instance_id: u64,
    decode_generation: u64,
}

impl PlaybackReducer {
    pub fn new(initial_volume: u16) -> Self {
        Self {
            snapshot: PlaybackSnapshot {
                volume: initial_volume,
                ..PlaybackSnapshot::default()
            },
            history: Vec::new(),
            context_source: Vec::new(),
            next_occurrence_id: 1,
            next_play_instance_id: 1,
            decode_generation: 0,
        }
    }

    pub fn snapshot(&self) -> &PlaybackSnapshot {
        &self.snapshot
    }

    pub fn decode_generation(&self) -> u64 {
        self.decode_generation
    }

    fn occurrence(
        &mut self,
        song: Song,
        section: QueueSection,
        index: Option<usize>,
    ) -> QueueEntry {
        let id = OccurrenceId(self.next_occurrence_id);
        self.next_occurrence_id = self.next_occurrence_id.saturating_add(1);
        QueueEntry {
            occurrence_id: id,
            section,
            song,
            context_index: index,
        }
    }

    fn touch(&mut self) {
        self.snapshot.revision = self.snapshot.revision.saturating_add(1);
    }

    fn cancel_decode(&mut self) {
        self.decode_generation = self.decode_generation.saturating_add(1);
    }

    fn set_position(&mut self, playback: Playback, elapsed_ms: u32) {
        self.snapshot.position = PlaybackPosition {
            playback,
            elapsed_ms,
            observed_at: (playback == Playback::Playing).then(Instant::now),
        };
    }

    fn freeze_position(&mut self, playback: Playback) {
        let elapsed = self.snapshot.position_now();
        self.set_position(playback, elapsed);
    }

    fn new_play_instance(&mut self) {
        self.snapshot.play_instance_id = self.next_play_instance_id;
        self.next_play_instance_id = self.next_play_instance_id.saturating_add(1);
    }

    fn request_current(&mut self, position_ms: u32) -> Option<DecodeRequest> {
        let current = self.snapshot.current.as_ref()?.clone();
        let position_ms = clamp_position(position_ms, current.song.duration_ms);
        self.cancel_decode();
        self.set_position(Playback::Loading, position_ms);
        self.snapshot.error = None;
        Some(DecodeRequest {
            token: DecodeToken {
                revision: self.snapshot.revision,
                generation: self.decode_generation,
                play_instance_id: self.snapshot.play_instance_id,
                occurrence_id: current.occurrence_id,
            },
            song: current.song,
            position_ms,
        })
    }

    fn take_next(&mut self) -> Option<QueueEntry> {
        if !self.snapshot.queue.manual.is_empty() {
            Some(self.snapshot.queue.manual.remove(0))
        } else if !self.snapshot.queue.context.is_empty() {
            Some(self.snapshot.queue.context.remove(0))
        } else {
            None
        }
    }

    fn put_back_first(&mut self, entry: QueueEntry) {
        match entry.section {
            QueueSection::Manual => self.snapshot.queue.manual.insert(0, entry),
            QueueSection::Context => self.snapshot.queue.context.insert(0, entry),
        }
    }

    fn select(&mut self, entry: QueueEntry, play: bool, position_ms: u32) -> Option<DecodeRequest> {
        self.snapshot.current = Some(entry);
        self.new_play_instance();
        if play {
            self.request_current(position_ms)
        } else {
            self.cancel_decode();
            self.set_position(
                Playback::Paused,
                clamp_position(position_ms, self.snapshot.duration_ms()),
            );
            None
        }
    }

    fn advance(&mut self, automatic: bool) -> Option<DecodeRequest> {
        if automatic && self.snapshot.repeat == RepeatMode::Track && self.snapshot.current.is_some()
        {
            // The same occurrence remains current and no queue row is consumed.
            self.new_play_instance();
            return self.request_current(0);
        }
        if let Some(current) = self.snapshot.current.take() {
            self.history.push(current);
        }
        let mut next = self.take_next();
        if next.is_none()
            && automatic
            && self.snapshot.repeat == RepeatMode::Context
            && !self.context_source.is_empty()
        {
            let source = self.context_source.clone();
            self.snapshot.queue.context = source
                .into_iter()
                .enumerate()
                .map(|(index, song)| self.occurrence(song, QueueSection::Context, Some(index)))
                .collect();
            if self.snapshot.shuffle {
                shuffle_entries(
                    &mut self.snapshot.queue.context,
                    self.snapshot.revision ^ self.next_occurrence_id,
                );
            }
            next = self.take_next();
        }
        match next {
            Some(entry) => self.select(entry, true, 0),
            None => {
                self.cancel_decode();
                self.snapshot.current = None;
                self.set_position(Playback::Stopped, 0);
                None
            }
        }
    }

    fn finish(
        self_transition: &mut Self,
        decode: Option<DecodeRequest>,
        output: OutputAction,
    ) -> Transition {
        Transition {
            receipt: CommandReceipt {
                revision: self_transition.snapshot.revision,
                decode_generation: self_transition.decode_generation,
            },
            snapshot: self_transition.snapshot.clone(),
            decode,
            output,
        }
    }

    fn apply(&mut self, command: PlayerCommand) -> Transition {
        self.touch();
        let mut output = OutputAction::None;
        let decode = match command {
            PlayerCommand::LoadContext(load) => {
                self.context_source = load.songs.clone();
                self.history.clear();
                let mut entries: Vec<QueueEntry> = load
                    .songs
                    .into_iter()
                    .enumerate()
                    .map(|(index, song)| self.occurrence(song, QueueSection::Context, Some(index)))
                    .collect();
                let start = load.start_index.min(entries.len().saturating_sub(1));
                let selected = (!entries.is_empty()).then(|| entries.remove(start));
                if self.snapshot.shuffle {
                    shuffle_entries(
                        &mut entries,
                        self.snapshot.revision ^ self.next_occurrence_id,
                    );
                } else {
                    // Starting in the middle means only following rows are next;
                    // earlier rows remain reachable through Previous.
                    let earlier = entries
                        .drain(..start.min(entries.len()))
                        .collect::<Vec<_>>();
                    self.history.extend(earlier);
                }
                self.snapshot.queue.context = entries;
                output = if load.play {
                    OutputAction::Replace
                } else {
                    OutputAction::Clear
                };
                match selected {
                    Some(entry) => self.select(entry, load.play, load.position_ms),
                    None => {
                        self.cancel_decode();
                        self.snapshot.current = None;
                        self.set_position(Playback::Stopped, 0);
                        None
                    }
                }
            }
            PlayerCommand::AppendContext(songs) => {
                for song in songs {
                    let index = self.context_source.len();
                    self.context_source.push(song.clone());
                    let entry = self.occurrence(song, QueueSection::Context, Some(index));
                    self.snapshot.queue.context.push(entry);
                }
                None
            }
            PlayerCommand::AddManual(song) => {
                let entry = self.occurrence(*song, QueueSection::Manual, None);
                self.snapshot.queue.manual.push(entry);
                None
            }
            PlayerCommand::ClearManual => {
                self.snapshot.queue.manual.clear();
                None
            }
            PlayerCommand::Next => {
                output = OutputAction::Replace;
                self.advance(false)
            }
            PlayerCommand::Previous => {
                output = OutputAction::Replace;
                if self.snapshot.position_now() > PREVIOUS_RESTART_MS || self.history.is_empty() {
                    self.request_current(0)
                } else {
                    let previous = self.history.pop();
                    if let Some(current) = self.snapshot.current.take() {
                        self.put_back_first(current);
                    }
                    previous.and_then(|entry| self.select(entry, true, 0))
                }
            }
            PlayerCommand::SkipTo(id) => {
                let mut selected = None;
                if let Some(index) = self
                    .snapshot
                    .queue
                    .manual
                    .iter()
                    .position(|entry| entry.occurrence_id == id)
                {
                    if let Some(current) = self.snapshot.current.take() {
                        self.history.push(current);
                    }
                    self.history
                        .extend(self.snapshot.queue.manual.drain(..index));
                    selected = Some(self.snapshot.queue.manual.remove(0));
                } else if let Some(index) = self
                    .snapshot
                    .queue
                    .context
                    .iter()
                    .position(|entry| entry.occurrence_id == id)
                {
                    if let Some(current) = self.snapshot.current.take() {
                        self.history.push(current);
                    }
                    self.history.append(&mut self.snapshot.queue.manual);
                    self.history
                        .extend(self.snapshot.queue.context.drain(..index));
                    selected = Some(self.snapshot.queue.context.remove(0));
                }
                if let Some(entry) = selected {
                    output = OutputAction::Replace;
                    self.select(entry, true, 0)
                } else {
                    None
                }
            }
            PlayerCommand::Toggle => match self.snapshot.position.playback {
                Playback::Playing | Playback::Loading => {
                    output = OutputAction::Clear;
                    self.freeze_position(Playback::Paused);
                    self.cancel_decode();
                    None
                }
                Playback::Paused => {
                    if self.snapshot.current.is_none()
                        && let Some(next) = self.take_next()
                    {
                        self.snapshot.current = Some(next);
                        self.new_play_instance();
                    }
                    output = OutputAction::Clear;
                    self.request_current(self.snapshot.position.elapsed_ms)
                }
                Playback::Stopped => {
                    if self.snapshot.current.is_none()
                        && let Some(next) = self.take_next()
                    {
                        self.snapshot.current = Some(next);
                    }
                    if self.snapshot.current.is_some() {
                        self.new_play_instance();
                    }
                    output = OutputAction::Clear;
                    self.request_current(0)
                }
            },
            PlayerCommand::Play => match self.snapshot.position.playback {
                Playback::Playing | Playback::Loading => None,
                Playback::Paused => {
                    if self.snapshot.current.is_none()
                        && let Some(next) = self.take_next()
                    {
                        self.snapshot.current = Some(next);
                        self.new_play_instance();
                    }
                    output = OutputAction::Clear;
                    self.request_current(self.snapshot.position.elapsed_ms)
                }
                Playback::Stopped => {
                    if self.snapshot.current.is_none()
                        && let Some(next) = self.take_next()
                    {
                        self.snapshot.current = Some(next);
                    }
                    if self.snapshot.current.is_some() {
                        self.new_play_instance();
                    }
                    output = OutputAction::Clear;
                    self.request_current(0)
                }
            },
            PlayerCommand::Pause => {
                if matches!(
                    self.snapshot.position.playback,
                    Playback::Playing | Playback::Loading
                ) {
                    output = OutputAction::Clear;
                    self.freeze_position(Playback::Paused);
                    self.cancel_decode();
                }
                None
            }
            PlayerCommand::Stop => {
                output = OutputAction::Clear;
                self.cancel_decode();
                self.set_position(Playback::Stopped, 0);
                None
            }
            PlayerCommand::Seek(position_ms) => {
                let position_ms = clamp_position(position_ms, self.snapshot.duration_ms());
                output = OutputAction::Clear;
                if matches!(
                    self.snapshot.position.playback,
                    Playback::Playing | Playback::Loading
                ) {
                    self.request_current(position_ms)
                } else {
                    self.cancel_decode();
                    self.set_position(Playback::Paused, position_ms);
                    None
                }
            }
            PlayerCommand::Volume(volume) | PlayerCommand::VolumePreview(volume) => {
                self.snapshot.volume = volume;
                None
            }
            PlayerCommand::Shuffle(enabled) => {
                self.snapshot.shuffle = enabled;
                if enabled {
                    shuffle_entries(
                        &mut self.snapshot.queue.context,
                        self.snapshot.revision ^ self.next_occurrence_id,
                    );
                } else {
                    self.snapshot
                        .queue
                        .context
                        .sort_by_key(|entry| entry.context_index.unwrap_or(usize::MAX));
                }
                None
            }
            PlayerCommand::Repeat(mode) => {
                self.snapshot.repeat = mode;
                None
            }
        };
        Self::finish(self, decode, output)
    }

    #[cfg(test)]
    pub(crate) fn apply_for_test(
        &mut self,
        command: PlayerCommand,
    ) -> (CommandReceipt, PlaybackSnapshot) {
        let transition = self.apply(command);
        (transition.receipt, transition.snapshot)
    }

    fn decoder_started(&mut self, token: DecodeToken) -> Option<Transition> {
        if !self.token_is_current(token) {
            return None;
        }
        self.touch();
        let elapsed = self.snapshot.position.elapsed_ms;
        self.set_position(Playback::Playing, elapsed);
        Some(Self::finish(self, None, OutputAction::None))
    }

    fn decoder_eof(&mut self, token: DecodeToken) -> Option<Transition> {
        if !self.token_is_current(token) {
            return None;
        }
        self.touch();
        let decode = self.advance(true);
        Some(Self::finish(self, decode, OutputAction::Clear))
    }

    fn decoder_error(&mut self, token: DecodeToken, message: String) -> Option<Transition> {
        if !self.token_is_current(token) {
            return None;
        }
        self.touch();
        self.cancel_decode();
        self.freeze_position(Playback::Paused);
        self.snapshot.error = Some(message);
        Some(Self::finish(self, None, OutputAction::Clear))
    }

    fn token_is_current(&self, token: DecodeToken) -> bool {
        token.revision <= self.snapshot.revision
            && token.generation == self.decode_generation
            && token.play_instance_id == self.snapshot.play_instance_id
            && self.snapshot.current_occurrence() == Some(token.occurrence_id)
    }
}

fn shuffle_entries(entries: &mut [QueueEntry], mut state: u64) {
    // Fixed xorshift makes reducer results reproducible from their revision.
    state = state.max(1);
    for index in (1..entries.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        entries.swap(index, state as usize % (index + 1));
    }
}

fn clamp_position(position_ms: u32, duration_ms: u32) -> u32 {
    if duration_ms == 0 {
        position_ms
    } else {
        position_ms.min(duration_ms)
    }
}

#[allow(clippy::large_enum_variant)]
pub enum EngineEvent {
    Snapshot(PlaybackSnapshot),
}

/// Called synchronously in revision order. Implementations must enqueue and
/// return; they must not call back into the same [`Engine`].
pub type Notify = Arc<dyn Fn(EngineEvent) + Send + Sync>;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("the playback engine has shut down")]
    Shutdown,
    #[error("the audio actor is unavailable")]
    ActorUnavailable,
}

pub struct Engine {
    shared: Arc<EngineShared>,
    actor: Mutex<Option<thread::JoinHandle<()>>>,
}

struct EngineShared {
    /// Orders reducer mutation and every resulting externally visible side
    /// effect as one transaction. The callback must stay non-blocking.
    transitions: TransitionSerial,
    reducer: Mutex<PlaybackReducer>,
    client: Arc<OpenSubsonicClient>,
    runtime: tokio::runtime::Handle,
    notify: Notify,
    control: mpsc::Sender<AudioControl>,
    audio: SyncSender<AudioMessage>,
    generation: Arc<AtomicU64>,
    generation_signal: tokio::sync::watch::Sender<u64>,
    published_revision: AtomicU64,
    volume: AtomicU16,
    shutting_down: AtomicBool,
    config: EngineConfig,
}

#[derive(Default)]
struct TransitionSerial(Mutex<()>);

impl TransitionSerial {
    fn run<T>(&self, action: impl FnOnce() -> T) -> T {
        let _guard = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        action()
    }
}

enum AudioControl {
    Begin(DecodeToken, OutputTransition),
    Clear(u64, OutputTransition),
    Shutdown,
}

struct AudioMessage {
    token: DecodeToken,
    payload: AudioPayload,
}

enum AudioPayload {
    Pcm(Vec<f64>),
    Eof,
    Error(String),
}

impl Engine {
    pub fn new(
        config: EngineConfig,
        client: Arc<OpenSubsonicClient>,
        runtime: tokio::runtime::Handle,
        notify: Notify,
    ) -> Result<Self> {
        let (control_tx, control_rx) = mpsc::channel();
        let (audio_tx, audio_rx) = mpsc::sync_channel(PCM_MESSAGES);
        let generation = Arc::new(AtomicU64::new(0));
        let (generation_signal, _) = tokio::sync::watch::channel(0);
        let initial_volume = config.initial_volume;
        let shared = Arc::new(EngineShared {
            transitions: TransitionSerial::default(),
            reducer: Mutex::new(PlaybackReducer::new(initial_volume)),
            client,
            runtime,
            notify,
            control: control_tx,
            audio: audio_tx,
            generation,
            generation_signal,
            published_revision: AtomicU64::new(0),
            volume: AtomicU16::new(initial_volume),
            shutting_down: AtomicBool::new(false),
            config,
        });
        let actor_shared = Arc::clone(&shared);
        let actor = thread::Builder::new()
            .name("audio-output".into())
            .spawn(move || run_audio_actor(actor_shared, control_rx, audio_rx))
            .context("unable to start the audio output thread")?;
        let snapshot = shared
            .reducer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot()
            .clone();
        (shared.notify)(EngineEvent::Snapshot(snapshot));
        Ok(Self {
            shared,
            actor: Mutex::new(Some(actor)),
        })
    }

    /// Applies a command synchronously to the authoritative reducer. The
    /// snapshot is emitted before any network or device work begins.
    pub fn command(&self, command: PlayerCommand) -> Result<CommandReceipt, EngineError> {
        self.shared.transitions.run(|| {
            if self.shared.shutting_down.load(Ordering::Acquire) {
                return Err(EngineError::Shutdown);
            }
            let transition = self
                .shared
                .reducer
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .apply(command);
            let receipt = transition.receipt;
            publish_transition(&self.shared, transition)?;
            Ok(receipt)
        })
    }

    pub fn snapshot(&self) -> PlaybackSnapshot {
        self.shared
            .reducer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .snapshot()
            .clone()
    }

    pub fn shutdown(&self) {
        let first = self.shared.transitions.run(|| {
            if self.shared.shutting_down.swap(true, Ordering::AcqRel) {
                return false;
            }
            let generation = self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
            self.shared.generation_signal.send_replace(generation);
            let _ = self.shared.control.send(AudioControl::Shutdown);
            true
        });
        if !first {
            return;
        }
        if let Some(actor) = self
            .actor
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = actor.join();
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn publish_transition(
    shared: &Arc<EngineShared>,
    transition: Transition,
) -> Result<(), EngineError> {
    let previous_generation = shared
        .generation
        .fetch_max(transition.receipt.decode_generation, Ordering::AcqRel);
    if transition.receipt.decode_generation > previous_generation {
        shared
            .generation_signal
            .send_replace(transition.receipt.decode_generation);
    }
    let active_generation = previous_generation.max(transition.receipt.decode_generation);
    let previous_revision = shared
        .published_revision
        .fetch_max(transition.receipt.revision, Ordering::AcqRel);
    if transition.receipt.revision > previous_revision {
        shared
            .volume
            .store(transition.snapshot.volume, Ordering::Release);
        (shared.notify)(EngineEvent::Snapshot(transition.snapshot));
    }

    // A newer command can publish between reducer mutation and this call.
    // Its generation owns the sink; old device/network effects must not run.
    if transition.receipt.decode_generation < active_generation {
        return Ok(());
    }

    // Carry intent to the output actor, not a guess based on Playing: a rapid
    // second skip already sees Loading while the old song can still be audible.
    let output_transition = match transition.output {
        OutputAction::Replace => OutputTransition::Smooth,
        OutputAction::None | OutputAction::Clear => OutputTransition::Immediate,
    };
    match (transition.output, &transition.decode) {
        (_, Some(request)) => shared
            .control
            .send(AudioControl::Begin(request.token, output_transition))
            .map_err(|_| EngineError::ActorUnavailable)?,
        (OutputAction::Clear | OutputAction::Replace, None) => shared
            .control
            .send(AudioControl::Clear(
                transition.receipt.decode_generation,
                output_transition,
            ))
            .map_err(|_| EngineError::ActorUnavailable)?,
        (OutputAction::None, None) => {}
    }
    if let Some(request) = transition.decode {
        spawn_stream(Arc::clone(shared), request);
    }
    Ok(())
}

fn update_reducer(
    shared: &Arc<EngineShared>,
    update: impl FnOnce(&mut PlaybackReducer) -> Option<Transition>,
) {
    shared.transitions.run(|| {
        if shared.shutting_down.load(Ordering::Acquire) {
            return;
        }
        let transition = update(
            &mut shared
                .reducer
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        if let Some(transition) = transition
            && publish_transition(shared, transition).is_err()
        {
            shared.shutting_down.store(true, Ordering::Release);
        }
    });
}

fn run_audio_actor(
    shared: Arc<EngineShared>,
    control: Receiver<AudioControl>,
    audio: Receiver<AudioMessage>,
) {
    let mut sink = RodioSink::new(shared.config.audio_device.clone(), shared.config.buffer_ms);
    let mut processor = PcmProcessor::new(
        Arc::clone(&shared.config.tap),
        Arc::clone(&shared.config.eq),
    );
    let mut active_generation = 0;
    let mut started_generation = None;

    loop {
        while let Ok(command) = control.try_recv() {
            if handle_audio_control(
                &shared,
                &mut sink,
                &mut processor,
                &mut active_generation,
                &mut started_generation,
                command,
            ) {
                return;
            }
        }

        let message = match audio.recv_timeout(Duration::from_millis(10)) {
            Ok(message) => message,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        // Begin is sent before its decoder can produce PCM, but these are
        // separate channels. If the actor was already blocked on `audio`, a
        // first PCM block can wake it before the outer control poll. Drain
        // control again so that block is never mistaken for stale audio.
        while let Ok(command) = control.try_recv() {
            if handle_audio_control(
                &shared,
                &mut sink,
                &mut processor,
                &mut active_generation,
                &mut started_generation,
                command,
            ) {
                return;
            }
        }
        if message.token.generation != active_generation
            || shared.generation.load(Ordering::Acquire) != message.token.generation
        {
            continue;
        }
        match message.payload {
            AudioPayload::Pcm(mut samples) => {
                processor.process(&mut samples, shared.volume.load(Ordering::Acquire));
                let samples: Vec<f32> = samples.into_iter().map(|sample| sample as f32).collect();
                match sink.write(&samples, message.token.generation, &shared.generation) {
                    Ok(()) => {
                        if started_generation != Some(message.token.generation) {
                            started_generation = Some(message.token.generation);
                            update_reducer(&shared, |reducer| {
                                reducer.decoder_started(message.token)
                            });
                        }
                    }
                    Err(OutputError::Cancelled) => {}
                    Err(error) => handle_output_error(&shared, message.token, error),
                }
            }
            AudioPayload::Eof => match sink.drain(message.token.generation, &shared.generation) {
                Ok(()) => {
                    update_reducer(&shared, |reducer| reducer.decoder_eof(message.token));
                }
                Err(OutputError::Cancelled) => {}
                Err(error) => handle_output_error(&shared, message.token, error),
            },
            AudioPayload::Error(error_message) => {
                update_reducer(&shared, |reducer| {
                    reducer.decoder_error(message.token, error_message)
                });
            }
        }
    }
}

fn handle_audio_control(
    shared: &Arc<EngineShared>,
    sink: &mut RodioSink,
    processor: &mut PcmProcessor,
    active_generation: &mut u64,
    started_generation: &mut Option<u64>,
    command: AudioControl,
) -> bool {
    match command {
        AudioControl::Begin(token, transition) => {
            if shared.generation.load(Ordering::Acquire) != token.generation {
                return false;
            }
            *active_generation = token.generation;
            *started_generation = None;
            shared.config.tap.clear();
            *processor = PcmProcessor::new(
                Arc::clone(&shared.config.tap),
                Arc::clone(&shared.config.eq),
            );
            if let Err(error) = sink.begin(transition, token.generation, &shared.generation) {
                handle_output_error(shared, token, error);
            }
            false
        }
        AudioControl::Clear(generation, transition) => {
            if shared.generation.load(Ordering::Acquire) != generation {
                return false;
            }
            *active_generation = generation;
            *started_generation = None;
            shared.config.tap.clear();
            // Clear can only be superseded while it fades; the newer control
            // is already queued and owns the next output operation.
            let _ = sink.clear(transition, generation, &shared.generation);
            false
        }
        AudioControl::Shutdown => {
            let generation = shared.generation.load(Ordering::Acquire);
            let _ = sink.clear(OutputTransition::Immediate, generation, &shared.generation);
            true
        }
    }
}

fn handle_output_error(shared: &Arc<EngineShared>, token: DecodeToken, error: OutputError) {
    if matches!(error, OutputError::Cancelled) {
        return;
    }
    update_reducer(shared, |reducer| {
        reducer.decoder_error(token, error.to_string())
    });
}

fn spawn_stream(shared: Arc<EngineShared>, request: DecodeRequest) {
    let task_shared = Arc::clone(&shared);
    shared.runtime.spawn(async move {
        if !generation_is_current(&task_shared, request.token.generation) {
            return;
        }
        let seek = stream_seek(request.position_ms, request.song.duration_ms);
        let mut cancelled = task_shared.generation_signal.subscribe();
        // `subscribe` observes the current value as already seen. Recheck
        // after subscribing so a generation change between the first check
        // and subscription cannot leave this request waiting indefinitely.
        if !generation_is_current(&task_shared, request.token.generation) {
            return;
        }
        let response = task_shared.client.open_stream(
            &request.song.id,
            task_shared.config.max_bitrate_kbps,
            seek.api_offset_secs,
        );
        tokio::pin!(response);
        let response = loop {
            tokio::select! {
                result = &mut response => break result,
                changed = cancelled.changed() => {
                    // The watch channel is only a wake-up. A delayed value
                    // from an older publication must never cancel the active
                    // generation.
                    if changed.is_err()
                        || !generation_is_current(&task_shared, request.token.generation)
                    {
                        return;
                    }
                }
            }
        };
        let response = match response {
            Ok(response) => response,
            Err(ApiError::EmptyAudioStream) => {
                send_audio_async(
                    &task_shared,
                    AudioMessage {
                        token: request.token,
                        payload: AudioPayload::Error(
                            "The server returned an empty audio stream".into(),
                        ),
                    },
                )
                .await;
                return;
            }
            Err(_) => {
                send_audio_async(
                    &task_shared,
                    AudioMessage {
                        token: request.token,
                        payload: AudioPayload::Error("Unable to start the audio stream".into()),
                    },
                )
                .await;
                return;
            }
        };
        if !generation_is_current(&task_shared, request.token.generation) {
            return;
        }

        let (stream_tx, stream_rx) = mpsc::sync_channel(STREAM_CHUNKS);
        let decoder_shared = Arc::clone(&task_shared);
        let token = request.token;
        let decoder = thread::Builder::new()
            .name(format!("audio-decode-{}", token.generation))
            .spawn(move || decode_stream(decoder_shared, token, stream_rx, seek.discard_ms));
        if decoder.is_err() {
            send_audio_async(
                &task_shared,
                AudioMessage {
                    token,
                    payload: AudioPayload::Error("Unable to start the audio decoder".into()),
                },
            )
            .await;
            return;
        }

        let mut bytes = response;
        loop {
            let next = loop {
                tokio::select! {
                    next = bytes.next_chunk() => break next,
                    changed = cancelled.changed() => {
                        if changed.is_err()
                            || !generation_is_current(&task_shared, token.generation)
                        {
                            return;
                        }
                    }
                }
            };
            let next = match next {
                Ok(next) => next,
                Err(_) => {
                    let _ = send_stream_chunk(
                        &task_shared,
                        token.generation,
                        &stream_tx,
                        StreamItem::Failed,
                    )
                    .await;
                    return;
                }
            };
            let Some(chunk) = next else {
                break;
            };
            if !generation_is_current(&task_shared, token.generation) {
                return;
            }
            for part in chunk.chunks(64 * 1_024) {
                if !send_stream_chunk(
                    &task_shared,
                    token.generation,
                    &stream_tx,
                    StreamItem::Data(part.to_vec()),
                )
                .await
                {
                    return;
                }
            }
        }
        // Dropping the sender is the decoder's normal, ordered EOF marker.
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StreamSeek {
    api_offset_secs: Option<u32>,
    discard_ms: u32,
}

fn stream_seek(position_ms: u32, duration_ms: u32) -> StreamSeek {
    // `timeOffset` has no response contract proving that a server honoured it:
    // some servers return 2xx and silently stream from zero. Correctness wins
    // over speed until the API exposes a verifiable capability, so every seek
    // starts at byte zero and the decoder discards canonical PCM locally.
    StreamSeek {
        api_offset_secs: None,
        discard_ms: clamp_position(position_ms, duration_ms),
    }
}

fn generation_is_current(shared: &EngineShared, generation: u64) -> bool {
    !shared.shutting_down.load(Ordering::Acquire)
        && shared.generation.load(Ordering::Acquire) == generation
}

async fn send_stream_chunk(
    shared: &EngineShared,
    generation: u64,
    sender: &SyncSender<StreamItem>,
    mut item: StreamItem,
) -> bool {
    loop {
        if !generation_is_current(shared, generation) {
            return false;
        }
        match sender.try_send(item) {
            Ok(()) => return true,
            Err(TrySendError::Full(returned)) => {
                item = returned;
                tokio::time::sleep(CANCEL_POLL).await;
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
}

async fn send_audio_async(shared: &EngineShared, message: AudioMessage) -> bool {
    let generation = message.token.generation;
    let mut message = message;
    loop {
        if !generation_is_current(shared, generation) {
            return false;
        }
        match shared.audio.try_send(message) {
            Ok(()) => return true,
            Err(TrySendError::Full(returned)) => {
                message = returned;
                tokio::time::sleep(CANCEL_POLL).await;
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
}

fn send_audio_blocking(shared: &EngineShared, mut message: AudioMessage) -> bool {
    let generation = message.token.generation;
    loop {
        if !generation_is_current(shared, generation) {
            return false;
        }
        match shared.audio.try_send(message) {
            Ok(()) => return true,
            Err(TrySendError::Full(returned)) => {
                message = returned;
                thread::sleep(CANCEL_POLL);
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
}

enum StreamItem {
    Data(Vec<u8>),
    Failed,
}

struct StreamingSource {
    receiver: Mutex<Receiver<StreamItem>>,
    current: Vec<u8>,
    offset: usize,
    generation: u64,
    active_generation: Arc<AtomicU64>,
    received_audio: Arc<AtomicBool>,
}

impl StreamingSource {
    fn new(
        receiver: Receiver<StreamItem>,
        generation: u64,
        active_generation: Arc<AtomicU64>,
        received_audio: Arc<AtomicBool>,
    ) -> Self {
        Self {
            receiver: Mutex::new(receiver),
            current: Vec::new(),
            offset: 0,
            generation,
            active_generation,
            received_audio,
        }
    }

    fn cancelled(&self) -> bool {
        self.active_generation.load(Ordering::Acquire) != self.generation
    }
}

impl Read for StreamingSource {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        loop {
            if self.cancelled() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
            if self.offset < self.current.len() {
                let count = output.len().min(self.current.len() - self.offset);
                output[..count]
                    .copy_from_slice(&self.current[self.offset..self.offset.saturating_add(count)]);
                self.offset += count;
                return Ok(count);
            }
            match self
                .receiver
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .recv_timeout(SOURCE_POLL)
            {
                Ok(StreamItem::Data(bytes)) => {
                    if !bytes.is_empty() {
                        self.received_audio.store(true, Ordering::Relaxed);
                    }
                    self.current = bytes;
                    self.offset = 0;
                }
                Ok(StreamItem::Failed) => {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionReset,
                        "audio download failed",
                    ));
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(0),
            }
        }
    }
}

impl Seek for StreamingSource {
    fn seek(&mut self, _position: SeekFrom) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "streaming audio is not seekable",
        ))
    }
}

impl MediaSource for StreamingSource {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

fn decode_stream(
    shared: Arc<EngineShared>,
    token: DecodeToken,
    stream: Receiver<StreamItem>,
    discard_ms: u32,
) {
    let received_audio = Arc::new(AtomicBool::new(false));
    let source = StreamingSource::new(
        stream,
        token.generation,
        Arc::clone(&shared.generation),
        Arc::clone(&received_audio),
    );
    let media = MediaSourceStream::new(Box::new(source), Default::default());
    // An empty server transcode is retried as the original file, so the
    // response is not necessarily MP3. Let the registered format readers
    // identify the container from its bytes.
    let hint = Hint::new();
    let probed = match symphonia::default::get_probe().format(
        &hint,
        media,
        &FormatOptions {
            // Ogg exposes the final granule as packet padding. The Opus
            // fallback consumes that trim so the decoded tail matches the
            // original stream rather than playing encoder padding.
            enable_gapless: true,
            ..FormatOptions::default()
        },
        &MetadataOptions::default(),
    ) {
        Ok(probed) => probed,
        Err(_) if !generation_is_current(&shared, token.generation) => return,
        Err(SymphoniaError::IoError(error)) if error.kind() == io::ErrorKind::ConnectionReset => {
            decoder_failed(&shared, token, "The audio download failed");
            return;
        }
        Err(_) if !received_audio.load(Ordering::Relaxed) => {
            decoder_failed(&shared, token, "The server returned an empty audio stream");
            return;
        }
        Err(_) => {
            decoder_failed(&shared, token, "The audio format is not supported");
            return;
        }
    };
    let mut format = probed.format;
    let (track_id, codec_params) = match format.default_track() {
        Some(track) => (track.id, track.codec_params.clone()),
        None => {
            decoder_failed(&shared, token, "The audio stream has no playable track");
            return;
        }
    };
    let mut decoder = match StreamDecoder::new(&codec_params) {
        Ok(decoder) => decoder,
        Err(()) => {
            decoder_failed(&shared, token, "The audio codec is not supported");
            return;
        }
    };
    let mut canonicalizer = Canonicalizer::default();
    let mut discard_frames =
        u64::from(discard_ms).saturating_mul(u64::from(PCM_SAMPLE_RATE)) / 1_000;

    loop {
        if !generation_is_current(&shared, token.generation) {
            return;
        }
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(error)) if error.kind() == io::ErrorKind::UnexpectedEof => {
                let _ = send_audio_blocking(
                    &shared,
                    AudioMessage {
                        token,
                        payload: AudioPayload::Eof,
                    },
                );
                return;
            }
            Err(SymphoniaError::IoError(error)) if error.kind() == io::ErrorKind::Interrupted => {
                return;
            }
            Err(_) => {
                decoder_failed(&shared, token, "The audio stream ended unexpectedly");
                return;
            }
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            // A damaged frame is recoverable; later packets can still play.
            Err(PacketDecodeError::Recoverable) => continue,
            Err(PacketDecodeError::Fatal) if !generation_is_current(&shared, token.generation) => {
                return;
            }
            Err(PacketDecodeError::Fatal) => {
                decoder_failed(&shared, token, "The audio stream could not be decoded");
                return;
            }
        };
        let canonical =
            match canonicalizer.process(decoded.samples(), decoded.channels, decoded.sample_rate) {
                Ok(samples) => samples,
                Err(message) => {
                    decoder_failed(&shared, token, message);
                    return;
                }
            };
        let frames = canonical.len() / PCM_CHANNELS;
        let skip = usize::try_from(discard_frames.min(frames as u64)).unwrap_or(frames);
        discard_frames = discard_frames.saturating_sub(skip as u64);
        let audible = &canonical[skip * PCM_CHANNELS..];
        for block in audible.chunks(PCM_BLOCK_FRAMES * PCM_CHANNELS) {
            if !send_audio_blocking(
                &shared,
                AudioMessage {
                    token,
                    payload: AudioPayload::Pcm(block.to_vec()),
                },
            ) {
                return;
            }
        }
    }
}

struct DecodedPacket {
    samples: DecodedSamples,
    channels: usize,
    sample_rate: u32,
}

impl DecodedPacket {
    fn samples(&self) -> &[f32] {
        match &self.samples {
            DecodedSamples::Symphonia(samples) => samples.samples(),
            DecodedSamples::Opus(samples) => samples,
        }
    }
}

enum DecodedSamples {
    Symphonia(SampleBuffer<f32>),
    Opus(Vec<f32>),
}

enum StreamDecoder {
    Symphonia(Box<dyn Decoder>),
    Opus(Box<OggOpusDecoder>),
}

impl StreamDecoder {
    fn new(params: &CodecParameters) -> std::result::Result<Self, ()> {
        match symphonia::default::get_codecs().make(params, &DecoderOptions::default()) {
            Ok(decoder) => Ok(Self::Symphonia(decoder)),
            // Prefer Symphonia when it eventually gains Opus support. The
            // fallback is selected only after the primary registry rejects the
            // codec that the demuxer actually found.
            Err(_) if params.codec == CODEC_TYPE_OPUS => OggOpusDecoder::new(params)
                .map(Box::new)
                .map(Self::Opus)
                .map_err(|_| ()),
            Err(_) => Err(()),
        }
    }

    fn decode(&mut self, packet: &Packet) -> std::result::Result<DecodedPacket, PacketDecodeError> {
        match self {
            Self::Symphonia(decoder) => {
                let decoded = decoder.decode(packet).map_err(|error| match error {
                    SymphoniaError::DecodeError(_) => PacketDecodeError::Recoverable,
                    _ => PacketDecodeError::Fatal,
                })?;
                let spec = *decoded.spec();
                let mut samples = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
                samples.copy_interleaved_ref(decoded);
                Ok(DecodedPacket {
                    samples: DecodedSamples::Symphonia(samples),
                    channels: spec.channels.count(),
                    sample_rate: spec.rate,
                })
            }
            Self::Opus(decoder) => decoder
                .decode(packet.buf(), packet.trim_end())
                .map(|decoded| DecodedPacket {
                    samples: DecodedSamples::Opus(decoded.samples),
                    channels: decoded.channels,
                    sample_rate: OggOpusDecoder::SAMPLE_RATE,
                })
                .map_err(|error| match error {
                    OpusError::InvalidPacket(_) => PacketDecodeError::Recoverable,
                    _ => PacketDecodeError::Fatal,
                }),
        }
    }
}

enum PacketDecodeError {
    Recoverable,
    Fatal,
}

fn decoder_failed(shared: &EngineShared, token: DecodeToken, message: impl Into<String>) {
    let _ = send_audio_blocking(
        shared,
        AudioMessage {
            token,
            payload: AudioPayload::Error(message.into()),
        },
    );
}

#[derive(Default)]
struct Canonicalizer {
    input_rate: u32,
    resampler: Option<Resampler>,
}

impl Canonicalizer {
    fn process(
        &mut self,
        samples: &[f32],
        channels: usize,
        sample_rate: u32,
    ) -> std::result::Result<Vec<f64>, &'static str> {
        if channels == 0 || sample_rate == 0 {
            return Err("The audio stream has an invalid channel or sample rate");
        }
        let stereo = downmix_to_stereo(samples, channels);
        if self.input_rate != sample_rate {
            self.input_rate = sample_rate;
            self.resampler = Resampler::new(sample_rate, PCM_SAMPLE_RATE, PCM_CHANNELS);
        }
        let stereo = match &mut self.resampler {
            Some(resampler) => resampler.process(&stereo),
            None => stereo,
        };
        Ok(stereo.into_iter().map(f64::from).collect())
    }
}

/// Converts arbitrary interleaved channel counts into the player's canonical
/// stereo layout. Extra channels contribute equally and quietly to both sides;
/// this preserves dialogue without allowing a wide layout to overdrive stereo.
fn downmix_to_stereo(samples: &[f32], channels: usize) -> Vec<f32> {
    if channels == 0 {
        return Vec::new();
    }
    let mut stereo = Vec::with_capacity(samples.len() / channels * PCM_CHANNELS);
    for frame in samples.chunks_exact(channels) {
        match channels {
            1 => stereo.extend_from_slice(&[frame[0], frame[0]]),
            2 => stereo.extend_from_slice(&frame[..2]),
            _ => {
                let ambience = frame[2..].iter().copied().sum::<f32>() / (channels - 2) as f32;
                stereo.push((frame[0] + 0.5 * ambience) / 1.5);
                stereo.push((frame[1] + 0.5 * ambience) / 1.5);
            }
        }
    }
    stereo
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: &str) -> Song {
        let mut song = Song::default();
        song.id.id = id.into();
        song.name = id.into();
        song.uri = song.id.uri();
        song.duration_ms = 180_000;
        song
    }

    fn load(ids: &[&str], play: bool) -> PlayerCommand {
        PlayerCommand::LoadContext(LoadContext {
            songs: ids.iter().map(|id| song(id)).collect(),
            start_index: 0,
            position_ms: 0,
            play,
        })
    }

    #[test]
    fn repeated_songs_are_distinct_occurrences() {
        let mut reducer = PlaybackReducer::new(u16::MAX);
        reducer.apply(load(&["same", "same"], false));
        reducer.apply(PlayerCommand::AddManual(Box::new(song("same"))));
        reducer.apply(PlayerCommand::AddManual(Box::new(song("same"))));
        let state = reducer.snapshot();
        let ids = [
            state.current_occurrence().unwrap(),
            state.queue.context[0].occurrence_id,
            state.queue.manual[0].occurrence_id,
            state.queue.manual[1].occurrence_id,
        ];
        for (index, id) in ids.iter().enumerate() {
            assert!(ids[index + 1..].iter().all(|other| other != id));
        }
    }

    #[test]
    fn loading_a_context_preserves_the_manual_queue() {
        let mut reducer = PlaybackReducer::new(1);
        reducer.apply(PlayerCommand::AddManual(Box::new(song("manual"))));
        let restored = reducer.apply(load(&["context-a", "context-b"], false));
        assert_eq!(restored.output, OutputAction::Clear);
        assert_eq!(reducer.snapshot().current_song().unwrap().name, "context-a");
        assert_eq!(reducer.snapshot().queue.manual[0].song.name, "manual");
        assert_eq!(reducer.snapshot().queue.context[0].song.name, "context-b");
    }

    #[test]
    fn appending_context_preserves_playback_and_existing_queue() {
        let mut reducer = PlaybackReducer::new(1);
        reducer.apply(PlayerCommand::AddManual(Box::new(song("manual"))));
        let loaded = reducer.apply(load(&["current", "existing"], true));
        reducer
            .decoder_started(loaded.decode.unwrap().token)
            .unwrap();
        let before = reducer.snapshot().clone();
        let decode_generation = reducer.decode_generation();

        let appended = reducer.apply(PlayerCommand::AppendContext(vec![
            song("new-a"),
            song("new-b"),
        ]));

        assert_eq!(appended.snapshot.current, before.current);
        assert_eq!(appended.snapshot.queue.manual, before.queue.manual);
        assert_eq!(appended.snapshot.position, before.position);
        assert_eq!(appended.snapshot.play_instance_id, before.play_instance_id);
        assert_eq!(appended.receipt.decode_generation, decode_generation);
        assert!(appended.decode.is_none());
        assert_eq!(appended.output, OutputAction::None);
        assert_eq!(
            &appended.snapshot.queue.context[..before.queue.context.len()],
            before.queue.context.as_slice()
        );
        assert_eq!(
            appended.snapshot.queue.context[before.queue.context.len()..]
                .iter()
                .map(|entry| entry.song.name.as_str())
                .collect::<Vec<_>>(),
            ["new-a", "new-b"]
        );
    }

    #[test]
    fn repeat_context_includes_appended_songs() {
        let mut reducer = PlaybackReducer::new(1);
        let loaded = reducer.apply(load(&["original"], true));
        reducer.apply(PlayerCommand::AppendContext(vec![
            song("appended-a"),
            song("appended-b"),
        ]));
        reducer.apply(PlayerCommand::Repeat(RepeatMode::Context));

        let appended_a = reducer.decoder_eof(loaded.decode.unwrap().token).unwrap();
        assert_eq!(
            appended_a.snapshot.current_song().unwrap().name,
            "appended-a"
        );
        let appended_b = reducer
            .decoder_eof(appended_a.decode.unwrap().token)
            .unwrap();
        assert_eq!(
            appended_b.snapshot.current_song().unwrap().name,
            "appended-b"
        );
        let repeated = reducer
            .decoder_eof(appended_b.decode.unwrap().token)
            .unwrap();
        assert_eq!(repeated.snapshot.current_song().unwrap().name, "original");
        assert_eq!(
            repeated
                .snapshot
                .queue
                .context
                .iter()
                .map(|entry| entry.song.name.as_str())
                .collect::<Vec<_>>(),
            ["appended-a", "appended-b"]
        );
    }

    #[test]
    fn appending_while_shuffled_preserves_prefix_and_input_order() {
        let mut reducer = PlaybackReducer::new(1);
        reducer.apply(load(&["current", "old-a", "old-b", "old-c"], false));
        reducer.apply(PlayerCommand::Shuffle(true));
        let existing = reducer.snapshot().queue.context.clone();

        reducer.apply(PlayerCommand::AppendContext(vec![
            song("new-a"),
            song("new-b"),
            song("new-c"),
            song("new-d"),
        ]));

        assert_eq!(
            &reducer.snapshot().queue.context[..existing.len()],
            existing.as_slice()
        );
        assert_eq!(
            reducer.snapshot().queue.context[existing.len()..]
                .iter()
                .map(|entry| entry.song.name.as_str())
                .collect::<Vec<_>>(),
            ["new-a", "new-b", "new-c", "new-d"]
        );

        reducer.apply(PlayerCommand::Shuffle(false));
        assert_eq!(
            reducer
                .snapshot()
                .queue
                .context
                .iter()
                .map(|entry| entry.song.name.as_str())
                .collect::<Vec<_>>(),
            [
                "old-a", "old-b", "old-c", "new-a", "new-b", "new-c", "new-d"
            ]
        );
    }

    #[test]
    fn next_consumes_the_visible_head_immediately() {
        let mut reducer = PlaybackReducer::new(1);
        reducer.apply(PlayerCommand::AddManual(Box::new(song("manual"))));
        reducer.apply(load(&["current", "context"], true));
        let transition = reducer.apply(PlayerCommand::Next);
        assert_eq!(transition.snapshot.current_song().unwrap().name, "manual");
        assert!(transition.snapshot.queue.manual.is_empty());
        assert_eq!(transition.snapshot.queue.context[0].song.name, "context");
        assert!(transition.decode.is_some());
        assert_eq!(transition.output, OutputAction::Replace);
    }

    #[test]
    fn rapid_replacements_keep_smoothing_without_waiting_for_a_decoder() {
        let mut reducer = PlaybackReducer::new(1);
        let loaded = reducer.apply(load(&["a", "b", "c"], true));
        let old = loaded.decode.unwrap().token;
        reducer.decoder_started(old).unwrap();

        let first_skip = reducer.apply(PlayerCommand::Next);
        let skipped = first_skip.decode.unwrap().token;
        assert_eq!(first_skip.output, OutputAction::Replace);
        assert_eq!(first_skip.snapshot.playback(), Playback::Loading);

        // The output actor may not have handled the first skip yet. Loading
        // must not lose the second command's instruction to fade the old audio.
        let second_skip = reducer.apply(PlayerCommand::Next);
        assert_eq!(second_skip.output, OutputAction::Replace);
        assert_eq!(second_skip.snapshot.current_song().unwrap().name, "c");
        assert!(second_skip.snapshot.queue.is_empty());
        assert!(reducer.decoder_started(skipped).is_none());
        assert!(reducer.decoder_eof(old).is_none());

        // Skipping beyond the last song still fades out, but has no incoming
        // decoder. A newly selected context remains an explicit replacement.
        let ended = reducer.apply(PlayerCommand::Next);
        assert_eq!(ended.output, OutputAction::Replace);
        assert_eq!(ended.snapshot.playback(), Playback::Stopped);
        assert!(ended.decode.is_none());
        let replacement = reducer.apply(load(&["new"], true));
        assert_eq!(replacement.output, OutputAction::Replace);
        assert_eq!(replacement.snapshot.current_song().unwrap().name, "new");
        let ended = reducer
            .decoder_eof(replacement.decode.unwrap().token)
            .unwrap();
        assert_eq!(ended.output, OutputAction::Clear);
        assert!(ended.decode.is_none());
    }

    #[test]
    fn natural_eof_plays_manual_before_context() {
        let mut reducer = PlaybackReducer::new(1);
        reducer.apply(PlayerCommand::AddManual(Box::new(song("manual"))));
        let loaded = reducer.apply(load(&["current", "context"], true));
        let token = loaded.decode.unwrap().token;
        let ended = reducer.decoder_eof(token).unwrap();
        assert_eq!(ended.output, OutputAction::Clear);
        assert_eq!(ended.snapshot.current_song().unwrap().name, "manual");
        assert!(ended.snapshot.queue.manual.is_empty());
        assert_eq!(ended.snapshot.queue.context[0].song.name, "context");
    }

    #[test]
    fn repeat_track_does_not_consume_or_replace_the_occurrence() {
        let mut reducer = PlaybackReducer::new(1);
        let loaded = reducer.apply(load(&["current", "next"], true));
        let token = loaded.decode.unwrap().token;
        reducer.apply(PlayerCommand::Repeat(RepeatMode::Track));
        let before = reducer.snapshot().clone();
        let ended = reducer.decoder_eof(token).unwrap();
        assert_eq!(ended.output, OutputAction::Clear);
        assert_eq!(
            ended.snapshot.current_occurrence(),
            before.current_occurrence()
        );
        assert_eq!(ended.snapshot.queue, before.queue);
        assert!(ended.snapshot.play_instance_id > before.play_instance_id);
        assert!(ended.receipt.decode_generation > token.generation);
    }

    #[test]
    fn repeat_context_creates_a_new_occurrence_cycle() {
        let mut reducer = PlaybackReducer::new(1);
        let loaded = reducer.apply(load(&["only"], true));
        let old_occurrence = loaded.snapshot.current_occurrence().unwrap();
        let token = loaded.decode.unwrap().token;
        reducer.apply(PlayerCommand::Repeat(RepeatMode::Context));
        let ended = reducer.decoder_eof(token).unwrap();
        assert_eq!(ended.snapshot.current_song().unwrap().name, "only");
        assert_ne!(ended.snapshot.current_occurrence(), Some(old_occurrence));
    }

    #[test]
    fn stale_eof_and_errors_cannot_touch_a_new_generation() {
        let mut reducer = PlaybackReducer::new(1);
        let first = reducer.apply(load(&["first", "second"], true));
        let stale = first.decode.unwrap().token;
        let second = reducer.apply(PlayerCommand::Next);
        let before = second.snapshot.clone();
        assert!(reducer.decoder_eof(stale).is_none());
        assert!(
            reducer
                .decoder_error(stale, "old download failed".into())
                .is_none()
        );
        assert_eq!(reducer.snapshot(), &before);
    }

    #[test]
    fn current_output_failure_pauses_without_advancing_the_queue() {
        let mut reducer = PlaybackReducer::new(1);
        let loaded = reducer.apply(load(&["current", "next"], true));
        let token = loaded.decode.unwrap().token;
        reducer.decoder_started(token).unwrap();

        let failed = reducer
            .decoder_error(token, "The audio output stopped working".into())
            .unwrap();
        assert_eq!(failed.output, OutputAction::Clear);
        assert_eq!(failed.snapshot.current_song().unwrap().name, "current");
        assert_eq!(failed.snapshot.queue.context[0].song.name, "next");
        assert_eq!(failed.snapshot.playback(), Playback::Paused);
        assert_eq!(
            failed.snapshot.error.as_deref(),
            Some("The audio output stopped working")
        );
        assert!(failed.receipt.decode_generation > token.generation);
    }

    #[test]
    fn skip_to_removes_rows_above_and_previous_restores_history() {
        let mut reducer = PlaybackReducer::new(1);
        reducer.apply(load(&["a", "b", "c"], true));
        let c = reducer.snapshot().queue.context[1].occurrence_id;
        let skipped = reducer.apply(PlayerCommand::SkipTo(c));
        assert_eq!(skipped.output, OutputAction::Replace);
        assert_eq!(reducer.snapshot().current_song().unwrap().name, "c");
        assert!(reducer.snapshot().queue.is_empty());
        let stale_skip = reducer.apply(PlayerCommand::SkipTo(c));
        assert_eq!(stale_skip.output, OutputAction::None);
        assert!(stale_skip.decode.is_none());
        let previous = reducer.apply(PlayerCommand::Previous);
        assert_eq!(previous.output, OutputAction::Replace);
        assert_eq!(reducer.snapshot().current_song().unwrap().name, "b");
        assert_eq!(reducer.snapshot().queue.context[0].song.name, "c");
    }

    #[test]
    fn clear_manual_and_shuffle_never_move_manual_rows() {
        let mut reducer = PlaybackReducer::new(1);
        reducer.apply(PlayerCommand::AddManual(Box::new(song("m1"))));
        reducer.apply(PlayerCommand::AddManual(Box::new(song("m2"))));
        reducer.apply(load(&["a", "b", "c", "d"], false));
        let manual = reducer.snapshot().queue.manual.clone();
        reducer.apply(PlayerCommand::Shuffle(true));
        assert_eq!(reducer.snapshot().queue.manual, manual);
        reducer.apply(PlayerCommand::Shuffle(false));
        assert_eq!(
            reducer
                .snapshot()
                .queue
                .context
                .iter()
                .map(|entry| entry.song.name.as_str())
                .collect::<Vec<_>>(),
            ["b", "c", "d"]
        );
        reducer.apply(PlayerCommand::ClearManual);
        assert!(reducer.snapshot().queue.manual.is_empty());
        assert_eq!(reducer.snapshot().queue.context.len(), 3);
    }

    #[test]
    fn reducer_ids_and_shuffle_are_deterministic() {
        let run = || {
            let mut reducer = PlaybackReducer::new(12);
            reducer.apply(load(&["a", "b", "c", "d"], false));
            reducer.apply(PlayerCommand::AddManual(Box::new(song("m"))));
            reducer.apply(PlayerCommand::Shuffle(true));
            reducer
                .snapshot()
                .queue
                .entries()
                .map(|entry| (entry.occurrence_id, entry.song.name.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn seek_uses_a_new_generation_and_old_started_is_ignored() {
        let mut reducer = PlaybackReducer::new(1);
        let loaded = reducer.apply(load(&["a"], true));
        let old = loaded.decode.unwrap().token;
        let play_instance = loaded.snapshot.play_instance_id;
        let seek = reducer.apply(PlayerCommand::Seek(30_000));
        assert_eq!(seek.output, OutputAction::Clear);
        assert!(seek.receipt.decode_generation > old.generation);
        assert_eq!(seek.snapshot.position.elapsed_ms, 30_000);
        assert_eq!(seek.snapshot.play_instance_id, play_instance);
        assert!(reducer.decoder_started(old).is_none());
    }

    #[test]
    fn restarting_the_current_occurrence_keeps_its_play_instance() {
        let mut reducer = PlaybackReducer::new(1);
        let loaded = reducer.apply(load(&["a", "b"], false));
        let play_instance = loaded.snapshot.play_instance_id;
        reducer.apply(PlayerCommand::Seek(PREVIOUS_RESTART_MS + 1));
        let restarted = reducer.apply(PlayerCommand::Previous);
        assert_eq!(restarted.output, OutputAction::Replace);
        assert_eq!(restarted.snapshot.current_song().unwrap().name, "a");
        assert_eq!(restarted.snapshot.play_instance_id, play_instance);
        assert_eq!(restarted.snapshot.position.elapsed_ms, 0);
    }

    #[test]
    fn stop_then_play_restarts_the_occurrence_as_a_new_play_instance() {
        let mut reducer = PlaybackReducer::new(1);
        let loaded = reducer.apply(load(&["a"], true));
        let occurrence = loaded.snapshot.current_occurrence();
        let play_instance = loaded.snapshot.play_instance_id;

        let stopped = reducer.apply(PlayerCommand::Stop);
        assert_eq!(stopped.output, OutputAction::Clear);
        assert_eq!(stopped.snapshot.current_occurrence(), occurrence);
        assert_eq!(stopped.snapshot.play_instance_id, play_instance);
        assert_eq!(stopped.snapshot.playback(), Playback::Stopped);
        assert_eq!(stopped.snapshot.position.elapsed_ms, 0);

        let resumed = reducer.apply(PlayerCommand::Play);
        assert_eq!(resumed.output, OutputAction::Clear);
        assert_eq!(resumed.snapshot.current_occurrence(), occurrence);
        assert!(resumed.snapshot.play_instance_id > play_instance);
        assert_eq!(resumed.snapshot.playback(), Playback::Loading);
        assert_eq!(resumed.snapshot.position.elapsed_ms, 0);
        assert!(resumed.decode.is_some());
    }

    #[test]
    fn stop_then_toggle_restarts_the_occurrence_as_a_new_play_instance() {
        let mut reducer = PlaybackReducer::new(1);
        let loaded = reducer.apply(load(&["a"], true));
        let occurrence = loaded.snapshot.current_occurrence();
        let play_instance = loaded.snapshot.play_instance_id;

        reducer.apply(PlayerCommand::Stop);
        let resumed = reducer.apply(PlayerCommand::Toggle);
        assert_eq!(resumed.snapshot.current_occurrence(), occurrence);
        assert!(resumed.snapshot.play_instance_id > play_instance);
        assert_eq!(resumed.snapshot.playback(), Playback::Loading);
        assert_eq!(resumed.snapshot.position.elapsed_ms, 0);
        assert!(resumed.decode.is_some());
    }

    #[test]
    fn paused_play_and_toggle_keep_the_play_instance() {
        for resume in [PlayerCommand::Play, PlayerCommand::Toggle] {
            let mut reducer = PlaybackReducer::new(1);
            let loaded = reducer.apply(load(&["a"], true));
            let occurrence = loaded.snapshot.current_occurrence();
            let play_instance = loaded.snapshot.play_instance_id;

            let paused = reducer.apply(PlayerCommand::Pause);
            assert_eq!(paused.output, OutputAction::Clear);
            let resumed = reducer.apply(resume);
            assert_eq!(resumed.output, OutputAction::Clear);
            assert_eq!(resumed.snapshot.current_occurrence(), occurrence);
            assert_eq!(resumed.snapshot.play_instance_id, play_instance);
            assert_eq!(resumed.snapshot.playback(), Playback::Loading);
            assert!(resumed.decode.is_some());
        }
    }

    #[test]
    fn advancing_to_another_occurrence_creates_a_play_instance() {
        let mut reducer = PlaybackReducer::new(1);
        let loaded = reducer.apply(load(&["a", "b"], true));
        let next = reducer.apply(PlayerCommand::Next);
        assert_ne!(
            next.snapshot.current_occurrence(),
            loaded.snapshot.current_occurrence()
        );
        assert!(next.snapshot.play_instance_id > loaded.snapshot.play_instance_id);
    }

    #[test]
    fn seek_never_trusts_an_unverifiable_server_offset() {
        let seek = stream_seek(30_250, 180_000);
        assert_eq!(seek.api_offset_secs, None);
        assert_eq!(seek.discard_ms, 30_250);

        // A missing duration means unknown, not zero-length: keep the full
        // local discard target rather than silently starting from the top.
        assert_eq!(stream_seek(30_250, 0).discard_ms, 30_250);
    }

    #[test]
    fn position_interpolates_only_while_playing_and_stops_at_duration() {
        let mut snapshot = PlaybackSnapshot {
            current: Some(QueueEntry {
                occurrence_id: OccurrenceId(1),
                section: QueueSection::Context,
                song: song("a"),
                context_index: Some(0),
            }),
            position: PlaybackPosition {
                playback: Playback::Playing,
                elapsed_ms: 179_000,
                observed_at: Some(Instant::now() - Duration::from_secs(2)),
            },
            ..PlaybackSnapshot::default()
        };
        assert_eq!(snapshot.position_now(), 180_000);
        snapshot.position.playback = Playback::Paused;
        assert_eq!(snapshot.position_now(), 179_000);
    }

    #[test]
    fn cancelled_stream_source_wakes_without_any_network_bytes() {
        let (_sender, receiver) = mpsc::sync_channel(1);
        let generation = Arc::new(AtomicU64::new(2));
        let received_audio = Arc::new(AtomicBool::new(false));
        let mut source = StreamingSource::new(receiver, 1, generation, received_audio);
        let started = Instant::now();
        let error = source.read(&mut [0; 16]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(started.elapsed() < SOURCE_POLL);
    }

    #[test]
    fn arbitrary_channels_downmix_to_stereo() {
        assert_eq!(
            downmix_to_stereo(&[0.25, -0.5], 1),
            [0.25, 0.25, -0.5, -0.5]
        );
        assert_eq!(downmix_to_stereo(&[0.25, -0.5], 2), [0.25, -0.5]);
        let surround = downmix_to_stereo(&[1.0, -1.0, 0.5, 0.5], 4);
        assert_eq!(surround.len(), 2);
        assert!((surround[0] - 5.0 / 6.0).abs() < 1e-6);
        assert!((surround[1] + 0.5).abs() < 1e-6);
    }

    #[test]
    fn concurrent_reducer_mutation_and_publication_are_serialized() {
        let serial = Arc::new(TransitionSerial::default());
        let reducer = Arc::new(Mutex::new(PlaybackReducer::new(0)));
        let published = Arc::new(Mutex::new(Vec::new()));
        let (first_reduced_tx, first_reduced_rx) = mpsc::channel();
        let (release_first_tx, release_first_rx) = mpsc::channel();
        let (second_attempted_tx, second_attempted_rx) = mpsc::channel();
        let (second_reduced_tx, second_reduced_rx) = mpsc::channel();

        let first = {
            let serial = Arc::clone(&serial);
            let reducer = Arc::clone(&reducer);
            let published = Arc::clone(&published);
            thread::spawn(move || {
                serial.run(|| {
                    let transition = reducer
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .apply(PlayerCommand::Volume(11));
                    first_reduced_tx.send(()).unwrap();
                    release_first_rx.recv().unwrap();
                    published
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push((transition.snapshot.revision, transition.snapshot.volume));
                });
            })
        };
        first_reduced_rx.recv().unwrap();

        let second = {
            let serial = Arc::clone(&serial);
            let reducer = Arc::clone(&reducer);
            let published = Arc::clone(&published);
            thread::spawn(move || {
                second_attempted_tx.send(()).unwrap();
                serial.run(|| {
                    let transition = reducer
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .apply(PlayerCommand::Volume(22));
                    second_reduced_tx.send(()).unwrap();
                    published
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push((transition.snapshot.revision, transition.snapshot.volume));
                });
            })
        };
        second_attempted_rx.recv().unwrap();
        assert!(matches!(
            second_reduced_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        release_first_tx.send(()).unwrap();
        first.join().unwrap();
        second.join().unwrap();
        second_reduced_rx.recv().unwrap();
        assert_eq!(
            *published.lock().unwrap_or_else(PoisonError::into_inner),
            [(1, 11), (2, 22)]
        );
        assert_eq!(
            reducer
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .snapshot()
                .volume,
            22
        );
    }
}
