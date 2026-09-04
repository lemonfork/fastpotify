#[cfg(target_os = "macos")]
fn main() {
    let mtm =
        objc2::MainThreadMarker::new().expect("AppKit test must run on the process main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.finishLaunching();
    macos::fullscreen_notifications_are_scoped_and_removed_on_drop(mtm);
    macos::tray_start_is_idempotent_and_stop_releases_the_native_window(&app);
    macos::switching_window_modes_keeps_the_same_status_window(&app);
    macos::close_to_tray_keeps_event_loop_alive_and_show_restores_window(&app);
}

#[cfg(target_os = "macos")]
mod macos {
    use fastpotify::app::{App, AppOptions};
    use fastpotify::backend::{AuthStatus, Waker};
    use fastpotify::model::Action;
    use fastpotify::paths::AppDirs;
    use fastpotify::settings::Settings;
    use fastpotify::tray::{TrayService, TrayState};
    use std::path::PathBuf;

    use objc2::rc::Retained;
    use objc2_app_kit::{NSApplication, NSWindow};

    pub fn fullscreen_notifications_are_scoped_and_removed_on_drop(mtm: objc2::MainThreadMarker) {
        use fastpotify::mac_window::FullscreenObserver;
        use objc2_app_kit::{
            NSWindowDidEnterFullScreenNotification, NSWindowDidExitFullScreenNotification,
            NSWindowWillExitFullScreenNotification,
        };
        use objc2_foundation::NSNotificationCenter;

        // SAFETY: AppKit is initialized on the main thread. Rust owns the
        // retained windows, so AppKit must not release them on close.
        let (window, other) = unsafe {
            let window = NSWindow::new(mtm);
            let other = NSWindow::new(mtm);
            window.setReleasedWhenClosed(false);
            other.setReleasedWhenClosed(false);
            (window, other)
        };
        let ctx = egui::Context::default();
        let observer = FullscreenObserver::new(&window, &ctx);
        let center = NSNotificationCenter::defaultCenter();
        assert!(!observer.is_active());
        // SAFETY: These are the documented NSWindow notifications, delivered
        // synchronously with a live NSWindow object on AppKit's main thread.
        unsafe {
            center
                .postNotificationName_object(NSWindowDidEnterFullScreenNotification, Some(&other));
            assert!(
                !observer.is_active(),
                "another window must not block mode changes"
            );
            center
                .postNotificationName_object(NSWindowDidEnterFullScreenNotification, Some(&window));
            assert!(observer.is_active());
            center
                .postNotificationName_object(NSWindowWillExitFullScreenNotification, Some(&window));
            assert!(
                observer.is_active(),
                "requesting exit is not completing exit"
            );
            center
                .postNotificationName_object(NSWindowDidExitFullScreenNotification, Some(&window));
        }
        assert!(!observer.is_active());
        drop(observer);

        // Settle egui's pending frames before checking that the released
        // notification target no longer asks for any work.
        for _ in 0..3 {
            let _ = ctx.run_logic(&egui::RawInput::default(), |_| {});
        }
        assert!(!ctx.has_requested_repaint());
        unsafe {
            center
                .postNotificationName_object(NSWindowDidEnterFullScreenNotification, Some(&window));
        }
        assert!(!ctx.has_requested_repaint());
    }

    fn windows(app: &NSApplication) -> Vec<Retained<NSWindow>> {
        app.windows().into_iter().collect()
    }

    fn contains(windows: &[Retained<NSWindow>], window: &NSWindow) -> bool {
        windows
            .iter()
            .any(|previous| std::ptr::eq::<NSWindow>(&**previous, window))
    }

    fn new_status_window(
        app: &NSApplication,
        previous_windows: &[Retained<NSWindow>],
    ) -> Retained<NSWindow> {
        app.windows()
            .into_iter()
            .find(|window| window.isVisible() && !contains(previous_windows, window))
            .expect("starting the tray must create a visible native status window")
    }

    fn frame(app: &mut App, ctx: &egui::Context, rect: egui::Rect) -> Vec<egui::ViewportCommand> {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, rect.size())),
            ..Default::default()
        };
        let viewport = input.viewports.entry(egui::ViewportId::ROOT).or_default();
        viewport.inner_rect = Some(rect);
        viewport.outer_rect = Some(rect);
        let mut output = ctx.run_ui(input, |ui| app.frame_ui(ui));
        let commands = output.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .clone();
        output.textures_delta.clear();
        commands
    }

    pub fn tray_start_is_idempotent_and_stop_releases_the_native_window(app: &NSApplication) {
        let mut tray = TrayService::new(|| {});
        assert_eq!(tray.state(), TrayState::Stopped);
        assert!(!tray.is_available());

        let before_start = windows(app);
        tray.start().expect("start native tray");
        assert_eq!(tray.state(), TrayState::Running);
        assert!(tray.is_available());
        let status_window = new_status_window(app, &before_start);
        let running_windows = windows(app);

        for _ in 0..2 {
            tray.start().expect("repeat tray startup");
            assert!(
                status_window.isVisible(),
                "duplicate start must not flash the tray"
            );
            let current_windows = windows(app);
            assert_eq!(current_windows.len(), running_windows.len());
            assert!(contains(&current_windows, &status_window));
        }

        tray.stop();
        assert_eq!(tray.state(), TrayState::Stopped);
        assert!(!tray.is_available());
        assert!(!status_window.isVisible());
        // AppKit may still enumerate a closed status window object here, but
        // it must no longer be visible or able to serve the tray.

        let before_restart = windows(app);
        tray.start()
            .expect("restart native tray after explicit shutdown");
        assert_eq!(tray.state(), TrayState::Running);
        let restarted_window = new_status_window(app, &before_restart);
        assert!(!std::ptr::eq::<NSWindow>(
            &*status_window,
            &*restarted_window
        ));
        tray.stop();
        assert!(!restarted_window.isVisible());
    }

    pub fn switching_window_modes_keeps_the_same_status_window(native: &NSApplication) {
        let directory = TestDirectory::new();
        let dirs = AppDirs {
            config: directory.0.join("config"),
            state: directory.0.join("state"),
            cache: directory.0.join("cache"),
        };
        dirs.ensure().expect("create isolated app directories");
        let mut app = App::new(
            &Waker::default(),
            dirs,
            Settings::default(),
            AppOptions {
                media_controls: false,
                tray: true,
                offline: true,
            },
        );
        let ctx = egui::Context::default();
        let verified = fastpotify::api::VerifiedServer::default();
        app.user = Some(verified.user.clone());
        app.auth = AuthStatus::Connected(Box::new(verified));
        let before_attach = windows(native);
        app.attach(&ctx);
        let status_window = new_status_window(native, &before_attach);
        let running_windows = windows(native);
        let main = egui::Rect::from_min_size(egui::pos2(100.0, 150.0), egui::vec2(1024.0, 768.0));
        let mini = egui::Rect::from_min_size(egui::pos2(60.0, 80.0), egui::vec2(550.0, 232.0));
        app.winamp.restore_pos = Some(mini.min.into());

        app.actions.push(Action::ToggleWinampWindow);
        frame(&mut app, &ctx, main);
        assert!(app.settings.winamp_window);
        let current_windows = windows(native);
        assert_eq!(current_windows.len(), running_windows.len());
        assert!(contains(&current_windows, &status_window));
        assert!(
            status_window.isVisible(),
            "switching to mini must not flash the tray"
        );

        frame(&mut app, &ctx, mini);
        assert!(app.settings.winamp_window);
        app.actions.push(Action::ToggleWinampWindow);
        frame(&mut app, &ctx, mini);
        assert!(!app.settings.winamp_window);
        let current_windows = windows(native);
        assert_eq!(current_windows.len(), running_windows.len());
        assert!(contains(&current_windows, &status_window));
        assert!(
            status_window.isVisible(),
            "switching back must not recreate the tray"
        );

        app.shutdown();
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "fastpotify-tray-macos-{}-{:016x}",
                std::process::id(),
                rand::random::<u64>()
            ));
            std::fs::create_dir(&root).expect("create isolated test directory");
            Self(root)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).expect("remove isolated test directory");
        }
    }

    pub fn close_to_tray_keeps_event_loop_alive_and_show_restores_window(native: &NSApplication) {
        let directory = TestDirectory::new();
        let dirs = AppDirs {
            config: directory.0.join("config"),
            state: directory.0.join("state"),
            cache: directory.0.join("cache"),
        };
        dirs.ensure().expect("create isolated app directories");
        let mut app = App::new(
            &Waker::default(),
            dirs,
            Settings::default(),
            AppOptions {
                media_controls: false,
                tray: true,
                offline: true,
            },
        );
        assert!(!app.hides_to_tray(), "a pending tray cannot hide a window");
        let ctx = egui::Context::default();
        let before_attach = windows(native);
        app.attach(&ctx);
        assert!(app.hides_to_tray(), "the native tray is now available");
        let status_window = new_status_window(native, &before_attach);

        let mut input = egui::RawInput::default();
        input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .events
            .push(egui::ViewportEvent::Close);
        let hidden = ctx.run_logic(&input, |ctx| app.background_frame(ctx));
        let hide_commands = &hidden.viewport_commands[&egui::ViewportId::ROOT];
        assert!(hide_commands.contains(&egui::ViewportCommand::CancelClose));
        assert!(hide_commands.contains(&egui::ViewportCommand::Visible(false)));
        assert!(!hide_commands.contains(&egui::ViewportCommand::Close));
        assert!(app.window_hidden);
        assert!(status_window.isVisible());

        app.actions.push(Action::ShowWindow);
        let shown = ctx.run_logic(&egui::RawInput::default(), |ctx| app.background_frame(ctx));
        let show_commands = &shown.viewport_commands[&egui::ViewportId::ROOT];
        assert!(show_commands.contains(&egui::ViewportCommand::Visible(true)));
        assert!(show_commands.contains(&egui::ViewportCommand::Minimized(false)));
        assert!(show_commands.contains(&egui::ViewportCommand::Focus));
        assert!(!app.window_hidden);
        assert!(status_window.isVisible());
        app.shutdown();
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {}
