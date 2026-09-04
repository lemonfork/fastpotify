//! Fullscreen completion for the one long-lived AppKit window.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSWindow, NSWindowDidEnterFullScreenNotification, NSWindowDidExitFullScreenNotification,
    NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSNotification, NSNotificationCenter, NSObject};

struct FullscreenState {
    active: Cell<bool>,
    ctx: egui::Context,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FastpotifyFullscreenObserver"]
    #[ivars = FullscreenState]
    struct NotificationTarget;

    impl NotificationTarget {
        #[unsafe(method(fullscreenActive:))]
        fn fullscreen_active(&self, _notification: &NSNotification) {
            self.ivars().active.set(true);
            self.ivars().ctx.request_repaint();
        }

        #[unsafe(method(fullscreenEnded:))]
        fn fullscreen_ended(&self, _notification: &NSNotification) {
            self.ivars().active.set(false);
            self.ivars().ctx.request_repaint();
        }
    }
);

/// Observes one exact window without replacing winit's delegate.
///
/// AppKit's completed-exit notification, not winit's requested fullscreen
/// value, releases the window for mini-player size and chrome changes.
pub struct FullscreenObserver {
    center: Retained<NSNotificationCenter>,
    target: Retained<NotificationTarget>,
}

impl FullscreenObserver {
    /// Must be created and dropped on the AppKit main thread while `window`
    /// remains alive. Callbacks only publish state and wake the UI loop.
    pub fn new(window: &NSWindow, ctx: &egui::Context) -> Self {
        let mtm = MainThreadMarker::from(window);
        let allocated = mtm
            .alloc::<NotificationTarget>()
            .set_ivars(FullscreenState {
                active: Cell::new(window.styleMask().contains(NSWindowStyleMask::FullScreen)),
                ctx: ctx.clone(),
            });
        // SAFETY: NotificationTarget inherits NSObject's init signature.
        let target: Retained<NotificationTarget> = unsafe { msg_send![super(allocated), init] };
        let center = NSNotificationCenter::defaultCenter();
        // SAFETY: Each selector accepts NSNotification, notifications are
        // scoped to this live main-thread NSWindow, and Drop removes target.
        unsafe {
            center.addObserver_selector_name_object(
                &target,
                sel!(fullscreenActive:),
                Some(NSWindowDidEnterFullScreenNotification),
                Some(window),
            );
            center.addObserver_selector_name_object(
                &target,
                sel!(fullscreenEnded:),
                Some(NSWindowDidExitFullScreenNotification),
                Some(window),
            );
        }
        Self { center, target }
    }

    /// Remains true throughout fullscreen exit, until AppKit completes it.
    pub fn is_active(&self) -> bool {
        self.target.ivars().active.get()
    }
}

impl Drop for FullscreenObserver {
    fn drop(&mut self) {
        // SAFETY: target is still retained, and this main-thread observer
        // registered only its own callbacks with this notification center.
        unsafe { self.center.removeObserver(&self.target) };
    }
}
