//! Native macOS Now Playing metadata and remote-command handling.
//!
//! Command callbacks cross back into the application only through a channel;
//! UI state stays on the application thread. The system receives textual
//! metadata and timing, but not Fastpotify's opaque artwork references: those
//! are neither dereferenceable URLs nor safe places for authentication data.

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{AnyThread, DefinedClass, define_class, msg_send, sel};
use objc2_foundation::{
    MainThreadMarker, NSMutableDictionary, NSNumber, NSObject, NSObjectProtocol, NSString,
};
use objc2_media_player::{
    MPChangePlaybackPositionCommandEvent, MPMediaItemPropertyAlbumTitle, MPMediaItemPropertyArtist,
    MPMediaItemPropertyPlaybackDuration, MPMediaItemPropertyTitle, MPNowPlayingInfoCenter,
    MPNowPlayingInfoMediaType, MPNowPlayingInfoPropertyDefaultPlaybackRate,
    MPNowPlayingInfoPropertyElapsedPlaybackTime, MPNowPlayingInfoPropertyMediaType,
    MPNowPlayingInfoPropertyPlaybackRate, MPNowPlayingPlaybackState, MPRemoteCommand,
    MPRemoteCommandCenter, MPRemoteCommandEvent, MPRemoteCommandHandlerStatus,
};

use crate::media::{MediaCommand, MediaState};
use crate::player::Playback;

type Wake = Arc<dyn Fn() + Send + Sync>;
type SystemInfo = NSMutableDictionary<NSString, AnyObject>;

fn deliver(
    sender: &Sender<MediaCommand>,
    wake: &Wake,
    command: MediaCommand,
) -> MPRemoteCommandHandlerStatus {
    if sender.send(command).is_err() {
        return MPRemoteCommandHandlerStatus::CommandFailed;
    }
    (wake.as_ref())();
    MPRemoteCommandHandlerStatus::Success
}

fn position_ms(seconds: f64) -> u32 {
    if !seconds.is_finite() || seconds <= 0.0 {
        0
    } else {
        (seconds * 1_000.0).min(f64::from(u32::MAX)) as u32
    }
}

fn set_position_command(track_uri: &str, seconds: f64) -> Option<MediaCommand> {
    (!track_uri.is_empty()).then(|| MediaCommand::SetPosition {
        track_uri: track_uri.to_owned(),
        position_ms: position_ms(seconds),
    })
}

fn insert_string(info: &SystemInfo, key: &NSString, value: &str) {
    let value = NSString::from_str(value);
    info.insert(key, &value);
}

fn insert_number(info: &SystemInfo, key: &NSString, value: f64) {
    let value = NSNumber::new_f64(value);
    info.insert(key, &value);
}

fn now_playing_info(state: &MediaState) -> Option<Retained<SystemInfo>> {
    let track = state.track.as_ref()?;
    let info = SystemInfo::new();
    // SAFETY: These are immutable MediaPlayer.framework key objects, and each
    // value has the property-list type required by the corresponding key.
    unsafe {
        insert_string(&info, MPMediaItemPropertyTitle, &track.title);
        insert_number(
            &info,
            MPMediaItemPropertyPlaybackDuration,
            f64::from(track.duration_ms) / 1_000.0,
        );
        insert_number(
            &info,
            MPNowPlayingInfoPropertyElapsedPlaybackTime,
            f64::from(state.position_ms) / 1_000.0,
        );
        insert_number(
            &info,
            MPNowPlayingInfoPropertyPlaybackRate,
            if state.playback == Playback::Playing {
                1.0
            } else {
                0.0
            },
        );
        insert_number(&info, MPNowPlayingInfoPropertyDefaultPlaybackRate, 1.0);
        let media_type = NSNumber::new_usize(MPNowPlayingInfoMediaType::Audio.0);
        info.insert(MPNowPlayingInfoPropertyMediaType, &media_type);
        if !track.artists.is_empty() {
            insert_string(&info, MPMediaItemPropertyArtist, &track.artists.join(", "));
        }
        if !track.album.is_empty() {
            insert_string(&info, MPMediaItemPropertyAlbumTitle, &track.album);
        }
    }
    // `track.art_url` is a `fastpotify-art:` reference. It deliberately does
    // not become an asset URL or image object here; the system cannot resolve
    // it, and asking AppKit to do so caused souvlaki's null-image abort.
    Some(info)
}

