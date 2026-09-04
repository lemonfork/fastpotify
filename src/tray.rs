//! Application-owned tray lifecycle and typed menu actions.
//!
//! Call `start` when the first native window is ready and `stop` at shutdown.
//! Hiding or changing the shape of a window leaves the tray running.
//! Only a running tray may be used to hide the window.
//! Native callbacks enqueue actions and wake the application; they never
//! mutate playback or window state themselves.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

#[cfg(target_os = "linux")]
#[path = "tray_linux.rs"]
mod platform;
#[cfg(not(target_os = "linux"))]
#[path = "tray_native.rs"]
mod platform;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    ShowMainWindow,
    ShowMiniPlayer,
    ShowCurrentWindow,
    ToggleWindowVisibility,
    PlayPause,
    Next,
    Previous,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayState {
    Stopped,
    /// Native shutdown was requested; start waits until the host has exited.
    Stopping,
    Running,
    /// Creation or the native host failed. A later start retries;
    /// this state cannot hide a window to the tray.
    Unavailable,
}

type Wake = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
struct ActionSender {
    sender: Sender<TrayAction>,
    wake: Wake,
}

impl ActionSender {
    fn new(sender: Sender<TrayAction>, wake: Wake) -> Self {
        Self { sender, wake }
    }

    fn send(&self, action: TrayAction) {
        if self.sender.send(action).is_ok() {
            (self.wake)();
        }
    }
}

/// Owns native resources. Drop stops the host on its owning thread; the
/// lifecycle below decides when to create, retain, or release it.
trait Host: Sized {
    fn start(actions: ActionSender, playing: bool) -> Result<Self, String>;
    fn set_playing(&mut self, playing: bool) -> Result<(), String>;
    fn stop(&mut self);
    fn is_alive(&self) -> bool;
}

enum State<H> {
    Stopped,
    Stopping {
        host: H,
        after: AfterStop,
    },
    Running {
        actions: Receiver<TrayAction>,
        host: H,
    },
    Unavailable,
}

#[derive(Clone, Copy)]
enum AfterStop {
    Stopped,
    Unavailable,
}

impl AfterStop {
    fn state<H>(self) -> State<H> {
        match self {
            Self::Stopped => State::Stopped,
            Self::Unavailable => State::Unavailable,
        }
    }
}

struct Lifecycle<H: Host> {
    state: State<H>,
    wake: Wake,
    playing: bool,
}

impl<H: Host> Lifecycle<H> {
    fn new(wake: Wake) -> Self {
        Self {
            state: State::Stopped,
            wake,
            playing: false,
        }
    }

    fn state(&self) -> TrayState {
        match &self.state {
            State::Stopped => TrayState::Stopped,
            State::Stopping { host, .. } if host.is_alive() => TrayState::Stopping,
            State::Stopping { after, .. } => match after {
                AfterStop::Stopped => TrayState::Stopped,
                AfterStop::Unavailable => TrayState::Unavailable,
            },
            State::Running { host, .. } if host.is_alive() => TrayState::Running,
            State::Running { .. } | State::Unavailable => TrayState::Unavailable,
        }
    }

    fn start(&mut self) -> Result<(), String> {
        self.retire_closed_host();
        if let State::Stopping { host, .. } = &mut self.state {
            host.stop();
            self.retire_closed_host();
            if matches!(self.state, State::Stopping { .. }) {
                return Err("the previous tray host is still stopping".into());
            }
        }
        if !matches!(self.state, State::Stopped | State::Unavailable) {
            return Ok(());
        }
        // Each native host gets a fresh channel; stopped callbacks cannot
        // reach a replacement, even if native cleanup finishes later.
        let (sender, actions) = mpsc::channel();
        match H::start(
            ActionSender::new(sender, Arc::clone(&self.wake)),
            self.playing,
        ) {
            Ok(host) => {
                self.state = State::Running { actions, host };
                Ok(())
            }
            Err(error) => {
                self.state = State::Unavailable;
                Err(error)
            }
        }
    }

    fn set_playing(&mut self, playing: bool) -> Result<(), String> {
        self.retire_closed_host();
        if self.playing == playing {
            return Ok(());
        }
        self.playing = playing;
        if let State::Running { host, .. } = &mut self.state
            && let Err(error) = host.set_playing(playing)
        {
            if let State::Running { actions, host } =
                std::mem::replace(&mut self.state, State::Unavailable)
            {
                drop(actions);
                self.retire(host, AfterStop::Unavailable);
            }
            return Err(error);
        }
        Ok(())
    }

    fn retire_closed_host(&mut self) {
        match &self.state {
            State::Running { host, .. } if !host.is_alive() => {
                log::warn!("the native tray host has stopped");
                self.state = State::Unavailable;
            }
            State::Stopping { host, after } if !host.is_alive() => self.state = after.state(),
            _ => {}
        }
    }

