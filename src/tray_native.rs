//! Native resources for the shared tray lifecycle on Windows and macOS.
//!
//! Windows owns its item on a message-loop thread. macOS keeps it on the
//! main thread, independently of the visible main/mini window.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, Once};

use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use super::{ActionSender, TrayAction};

const SHOW: &str = "show";
const SHOW_MINI: &str = "show-mini";
const PLAY_PAUSE: &str = "play-pause";
const NEXT: &str = "next";
const PREVIOUS: &str = "previous";
const QUIT: &str = "quit";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Owner(u64);

impl Owner {
    fn new() -> Self {
        static NEXT_OWNER: AtomicU64 = AtomicU64::new(1);
        Self(NEXT_OWNER.fetch_add(1, Ordering::Relaxed))
    }

    fn menu_id(self, action: &str) -> MenuId {
        MenuId::new(format!("{}:{action}", self.0))
    }
}

fn action_for(id: &MenuId) -> Option<(Owner, TrayAction)> {
    let (owner, action) = id.0.split_once(':')?;
    let owner = Owner(owner.parse().ok()?);
    let action = match action {
        SHOW => TrayAction::ShowMainWindow,
        SHOW_MINI => TrayAction::ShowMiniPlayer,
        PLAY_PAUSE => TrayAction::PlayPause,
        NEXT => TrayAction::Next,
        PREVIOUS => TrayAction::Previous,
        QUIT => TrayAction::Quit,
        _ => return None,
    };
    Some((owner, action))
}

/// muda's handler is process-global and can only be installed once. The
/// route changes with the native owner, without leaving a callback pointing
/// at the first service's disconnected channel.
#[derive(Default)]
struct Router(Mutex<Option<(Owner, ActionSender)>>);

impl Router {
    fn bind(&self, owner: Owner, actions: ActionSender) {
        *self.0.lock().expect("tray route poisoned") = Some((owner, actions));
    }

    fn clear(&self, owner: Owner) {
        let mut route = self.0.lock().expect("tray route poisoned");
        if route.as_ref().is_some_and(|(active, _)| *active == owner) {
            *route = None;
        }
    }

    fn menu(&self, id: &MenuId) {
        let Some((owner, action)) = action_for(id) else {
            return;
        };
        let sender = self
            .0
            .lock()
            .expect("tray route poisoned")
            .as_ref()
            .filter(|(active, _)| *active == owner)
            .map(|(_, sender)| sender.clone());
        if let Some(sender) = sender {
            sender.send(action);
        }
    }

    #[cfg(target_os = "macos")]
    fn reopen(&self) {
        let sender = self
            .0
            .lock()
            .expect("tray route poisoned")
            .as_ref()
            .map(|(_, sender)| sender.clone());
        if let Some(sender) = sender {
            sender.send(TrayAction::ShowCurrentWindow);
        }
    }
}

static ROUTER: Router = Router(Mutex::new(None));

#[cfg(any(windows, test))]
static WORKER_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A timed-out startup still owns native resources until its worker exits.
/// Keep that exclusivity even when no Platform was returned to the caller.
#[cfg(any(windows, test))]
struct WorkerLease;

#[cfg(any(windows, test))]
impl WorkerLease {
    fn acquire() -> Result<Self, String> {
        WORKER_ACTIVE
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .map(|_| Self)
            .map_err(|_| "the previous tray worker has not finished shutting down".to_owned())
    }
}

#[cfg(any(windows, test))]
impl Drop for WorkerLease {
    fn drop(&mut self) {
        WORKER_ACTIVE.store(false, Ordering::Release);
    }
}

fn install_event_handlers() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        // Install before creating the first menu: muda initializes its
        // handler cell on the first event, even when no handler is set.
        MenuEvent::set_event_handler(Some(|event: MenuEvent| ROUTER.menu(&event.id)));
        // Menus own clicks. Consume pointer events so hovering does not
        // accumulate messages in tray-icon's unbounded fallback channel.
        tray_icon::TrayIconEvent::set_event_handler(Some(|_| {}));
    });
}

fn play_pause_label(playing: bool) -> &'static str {
    if playing { "Pause" } else { "Play" }
}

struct Item {
    _icon: TrayIcon,
    play_pause: MenuItem,
}