fn playback_state(playback: Playback) -> MPNowPlayingPlaybackState {
    match playback {
        Playback::Playing => MPNowPlayingPlaybackState::Playing,
        Playback::Paused | Playback::Loading => MPNowPlayingPlaybackState::Paused,
        Playback::Stopped => MPNowPlayingPlaybackState::Stopped,
    }
}

struct CommandTargetIvars {
    sender: Sender<MediaCommand>,
    wake: Wake,
    track_uri: Arc<Mutex<String>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. All ivars are safe to
    // access from the queue on which MediaPlayer invokes the target.
    #[unsafe(super(NSObject))]
    #[name = "FastpotifyMediaCommandTarget"]
    #[ivars = CommandTargetIvars]
    struct CommandTarget;

    impl CommandTarget {
        #[unsafe(method(play:))]
        fn play(&self, _event: &MPRemoteCommandEvent) -> MPRemoteCommandHandlerStatus {
            self.send(MediaCommand::Play)
        }

        #[unsafe(method(pause:))]
        fn pause(&self, _event: &MPRemoteCommandEvent) -> MPRemoteCommandHandlerStatus {
            self.send(MediaCommand::Pause)
        }

        #[unsafe(method(togglePlayPause:))]
        fn toggle_play_pause(
            &self,
            _event: &MPRemoteCommandEvent,
        ) -> MPRemoteCommandHandlerStatus {
            self.send(MediaCommand::PlayPause)
        }

        #[unsafe(method(nextTrack:))]
        fn next_track(&self, _event: &MPRemoteCommandEvent) -> MPRemoteCommandHandlerStatus {
            self.send(MediaCommand::Next)
        }

        #[unsafe(method(previousTrack:))]
        fn previous_track(&self, _event: &MPRemoteCommandEvent) -> MPRemoteCommandHandlerStatus {
            self.send(MediaCommand::Previous)
        }

        #[unsafe(method(changePlaybackPosition:))]
        fn change_playback_position(
            &self,
            event: &MPChangePlaybackPositionCommandEvent,
        ) -> MPRemoteCommandHandlerStatus {
            self.guarded(|| {
                let uri = self
                    .ivars()
                    .track_uri
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone();
                // SAFETY: MediaPlayer sends this selector only for its change-
                // position command, whose event has this concrete class.
                let seconds = unsafe { event.positionTime() };
                let Some(command) = set_position_command(&uri, seconds) else {
                    return MPRemoteCommandHandlerStatus::NoActionableNowPlayingItem;
                };
                deliver(&self.ivars().sender, &self.ivars().wake, command)
            })
        }
    }

    // SAFETY: NSObjectProtocol adds no requirements beyond NSObject here.
    unsafe impl NSObjectProtocol for CommandTarget {}
);

impl CommandTarget {
    fn new(
        sender: Sender<MediaCommand>,
        wake: Wake,
        track_uri: Arc<Mutex<String>>,
    ) -> Retained<Self> {
        let this = Self::alloc().set_ivars(CommandTargetIvars {
            sender,
            wake,
            track_uri,
        });
        // SAFETY: This invokes NSObject's designated initializer after all
        // Rust ivars have been initialized.
        unsafe { msg_send![super(this), init] }
    }

    fn guarded(
        &self,
        action: impl FnOnce() -> MPRemoteCommandHandlerStatus,
    ) -> MPRemoteCommandHandlerStatus {
        match catch_unwind(AssertUnwindSafe(action)) {
            Ok(status) => status,
            Err(_) => {
                log::error!("a macOS media-command callback panicked");
                MPRemoteCommandHandlerStatus::CommandFailed
            }
        }
    }

    fn send(&self, command: MediaCommand) -> MPRemoteCommandHandlerStatus {
        self.guarded(|| deliver(&self.ivars().sender, &self.ivars().wake, command))
    }
}

fn register(command: &MPRemoteCommand, target: &AnyObject, action: Sel) {
    // SAFETY: Each selector below is implemented by `CommandTarget` with the
    // event argument and integer return type MPRemoteCommand expects.
    unsafe {
        command.setEnabled(true);
        command.addTarget_action(target, action);
    }
}

