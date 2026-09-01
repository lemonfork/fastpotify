//! The Settings page.

use egui::{Align, CornerRadius, Frame, Layout, Margin, Stroke, Vec2};

use crate::app::App;
use crate::model::{Action, Dialog};
use crate::settings::ThemeChoice;
use crate::theme::{self, Icon, Palette};

use super::widgets;

const PLAYBACK_DIRTY_ID: &str = "playback-settings-dirty";

fn section(
    ui: &mut egui::Ui,
    palette: &Palette,
    title: &str,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    ui.add_space(10.0);
    theme::text(ui, title, theme::bold(18.0), palette.text);
    ui.add_space(8.0);
    Frame::new()
        .fill(
            palette
                .surface
                .gamma_multiply(if palette.dark { 0.7 } else { 1.0 }),
        )
        .stroke(Stroke::new(1.0, palette.outline))
        .corner_radius(CornerRadius::same(theme::RADIUS + 2))
        .inner_margin(Margin::symmetric(20, 16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width().min(760.0));
            add_contents(ui);
        });
    ui.add_space(8.0);
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    ui.add_space(8.0);
    theme::text(ui, "Settings", theme::bold(28.0), palette.text);
    ui.add_space(4.0);
    let dirty_id = egui::Id::new(PLAYBACK_DIRTY_ID);
    let mut playback_dirty = ui
        .data(|data| data.get_temp::<bool>(dirty_id))
        .unwrap_or(false);
    let mut changed = false;

    section(ui, &palette, "Server account", |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            widgets::cover(ui, &palette, None, 56.0, 28.0, Icon::User);
            ui.vertical(|ui| {
                let name = app
                    .user
                    .as_ref()
                    .map(|user| user.name().to_string())
                    .unwrap_or_else(|| "OpenSubsonic user".to_string());
                theme::text(ui, name, theme::semibold(16.0), palette.text);
                theme::text(
                    ui,
                    "Signed in to your music server",
                    theme::regular(13.0),
                    palette.secondary,
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::pill_button(ui, &palette, "Sign out", false).clicked() {
                    app.actions.push(Action::SignOut);
                }
            });
        });
    });

    section(ui, &palette, "Playback", |ui| {
        widgets::setting_row(
            ui,
            &palette,
            "Maximum streaming bitrate",
            "Your server may transcode above-limit songs. Lower values use less network data.",
            |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for (kbps, label) in [
                        (320u16, "Very high · 320 kbps"),
                        (160, "High · 160 kbps"),
                        (96, "Normal · 96 kbps"),
                    ] {
                        if theme::soft_button(
                            ui,
                            &palette,
                            None,
                            label,
                            app.settings.bitrate == kbps,
                        )
                        .clicked()
                            && app.settings.bitrate != kbps
                        {
                            app.settings.bitrate = kbps;
                            changed = true;
                            playback_dirty = true;
                        }
                    }
                });
            },
        );
        let mut output = app.settings.audio_device.clone().unwrap_or_default();
        widgets::setting_row(
            ui,
            &palette,
            "Audio output",
            "Leave empty to follow the system default output device.",
            |ui| {
                let response = Frame::new()
                    .fill(palette.surface)
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut output)
                                .hint_text(egui::RichText::new("System default").color(palette.dim))
                                .font(theme::regular(13.0))
                                .frame(egui::Frame::NONE)
                                .desired_width(220.0),
                        )
                    })
                    .inner;
                if response.changed() {
                    let output = output.trim().to_string();
                    app.settings.audio_device = (!output.is_empty()).then_some(output);
                    changed = true;
                    playback_dirty = true;
                }
            },
        );
        widgets::setting_row(
            ui,
            &palette,
            "Keep music playing when the window closes",
            super::keys::platform_shortcut(
                "Fastpotify hides to the system tray. Quit from the tray menu or with Ctrl+Q.",
                "Fastpotify hides to the system tray. Quit from the tray menu or with Cmd+Q.",
            ),
            |ui| {
                if widgets::switch(ui, &palette, &mut app.settings.keep_playing_in_background)
                    .changed()
                {
                    changed = true;
                }
            },
        );
        widgets::setting_row(
            ui,
            &palette,
            "Check for updates",
            "Checks GitHub once a day. No personal data is sent.",
            |ui| {
                if widgets::switch(ui, &palette, &mut app.settings.check_for_updates).changed() {
                    changed = true;
                }
            },
        );
        widgets::setting_row(
            ui,
            &palette,
            "Output buffer",
            "More buffering can prevent clicks on busy computers. Less buffering makes controls respond sooner.",
            |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let current = app.settings.audio_buffer_ms;
                    for ms in [50u32, 100, 200] {
                        let label = format!("{ms} ms");
                        if theme::soft_button(ui, &palette, None, &label, current == ms).clicked()
                            && current != ms
                        {
                            app.settings.audio_buffer_ms = ms;
                            changed = true;
                            playback_dirty = true;
                        }
                    }
                });
            },
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if playback_dirty {
                if theme::pill_button(ui, &palette, "Apply and restart playback", true).clicked() {
                    app.actions.push(Action::RestartEngine);
                    playback_dirty = false;
                }
                theme::subtle(
                    ui,
                    &palette,
                    "Restart local playback to apply these settings.",
                );
            } else {
                theme::subtle(ui, &palette, "Playback settings applied.");
            }
        });
    });

    section(ui, &palette, "Appearance", |ui| {
        widgets::setting_row(ui, &palette, "Theme", "", |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for choice in ThemeChoice::ALL {
                    if theme::soft_button(
                        ui,
                        &palette,
                        None,
                        choice.label(),
                        app.settings.theme == choice,
                    )
                    .clicked()
                        && app.settings.theme != choice
                    {
                        app.settings.theme = choice;
                        changed = true;
                    }
                }
            });
        });
        widgets::setting_row(
            ui,
            &palette,
            "Colour from album art",
            "Use the current cover's colour on pages and the player bar.",
            |ui| {
                if widgets::switch(ui, &palette, &mut app.settings.accent_from_art).changed() {
                    changed = true;
                }
            },
        );
        widgets::setting_row(
            ui,
            &palette,
            "Compact library sidebar",
            "Show names without covers in the sidebar.",
            |ui| {
                if widgets::switch(ui, &palette, &mut app.settings.sidebar_compact).changed() {
                    changed = true;
                }
            },
        );
        widgets::setting_row(
            ui,
            &palette,
            "Compact track list",
            "Show each track on one line without a cover.",
            |ui| {
                if widgets::switch(ui, &palette, &mut app.settings.tracklist_compact).changed() {
                    changed = true;
                }
            },
        );
        widgets::setting_row(
            ui,
            &palette,
            "Interface zoom",
            super::keys::platform_shortcut(
                "Ctrl+Plus and Ctrl+Minus work anywhere; Ctrl+0 resets.",
                "Cmd+Plus and Cmd+Minus work anywhere; Cmd+0 resets.",
            ),
            |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let mut zoom = app.settings.zoom;
                    if theme::soft_button(ui, &palette, None, "-", false).clicked() {
                        zoom = (zoom - 0.1).max(0.5);
                    }
                    theme::text(
                        ui,
                        format!("{:.0}%", zoom * 100.0),
                        theme::medium(13.5),
                        palette.text,
                    );
                    if theme::soft_button(ui, &palette, None, "+", false).clicked() {
                        zoom = (zoom + 0.1).min(2.5);
                    }
                    if (zoom - app.settings.zoom).abs() > 0.001 {
                        app.settings.zoom = zoom;
                        ui.ctx().set_zoom_factor(zoom);
                        app.mark_settings_dirty();
                    }
                });
            },
        );
    });

    section(ui, &palette, "Winamp skins", |ui| {
        widgets::setting_row(
            ui,
            &palette,
            "Mini player",
            super::keys::platform_shortcut(
                "Use classic Winamp .wsz skins. Press Ctrl+M or click the skin logo to return. Drop a skin on either window to add it.",
                "Use classic Winamp .wsz skins. Press Cmd+Shift+M or click the skin logo to return. Drop a skin on either window to add it.",
            ),
            |ui| {
                if theme::pill_button(ui, &palette, "Switch to it", true).clicked() {
                    app.actions.push(Action::ToggleWinampWindow);
                }
            },
        );
        let folder = app.dirs.skins_dir();
        app.winamp.refresh_choices(&folder);
        widgets::setting_row(
            ui,
            &palette,
            "Skin",
            &format!(
                "Installed skins are in {}. Find more at the Winamp Skin Museum.",
                folder.display()
            ),
            |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if theme::soft_button(ui, &palette, Some(Icon::Globe), "Skin Museum", false)
                        .clicked()
                    {
                        app.actions
                            .push(Action::OpenUrl("https://skins.webamp.org/".into()));
                    }
                    if theme::soft_button(
                        ui,
                        &palette,
                        Some(Icon::ExternalLink),
                        "Open folder",
                        false,
                    )
                    .clicked()
                    {
                        app.actions.push(Action::OpenSkinsFolder);
                    }
                });
            },
        );
        let choices = app.winamp.choices.clone();
        let mut options: Vec<(usize, &str)> = vec![(0, "Fastpotify")];
        options.extend(
            choices
                .iter()
                .enumerate()
                .map(|(index, choice)| (index + 1, choice.label())),
        );
        let current = app
            .settings
            .skin
            .as_deref()
            .and_then(|name| choices.iter().position(|choice| choice.name == name))
            .map_or(0, |index| index + 1);
        if let Some(picked) = widgets::chips(ui, &palette, &options, current)
            && picked != current
        {
            let name = picked
                .checked_sub(1)
                .map(|index| choices[index].name.clone());
            app.actions.push(Action::SetSkin(name));
        }
        ui.add_space(4.0);
        widgets::setting_row(
            ui,
            &palette,
            "Size",
            "Whole-number scaling keeps skin pixels sharp.",
            |ui| {
                let scale =
                    crate::winamp::WinampState::scale(&app.settings, ui.ctx().pixels_per_point());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for candidate in 1..=crate::winamp::MAX_SCALE {
                        let label = format!("{candidate}x");
                        if theme::soft_button(ui, &palette, None, &label, candidate == scale)
                            .clicked()
                            && candidate != scale
                        {
                            app.actions.push(Action::SetSkinScale(candidate as u8));
                        }
                    }
                });
            },
        );
        widgets::setting_row(
            ui,
            &palette,
            "Always on top",
            "Keep the Winamp window above everything else.",
            |ui| {
                let mut on_top = app.settings.winamp_on_top;
                if widgets::switch(ui, &palette, &mut on_top).changed() {
                    app.actions.push(Action::ToggleWinampOnTop);
                }
            },
        );
    });

    section(ui, &palette, "Equalizer", |ui| {
        widgets::setting_row(
            ui,
            &palette,
            "Equalizer",
            "A ten-band equalizer for playback on this computer.",
            |ui| {
                let mut on = app.settings.eq_on;
                if widgets::switch(ui, &palette, &mut on).changed() {
                    app.actions.push(Action::ToggleEq);
                }
            },
        );
        let names: Vec<(usize, &str)> = crate::eq::PRESETS
            .iter()
            .enumerate()
            .map(|(index, preset)| (index, preset.name))
            .collect();
        let current = crate::eq::PRESETS
            .iter()
            .position(|preset| preset.bands_db == app.settings.eq_bands_db)
            .unwrap_or(usize::MAX);
        if let Some(picked) = widgets::chips(ui, &palette, &names, current) {
            app.actions.push(Action::ApplyEqPreset(picked));
        }
        ui.add_space(10.0);
        eq_curve(ui, &palette, &crate::app::eq_settings(&app.settings));
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            let on = app.settings.eq_on;
            let mut preamp = app.settings.eq_preamp_db;
            if eq_slider(ui, &palette, "Pre", &mut preamp, on) {
                app.actions.push(Action::SetEqPreamp(preamp));
            }
            for (band, hz) in crate::eq::BANDS.iter().enumerate() {
                let mut gain = app.settings.eq_bands_db[band];
                if eq_slider(ui, &palette, &hertz(*hz), &mut gain, on) {
                    app.actions.push(Action::SetEqBand(band, gain));
                }
            }
        });
    });

    section(ui, &palette, "Storage", |ui| {
        widgets::setting_row(
            ui,
            &palette,
            "Artwork cache",
            &format!("Stored in {}", app.dirs.art_cache_dir().display()),
            |ui| {
                if theme::soft_button(ui, &palette, Some(Icon::Trash), "Clear artwork", false)
                    .clicked()
                {
                    app.actions.push(Action::ClearArtCache);
                }
            },
        );
        widgets::setting_row(
            ui,
            &palette,
            "Play history",
            "Tracks played here are stored per server on this device. Clearing them does not delete the server's play history.",
            |ui| {
                if theme::soft_button(ui, &palette, Some(Icon::Trash), "Clear history", false)
                    .clicked()
                {
                    app.actions.push(Action::ClearPlayHistory);
                }
            },
        );
        widgets::setting_row(
            ui,
            &palette,
            "Sign-in",
            &format!(
                "Credentials are kept in {}",
                app.dirs.credentials_file().display()
            ),
            |_| {},
        );
    });

    section(ui, &palette, "About", |ui| {
        ui.horizontal(|ui| {
            let (logo, _) = ui.allocate_exact_size(Vec2::splat(40.0), egui::Sense::hover());
            theme::logo(ui, logo.center(), 40.0, palette.accent, palette.on_accent);
            ui.vertical(|ui| {
                theme::text(
                    ui,
                    format!("Fastpotify {}", env!("CARGO_PKG_VERSION")),
                    theme::semibold(15.0),
                    palette.text,
                );
                theme::text(
                    ui,
                    "Built with Rust and egui for OpenSubsonic music servers.",
                    theme::regular(13.0),
                    palette.secondary,
                );
            });
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if theme::soft_button(ui, &palette, Some(Icon::Info), "Keyboard shortcuts", false)
                .clicked()
            {
                app.actions.push(Action::ShowDialog(Dialog::Shortcuts));
            }
            if theme::soft_button(ui, &palette, Some(Icon::ExternalLink), "Source code", false)
                .clicked()
            {
                app.actions
                    .push(Action::OpenUrl(env!("CARGO_PKG_REPOSITORY").into()));
            }
        });
    });

    ui.data_mut(|data| data.insert_temp(dirty_id, playback_dirty));
    if changed {
        app.actions.push(Action::SettingsChanged);
    }
}

