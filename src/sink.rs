//! Native PCM output for local Navidrome playback.
//!
//! The player gives this module interleaved stereo at 44.1 kHz. Device
//! discovery and stream creation stay here, off the UI thread. A new decode
//! generation can fade already queued sound before replacing it. Every wait
//! observes the shared generation so Next, Seek, and shutdown can interrupt it.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait};
use rodio::Source;

use crate::resample::Resampler;

/// The canonical format between the decoder, DSP stages, and output.
pub const PCM_SAMPLE_RATE: u32 = 44_100;
pub const PCM_CHANNELS: usize = 2;

/// The backend name retained for settings migration and diagnostics.
pub const NAME: &str = "rodio";

/// Maximum queued rodio chunks. Decoder blocks remain small, keeping the
/// audible queue bounded while leaving enough slack for scheduler jitter.
const QUEUE_LIMIT: usize = 6;

/// How often a blocked writer checks cancellation and device health.
const CANCEL_POLL: Duration = Duration::from_millis(5);

/// Maximum time natural EOF waits for already queued audio to be heard.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(3);

/// Length of each side of an explicit playback transition.
const INTERRUPT_FADE: Duration = Duration::from_millis(10);
const TRANSITION_POLL: Duration = Duration::from_millis(1);

/// How often playback looks at which output the system calls its default.
const DEFAULT_CHECK_INTERVAL: Duration = Duration::from_secs(2);

/// Default Windows device buffer length in milliseconds.
///
/// Small platform defaults can click under load (#88). A 100 ms buffer avoids
/// these underruns while keeping controls responsive.
pub const DEFAULT_BUFFER_MS: u32 = 100;

/// Allowed Windows device buffer range. Lower values can click; higher values
/// delay playback controls.
pub const BUFFER_MS_RANGE: std::ops::RangeInclusive<u32> = 20..=500;

/// Whether replacing queued audio smooths the discontinuity at its boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputTransition {
    Immediate,
    Smooth,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum OutputError {
    #[error("audio output was cancelled")]
    Cancelled,
    #[error("PCM must contain complete stereo frames")]
    InvalidPcm,
    #[error("{0}")]
    Unavailable(String),
}

/// A sample-clocked gain shared by every chunk in one output queue. Only the
/// audio callback advances the level; the actor can request a fade at any time.
struct Envelope {
    level: AtomicU32,
    target: AtomicU32,
    retired: AtomicBool,
    frames: u32,
}

impl Envelope {
    fn new(sample_rate: u32, fade_in: bool) -> Arc<Self> {
        let frames = fade_frames(sample_rate);
        Arc::new(Self {
            level: AtomicU32::new(if fade_in { 0 } else { frames }),
            target: AtomicU32::new(frames),
            retired: AtomicBool::new(false),
            frames,
        })
    }

    fn fade_out(&self) {
        self.target.store(0, Ordering::Relaxed);
    }

    fn silent(&self) -> bool {
        self.level.load(Ordering::Relaxed) == 0
    }

    fn gain(&self) -> f32 {
        self.level.load(Ordering::Relaxed) as f32 / self.frames as f32
    }

    fn advance_frame(&self) {
        let level = self.level.load(Ordering::Relaxed);
        let target = self.target.load(Ordering::Relaxed);
        let next = match level.cmp(&target) {
            std::cmp::Ordering::Less => level + 1,
            std::cmp::Ordering::Greater => level - 1,
            std::cmp::Ordering::Equal => level,
        };
        self.level.store(next, Ordering::Relaxed);
    }
}

fn fade_frames(sample_rate: u32) -> u32 {
    (u64::from(sample_rate) * INTERRUPT_FADE.as_millis() as u64 / 1_000).max(1) as u32
}

/// Runs after DSP and volume, in rodio's callback, including for PCM that was
/// queued before the actor received the transition request.
struct TransitionSource {
    inner: rodio::buffer::SamplesBuffer,
    envelope: Arc<Envelope>,
    channel: usize,
    gain: f32,
}

impl TransitionSource {
    fn new(inner: rodio::buffer::SamplesBuffer, envelope: Arc<Envelope>) -> Self {
        Self {
            inner,
            envelope,
            channel: 0,
            gain: 1.0,
        }
    }
}