fn unregister(command: &MPRemoteCommand, target: &AnyObject) {
    // SAFETY: `target` is the same live object registered with this command.
    unsafe { command.removeTarget(Some(target)) };
}

struct Bridge {
    center: Retained<MPNowPlayingInfoCenter>,
    commands: Retained<MPRemoteCommandCenter>,
    target: Retained<CommandTarget>,
    last: Option<MediaState>,
    track_uri: Arc<Mutex<String>>,
}

impl Bridge {
    fn new(sender: Sender<MediaCommand>, wake: Wake, _mtm: MainThreadMarker) -> Self {
        // SAFETY: These framework singletons are available on every supported
        // macOS version and return retained, non-null objects.
        let center = unsafe { MPNowPlayingInfoCenter::defaultCenter() };
        let commands = unsafe { MPRemoteCommandCenter::sharedCommandCenter() };
        let track_uri: Arc<Mutex<String>> = Arc::default();
        let target = CommandTarget::new(sender, wake, Arc::clone(&track_uri));
        let target_object: &AnyObject = &target;

        // SAFETY: The generated accessors return the shared center's command
        // objects. Registration itself validates the matching selectors.
        unsafe {
            register(&commands.playCommand(), target_object, sel!(play:));
            register(&commands.pauseCommand(), target_object, sel!(pause:));
            register(
                &commands.togglePlayPauseCommand(),
                target_object,
                sel!(togglePlayPause:),
            );
            register(
                &commands.nextTrackCommand(),
                target_object,
                sel!(nextTrack:),
            );
            register(
                &commands.previousTrackCommand(),
                target_object,
                sel!(previousTrack:),
            );
            register(
                &commands.changePlaybackPositionCommand(),
                target_object,
                sel!(changePlaybackPosition:),
            );
        }

        Self {
            center,
            commands,
            target,
            last: None,
            track_uri,
        }
    }

    fn apply(&mut self, state: MediaState) {
        let track_changed = self
            .last
            .as_ref()
            .is_none_or(|last| last.track != state.track);
        let playback_changed = self
            .last
            .as_ref()
            .is_none_or(|last| last.playback != state.playback);
        if track_changed {
            *self
                .track_uri
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = state
                .track
                .as_ref()
                .map(|track| track.uri.clone())
                .unwrap_or_default();
        }
        if track_changed || playback_changed {
            self.publish(&state);
        }
        self.last = Some(state);
    }

    fn seeked(&mut self, position_ms: u32) {
        let Some(mut state) = self.last.take() else {
            return;
        };
        state.position_ms = position_ms;
        self.publish(&state);
        self.last = Some(state);
    }

    fn publish(&self, state: &MediaState) {
        // SAFETY: The dictionary contains only the documented NSString and
        // NSNumber values for these keys, and the center copies it.
        unsafe {
            if let Some(info) = now_playing_info(state) {
                self.center.setNowPlayingInfo(Some(&info));
                self.center.setPlaybackState(playback_state(state.playback));
            } else {
                self.center.setNowPlayingInfo(None);
                self.center
                    .setPlaybackState(MPNowPlayingPlaybackState::Stopped);
            }
        }
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let target: &AnyObject = &self.target;
        // SAFETY: These return the same shared command instances used during
        // registration, while `self.target` is still alive.
        unsafe {
            unregister(&self.commands.playCommand(), target);
            unregister(&self.commands.pauseCommand(), target);
            unregister(&self.commands.togglePlayPauseCommand(), target);
            unregister(&self.commands.nextTrackCommand(), target);
            unregister(&self.commands.previousTrackCommand(), target);
            unregister(&self.commands.changePlaybackPositionCommand(), target);
        }
    }
}

struct ActiveBridge {
    owner: u64,
    bridge: Bridge,
}

thread_local! {
    /// Framework objects remain on the main thread even while `App` moves in
    /// and out of eframe's `Send` window-creation closure.
    static ACTIVE_BRIDGE: RefCell<Option<ActiveBridge>> = const { RefCell::new(None) };
}

static NEXT_SERVICE_ID: AtomicU64 = AtomicU64::new(1);