fn build(owner: Owner, playing: bool) -> Result<Item, Box<dyn std::error::Error>> {
    install_event_handlers();
    let size = 32u32;
    #[cfg(windows)]
    let icon = Icon::from_rgba(crate::util::app_icon_rgba(size as usize), size, size)?;
    // AppKit renders template images in the menu bar's current appearance.
    #[cfg(target_os = "macos")]
    let icon = Icon::from_rgba(crate::util::tray_template_rgba(size as usize), size, size)?;
    let menu = Menu::new();
    let play_pause = MenuItem::with_id(
        owner.menu_id(PLAY_PAUSE),
        play_pause_label(playing),
        true,
        None,
    );
    menu.append_items(&[
        &MenuItem::with_id(owner.menu_id(SHOW), "Show Main Window", true, None),
        &MenuItem::with_id(owner.menu_id(SHOW_MINI), "Open Mini Player", true, None),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id(owner.menu_id(PREVIOUS), "Previous", true, None),
        &play_pause,
        &MenuItem::with_id(owner.menu_id(NEXT), "Next", true, None),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id(owner.menu_id(QUIT), "Quit", true, None),
    ])?;
    let builder = TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("Fastpotify")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(true);
    #[cfg(target_os = "macos")]
    let builder = builder.with_icon_as_template(true);
    Ok(Item {
        _icon: builder.build()?,
        play_pause,
    })
}