impl Iterator for TransitionSource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        let sample = self.inner.next()?;
        // Rodio checks its stop flag only every 5 ms. A retired queue must
        // not overlap its replacement during the rest of that control period.
        if self.envelope.retired.load(Ordering::Relaxed) {
            return Some(0.0);
        }
        if self.channel == 0 {
            self.gain = self.envelope.gain();
        }
        self.channel = (self.channel + 1) % PCM_CHANNELS;
        if self.channel == 0 {
            // Publish silence only after both channels used the final gain,
            // so a completed smooth handoff cannot cut its last frame in half.
            self.envelope.advance_frame();
        }
        Some(sample * self.gain)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl Source for TransitionSource {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }

    fn channels(&self) -> rodio::ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
}

struct OutputQueue {
    sink: rodio::Sink,
    envelope: Arc<Envelope>,
}

impl OutputQueue {
    fn new(mixer: &rodio::mixer::Mixer, sample_rate: u32, fade_in: bool) -> Self {
        Self {
            sink: rodio::Sink::connect_new(mixer),
            envelope: Envelope::new(sample_rate, fade_in),
        }
    }

    /// `wait` permits another callback to run, returning false on deadline or
    /// device failure. Keeping it separate also lets tests drive rodio's mixer
    /// synchronously without a device or scheduler-dependent sleeps.
    fn interrupt(
        &self,
        transition: OutputTransition,
        generation: u64,
        current_generation: &AtomicU64,
        pending_fade_in: &mut bool,
        mut wait: impl FnMut() -> bool,
    ) -> Result<(), OutputError> {
        check_generation(generation, current_generation)?;
        if transition == OutputTransition::Smooth
            && !self.envelope.retired.load(Ordering::Relaxed)
            && !self.sink.is_paused()
            && !self.sink.empty()
        {
            // Remember the handoff before waiting: a newer skip can cancel
            // this call after the callback has already faded the old queue.
            *pending_fade_in = true;
            self.envelope.fade_out();
            loop {
                check_generation(generation, current_generation)?;
                if self.envelope.silent() || self.sink.empty() || !wait() {
                    break;
                }
            }
        }
        check_generation(generation, current_generation)?;
        // `clear` and appending to a stopped sink can block until callbacks
        // drain the old queue. Never reuse this sink after stopping it.
        self.envelope.retired.store(true, Ordering::Relaxed);
        self.sink.stop();
        Ok(())
    }

    fn append(&self, sample_rate: u32, samples: Vec<f32>) {
        let source = rodio::buffer::SamplesBuffer::new(
            PCM_CHANNELS as rodio::ChannelCount,
            sample_rate as rodio::SampleRate,
            samples,
        );
        self.sink
            .append(TransitionSource::new(source, Arc::clone(&self.envelope)));
    }
}

fn check_generation(generation: u64, current_generation: &AtomicU64) -> Result<(), OutputError> {
    if current_generation.load(Ordering::Acquire) == generation {
        Ok(())
    } else {
        Err(OutputError::Cancelled)
    }
}

fn transition_timeout(buffer_ms: u32) -> Duration {
    // Native callback periods are driver-selected. Give them the same slack
    // as the default Windows buffer without changing those device settings.
    let buffer_ms = if cfg!(windows) {
        buffer_ms.clamp(*BUFFER_MS_RANGE.start(), *BUFFER_MS_RANGE.end())
    } else {
        DEFAULT_BUFFER_MS
    };
    Duration::from_millis(u64::from(buffer_ms)) + INTERRUPT_FADE * 2
}

/// The buffer to ask the device for, in frames.
fn engine_buffer(
    sample_rate: u32,
    ms: u32,
    supported: cpal::SupportedBufferSize,
) -> cpal::BufferSize {
    let ms = ms.clamp(*BUFFER_MS_RANGE.start(), *BUFFER_MS_RANGE.end());
    let frames = (u64::from(sample_rate) * u64::from(ms) / 1000).max(1) as u32;
    match supported {
        cpal::SupportedBufferSize::Range { min, max } if min <= max && max > 0 => {
            cpal::BufferSize::Fixed(frames.clamp(min.max(1), max))
        }
        _ => cpal::BufferSize::Fixed(frames),
    }
}

pub struct RodioSink {
    /// The output device name from Settings; `None` follows the default.
    device: Option<String>,
    output: Option<Output>,
    watch: Option<DefaultWatch>,
    buffer_ms: u32,
    /// Survives cancelled handoffs and output-device replacement until real
    /// replacement PCM is queued, or a completed clear/immediate begin.
    pending_fade_in: bool,
}

struct Output {
    queue: OutputQueue,
    _stream: rodio::OutputStream,
    device_name: Option<String>,
    failed: Arc<AtomicBool>,
    sample_rate: u32,
    resampler: Option<Resampler>,
}

