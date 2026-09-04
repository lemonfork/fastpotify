//! Linux status-notifier host. The common tray state machine owns its lifetime;
//! ksni serves the item on its own thread while the visible window changes.

use ksni::blocking::TrayMethods;

use super::{ActionSender, Host, TrayAction};

struct FastTray {
    actions: ActionSender,
    playing: bool,
}

impl ksni::Tray for FastTray {
    fn id(&self) -> String {
        "fastpotify".into()
    }

    fn title(&self) -> String {
        "Fastpotify".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let size = 64usize;
        let rgba = crate::util::app_icon_rgba(size);
        // ksni wants ARGB32 in network byte order.
        let mut data = Vec::with_capacity(rgba.len());
        let (pixels, _) = rgba.as_chunks::<4>();
        for [r, g, b, a] in pixels {
            data.extend_from_slice(&[*a, *r, *g, *b]);
        }
        vec![ksni::Icon {
            width: size as i32,
            height: size as i32,
            data,
        }]
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.actions.send(TrayAction::ToggleWindowVisibility);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        vec![
            StandardItem {
                label: "Show Main Window".into(),
                activate: Box::new(|tray: &mut Self| tray.actions.send(TrayAction::ShowMainWindow)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Open Mini Player".into(),
                activate: Box::new(|tray: &mut Self| tray.actions.send(TrayAction::ShowMiniPlayer)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Previous".into(),
                activate: Box::new(|tray: &mut Self| tray.actions.send(TrayAction::Previous)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: if self.playing {
                    "Pause".into()
                } else {
                    "Play".into()
                },
                activate: Box::new(|tray: &mut Self| tray.actions.send(TrayAction::PlayPause)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Next".into(),
                activate: Box::new(|tray: &mut Self| tray.actions.send(TrayAction::Next)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|tray: &mut Self| tray.actions.send(TrayAction::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

pub(super) struct Platform {
    handle: ksni::blocking::Handle<FastTray>,
}

impl Host for Platform {
    fn start(actions: ActionSender, playing: bool) -> Result<Self, String> {
        FastTray { actions, playing }
            .spawn()
            .map(|handle| Self { handle })
            .map_err(|error| error.to_string())
    }

    fn set_playing(&mut self, playing: bool) -> Result<(), String> {
        self.handle
            .update(|tray| tray.playing = playing)
            .ok_or_else(|| "status-notifier service has stopped".into())
    }

    fn is_alive(&self) -> bool {
        !self.handle.is_closed()
    }

    fn stop(&mut self) {
        // Queue shutdown without waiting for D-Bus work on the UI thread.
        self.handle.shutdown();
    }
}

impl Drop for Platform {
    fn drop(&mut self) {
        self.stop();
    }
}
