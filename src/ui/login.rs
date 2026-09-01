//! Sign in to a Navidrome or compatible OpenSubsonic server.

use egui::{Align, CornerRadius, Frame, Layout, Margin, Stroke, Vec2};

use crate::app::App;
use crate::backend::AuthStatus;
use crate::model::Action;
use crate::theme;

pub fn show(app: &mut App, ui: &mut egui::Ui, signing_in: bool) {
    let palette = app.palette;
    egui::CentralPanel::default()
        .frame(Frame::new().fill(palette.window))
        .show(ui, |ui| {
            let rect = ui.max_rect();
            super::titlebar_drag(ui, rect);
            let top = super::blend(palette.window, palette.accent, 0.10);
            super::widgets::paint_vertical_gradient(ui, rect, top, palette.window);

            let card_width = 460.0_f32.min(rect.width() - 32.0);
            let card_height = 520.0_f32.min(rect.height() - 64.0);
            let card = egui::Rect::from_center_size(
                rect.center() - Vec2::new(0.0, 8.0),
                Vec2::new(card_width, card_height),
            );
            let mut card_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(card)
                    .layout(Layout::top_down(Align::Center)),
            );
            Frame::new()
                .fill(palette.panel)
                .stroke(Stroke::new(1.0, palette.outline))
                .corner_radius(CornerRadius::same(theme::RADIUS + 8))
                .inner_margin(Margin::same(36))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 16],
                    blur: 48,
                    spread: 0,
                    color: palette.shadow,
                })
                .show(&mut card_ui, |ui| {
                    ui.set_width(card_width - 72.0);
                    ui.spacing_mut().item_spacing.y = 8.0;
                    let (logo, _) =
                        ui.allocate_exact_size(Vec2::splat(64.0), egui::Sense::hover());
                    theme::logo(ui, logo.center(), 64.0, palette.accent, palette.on_accent);
                    ui.add_space(4.0);
                    theme::text(ui, "Fastpotify", theme::bold(28.0), palette.text);
                    theme::text(
                        ui,
                        "Your music, from your server.",
                        theme::regular(14.5),
                        palette.secondary,
                    );
                    ui.add_space(18.0);

                    field_label(ui, app, "Server URL");
                    let server = ui.add_enabled(
                        !signing_in,
                        egui::TextEdit::singleline(&mut app.login_server)
                            .hint_text("https://music.example.com")
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(4.0);

                    field_label(ui, app, "Username");
                    let username = ui.add_enabled(
                        !signing_in,
                        egui::TextEdit::singleline(&mut app.login_username)
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(4.0);

                    field_label(ui, app, "Password");
                    let password = ui.add_enabled(
                        !signing_in,
                        egui::TextEdit::singleline(&mut app.login_password)
                            .password(true)
                            .desired_width(f32::INFINITY),
                    );

                    if let Some(warning) = app.login_security_warning() {
                        ui.add_space(4.0);
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(warning)
                                .font(theme::regular(12.0))
                                .color(palette.warning),
                            )
                            .wrap(),
                        );
                    }

                    if let AuthStatus::Failed(message) = &app.auth {
                        ui.add_space(4.0);
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(message)
                                    .font(theme::regular(12.5))
                                    .color(palette.danger),
                            )
                            .wrap(),
                        );
                    }

                    ui.add_space(12.0);
                    if signing_in {
                        ui.horizontal(|ui| {
                            ui.add_space((ui.available_width() - 190.0).max(0.0) / 2.0);
                            theme::spinner(ui, 18.0, palette.accent);
                            theme::text(
                                ui,
                                "Signing in…",
                                theme::medium(14.0),
                                palette.text,
                            );
                        });
                    } else {
                        let complete = !app.login_server.trim().is_empty()
                            && !app.login_username.trim().is_empty()
                            && !app.login_password.is_empty();
                        let enter = ui.input(|input| input.key_pressed(egui::Key::Enter));
                        if big_button(ui, app, "Sign in", complete)
                            || (complete
                                && enter
                                && (server.has_focus()
                                    || username.has_focus()
                                    || password.has_focus()))
                        {
                            app.actions.push(Action::SignIn);
                        }
                    }
                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(
                                "Compatible with Navidrome and OpenSubsonic servers. Your password is stored only in this app's private profile.",
                            )
                            .font(theme::regular(12.0))
                            .color(palette.secondary),
                        )
                        .wrap(),
                    );
                });

            ui.painter().text(
                egui::pos2(rect.center().x, rect.bottom() - 18.0),
                egui::Align2::CENTER_BOTTOM,
                format!("Fastpotify {}", env!("CARGO_PKG_VERSION")),
                theme::regular(11.5),
                palette.dim,
            );
        });
}

fn field_label(ui: &mut egui::Ui, app: &App, label: &str) {
    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
        theme::text(ui, label, theme::semibold(12.5), app.palette.secondary);
    });
}

fn big_button(ui: &mut egui::Ui, app: &App, label: &str, enabled: bool) -> bool {
    let palette = app.palette;
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_string(), theme::bold(15.0), palette.on_accent);
    let size = Vec2::new(ui.available_width().min(300.0), 46.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let fill = if !enabled {
        palette.surface_hover
    } else if response.hovered() {
        palette.accent_hover
    } else {
        palette.accent
    };
    ui.painter().rect_filled(rect, 23.0, fill);
    let color = if enabled {
        palette.on_accent
    } else {
        palette.dim
    };
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, color);
    enabled
        && response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
}