impl Output {
    fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    fn reset(&mut self, fade_in: bool) {
        self.queue = OutputQueue::new(self._stream.mixer(), self.sample_rate, fade_in);
        self.resampler = Resampler::new(PCM_SAMPLE_RATE, self.sample_rate, PCM_CHANNELS);
    }
}

impl RodioSink {
    pub fn new(device: Option<String>, buffer_ms: u32) -> Self {
        Self {
            device,
            output: None,
            watch: None,
            buffer_ms,
            pending_fade_in: false,
        }
    }

    /// Follows the system default output when no device is selected.
    fn follow_default(&mut self, at_once: bool) {
        if cfg!(target_os = "linux") || self.device.is_some() {
            return;
        }
        let Some(output) = &self.output else {
            return;
        };
        let watch = self.watch.get_or_insert_with(DefaultWatch::start);
        let current = if at_once { watch.ask() } else { watch.name() };
        if current.is_some() && current != output.device_name {
            log::info!(
                "the default audio output is now {}; moving playback to it",
                current.as_deref().unwrap_or("[unknown device]")
            );
            self.output = None;
        }
    }

    fn ensure_open(&mut self) -> Result<(), OutputError> {
        if self.output.as_ref().is_some_and(Output::failed) {
            log::warn!("the audio output stopped working; reopening it");
            self.output = None;
        }
        if self.output.is_some() {
            return Ok(());
        }
        self.output = Some(
            open_output(self.device.as_deref(), self.buffer_ms, self.pending_fade_in)
                .map_err(|error| OutputError::Unavailable(error.to_string()))?,
        );
        Ok(())
    }

    /// Replaces old PCM, smoothing only an explicit interruption of playing
    /// audio. The bounded fade wait runs on the audio actor, never the UI.
    pub fn begin(
        &mut self,
        transition: OutputTransition,
        generation: u64,
        current_generation: &AtomicU64,
    ) -> Result<(), OutputError> {
        self.interrupt(transition, generation, current_generation)?;
        // A completed hard cut ends any earlier handoff even if opening the
        // device subsequently fails or is superseded by another command.
        self.pending_fade_in &= transition == OutputTransition::Smooth;
        take_precedence();
        self.follow_default(true);
        check_generation(generation, current_generation)?;
        self.ensure_open()?;
        check_generation(generation, current_generation)?;
        if let Some(output) = &mut self.output {
            output.reset(self.pending_fade_in);
            output.queue.sink.play();
        }
        Ok(())
    }

    pub fn pause(&mut self) {
        if let Some(output) = &self.output {
            output.queue.sink.pause();
        }
    }

    pub fn resume(&mut self) -> Result<(), OutputError> {
        self.ensure_open()?;
        if let Some(output) = &self.output {
            output.queue.sink.play();
        }
        Ok(())
    }

    /// Stops playback without draining the full queue or opening a device.
    /// A completed clear never leaves a fade-in for the next play request.
    pub fn clear(
        &mut self,
        transition: OutputTransition,
        generation: u64,
        current_generation: &AtomicU64,
    ) -> Result<(), OutputError> {
        self.interrupt(transition, generation, current_generation)?;
        check_generation(generation, current_generation)?;
        self.pending_fade_in = false;
        if let Some(output) = &mut self.output {
            output.reset(false);
            output.queue.sink.pause();
        }
        Ok(())
    }

    fn interrupt(
        &mut self,
        transition: OutputTransition,
        generation: u64,
        current_generation: &AtomicU64,
    ) -> Result<(), OutputError> {
        check_generation(generation, current_generation)?;
        let Some(output) = &self.output else {
            return Ok(());
        };
        let deadline = Instant::now() + transition_timeout(self.buffer_ms);
        output.queue.interrupt(
            transition,
            generation,
            current_generation,
            &mut self.pending_fade_in,
            || {
                if output.failed() || Instant::now() >= deadline {
                    return false;
                }
                thread::sleep(TRANSITION_POLL);
                true
            },
        )
    }