pub struct MediaService {
    id: u64,
    commands: Receiver<MediaCommand>,
    sender: Sender<MediaCommand>,
    wake: Wake,
    pending: MediaState,
}

impl MediaService {
    pub fn spawn(wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (sender, commands) = std::sync::mpsc::channel();
        Self {
            id: NEXT_SERVICE_ID.fetch_add(1, Ordering::Relaxed),
            commands,
            sender,
            wake: Arc::new(wake),
            pending: MediaState::default(),
        }
    }

    /// Initializes MediaPlayer only after eframe has installed NSApplication
    /// and its event loop. Recreating a window keeps the existing handlers.
    pub fn attach(&mut self) {
        let Some(mtm) = MainThreadMarker::new() else {
            log::warn!("macOS media controls must be attached on the main thread");
            return;
        };
        ACTIVE_BRIDGE.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.as_ref().is_some_and(|active| active.owner == self.id) {
                return;
            }
            let mut bridge = Bridge::new(self.sender.clone(), Arc::clone(&self.wake), mtm);
            bridge.apply(self.pending.clone());
            *slot = Some(ActiveBridge {
                owner: self.id,
                bridge,
            });
        });
    }

    pub fn drain_commands(&self) -> Vec<MediaCommand> {
        self.commands.try_iter().collect()
    }

    pub fn update(&mut self, state: MediaState) {
        if MainThreadMarker::new().is_some() {
            ACTIVE_BRIDGE.with(|slot| {
                if let Some(active) = slot
                    .borrow_mut()
                    .as_mut()
                    .filter(|active| active.owner == self.id)
                {
                    active.bridge.apply(state.clone());
                }
            });
        }
        self.pending = state;
    }

    pub fn seeked(&mut self, position_ms: u32) {
        self.pending.position_ms = position_ms;
        if MainThreadMarker::new().is_some() {
            ACTIVE_BRIDGE.with(|slot| {
                if let Some(active) = slot
                    .borrow_mut()
                    .as_mut()
                    .filter(|active| active.owner == self.id)
                {
                    active.bridge.seeked(position_ms);
                }
            });
        }
    }
}

impl Drop for MediaService {
    fn drop(&mut self) {
        if MainThreadMarker::new().is_some() {
            ACTIVE_BRIDGE.with(|slot| {
                let owned = slot
                    .borrow()
                    .as_ref()
                    .is_some_and(|active| active.owner == self.id);
                if owned {
                    slot.borrow_mut().take();
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_media_player::MPNowPlayingInfoPropertyAssetURL;

    use crate::media::MediaTrack;

    fn state() -> MediaState {
        MediaState {
            playback: Playback::Playing,
            track: Some(MediaTrack {
                uri: "fastpotify:song:0123456789abcdef0123456789abcdef01234567:dHJhY2s".into(),
                title: "Track".into(),
                artists: vec!["One".into(), "Two".into()],
                album: "Record".into(),
                art_url: Some(
                    "fastpotify-art:0123456789abcdef0123456789abcdef01234567:Y292ZXI".into(),
                ),
                duration_ms: 180_000,
            }),
            position_ms: 12_500,
            ..MediaState::default()
        }
    }

    #[test]
    fn opaque_artwork_reference_is_not_published_as_a_system_url() {
        let info = now_playing_info(&state()).unwrap();

        // SAFETY: This is an immutable MediaPlayer.framework key object.
        assert!(
            info.objectForKey(unsafe { MPNowPlayingInfoPropertyAssetURL })
                .is_none()
        );
    }

    #[test]
    fn service_remains_send_while_framework_objects_stay_on_the_main_thread() {
        fn assert_send<T: Send>() {}

        assert_send::<MediaService>();
    }

    #[test]
    fn set_position_keeps_the_current_track_identity_and_bounds_milliseconds() {
        assert_eq!(
            set_position_command("current", 12.345),
            Some(MediaCommand::SetPosition {
                track_uri: "current".into(),
                position_ms: 12_345,
            })
        );
        assert!(set_position_command("", 12.345).is_none());
        assert_eq!(position_ms(f64::INFINITY), 0);
        assert_eq!(position_ms(f64::MAX), u32::MAX);
    }
}
