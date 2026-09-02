//! The Settings page.

use egui::containers::scroll_area::ScrollBarVisibility;
use egui::{Align, Color32, CornerRadius, Frame, Layout, Margin, Sense, Stroke, Vec2, vec2};

use crate::app::App;
use crate::model::{Action, Dialog};
use crate::settings::ThemeChoice;
use crate::theme::{self, Icon, Palette};

use super::widgets;

const PLAYBACK_DIRTY_ID: &str = "playback-settings-dirty";
const SELECTED_SECTION_ID: &str = "settings-selected-section";
const WIDE_LAYOUT_BREAKPOINT: f32 = 760.0;
const ROW_LAYOUT_BREAKPOINT: f32 = 560.0;
const CATEGORY_RAIL_WIDTH: f32 = 220.0;
const CATEGORY_GUTTER: f32 = 44.0;
const CONTENT_MAX_WIDTH: f32 = 860.0;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum SettingsSection {
    Account,
    #[default]
    Playback,
    Appearance,
    WinampSkins,
    Equalizer,
    Storage,
    About,
}

impl SettingsSection {
    const ALL: [Self; 7] = [
        Self::Account,
        Self::Playback,
        Self::Appearance,
        Self::WinampSkins,
        Self::Equalizer,
        Self::Storage,
        Self::About,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Account => "Account",
            Self::Playback => "Playback",
            Self::Appearance => "Appearance",
            Self::WinampSkins => "Winamp skins",
            Self::Equalizer => "Equalizer",
            Self::Storage => "Storage",
            Self::About => "About",
        }
    }

    const fn icon(self) -> Icon {
        match self {
            Self::Account => Icon::User,
            Self::Playback => Icon::CirclePlay,
            Self::Appearance => Icon::Sparkles,
            Self::WinampSkins => Icon::Zap,
            Self::Equalizer => Icon::AudioLines,
            Self::Storage => Icon::Disc,
            Self::About => Icon::Info,
        }
    }
}

fn horizontal_rule(ui: &mut egui::Ui, palette: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(1.0, palette.outline),
    );
}

fn wrapped_text(
    ui: &mut egui::Ui,
    text: impl Into<String>,
    font: egui::FontId,
    color: Color32,
) -> egui::Response {
    let text = text.into();
    let galley = crate::bidi::layout(
        ui.painter(),
        &text,
        font,
        color,
        ui.available_width(),
        usize::MAX,
        None,
    );
    ui.add(egui::Label::new(galley).selectable(false))
}