    /// Writes one canonical PCM block. A changed generation interrupts both
    /// resampling and back-pressure waits before stale samples can be queued.
    pub fn write(
        &mut self,
        samples: &[f32],
        generation: u64,
        current_generation: &AtomicU64,
    ) -> Result<(), OutputError> {
        if !samples.len().is_multiple_of(PCM_CHANNELS) {
            return Err(OutputError::InvalidPcm);
        }
        if current_generation.load(Ordering::Acquire) != generation {
            return Err(OutputError::Cancelled);
        }
        if samples.is_empty() {
            return Ok(());
        }
        self.follow_default(false);
        self.ensure_open()?;
        let Some(output) = &mut self.output else {
            return Err(OutputError::Unavailable(
                "The audio output is not open".into(),
            ));
        };
        let converted = match &mut output.resampler {
            Some(resampler) => resampler.process(samples),
            None => samples.to_vec(),
        };
        if current_generation.load(Ordering::Acquire) != generation {
            return Err(OutputError::Cancelled);
        }
        if converted.is_empty() {
            return Ok(());
        }
        output.queue.append(output.sample_rate, converted);
        self.pending_fade_in = false;
        while output.queue.sink.len() > QUEUE_LIMIT {
            if current_generation.load(Ordering::Acquire) != generation {
                return Err(OutputError::Cancelled);
            }
            if output.failed() {
                return Err(OutputError::Unavailable(
                    "The audio output stopped working".into(),
                ));
            }
            thread::sleep(CANCEL_POLL);
        }
        Ok(())
    }

    /// Waits for natural EOF to become audible, but never delays a newer
    /// command. Cancellation is distinct from a current output failure so the
    /// player cannot silently leave the queue stuck in `Playing`.
    pub fn drain(
        &self,
        generation: u64,
        current_generation: &AtomicU64,
    ) -> Result<(), OutputError> {
        if current_generation.load(Ordering::Acquire) != generation {
            return Err(OutputError::Cancelled);
        }
        let Some(output) = &self.output else {
            return Ok(());
        };
        let deadline = Instant::now() + DRAIN_TIMEOUT;
        loop {
            if let Some(result) = drain_poll(
                generation,
                current_generation.load(Ordering::Acquire),
                output.failed(),
                output.queue.sink.empty(),
                Instant::now() >= deadline,
            ) {
                return result;
            }
            thread::sleep(CANCEL_POLL);
        }
    }
}

fn drain_poll(
    generation: u64,
    current_generation: u64,
    output_failed: bool,
    output_empty: bool,
    timed_out: bool,
) -> Option<Result<(), OutputError>> {
    if current_generation != generation {
        Some(Err(OutputError::Cancelled))
    } else if output_failed {
        Some(Err(OutputError::Unavailable(
            "The audio output stopped working".into(),
        )))
    } else if output_empty {
        Some(Ok(()))
    } else if timed_out {
        Some(Err(OutputError::Unavailable(
            "The audio output did not finish playing in time".into(),
        )))
    } else {
        None
    }
}

/// Opens the stream at canonical stereo 44.1 kHz, else at the device's own
/// rate, else at whatever rodio can negotiate.
fn open_stream(
    device: &cpal::Device,
    on_error: impl FnMut(cpal::StreamError) + Send + Clone + 'static,
    buffer_ms: u32,
) -> Result<rodio::OutputStream, rodio::StreamError> {
    let supported = device
        .default_output_config()
        .map(|config| *config.buffer_size())
        .unwrap_or(cpal::SupportedBufferSize::Unknown);
    let builder = |sample_rate: u32, buffer: bool| -> Result<_, rodio::StreamError> {
        let builder = rodio::OutputStreamBuilder::from_device(device.clone())?
            .with_channels(PCM_CHANNELS as rodio::ChannelCount)
            .with_sample_rate(sample_rate as rodio::SampleRate)
            .with_error_callback(on_error.clone());
        Ok(if buffer {
            builder.with_buffer_size(engine_buffer(sample_rate, buffer_ms, supported))
        } else {
            builder
        })
    };
    // The fixed engine buffer addresses Windows shared-mode underruns (#88).
    // CoreAudio, ALSA, PulseAudio, and PipeWire keep their proven
    // driver-selected callback periods.
    let fixed_buffer = cfg!(windows);
    if let Ok(stream) = builder(PCM_SAMPLE_RATE, fixed_buffer)?.open_stream() {
        return Ok(stream);
    }
    if let Ok(config) = device.default_output_config()
        && let Ok(stream) = builder(config.sample_rate().0, fixed_buffer)?.open_stream()
    {
        return Ok(stream);
    }
    builder(PCM_SAMPLE_RATE, false)?.open_stream_or_fallback()
}

/// Raises the Windows decoder thread one step above normal to prevent queued
/// audio from running out under load (#88).
///
/// Linux requires rtkit; CoreAudio owns its real-time callback on macOS.
#[cfg(windows)]
fn take_precedence() {
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
    };
    // SAFETY: the current thread's pseudo-handle needs no closing.
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
    }
}