    fn drain_actions(&mut self) -> Vec<TrayAction> {
        self.retire_closed_host();
        match &self.state {
            State::Running { actions, .. } => actions.try_iter().collect(),
            _ => Vec::new(),
        }
    }

    fn stop(&mut self) {
        match std::mem::replace(&mut self.state, State::Stopped) {
            State::Running { actions, host } => {
                drop(actions);
                self.retire(host, AfterStop::Stopped);
            }
            State::Stopping { host, .. } => self.retire(host, AfterStop::Stopped),
            _ => {}
        }
    }

    fn retire(&mut self, mut host: H, after: AfterStop) {
        host.stop();
        self.state = if host.is_alive() {
            State::Stopping { host, after }
        } else {
            after.state()
        };
    }
}

/// One service for the application's lifetime, independent of window mode.
/// On macOS, lifecycle and playback-label methods must run on the main thread.
pub struct TrayService {
    lifecycle: Lifecycle<platform::Platform>,
}

impl TrayService {
    /// Prepares an inert service. `start` requests native registration.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            lifecycle: Lifecycle::new(Arc::new(wake)),
        }
    }

    /// Starts, or retries an unavailable tray. Repeated starts are harmless.
    /// Returns an error while an earlier host is still stopping.
    /// Call after the first native window is ready; macOS requires AppKit's
    /// main thread and a live event loop.
    pub fn start(&mut self) -> Result<(), String> {
        self.lifecycle.start()
    }

    pub fn state(&self) -> TrayState {
        self.lifecycle.state()
    }

    pub fn is_available(&self) -> bool {
        self.state() == TrayState::Running
    }

    /// Remembers the latest playback state even before startup or while unavailable.
    pub fn set_playing(&mut self, playing: bool) -> Result<(), String> {
        self.lifecycle.set_playing(playing)
    }

    /// Takes menu/Dock actions in arrival order. Apply them on the UI thread.
    pub fn drain_actions(&mut self) -> Vec<TrayAction> {
        self.lifecycle.drain_actions()
    }

    /// Discards pending clicks and requests native shutdown without waiting.
    /// Drop does the same cleanup. A new start must wait for `Stopped`, so
    /// the old and new native hosts never overlap.
    pub fn stop(&mut self) {
        self.lifecycle.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Default)]
    struct Native {
        starts: usize,
        drops: usize,
        playing: bool,
        fail_start: bool,
        fail_update: bool,
        deferred_stop: bool,
        closed: bool,
        callbacks: Vec<ActionSender>,
    }

    thread_local! {
        static NATIVE: RefCell<Rc<RefCell<Native>>> = RefCell::default();
    }

    struct FakeHost(Rc<RefCell<Native>>);

    impl Host for FakeHost {
        fn start(actions: ActionSender, playing: bool) -> Result<Self, String> {
            let native = NATIVE.with(|slot| Rc::clone(&slot.borrow()));
            {
                let mut state = native.borrow_mut();
                state.starts += 1;
                state.callbacks.push(actions);
                if state.fail_start {
                    return Err("start failed".into());
                }
                state.closed = false;
                state.playing = playing;
            }
            Ok(Self(native))
        }

        fn set_playing(&mut self, playing: bool) -> Result<(), String> {
            let mut state = self.0.borrow_mut();
            if state.fail_update {
                return Err("update failed".into());
            }
            state.playing = playing;
            Ok(())
        }

        fn is_alive(&self) -> bool {
            !self.0.borrow().closed
        }

        fn stop(&mut self) {
            let mut state = self.0.borrow_mut();
            if !state.deferred_stop {
                state.closed = true;
            }
        }
    }

    impl Drop for FakeHost {
        fn drop(&mut self) {
            self.0.borrow_mut().drops += 1;
        }
    }

    fn harness() -> (Lifecycle<FakeHost>, Rc<RefCell<Native>>, Arc<AtomicUsize>) {
        let native = Rc::new(RefCell::new(Native::default()));
        NATIVE.with(|slot| *slot.borrow_mut() = Rc::clone(&native));
        let woken = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&woken);
        let lifecycle = Lifecycle::new(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        (lifecycle, native, woken)
    }

    #[test]
    fn start_uses_latest_playback_and_duplicate_start_keeps_the_host() {
        let (mut tray, native, _) = harness();
        tray.set_playing(true).unwrap();
        assert_eq!(tray.state(), TrayState::Stopped);
        assert_eq!(native.borrow().starts, 0);
        tray.start().unwrap();
        tray.start().unwrap();
        assert_eq!(tray.state(), TrayState::Running);
        assert_eq!(native.borrow().starts, 1);
        assert!(native.borrow().playing);
    }

    #[test]
    fn failed_creation_retries_with_a_fresh_action_channel() {
        let (mut tray, native, woken) = harness();
        native.borrow_mut().fail_start = true;
        assert!(tray.start().is_err());
        assert_eq!(tray.state(), TrayState::Unavailable);
        let obsolete_click = native.borrow().callbacks[0].clone();
        native.borrow_mut().fail_start = false;
        tray.set_playing(true).unwrap();
        tray.start().unwrap();
        obsolete_click.send(TrayAction::Quit);
        native.borrow().callbacks[1].send(TrayAction::PlayPause);
        assert_eq!(tray.drain_actions(), [TrayAction::PlayPause]);
        assert_eq!(woken.load(Ordering::SeqCst), 1);
        assert!(native.borrow().playing);
    }

    #[test]
    fn stop_discards_pending_and_late_clicks_and_restart_is_explicit() {
        let (mut tray, native, woken) = harness();
        tray.start().unwrap();
        let click = native.borrow().callbacks[0].clone();
        click.send(TrayAction::Next);
        tray.stop();
        tray.stop();
        assert_eq!(tray.state(), TrayState::Stopped);
        assert!(tray.drain_actions().is_empty());
        tray.start().unwrap();
        click.send(TrayAction::Quit);
        native.borrow().callbacks[1].send(TrayAction::Previous);
        assert_eq!(tray.drain_actions(), [TrayAction::Previous]);
        assert_eq!(woken.load(Ordering::SeqCst), 2);
        assert_eq!(native.borrow().drops, 1);
        drop(tray);
        assert_eq!(native.borrow().drops, 2);
    }

    #[test]
    fn a_dead_native_host_cannot_accept_actions_or_hide_the_window() {
        let (mut tray, native, _) = harness();
        tray.start().unwrap();
        assert_eq!(tray.state(), TrayState::Running);
        native.borrow_mut().closed = true;
        assert_eq!(tray.state(), TrayState::Unavailable);
        assert!(tray.drain_actions().is_empty());
        assert_eq!(native.borrow().drops, 1);
    }

    #[test]
    fn failed_label_update_releases_host_and_remembers_desired_playback() {
        let (mut tray, native, _) = harness();
        tray.start().unwrap();
        native.borrow_mut().fail_update = true;
        assert!(tray.set_playing(true).is_err());
        assert_eq!(tray.state(), TrayState::Unavailable);
        assert_eq!(native.borrow().drops, 1);
        native.borrow_mut().fail_update = false;
        tray.start().unwrap();
        assert!(native.borrow().playing);
    }

    #[test]
    fn asynchronous_stop_must_finish_before_a_replacement_can_start() {
        let (mut tray, native, woken) = harness();
        tray.start().unwrap();
        let click = native.borrow().callbacks[0].clone();
        native.borrow_mut().deferred_stop = true;
        tray.stop();
        tray.stop();
        assert_eq!(tray.state(), TrayState::Stopping);
        assert!(tray.start().is_err());
        click.send(TrayAction::Quit);
        assert!(tray.drain_actions().is_empty());
        assert_eq!(woken.load(Ordering::SeqCst), 0);
        assert_eq!(native.borrow().starts, 1);
        assert_eq!(native.borrow().drops, 0);

        native.borrow_mut().closed = true;
        assert_eq!(tray.state(), TrayState::Stopped);
        tray.start().unwrap();
        assert_eq!(tray.state(), TrayState::Running);
        assert_eq!(native.borrow().starts, 2);
        assert_eq!(native.borrow().drops, 1);
    }

    #[test]
    fn failed_update_waits_for_cleanup_and_explicit_stop_cancels_recovery() {
        let (mut tray, native, woken) = harness();
        tray.start().unwrap();
        let click = native.borrow().callbacks[0].clone();
        {
            let mut state = native.borrow_mut();
            state.deferred_stop = true;
            state.fail_update = true;
        }
        assert!(tray.set_playing(true).is_err());
        assert_eq!(tray.state(), TrayState::Stopping);
        assert!(tray.start().is_err());
        click.send(TrayAction::Quit);
        assert!(tray.drain_actions().is_empty());
        assert_eq!(woken.load(Ordering::SeqCst), 0);
        assert_eq!(native.borrow().starts, 1);

        native.borrow_mut().closed = true;
        assert_eq!(tray.state(), TrayState::Unavailable);
        tray.start().unwrap();
        assert!(native.borrow().playing);
        assert_eq!(native.borrow().starts, 2);

        assert!(tray.set_playing(false).is_err());
        tray.stop();
        native.borrow_mut().closed = true;
        assert_eq!(tray.state(), TrayState::Stopped);
        assert_eq!(native.borrow().starts, 2);
    }

    #[test]
    fn a_later_start_can_retry_an_unsuccessful_shutdown_request() {
        let (mut tray, native, _) = harness();
        tray.start().unwrap();
        native.borrow_mut().deferred_stop = true;
        tray.stop();
        assert_eq!(tray.state(), TrayState::Stopping);
        native.borrow_mut().deferred_stop = false;
        tray.start().unwrap();
        assert_eq!(tray.state(), TrayState::Running);
        assert_eq!(native.borrow().starts, 2);
        assert_eq!(native.borrow().drops, 1);
    }
}