fn rail_item(
    ui: &mut egui::Ui,
    palette: &Palette,
    section: SettingsSection,
    selected: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 51.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let fill = if selected {
            palette.surface
        } else if response.hovered() || response.has_focus() {
            palette.surface_hover
        } else {
            Color32::TRANSPARENT
        };
        ui.painter()
            .rect_filled(rect, CornerRadius::same(theme::RADIUS), fill);
        if selected {
            let indicator = egui::Rect::from_min_max(
                egui::pos2(rect.left(), rect.top() + 2.0),
                egui::pos2(rect.left() + 3.0, rect.bottom() - 2.0),
            );
            ui.painter().rect_filled(indicator, 2.0, palette.accent);
        }
        let icon_color = if selected {
            palette.accent
        } else {
            palette.secondary
        };
        let icon_rect = egui::Rect::from_center_size(
            egui::pos2(rect.left() + 25.0, rect.center().y),
            Vec2::splat(20.0),
        );
        section
            .icon()
            .image(icon_color, 20.0)
            .paint_at(ui, icon_rect);
        let text_color = if selected {
            palette.text
        } else {
            palette.secondary
        };
        let galley = ui.painter().layout_no_wrap(
            section.label().to_owned(),
            theme::medium(15.0),
            text_color,
        );
        ui.painter().galley(
            egui::pos2(rect.left() + 51.0, rect.center().y - galley.size().y / 2.0),
            galley,
            text_color,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn choice_button(
    ui: &mut egui::Ui,
    palette: &Palette,
    label: &str,
    selected: bool,
) -> egui::Response {
    let text_color = if selected {
        Color32::WHITE
    } else {
        palette.text
    };
    let galley = ui.painter().layout_no_wrap(
        crate::bidi::display_text(label).into_owned(),
        theme::medium(12.5),
        text_color,
    );
    let (rect, response) =
        ui.allocate_exact_size(vec2(galley.size().x + 14.0, 32.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let fill = if selected {
            if response.hovered() {
                palette.accent_hover
            } else {
                palette.accent
            }
        } else if response.hovered() || response.has_focus() {
            palette.surface_hover
        } else {
            palette.surface
        };
        ui.painter().rect_filled(rect, CornerRadius::same(7), fill);
        if !selected {
            ui.painter().rect_stroke(
                rect,
                CornerRadius::same(7),
                Stroke::new(1.0, palette.outline),
                egui::StrokeKind::Inside,
            );
        }
        ui.painter()
            .galley(rect.center() - galley.size() / 2.0, galley, text_color);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn choice_group<T, L>(
    ui: &mut egui::Ui,
    palette: &Palette,
    options: &[(T, L)],
    current: T,
) -> Option<T>
where
    T: Copy + PartialEq,
    L: AsRef<str>,
{
    let gap = 2.0;
    let buttons_width = options
        .iter()
        .map(|(_, label)| {
            ui.painter()
                .layout_no_wrap(
                    crate::bidi::display_text(label.as_ref()).into_owned(),
                    theme::medium(12.5),
                    Color32::WHITE,
                )
                .size()
                .x
                + 14.0
        })
        .sum::<f32>()
        + gap * options.len().saturating_sub(1) as f32;
    let width = (buttons_width.ceil() + 2.0)
        .min(ui.available_width())
        .max(0.0);
    let leading_space = (width - buttons_width - 0.5).max(0.0);
    let mut picked = None;
    ltr_control_group(ui, width, true, |ui| {
        ui.spacing_mut().item_spacing = vec2(gap, gap);
        ui.add_space(leading_space);
        for (value, label) in options {
            if choice_button(ui, palette, label.as_ref(), *value == current).clicked() {
                picked = Some(*value);
            }
        }
    });
    picked
}

fn ltr_control_group<R>(
    ui: &mut egui::Ui,
    width: f32,
    wrap: bool,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.allocate_ui_with_layout(
        vec2(width.min(ui.available_width()).max(0.0), 0.0),
        Layout::left_to_right(Align::Center).with_main_wrap(wrap),
        add_contents,
    )
    .inner
}

fn soft_button_width(ui: &egui::Ui, label: &str, has_icon: bool) -> f32 {
    let text = ui.painter().layout_no_wrap(
        crate::bidi::display_text(label).into_owned(),
        theme::medium(13.0),
        Color32::WHITE,
    );
    text.size().x + if has_icon { 21.0 } else { 0.0 } + 24.0
}

fn preference_row(
    ui: &mut egui::Ui,
    palette: &Palette,
    label: &str,
    description: &str,
    control: impl FnOnce(&mut egui::Ui),
) {
    ui.add_space(9.0);
    let wide = ui.available_width() >= ROW_LAYOUT_BREAKPOINT;
    if wide {
        let available = ui.available_width();
        let label_width = (available * 0.34).clamp(190.0, 320.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.allocate_ui_with_layout(
                vec2(label_width, 0.0),
                Layout::top_down(Align::Min),
                |ui| {
                    wrapped_text(ui, label, theme::medium(14.5), palette.text);
                    if !description.is_empty() {
                        ui.add_space(4.0);
                        wrapped_text(ui, description, theme::regular(12.5), palette.secondary);
                    }
                },
            );
            ui.add_space(16.0);
            let control_width = ui.available_width();
            ui.allocate_ui_with_layout(
                vec2(control_width, 0.0),
                Layout::right_to_left(Align::Center),
                control,
            );
        });
    } else {
        wrapped_text(ui, label, theme::medium(14.5), palette.text);
        if !description.is_empty() {
            ui.add_space(4.0);
            wrapped_text(ui, description, theme::regular(12.5), palette.secondary);
        }
        ui.add_space(9.0);
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), 0.0),
            Layout::left_to_right(Align::Center),
            control,
        );
    }
    ui.add_space(9.0);
    horizontal_rule(ui, palette);
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let dirty_id = egui::Id::new(PLAYBACK_DIRTY_ID);
    let selected_id = egui::Id::new(SELECTED_SECTION_ID);
    let mut playback_dirty = ui
        .data(|data| data.get_temp::<bool>(dirty_id))
        .unwrap_or(false);
    let mut selected = ui
        .data(|data| data.get_temp::<SettingsSection>(selected_id))
        .unwrap_or_default();
    let mut changed = false;

    ui.add_space(8.0);
    if ui.available_width() >= WIDE_LAYOUT_BREAKPOINT {
        let available_height = ui.available_height();
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.allocate_ui_with_layout(
                vec2(CATEGORY_RAIL_WIDTH, available_height),
                Layout::top_down(Align::Min),
                |ui| {
                    theme::text(ui, "Settings", theme::bold(28.0), palette.text);
                    ui.add_space(20.0);
                    egui::ScrollArea::vertical()
                        .id_salt("settings-category-rail")
                        .scroll_bar_visibility(ScrollBarVisibility::AlwaysHidden)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.spacing_mut().item_spacing.y = 3.0;
                            for section in SettingsSection::ALL {
                                if rail_item(ui, &palette, section, selected == section).clicked() {
                                    selected = section;
                                }
                            }
                        });
                },
            );
            ui.add_space(CATEGORY_GUTTER);
            let width = ui.available_width().min(CONTENT_MAX_WIDTH);
            ui.allocate_ui_with_layout(
                vec2(width, available_height),
                Layout::top_down(Align::Min),
                |ui| {
                    ui.set_width(width);
                    show_scrolling_section(
                        app,
                        ui,
                        &palette,
                        selected,
                        &mut playback_dirty,
                        &mut changed,
                    );
                },
            );
        });
    } else {
        theme::text(ui, "Settings", theme::bold(28.0), palette.text);
        ui.add_space(14.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            for section in SettingsSection::ALL {
                if choice_button(ui, &palette, section.label(), selected == section).clicked() {
                    selected = section;
                }
            }
        });
        ui.add_space(18.0);
        show_scrolling_section(
            app,
            ui,
            &palette,
            selected,
            &mut playback_dirty,
            &mut changed,
        );
    }

    ui.data_mut(|data| {
        data.insert_temp(selected_id, selected);
        data.insert_temp(dirty_id, playback_dirty);
    });
    if changed {
        app.actions.push(Action::SettingsChanged);
    }
}