#[cfg(not(windows))]
fn take_precedence() {}

/// Last default-output name, polled away from the audio actor because device
/// enumeration can block. The thread ends when the sink is dropped.
struct DefaultWatch(Arc<Mutex<Option<String>>>);

impl DefaultWatch {
    fn start() -> Self {
        let shared = Arc::new(Mutex::new(None));
        let weak = Arc::downgrade(&shared);
        let watching = thread::Builder::new()
            .name("audio-default-watch".into())
            .spawn(move || {
                while let Some(shared) = weak.upgrade() {
                    let name = default_output_name();
                    *shared.lock().unwrap_or_else(PoisonError::into_inner) = name;
                    drop(shared);
                    thread::sleep(DEFAULT_CHECK_INTERVAL);
                }
            });
        if let Err(error) = watching {
            log::warn!("cannot watch the default audio output: {error}");
        }
        Self(shared)
    }

    fn name(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn ask(&self) -> Option<String> {
        let name = default_output_name();
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = name.clone();
        name
    }
}

fn default_output_name() -> Option<String> {
    cpal::default_host()
        .default_output_device()
        .and_then(|device| device.name().ok())
}

#[derive(Debug, thiserror::Error)]
enum OpenError {
    #[error("No audio output device was found. Connect or enable one, then press play again.")]
    NoDevice,
    #[error("Cannot list the audio devices: {0}")]
    Devices(#[from] cpal::DevicesError),
    #[error("Cannot open the audio output: {0}")]
    Stream(#[from] rodio::StreamError),
}

fn open_output(
    preferred: Option<&str>,
    buffer_ms: u32,
    fade_in: bool,
) -> Result<Output, OpenError> {
    let host = cpal::default_host();
    let device = match preferred.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => {
            let chosen = host
                .output_devices()?
                .find(|device| device.name().is_ok_and(|found| found == name));
            match chosen {
                Some(device) => device,
                None => {
                    log::warn!("configured audio device is unavailable; using the default");
                    host.default_output_device().ok_or(OpenError::NoDevice)?
                }
            }
        }
        None => host.default_output_device().ok_or(OpenError::NoDevice)?,
    };
    let device_name = device.name().ok();
    log::info!(
        "audio output: {}",
        device_name.as_deref().unwrap_or("[unknown device]")
    );

    let failed = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&failed);
    let on_error = move |error: cpal::StreamError| {
        log::error!("audio stream error: {error}");
        flag.store(true, Ordering::Relaxed);
    };
    let mut stream = open_stream(&device, on_error, buffer_ms)?;
    stream.log_on_drop(false);
    let sample_rate = stream.config().sample_rate();
    let resampler = Resampler::new(PCM_SAMPLE_RATE, sample_rate, PCM_CHANNELS);
    if resampler.is_some() {
        log::info!(
            "the output runs at {sample_rate} Hz; PCM is converted from {PCM_SAMPLE_RATE} Hz"
        );
    }
    let queue = OutputQueue::new(stream.mixer(), sample_rate, fade_in);
    Ok(Output {
        queue,
        _stream: stream,
        device_name,
        failed,
        sample_rate,
        resampler,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(samples: Vec<f32>, rate: u32, envelope: &Arc<Envelope>) -> TransitionSource {
        TransitionSource::new(
            rodio::buffer::SamplesBuffer::new(PCM_CHANNELS as u16, rate, samples),
            Arc::clone(envelope),
        )
    }

    #[test]
    fn a_ten_millisecond_fade_crosses_chunks_without_splitting_stereo_frames() {
        for rate in [1_000, 44_100, 48_000] {
            let frames = fade_frames(rate) as usize;
            for fade_in in [false, true] {
                let envelope = Envelope::new(rate, fade_in);
                if !fade_in {
                    envelope.fade_out();
                }
                let mut heard = Vec::new();
                for chunk_frames in [3, frames / 2, frames + 4] {
                    let samples = [1.0, -0.5].repeat(chunk_frames);
                    heard.extend(source(samples, rate, &envelope));
                }
                for (frame, samples) in heard.as_chunks::<PCM_CHANNELS>().0.iter().enumerate() {
                    let level = if fade_in {
                        frame.min(frames)
                    } else {
                        frames.saturating_sub(frame)
                    };
                    let gain = level as f32 / frames as f32;
                    assert_eq!(samples, &[gain, -0.5 * gain]);
                }
            }
        }
    }

    #[test]
    fn uninterrupted_pcm_keeps_every_sample_including_at_chunk_boundaries() {
        let envelope = Envelope::new(PCM_SAMPLE_RATE, false);
        let samples = [-0.75, 0.125, 1.0, -1.0, 0.0, -0.0].repeat(300);
        let heard: Vec<_> = samples
            .chunks(14)
            .flat_map(|samples| source(samples.to_vec(), PCM_SAMPLE_RATE, &envelope))
            .collect();
        assert_eq!(heard, samples);
    }

    #[test]
    fn silence_is_announced_only_after_the_whole_stereo_frame() {
        let envelope = Envelope::new(100, false);
        envelope.fade_out();
        let mut playing = source(vec![1.0, -0.5, 1.0, -0.5], 100, &envelope);
        assert_eq!(playing.next(), Some(1.0));
        assert!(!envelope.silent());
        assert_eq!(playing.next(), Some(-0.5));
        assert!(envelope.silent());
        assert_eq!(playing.next(), Some(0.0));
        assert_eq!(playing.next(), Some(-0.0));
    }

    #[test]
    fn immediate_replacement_never_waits_or_mixes_the_previous_pcm_tail() {
        let generation = AtomicU64::new(2);
        let (mixer, mut callback) = rodio::mixer::mixer(2, PCM_SAMPLE_RATE);
        let old = OutputQueue::new(&mixer, PCM_SAMPLE_RATE, false);
        old.append(PCM_SAMPLE_RATE, vec![0.75; 4_000]);
        // Enter an active rodio control period; stop alone would leave its
        // already-started source audible for up to another five milliseconds.
        assert!(callback.by_ref().take(2_000).any(|sample| sample == 0.75));
        // Finish the frame: samples already fetched by rodio's channel
        // converter cannot be recalled by any per-source control.
        assert_eq!(callback.next(), Some(0.75));
        assert!(!old.sink.empty());
        let mut pending = false;
        old.interrupt(
            OutputTransition::Immediate,
            2,
            &generation,
            &mut pending,
            || panic!("immediate replacement must not wait for callbacks"),
        )
        .unwrap();
        let new = OutputQueue::new(&mixer, PCM_SAMPLE_RATE, false);
        new.append(PCM_SAMPLE_RATE, vec![0.25; 4_000]);
        let heard: Vec<_> = callback.by_ref().take(2_000).collect();
        assert!(heard.contains(&0.25));
        assert!(heard.iter().all(|sample| *sample == 0.0 || *sample == 0.25));
    }

    #[test]
    fn queued_audio_completes_its_fade_on_the_callback_before_replacement() {
        let generation = AtomicU64::new(2);
        let (mixer, mut callback) = rodio::mixer::mixer(2, PCM_SAMPLE_RATE);
        let old = OutputQueue::new(&mixer, PCM_SAMPLE_RATE, false);
        old.append(PCM_SAMPLE_RATE, vec![1.0; 8_000]);
        assert!(callback.by_ref().take(2_000).any(|sample| sample == 1.0));
        assert_eq!(callback.next(), Some(1.0));
        // Rodio begins an empty queue with a short mono silence. Let its
        // initial format-conversion span finish before measuring stereo PCM.
        assert!(callback.by_ref().take(1_200).all(|sample| sample == 1.0));
        let mut pending = false;
        let mut faded = Vec::new();
        old.interrupt(
            OutputTransition::Smooth,
            2,
            &generation,
            &mut pending,
            || {
                assert!(
                    faded.len() < 2_000,
                    "the callback did not complete its fade"
                );
                faded.extend(callback.by_ref().take(PCM_CHANNELS));
                true
            },
        )
        .unwrap();
        let frames = fade_frames(PCM_SAMPLE_RATE) as usize;
        assert_eq!(faded.len(), frames * PCM_CHANNELS);
        for (frame, samples) in faded.as_chunks::<PCM_CHANNELS>().0.iter().enumerate() {
            let gain = (frames - frame) as f32 / frames as f32;
            assert_eq!(samples, &[gain, gain]);
        }
        assert!(old.envelope.silent());
        assert!(pending);
        let replacement = OutputQueue::new(&mixer, PCM_SAMPLE_RATE, pending);
        replacement.append(PCM_SAMPLE_RATE, vec![0.5; 8_000]);
        let heard: Vec<_> = callback.by_ref().take(4_000).collect();
        let first_audible = heard.iter().copied().find(|sample| *sample > 0.0).unwrap();
        assert!(first_audible <= 0.5 / frames as f32);
        assert!(heard.contains(&0.5));
        assert!(heard.iter().all(|sample| (0.0..=0.5).contains(sample)));
    }

    #[test]
    fn cancelled_handoff_keeps_its_fade_even_if_the_old_queue_then_runs_out() {
        let generation = AtomicU64::new(2);
        let (mixer, mut callback) = rodio::mixer::mixer(2, PCM_SAMPLE_RATE);
        let old = OutputQueue::new(&mixer, PCM_SAMPLE_RATE, false);
        old.append(PCM_SAMPLE_RATE, vec![0.5; 2_000]);
        let mut pending = false;
        let result = old.interrupt(
            OutputTransition::Smooth,
            2,
            &generation,
            &mut pending,
            || {
                generation.store(3, Ordering::Release);
                true
            },
        );
        assert_eq!(result, Err(OutputError::Cancelled));
        assert!(pending);
        assert!(!old.envelope.retired.load(Ordering::Relaxed));
        for _ in 0..10_000 {
            if old.sink.empty() {
                break;
            }
            callback.next();
        }
        assert!(old.sink.empty());
        old.interrupt(
            OutputTransition::Smooth,
            3,
            &generation,
            &mut pending,
            || panic!("an empty queue must not wait"),
        )
        .unwrap();
        assert!(pending);
        // Rebuilding the mixer also models a device change between requests.
        let (replacement_mixer, _) = rodio::mixer::mixer(2, PCM_SAMPLE_RATE);
        let replacement = OutputQueue::new(&replacement_mixer, PCM_SAMPLE_RATE, pending);
        assert_eq!(
            source(vec![1.0; 2], PCM_SAMPLE_RATE, &replacement.envelope).collect::<Vec<_>>(),
            [0.0, 0.0]
        );
    }

    #[test]
    fn repeated_smooth_handoffs_before_pcm_keep_the_pending_fade_in() {
        let generation = AtomicU64::new(3);
        let (mixer, _) = rodio::mixer::mixer(2, PCM_SAMPLE_RATE);
        let mut pending = true;
        for _ in 0..3 {
            let awaiting_pcm = OutputQueue::new(&mixer, PCM_SAMPLE_RATE, pending);
            awaiting_pcm
                .interrupt(
                    OutputTransition::Smooth,
                    3,
                    &generation,
                    &mut pending,
                    || panic!("a not-yet-fed replacement must not wait"),
                )
                .unwrap();
            assert!(pending);
        }
        let replacement = OutputQueue::new(&mixer, PCM_SAMPLE_RATE, pending);
        assert_eq!(
            source(vec![1.0; 2], PCM_SAMPLE_RATE, &replacement.envelope).collect::<Vec<_>>(),
            [0.0, 0.0]
        );
    }

    #[test]
    fn empty_and_paused_queues_do_not_create_a_fade_in() {
        let generation = AtomicU64::new(1);
        let (mixer, _) = rodio::mixer::mixer(2, PCM_SAMPLE_RATE);
        for paused in [false, true] {
            let queue = OutputQueue::new(&mixer, PCM_SAMPLE_RATE, false);
            if paused {
                queue.append(PCM_SAMPLE_RATE, vec![0.5; 100]);
                queue.sink.pause();
            }
            let mut pending = false;
            queue
                .interrupt(
                    OutputTransition::Smooth,
                    1,
                    &generation,
                    &mut pending,
                    || panic!("there is no playing audio to fade"),
                )
                .unwrap();
            assert!(!pending);
        }
    }

    #[test]
    fn a_stalled_callback_is_retired_when_the_fade_wait_expires() {
        let generation = AtomicU64::new(2);
        let (mixer, mut callback) = rodio::mixer::mixer(2, PCM_SAMPLE_RATE);
        let queue = OutputQueue::new(&mixer, PCM_SAMPLE_RATE, false);
        queue.append(PCM_SAMPLE_RATE, vec![0.75; 2_000]);
        let mut pending = false;
        let mut polls = 0;
        queue
            .interrupt(
                OutputTransition::Smooth,
                2,
                &generation,
                &mut pending,
                || {
                    polls += 1;
                    // No device callbacks: the bounded deadline or a stream
                    // error ends the wait, without needing to drain rodio.
                    polls < 3
                },
            )
            .unwrap();
        assert_eq!(polls, 3);
        assert!(pending);
        // The previous generation may be cancelled during device discovery
        // after retiring its queue but before connecting a replacement.
        queue
            .interrupt(
                OutputTransition::Smooth,
                2,
                &generation,
                &mut pending,
                || panic!("a retired queue cannot make fade progress"),
            )
            .unwrap();
        assert!(callback.by_ref().take(2_000).all(|sample| sample == 0.0));
    }

    #[test]
    fn cancellation_wins_over_a_finished_or_expired_fade_wait() {
        let generation = AtomicU64::new(2);
        let (mixer, _) = rodio::mixer::mixer(2, PCM_SAMPLE_RATE);
        let queue = OutputQueue::new(&mixer, PCM_SAMPLE_RATE, false);
        queue.append(PCM_SAMPLE_RATE, vec![0.5; 1_000]);
        let mut pending = false;
        assert_eq!(
            queue.interrupt(
                OutputTransition::Smooth,
                2,
                &generation,
                &mut pending,
                || {
                    generation.store(3, Ordering::Release);
                    false
                },
            ),
            Err(OutputError::Cancelled)
        );
        assert!(pending);
        assert!(!queue.envelope.retired.load(Ordering::Relaxed));
        assert_eq!(
            queue.interrupt(
                OutputTransition::Immediate,
                2,
                &generation,
                &mut pending,
                || panic!("a stale command must not wait"),
            ),
            Err(OutputError::Cancelled)
        );
        assert!(!queue.envelope.retired.load(Ordering::Relaxed));
    }

    #[test]
    fn empty_and_cancelled_writes_preserve_the_pending_handoff_without_opening_a_device() {
        let generation = AtomicU64::new(2);
        let mut sink = RodioSink::new(None, DEFAULT_BUFFER_MS);
        sink.pending_fade_in = true;
        assert_eq!(sink.write(&[], 2, &generation), Ok(()));
        assert_eq!(
            sink.write(&[0.5, 0.5], 1, &generation),
            Err(OutputError::Cancelled)
        );
        assert!(sink.pending_fade_in);
        assert!(sink.output.is_none());
        assert_eq!(
            sink.clear(OutputTransition::Smooth, 1, &generation),
            Err(OutputError::Cancelled)
        );
        assert!(sink.pending_fade_in);
        sink.clear(OutputTransition::Smooth, 2, &generation)
            .unwrap();
        assert!(!sink.pending_fade_in);
        assert!(sink.output.is_none());
    }

    #[test]
    fn the_buffer_follows_the_setting_and_the_rate() {
        let unknown = cpal::SupportedBufferSize::Unknown;
        assert_eq!(
            engine_buffer(44_100, 100, unknown),
            cpal::BufferSize::Fixed(4410)
        );
        assert_eq!(
            engine_buffer(48_000, 100, unknown),
            cpal::BufferSize::Fixed(4800)
        );
        assert_eq!(
            engine_buffer(44_100, 20, unknown),
            cpal::BufferSize::Fixed(882)
        );
    }

    #[test]
    fn a_device_range_is_respected() {
        let range = cpal::SupportedBufferSize::Range { min: 64, max: 2048 };
        assert_eq!(
            engine_buffer(44_100, 100, range),
            cpal::BufferSize::Fixed(2048)
        );
        let tiny = cpal::SupportedBufferSize::Range {
            min: 4096,
            max: 8192,
        };
        assert_eq!(
            engine_buffer(44_100, 20, tiny),
            cpal::BufferSize::Fixed(4096)
        );
    }

    #[test]
    fn wild_buffer_values_are_clamped() {
        let unknown = cpal::SupportedBufferSize::Unknown;
        assert_eq!(
            engine_buffer(44_100, 0, unknown),
            engine_buffer(44_100, *BUFFER_MS_RANGE.start(), unknown)
        );
        assert_eq!(
            engine_buffer(44_100, 100_000, unknown),
            engine_buffer(44_100, *BUFFER_MS_RANGE.end(), unknown)
        );
    }

    #[test]
    fn drain_distinguishes_cancellation_from_current_output_failure() {
        assert_eq!(
            drain_poll(7, 8, false, false, false),
            Some(Err(OutputError::Cancelled))
        );
        assert_eq!(
            drain_poll(7, 7, true, false, false),
            Some(Err(OutputError::Unavailable(
                "The audio output stopped working".into()
            )))
        );
        assert_eq!(
            drain_poll(7, 7, false, false, true),
            Some(Err(OutputError::Unavailable(
                "The audio output did not finish playing in time".into()
            )))
        );
        assert_eq!(drain_poll(7, 7, false, true, false), Some(Ok(())));
        assert_eq!(drain_poll(7, 7, false, false, false), None);
    }
}