#[cfg(windows)]
mod host {
    use std::sync::mpsc::{Receiver, Sender};
    use std::thread::JoinHandle;
    use std::time::Duration;

    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW,
        TranslateMessage, WM_APP, WM_QUIT,
    };

    use super::*;

    pub struct Platform {
        owner: Owner,
        playing: Sender<bool>,
        thread_id: u32,
        thread: JoinHandle<()>,
        stop_requested: bool,
    }

    impl super::super::Host for Platform {
        fn start(actions: ActionSender, playing: bool) -> Result<Self, String> {
            let lease = WorkerLease::acquire()?;
            let owner = Owner::new();
            let (playing_tx, playing_rx) = std::sync::mpsc::channel();
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let (accept_tx, accept_rx) = std::sync::mpsc::channel();
            let thread = std::thread::Builder::new()
                .name("fastpotify-tray".to_owned())
                .spawn(move || {
                    // run drops its Item before returning, including every
                    // startup failure. The lease is released only after it.
                    let _lease = lease;
                    run(owner, playing, playing_rx, ready_tx, accept_rx);
                    ROUTER.clear(owner);
                })
                .map_err(|error| error.to_string())?;
            let thread_id = ready_rx
                .recv_timeout(Duration::from_secs(5))
                .map_err(|_| "the tray thread did not answer".to_string())??;
            ROUTER.bind(owner, actions);
            // A timed-out caller drops accept_tx. Even if readiness raced
            // with the timeout, the worker then drops its item and exits.
            if accept_tx.send(()).is_err() {
                ROUTER.clear(owner);
                return Err("the tray thread stopped during startup".to_string());
            }
            Ok(Self {
                owner,
                playing: playing_tx,
                thread_id,
                thread,
                stop_requested: false,
            })
        }

        fn set_playing(&mut self, playing: bool) -> Result<(), String> {
            self.playing
                .send(playing)
                .map_err(|_| "the tray thread has stopped".to_string())?;
            post(self.thread_id, WM_APP)
        }

        fn is_alive(&self) -> bool {
            !self.thread.is_finished()
        }

        fn stop(&mut self) {
            self.release();
        }
    }

    impl Platform {
        fn release(&mut self) {
            ROUTER.clear(self.owner);
            // Never join on the UI thread; the worker destroys its own item
            // when it consumes WM_QUIT. is_alive acknowledges that cleanup.
            if !self.stop_requested && !self.thread.is_finished() {
                match post(self.thread_id, WM_QUIT) {
                    Ok(()) => self.stop_requested = true,
                    Err(error) => log::warn!("the tray thread could not be stopped: {error}"),
                }
            }
        }
    }

    impl Drop for Platform {
        fn drop(&mut self) {
            self.release();
        }
    }

    fn post(thread_id: u32, message: u32) -> Result<(), String> {
        if unsafe { PostThreadMessageW(thread_id, message, 0, 0) } == 0 {
            Err(std::io::Error::last_os_error().to_string())
        } else {
            Ok(())
        }
    }

    fn run(
        owner: Owner,
        playing: bool,
        updates: Receiver<bool>,
        ready: Sender<Result<u32, String>>,
        accepted: Receiver<()>,
    ) {
        // PostThreadMessage requires the target's message queue to exist.
        let mut message: MSG = unsafe { std::mem::zeroed() };
        unsafe {
            PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
        }
        let item = match build(owner, playing) {
            Ok(item) => item,
            Err(error) => {
                let _ = ready.send(Err(error.to_string()));
                return;
            }
        };
        if ready.send(Ok(unsafe { GetCurrentThreadId() })).is_err() || accepted.recv().is_err() {
            return;
        }
        while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {
            if message.message == WM_APP {
                while let Ok(playing) = updates.try_recv() {
                    item.play_pause.set_text(play_pause_label(playing));
                }
                continue;
            }
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod host {
    use std::cell::RefCell;
    use std::ffi::CString;

    use objc2::runtime::{AnyClass, AnyObject, Bool, MethodImplementation, Sel};
    use objc2::{Encode, MainThreadMarker, sel};
    use objc2_app_kit::NSApplication;

    use super::*;

    thread_local! {
        static ITEM: RefCell<Option<(Owner, Item)>> = const { RefCell::new(None) };
    }

    // A token keeps App Send without moving AppKit objects off the main
    // thread. Every resource operation checks that thread before access.
    pub struct Platform {
        owner: Owner,
    }

    impl super::super::Host for Platform {
        fn start(actions: ActionSender, playing: bool) -> Result<Self, String> {
            let mtm = main_thread()?;
            if ITEM.with(|slot| slot.borrow().is_some()) {
                return Err("a macOS status item is already running".to_owned());
            }
            let owner = Owner::new();
            ROUTER.bind(owner, actions);
            install_reopen_handler(&NSApplication::sharedApplication(mtm));
            match build(owner, playing) {
                Ok(item) => {
                    ITEM.with(|slot| *slot.borrow_mut() = Some((owner, item)));
                    Ok(Self { owner })
                }
                Err(error) => {
                    ROUTER.clear(owner);
                    Err(error.to_string())
                }
            }
        }

        fn set_playing(&mut self, playing: bool) -> Result<(), String> {
            main_thread()?;
            self.with_item(|item| {
                item.play_pause.set_text(play_pause_label(playing));
                Ok(())
            })
        }

        fn is_alive(&self) -> bool {
            MainThreadMarker::new().is_some()
                && ITEM.with(|slot| {
                    slot.borrow()
                        .as_ref()
                        .is_some_and(|(owner, _)| *owner == self.owner)
                })
        }

        fn stop(&mut self) {
            self.release();
        }
    }

    impl Platform {
        fn with_item(&self, apply: impl FnOnce(&Item) -> Result<(), String>) -> Result<(), String> {
            ITEM.with(|slot| {
                let slot = slot.borrow();
                let Some((owner, item)) = slot.as_ref() else {
                    return Err("the macOS status item has stopped".to_owned());
                };
                if *owner != self.owner {
                    return Err("the macOS status item belongs to another lifecycle".to_owned());
                }
                apply(item)
            })
        }

        fn release(&mut self) {
            ROUTER.clear(self.owner);
            if let Err(error) = main_thread() {
                log::error!("the status item could not be removed: {error}");
                return;
            }
            ITEM.with(|slot| {
                let mut slot = slot.borrow_mut();
                if slot.as_ref().is_some_and(|(owner, _)| *owner == self.owner) {
                    *slot = None;
                }
            });
        }
    }

    impl Drop for Platform {
        fn drop(&mut self) {
            self.release();
        }
    }

    fn main_thread() -> Result<MainThreadMarker, String> {
        MainThreadMarker::new()
            .ok_or_else(|| "the macOS status item requires the main thread".to_owned())
    }

    /// AppKit reports a minimized window as visible too. A Dock click must
    /// raise the current window mode and wake its potentially idle loop.
    pub(super) fn request_reopen(_has_visible_windows: bool) -> Bool {
        ROUTER.reopen();
        Bool::YES
    }

    extern "C-unwind" fn application_should_handle_reopen(
        _delegate: *mut AnyObject,
        _selector: Sel,
        _application: *mut NSApplication,
        has_visible_windows: Bool,
    ) -> Bool {
        request_reopen(has_visible_windows.as_bool())
    }

    fn install_reopen_handler(app: &NSApplication) {
        let Some(delegate) = app.delegate() else {
            log::warn!("the macOS application delegate is unavailable");
            return;
        };
        let delegate: &AnyObject = AsRef::<AnyObject>::as_ref(&*delegate);
        let class = delegate.class();
        let selector = sel!(applicationShouldHandleReopen:hasVisibleWindows:);
        if class.responds_to(selector) {
            return;
        }
        let implementation: extern "C-unwind" fn(
            *mut AnyObject,
            Sel,
            *mut NSApplication,
            Bool,
        ) -> Bool = application_should_handle_reopen;
        let types = CString::new(format!("{}@:@{}", Bool::ENCODING, Bool::ENCODING))
            .expect("valid Objective-C type encoding");
        let installed = unsafe {
            objc2::ffi::class_addMethod(
                class as *const AnyClass as *mut AnyClass,
                selector,
                implementation.__imp(),
                types.as_ptr(),
            )
        };
        if !installed.as_bool() {
            log::warn!("the macOS Dock reopen handler could not be installed");
        }
    }
}

pub(super) use host::Platform;

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    use super::*;

    #[test]
    fn a_detached_startup_worker_blocks_restart_until_it_finishes() {
        let lease = WorkerLease::acquire().expect("the first worker owns the tray");
        let (release, cancelled) = std::sync::mpsc::channel();
        let (finished, cleanup) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            cancelled
                .recv()
                .expect("startup caller releases the worker");
            drop(lease);
            finished.send(()).expect("report completed cleanup");
        });

        // A startup timeout drops the JoinHandle without waiting for build.
        // Losing that handle must not allow a second native worker to start.
        drop(worker);
        assert!(WorkerLease::acquire().is_err());
        release.send(()).expect("cancel the pending startup");
        cleanup.recv().expect("wait for deterministic cleanup");
        let _replacement = WorkerLease::acquire().expect("restart after worker cleanup");
    }

    #[test]
    fn replacing_the_owner_rejects_old_menu_events_and_old_cleanup() {
        let router = Router::default();
        let old = Owner::new();
        let current = Owner::new();
        let (old_tx, old_rx) = std::sync::mpsc::channel();
        let (current_tx, current_rx) = std::sync::mpsc::channel();
        let woken = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&woken);
        router.bind(old, ActionSender::new(old_tx, Arc::new(|| {})));
        router.bind(
            current,
            ActionSender::new(
                current_tx,
                Arc::new(move || {
                    count.fetch_add(1, Ordering::SeqCst);
                }),
            ),
        );
        router.clear(old);
        router.menu(&old.menu_id(PLAY_PAUSE));
        assert!(old_rx.try_recv().is_err());
        assert!(current_rx.try_recv().is_err());
        assert_eq!(woken.load(Ordering::SeqCst), 0);

        router.menu(&current.menu_id(PLAY_PAUSE));
        assert_eq!(current_rx.try_recv(), Ok(TrayAction::PlayPause));
        assert_eq!(woken.load(Ordering::SeqCst), 1);

        router.clear(current);
        router.menu(&current.menu_id(PLAY_PAUSE));
        assert!(current_rx.try_recv().is_err());
        assert_eq!(woken.load(Ordering::SeqCst), 1);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_dock_click_asks_for_the_current_window_and_for_a_frame() {
        let owner = Owner::new();
        let (sender, commands) = std::sync::mpsc::channel();
        let woken = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&woken);
        ROUTER.bind(
            owner,
            ActionSender::new(
                sender,
                Arc::new(move || {
                    count.fetch_add(1, Ordering::SeqCst);
                }),
            ),
        );

        // The minimized-window case reports true; close-to-tray reports
        // false. Both must deliver a request and schedule its reader.
        for (visible, expected_wakes) in [(true, 1), (false, 2)] {
            assert!(host::request_reopen(visible).as_bool());
            assert_eq!(commands.try_recv(), Ok(TrayAction::ShowCurrentWindow));
            assert_eq!(woken.load(Ordering::SeqCst), expected_wakes);
        }
        ROUTER.clear(owner);
    }
}
