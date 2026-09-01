//! Native PCM output for local Navidrome playback.
//!
//! The player gives this module interleaved stereo at 44.1 kHz. Device
//! discovery and stream creation stay here, off the UI thread. A new decode
//! generation clears queued sound immediately, and every blocking drain loop
//! observes the shared generation so Next, Seek, and shutdown can interrupt it.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait};

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

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum OutputError {
    #[error("audio output was cancelled")]
    Cancelled,
    #[error("PCM must contain complete stereo frames")]
    InvalidPcm,
    #[error("{0}")]
    Unavailable(String),
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
}

struct Output {
    sink: rodio::Sink,
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

    fn reset(&mut self) {
        self.sink.clear();
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
            open_output(self.device.as_deref(), self.buffer_ms)
                .map_err(|error| OutputError::Unavailable(error.to_string()))?,
        );
        Ok(())
    }

    /// Starts a fresh decode generation and drops every old queued sample.
    pub fn begin(&mut self) -> Result<(), OutputError> {
        take_precedence();
        self.follow_default(true);
        self.ensure_open()?;
        if let Some(output) = &mut self.output {
            output.reset();
            output.sink.play();
        }
        Ok(())
    }

    pub fn pause(&mut self) {
        if let Some(output) = &self.output {
            output.sink.pause();
        }
    }

    pub fn resume(&mut self) -> Result<(), OutputError> {
        self.ensure_open()?;
        if let Some(output) = &self.output {
            output.sink.play();
        }
        Ok(())
    }

    /// Clears immediately; unlike natural EOF this never drains old PCM.
    pub fn clear(&mut self) {
        if let Some(output) = &mut self.output {
            output.reset();
            output.sink.pause();
        }
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
        output.sink.append(rodio::buffer::SamplesBuffer::new(
            PCM_CHANNELS as rodio::ChannelCount,
            output.sample_rate as rodio::SampleRate,
            converted,
        ));
        while output.sink.len() > QUEUE_LIMIT {
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
                output.sink.empty(),
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

fn open_output(preferred: Option<&str>, buffer_ms: u32) -> Result<Output, OpenError> {
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
    let sink = rodio::Sink::connect_new(stream.mixer());
    Ok(Output {
        sink,
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