fn show_scrolling_section(
    app: &mut App,
    ui: &mut egui::Ui,
    palette: &Palette,
    selected: SettingsSection,
    playback_dirty: &mut bool,
    changed: &mut bool,
) {
    egui::ScrollArea::vertical()
        .id_salt(("settings-section", selected.label()))
        .scroll_bar_visibility(ScrollBarVisibility::AlwaysHidden)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.push_id(selected.label(), |ui| {
                theme::text(ui, selected.label(), theme::bold(26.0), palette.text);
                ui.add_space(10.0);
                horizontal_rule(ui, palette);
                match selected {
                    SettingsSection::Account => account(app, ui, palette),
                    SettingsSection::Playback => {
                        playback(app, ui, palette, playback_dirty, changed);
                    }
                    SettingsSection::Appearance => appearance(app, ui, palette, changed),
                    SettingsSection::WinampSkins => winamp_skins(app, ui, palette),
                    SettingsSection::Equalizer => equalizer(app, ui, palette),
                    SettingsSection::Storage => storage(app, ui, palette),
                    SettingsSection::About => about(app, ui, palette),
                }
            });
            ui.add_space(12.0);
        });
}

fn account(app: &mut App, ui: &mut egui::Ui, palette: &Palette) {
    let name = app
        .user
        .as_ref()
        .map(|user| user.name().to_string())
        .unwrap_or_else(|| "OpenSubsonic user".to_string());
    preference_row(ui, palette, &name, "Signed in to your music server", |ui| {
        if theme::pill_button(ui, palette, "Sign out", false).clicked() {
            app.actions.push(Action::SignOut);
        }
    });
}