/// A band's frequency the short way: 60, 170, 1K, 16K.
fn hertz(hz: f32) -> String {
    if hz >= 1000.0 {
        format!("{}K", (hz / 1000.0).round() as u32)
    } else {
        format!("{}", hz.round() as u32)
    }
}

/// One vertical slider in the app's own style: the track filled from
/// 0 dB, the handle in the middle when flat, a double-click to put it
/// back there. Returns whether it moved.
fn eq_slider(ui: &mut egui::Ui, palette: &Palette, label: &str, value: &mut f32, on: bool) -> bool {
    use egui::{Rect, Stroke, pos2, vec2};
    let range = crate::eq::RANGE_DB;
    ui.vertical(|ui| {
        let (rect, response) =
            ui.allocate_exact_size(vec2(30.0, 118.0), egui::Sense::click_and_drag());
        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
        let track = Rect::from_center_size(rect.center(), vec2(4.0, rect.height() - 20.0));
        let y_of = |db: f32| track.bottom() - (db + range) / (2.0 * range) * track.height();
        let mut changed = false;
        if response.double_clicked() {
            *value = 0.0;
            changed = true;
        } else if (response.dragged() || response.clicked())
            && let Some(pos) = response.interact_pointer_pos()
        {
            let db = (track.bottom() - pos.y) / track.height() * 2.0 * range - range;
            let db = (db.clamp(-range, range) * 10.0).round() / 10.0;
            if db != *value {
                *value = db;
                changed = true;
            }
        }
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            painter.rect_filled(track, 2.0, palette.surface_active);
            let fill = if on { palette.accent } else { palette.dim };
            let (top, bottom) = (y_of(value.max(0.0)), y_of(value.min(0.0)));
            painter.rect_filled(
                Rect::from_min_max(pos2(track.left(), top), pos2(track.right(), bottom)),
                2.0,
                fill,
            );
            painter.hline(
                (track.left() - 3.0)..=(track.right() + 3.0),
                y_of(0.0),
                Stroke::new(1.0, palette.dim),
            );
            let handle = pos2(track.center().x, y_of(*value));
            painter.circle_filled(handle, 7.0, palette.text);
            if response.hovered() || response.dragged() {
                painter.text(
                    pos2(track.center().x, rect.top() + 2.0),
                    egui::Align2::CENTER_TOP,
                    format!("{value:+.1}"),
                    theme::regular(11.0),
                    palette.secondary,
                );
            }
        }
        theme::text(ui, label, theme::regular(11.5), palette.secondary);
        changed
    })
    .inner
}