fn playback(
    app: &mut App,
    ui: &mut egui::Ui,
    palette: &Palette,
    playback_dirty: &mut bool,
    changed: &mut bool,
) {
    preference_row(
        ui,
        palette,
        "Maximum streaming bitrate",
        "Your server may transcode above-limit songs. Lower values use less network data.",
        |ui| {
            let options = [
                (96u16, "Normal · 96 kbps"),
                (160, "High · 160 kbps"),
                (320, "Very high · 320 kbps"),
            ];
            if let Some(kbps) = choice_group(ui, palette, &options, app.settings.bitrate)
                && app.settings.bitrate != kbps
            {
                app.settings.bitrate = kbps;
                *changed = true;
                *playback_dirty = true;
            }
        },
    );
    let mut output = app.settings.audio_device.clone().unwrap_or_default();
    preference_row(
        ui,
        palette,
        "Audio output",
        "Leave empty to follow the system default output device.",
        |ui| {
            let width = ui.available_width().min(220.0);
            let response = Frame::new()
                .fill(palette.surface)
                .stroke(Stroke::new(1.0, palette.outline))
                .corner_radius(CornerRadius::same(7))
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut output)
                            .hint_text(egui::RichText::new("System default").color(palette.dim))
                            .font(theme::regular(13.0))
                            .frame(egui::Frame::NONE)
                            .desired_width(width),
                    )
                })
                .inner;
            if response.changed() {
                let output = output.trim().to_string();
                app.settings.audio_device = (!output.is_empty()).then_some(output);
                *changed = true;
                *playback_dirty = true;
            }
        },
    );
    preference_row(
        ui,
        palette,
        "Keep music playing when the window closes",
        super::keys::platform_shortcut(
            "Fastpotify hides to the system tray. Quit from the tray menu or with Ctrl+Q.",
            "Fastpotify hides to the system tray. Quit from the tray menu or with Cmd+Q.",
        ),
        |ui| {
            if widgets::switch(ui, palette, &mut app.settings.keep_playing_in_background).changed()
            {
                *changed = true;
            }
        },
    );
    preference_row(
        ui,
        palette,
        "Check for updates",
        "Checks GitHub once a day. No personal data is sent.",
        |ui| {
            if widgets::switch(ui, palette, &mut app.settings.check_for_updates).changed() {
                *changed = true;
            }
        },
    );
    preference_row(
        ui,
        palette,
        "Output buffer",
        "More buffering can prevent clicks on busy computers. Less buffering makes controls respond sooner.",
        |ui| {
            let options = [(200u32, "200 ms"), (100, "100 ms"), (50, "50 ms")];
            let current = app.settings.audio_buffer_ms;
            if let Some(ms) = choice_group(ui, palette, &options, current)
                && current != ms
            {
                app.settings.audio_buffer_ms = ms;
                *changed = true;
                *playback_dirty = true;
            }
        },
    );
    ui.add_space(8.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(12.0, 8.0);
        if *playback_dirty {
            if theme::pill_button(ui, palette, "Apply and restart playback", true).clicked() {
                app.actions.push(Action::RestartEngine);
                *playback_dirty = false;
            }
            wrapped_text(
                ui,
                "Restart local playback to apply these settings.",
                theme::regular(13.0),
                palette.secondary,
            );
        } else {
            theme::subtle(ui, palette, "Playback settings applied.");
        }
    });
}

fn appearance(app: &mut App, ui: &mut egui::Ui, palette: &Palette, changed: &mut bool) {
    preference_row(ui, palette, "Theme", "", |ui| {
        let options = ThemeChoice::ALL.map(|choice| (choice, choice.label()));
        if let Some(choice) = choice_group(ui, palette, &options, app.settings.theme)
            && app.settings.theme != choice
        {
            app.settings.theme = choice;
            *changed = true;
        }
    });
    preference_row(
        ui,
        palette,
        "Colour from album art",
        "Use the current cover's colour on pages and the player bar.",
        |ui| {
            if widgets::switch(ui, palette, &mut app.settings.accent_from_art).changed() {
                *changed = true;
            }
        },
    );
    preference_row(
        ui,
        palette,
        "Compact library sidebar",
        "Show names without covers in the sidebar.",
        |ui| {
            if widgets::switch(ui, palette, &mut app.settings.sidebar_compact).changed() {
                *changed = true;
            }
        },
    );
    preference_row(
        ui,
        palette,
        "Compact track list",
        "Show each track on one line without a cover.",
        |ui| {
            if widgets::switch(ui, palette, &mut app.settings.tracklist_compact).changed() {
                *changed = true;
            }
        },
    );
    preference_row(
        ui,
        palette,
        "Interface zoom",
        super::keys::platform_shortcut(
            "Ctrl+Plus and Ctrl+Minus work anywhere; Ctrl+0 resets.",
            "Cmd+Plus and Cmd+Minus work anywhere; Cmd+0 resets.",
        ),
        |ui| {
            ltr_control_group(ui, 112.0, false, |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let mut zoom = app.settings.zoom;
                if theme::icon_button(
                    ui,
                    Icon::Minus,
                    15.0,
                    palette.secondary,
                    palette.text,
                    "Zoom out",
                )
                .clicked()
                {
                    zoom = (zoom - 0.1).max(0.5);
                }
                theme::text(
                    ui,
                    format!("{:.0}%", zoom * 100.0),
                    theme::medium(13.5),
                    palette.text,
                );
                if theme::icon_button(
                    ui,
                    Icon::Plus,
                    15.0,
                    palette.secondary,
                    palette.text,
                    "Zoom in",
                )
                .clicked()
                {
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
}

fn winamp_skins(app: &mut App, ui: &mut egui::Ui, palette: &Palette) {
    preference_row(
        ui,
        palette,
        "Mini player",
        super::keys::platform_shortcut(
            "Use classic Winamp .wsz skins. Press Ctrl+M or click the skin logo to return. Drop a skin on either window to add it.",
            "Use classic Winamp .wsz skins. Press Cmd+Shift+M or click the skin logo to return. Drop a skin on either window to add it.",
        ),
        |ui| {
            if theme::pill_button(ui, palette, "Switch to it", true).clicked() {
                app.actions.push(Action::ToggleWinampWindow);
            }
        },
    );
    let folder = app.dirs.skins_dir();
    app.winamp.refresh_choices(&folder);
    preference_row(
        ui,
        palette,
        "Skin",
        &format!(
            "Installed skins are in {}. Find more at the Winamp Skin Museum.",
            folder.display()
        ),
        |ui| {
            let width = soft_button_width(ui, "Skin Museum", true)
                + soft_button_width(ui, "Open folder", true)
                + 6.0;
            ltr_control_group(ui, width, true, |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if theme::soft_button(ui, palette, Some(Icon::Globe), "Skin Museum", false)
                    .clicked()
                {
                    app.actions
                        .push(Action::OpenUrl("https://skins.webamp.org/".into()));
                }
                if theme::soft_button(ui, palette, Some(Icon::ExternalLink), "Open folder", false)
                    .clicked()
                {
                    app.actions.push(Action::OpenSkinsFolder);
                }
            });
        },
    );
    let choices = app.winamp.choices.clone();
    let current = app
        .settings
        .skin
        .as_deref()
        .and_then(|name| choices.iter().position(|choice| choice.name == name))
        .map_or(0, |index| index + 1);
    let mut skin_options = vec![(0usize, "Fastpotify".to_owned())];
    skin_options.extend(
        choices
            .iter()
            .enumerate()
            .map(|(index, choice)| (index + 1, choice.label().to_owned())),
    );
    preference_row(
        ui,
        palette,
        "Installed skin",
        "Choose Fastpotify's built-in skin or an installed .wsz skin.",
        |ui| {
            if let Some(picked) = choice_group(ui, palette, &skin_options, current)
                && picked != current
            {
                let name = picked
                    .checked_sub(1)
                    .map(|index| choices[index].name.clone());
                app.actions.push(Action::SetSkin(name));
            }
        },
    );
    preference_row(
        ui,
        palette,
        "Size",
        "Whole-number scaling keeps skin pixels sharp.",
        |ui| {
            let scale =
                crate::winamp::WinampState::scale(&app.settings, ui.ctx().pixels_per_point());
            let options: Vec<(u32, String)> = (1..=crate::winamp::MAX_SCALE)
                .map(|candidate| (candidate, format!("{candidate}x")))
                .collect();
            if let Some(candidate) = choice_group(ui, palette, &options, scale)
                && candidate != scale
            {
                app.actions.push(Action::SetSkinScale(candidate as u8));
            }
        },
    );
    preference_row(
        ui,
        palette,
        "Always on top",
        "Keep the Winamp window above everything else.",
        |ui| {
            let mut on_top = app.settings.winamp_on_top;
            if widgets::switch(ui, palette, &mut on_top).changed() {
                app.actions.push(Action::ToggleWinampOnTop);
            }
        },
    );
}

fn equalizer(app: &mut App, ui: &mut egui::Ui, palette: &Palette) {
    preference_row(
        ui,
        palette,
        "Equalizer",
        "A ten-band equalizer for playback on this computer.",
        |ui| {
            let mut on = app.settings.eq_on;
            if widgets::switch(ui, palette, &mut on).changed() {
                app.actions.push(Action::ToggleEq);
            }
        },
    );
    let current = crate::eq::PRESETS
        .iter()
        .position(|preset| preset.bands_db == app.settings.eq_bands_db)
        .unwrap_or(usize::MAX);
    let options: Vec<(usize, &str)> = crate::eq::PRESETS
        .iter()
        .enumerate()
        .map(|(index, preset)| (index, preset.name))
        .collect();
    preference_row(
        ui,
        palette,
        "Preset",
        "Choose a starting curve, then fine-tune any band below.",
        |ui| {
            if let Some(picked) = choice_group(ui, palette, &options, current) {
                app.actions.push(Action::ApplyEqPreset(picked));
            }
        },
    );
    ui.add_space(12.0);
    eq_curve(ui, palette, &crate::app::eq_settings(&app.settings));
    ui.add_space(12.0);
    ui.horizontal_wrapped(|ui| {
        let slider_count = crate::eq::BANDS.len() as f32 + 1.0;
        let gaps = (slider_count - 1.0).max(1.0);
        ui.spacing_mut().item_spacing.x =
            ((ui.available_width() - slider_count * 30.0) / gaps).clamp(4.0, 14.0);
        ui.spacing_mut().item_spacing.y = 12.0;
        let on = app.settings.eq_on;
        let mut preamp = app.settings.eq_preamp_db;
        if eq_slider(ui, palette, "Pre", &mut preamp, on) {
            app.actions.push(Action::SetEqPreamp(preamp));
        }
        for (band, hz) in crate::eq::BANDS.iter().enumerate() {
            let mut gain = app.settings.eq_bands_db[band];
            if eq_slider(ui, palette, &hertz(*hz), &mut gain, on) {
                app.actions.push(Action::SetEqBand(band, gain));
            }
        }
    });
}

fn storage(app: &mut App, ui: &mut egui::Ui, palette: &Palette) {
    preference_row(
        ui,
        palette,
        "Artwork cache",
        &format!("Stored in {}", app.dirs.art_cache_dir().display()),
        |ui| {
            if theme::soft_button(ui, palette, Some(Icon::Trash), "Clear artwork", false).clicked()
            {
                app.actions.push(Action::ClearArtCache);
            }
        },
    );
    preference_row(
        ui,
        palette,
        "Play history",
        "Tracks played here are stored per server on this device. Clearing them does not delete the server's play history.",
        |ui| {
            if theme::soft_button(ui, palette, Some(Icon::Trash), "Clear history", false).clicked()
            {
                app.actions.push(Action::ClearPlayHistory);
            }
        },
    );
    preference_row(
        ui,
        palette,
        "Sign-in",
        &format!(
            "Credentials are kept in {}",
            app.dirs.credentials_file().display()
        ),
        |_| {},
    );
}

fn about(app: &mut App, ui: &mut egui::Ui, palette: &Palette) {
    preference_row(
        ui,
        palette,
        &format!("Fastpotify {}", env!("CARGO_PKG_VERSION")),
        "Built with Rust and egui for OpenSubsonic music servers.",
        |ui| {
            let width = soft_button_width(ui, "Keyboard shortcuts", true)
                + soft_button_width(ui, "Source code", true)
                + 8.0;
            ltr_control_group(ui, width, true, |ui| {
                ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                if theme::soft_button(ui, palette, Some(Icon::Info), "Keyboard shortcuts", false)
                    .clicked()
                {
                    app.actions.push(Action::ShowDialog(Dialog::Shortcuts));
                }
                if theme::soft_button(ui, palette, Some(Icon::ExternalLink), "Source code", false)
                    .clicked()
                {
                    app.actions
                        .push(Action::OpenUrl(env!("CARGO_PKG_REPOSITORY").into()));
                }
            });
        },
    );
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