/// The equalizer's response over the audible range, the bands marked on
/// it: the shape says what a row of numbers cannot.
fn eq_curve(ui: &mut egui::Ui, palette: &Palette, settings: &crate::eq::EqSettings) {
    use egui::{Shape, Stroke, pos2, vec2};
    let width = ui.available_width().min(720.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 120.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, theme::RADIUS as f32, palette.surface);
    let plot = rect.shrink2(vec2(10.0, 12.0));
    let (low, high) = (20f32.log10(), 20_000f32.log10());
    let x_of = |hz: f32| plot.left() + (hz.log10() - low) / (high - low) * plot.width();
    let y_of = |db: f32| {
        plot.center().y
            - db.clamp(-crate::eq::RANGE_DB, crate::eq::RANGE_DB) / crate::eq::RANGE_DB
                * plot.height()
                / 2.0
    };
    for db in [-12.0, -6.0, 0.0, 6.0, 12.0] {
        let color = if db == 0.0 {
            palette.dim
        } else {
            palette.outline
        };
        painter.hline(plot.x_range(), y_of(db), Stroke::new(1.0, color));
    }
    for hz in crate::eq::BANDS {
        painter.vline(x_of(hz), plot.y_range(), Stroke::new(1.0, palette.outline));
    }
    let curve = settings.curve();
    let points: Vec<egui::Pos2> = (0..=240)
        .map(|step| {
            let t = step as f32 / 240.0;
            let hz = 10f32.powf(low + t * (high - low));
            pos2(plot.left() + t * plot.width(), y_of(curve.db_at(hz)))
        })
        .collect();
    let color = if settings.on {
        palette.accent
    } else {
        palette.dim
    };
    painter.add(Shape::line(points, Stroke::new(2.0, color)));
    for (hz, db) in crate::eq::BANDS.iter().zip(settings.bands_db) {
        painter.circle_filled(pos2(x_of(*hz), y_of(db + settings.preamp_db)), 3.0, color);
    }
}
