//! Home, search, Favorites, and the user's playlists.

use egui::{Align, CornerRadius, Frame, Layout, Margin, Rect, Sense, Vec2, pos2, vec2};

use crate::api::models::{MediaId, Playlist, pick_image};
use crate::app::App;
use crate::model::{Action, Dialog, DragTrack, Loadable, Page};
use crate::theme::{self, Icon, Palette};

const DEFAULT_ROW_HEIGHT: f32 = 60.0;
const COMPACT_ROW_HEIGHT: f32 = 32.0;

struct Entry {
    image: Option<String>,
    name: String,
    subtitle: String,
    page: Page,
    media: Option<MediaId>,
    favorite: bool,
    owned_playlist: Option<Playlist>,
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let top = 12 + theme::titlebar_inset(ui.ctx()) as i8;
    let panel = egui::Panel::left("sidebar")
        .resizable(true)
        .default_size(app.settings.sidebar_width)
        .size_range(210.0..=440.0)
        .show_separator_line(false)
        .frame(Frame::new().fill(palette.panel).inner_margin(Margin {
            left: 12,
            right: 8,
            top,
            bottom: 8,
        }));
    let response = panel.show(ui, |ui| {
        art_panel(app, ui);
        contents(app, ui);
    });
    let width = response.response.rect.width();
    if (width - app.settings.sidebar_width).abs() > 1.0 {
        app.settings.sidebar_width = width;
        app.actions.push(Action::SettingsChanged);
    }
}

fn art_panel(app: &mut App, ui: &mut egui::Ui) {
    if !app.settings.art_expanded {
        return;
    }
    let Some(now) = app.now_playing() else {
        return;
    };
    let Some(url) = now.song.image(512).map(str::to_string) else {
        return;
    };
    let palette = app.palette;
    let side = ui
        .available_width()
        .min(ui.available_height() * 0.45)
        .max(80.0);
    egui::Panel::bottom("sidebar-art")
        .exact_size(side)
        .resizable(false)
        .show_separator_line(false)
        .frame(Frame::new())
        .show(ui, |ui| {
            let rect = Rect::from_min_size(
                ui.max_rect().left_top(),
                Vec2::splat(side.min(ui.available_width())),
            );
            super::widgets::paint_cover(ui, &palette, Some(&url), rect, 8.0, Icon::Music);
            let art = ui
                .interact(rect, egui::Id::new("sidebar-art"), Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            let chevron_rect = Rect::from_center_size(
                pos2(rect.right() - 16.0, rect.top() + 16.0),
                Vec2::splat(20.0),
            );
            let over_chevron = ui.rect_contains_pointer(chevron_rect);
            if art.hovered() || over_chevron {
                let chevron = ui
                    .interact(
                        chevron_rect,
                        egui::Id::new("sidebar-art-collapse"),
                        Sense::click(),
                    )
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                ui.painter().circle_filled(
                    chevron_rect.center(),
                    10.0,
                    palette.panel.gamma_multiply(0.9),
                );
                Icon::ChevronDown.image(palette.text, 14.0).paint_at(
                    ui,
                    Rect::from_center_size(chevron_rect.center(), Vec2::splat(14.0)),
                );
                if chevron.clicked() {
                    app.settings.art_expanded = false;
                    app.actions.push(Action::SettingsChanged);
                }
            }
            if art.clicked()
                && !over_chevron
                && let Some(album) = &now.song.album
            {
                app.actions
                    .push(Action::Open(Page::Album(album.id.clone())));
            }
        });
}

fn nav_row(
    ui: &mut egui::Ui,
    palette: &Palette,
    icon: Icon,
    label: &str,
    active: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let color = if active || response.hovered() {
            palette.text
        } else {
            palette.secondary
        };
        icon.image(color, 22.0).paint_at(
            ui,
            Rect::from_center_size(pos2(rect.left() + 22.0, rect.center().y), Vec2::splat(22.0)),
        );
        ui.painter().text(
            pos2(rect.left() + 46.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            theme::bold(15.0),
            color,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn contents(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let current_page = app.page().clone();
    ui.add_space(4.0);
    if nav_row(
        ui,
        &palette,
        Icon::House,
        "Home",
        current_page == Page::Home,
    )
    .clicked()
    {
        app.actions.push(Action::Open(Page::Home));
    }
    if nav_row(
        ui,
        &palette,
        Icon::Search,
        "Search",
        current_page == Page::Search,
    )
    .clicked()
    {
        app.actions.push(Action::FocusSearch);
    }
    ui.add_space(10.0);
    ui.painter().hline(
        ui.max_rect().x_range().shrink(4.0),
        ui.cursor().top(),
        egui::Stroke::new(1.0, palette.outline),
    );
    ui.add_space(10.0);

    let search_id = egui::Id::new("sidebar-show-search");
    let mut show_search = ui
        .data(|data| data.get_temp::<bool>(search_id))
        .unwrap_or(false);
    let mut focus_search = false;
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        theme::icon(ui, Icon::Library, 22.0, palette.secondary);
        ui.add_space(2.0);
        theme::text(ui, "Library", theme::bold(15.0), palette.text);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if theme::icon_button(
                ui,
                Icon::PanelLeft,
                16.0,
                palette.secondary,
                palette.text,
                super::keys::platform_shortcut("Hide sidebar (Ctrl+B)", "Hide sidebar (Cmd+B)"),
            )
            .clicked()
            {
                app.actions.push(Action::ToggleSidebar);
            }
            if theme::icon_button(
                ui,
                Icon::Plus,
                16.0,
                palette.secondary,
                palette.text,
                "Create a playlist",
            )
            .clicked()
            {
                app.actions.push(Action::ShowDialog(Dialog::CreatePlaylist {
                    name: String::new(),
                    public: false,
                    songs: Vec::new(),
                }));
            }
            if theme::icon_button(
                ui,
                Icon::Search,
                16.0,
                palette.secondary,
                palette.text,
                "Search your library",
            )
            .clicked()
            {
                show_search = !show_search;
                if show_search {
                    focus_search = true;
                } else {
                    app.library.filter.clear();
                }
            }
        });
    });
    ui.data_mut(|data| data.insert_temp(search_id, show_search));
    if show_search {
        ui.add_space(6.0);
        let width = ui.available_width() - 4.0;
        let response = super::widgets::search_field(
            ui,
            &palette,
            egui::Id::new("sidebar-search"),
            &mut app.library.filter,
            "Favorites and playlists",
            width,
        );
        if focus_search {
            response.request_focus();
        }
    }
    ui.add_space(8.0);

    let needle = app.library.filter.trim().to_lowercase();
    let mut entries = Vec::new();
    if needle.is_empty() || "favorites".contains(&needle) {
        let total = app
            .library
            .favorite_songs
            .total
            .unwrap_or(app.library.favorite_songs.items.len() as u32);
        entries.push(Entry {
            image: None,
            name: "Favorites".to_string(),
            subtitle: format!("{} songs", crate::util::format_count(total as u64)),
            page: Page::Favorites,
            media: None,
            favorite: true,
            owned_playlist: None,
        });
    }

    let (loading, error) = match &app.library.playlists {
        Loadable::Loaded(playlists) => {
            for playlist in playlists {
                let haystack =
                    format!("{} {}", playlist.name, playlist.owner_name()).to_lowercase();
                if !needle.is_empty() && !haystack.contains(&needle) {
                    continue;
                }
                let owned = app
                    .user
                    .as_ref()
                    .is_some_and(|user| user.roles.playlist && playlist.owned_by(&user.id));
                entries.push(Entry {
                    image: pick_image(&playlist.images, 64).map(str::to_string),
                    name: playlist.name.clone(),
                    subtitle: format!(
                        "Playlist • {} songs",
                        crate::util::format_count(playlist.track_total() as u64)
                    ),
                    page: Page::Playlist(playlist.id.clone()),
                    media: Some(playlist.id.clone()),
                    favorite: false,
                    owned_playlist: owned.then(|| playlist.clone()),
                });
            }
            (false, None)
        }
        Loadable::Loading | Loadable::NotLoaded => (true, None),
        Loadable::Failed(error) => (false, Some(error.clone())),
    };
    entries.sort_by_key(|entry| {
        let pinned = entry
            .media
            .as_ref()
            .is_some_and(|id| app.settings.pinned_contexts.contains(&id.uri()));
        (!entry.favorite, !pinned, entry.name.to_lowercase())
    });
    library_rows(app, ui, entries, loading, error, &needle, &current_page);
}

fn library_rows(
    app: &mut App,
    ui: &mut egui::Ui,
    entries: Vec<Entry>,
    loading: bool,
    error: Option<String>,
    needle: &str,
    current_page: &Page,
) {
    let palette = app.palette;
    let playing_context = app.playing_context_id().cloned();
    egui::ScrollArea::vertical()
        .id_salt("sidebar-list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if loading {
                super::widgets::loading_row(ui, &palette);
            }
            if let Some(error) = &error {
                super::widgets::error_row(ui, app, error, None);
            }
            if entries.is_empty() && !loading && error.is_none() {
                ui.add_space(12.0);
                theme::subtle(
                    ui,
                    &palette,
                    if needle.is_empty() {
                        "No favorites or playlists yet."
                    } else {
                        "No matches."
                    },
                );
            }
            let compact = app.settings.sidebar_compact;
            let row_height = if compact {
                COMPACT_ROW_HEIGHT
            } else {
                DEFAULT_ROW_HEIGHT
            };
            super::widgets::virtual_rows(ui, entries.len(), row_height, |ui, index| {
                let entry = &entries[index];
                let (rect, response) = ui.allocate_exact_size(
                    vec2(ui.available_width(), row_height),
                    Sense::click_and_drag(),
                );
                let active = &entry.page == current_page;
                let playing = entry.media.as_ref().is_some_and(|id| {
                    app.believed_playing() && playing_context.as_ref() == Some(id)
                });
                let pinned = entry
                    .media
                    .as_ref()
                    .is_some_and(|id| app.settings.pinned_contexts.contains(&id.uri()));
                if ui.is_rect_visible(rect) {
                    if active {
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(6), palette.surface);
                    } else if response.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(6),
                            palette.surface_hover.gamma_multiply(0.6),
                        );
                    }
                    if compact {
                        let right = rect.right() - if playing || pinned { 28.0 } else { 8.0 };
                        crate::bidi::paint_line(
                            &ui.painter().with_clip_rect(Rect::from_min_max(
                                pos2(rect.left() + 8.0, rect.top()),
                                pos2(right, rect.bottom()),
                            )),
                            rect.left() + 8.0,
                            right,
                            rect.center().y,
                            &entry.name,
                            theme::medium(13.5),
                            if playing {
                                palette.accent
                            } else {
                                palette.text
                            },
                        );
                    } else {
                        let cover = Rect::from_center_size(
                            pos2(rect.left() + 30.0, rect.center().y),
                            Vec2::splat(44.0),
                        );
                        if entry.favorite {
                            favorites_cover(ui, cover, 6.0);
                        } else {
                            super::widgets::paint_cover(
                                ui,
                                &palette,
                                entry.image.as_deref(),
                                cover,
                                6.0,
                                Icon::Music,
                            );
                        }
                        let text_left = cover.right() + 12.0;
                        let text_right = rect.right() - if playing || pinned { 28.0 } else { 8.0 };
                        let painter = ui.painter().with_clip_rect(Rect::from_min_max(
                            pos2(text_left, rect.top()),
                            pos2(text_right, rect.bottom()),
                        ));
                        crate::bidi::paint_line(
                            &painter,
                            text_left,
                            text_right,
                            rect.center().y - 9.0,
                            &entry.name,
                            theme::medium(14.0),
                            if playing {
                                palette.accent
                            } else {
                                palette.text
                            },
                        );
                        crate::bidi::paint_line(
                            &painter,
                            text_left,
                            text_right,
                            rect.center().y + 10.0,
                            &entry.subtitle,
                            theme::regular(12.5),
                            palette.secondary,
                        );
                    }
                    if playing {
                        Icon::Volume2.image(palette.accent, 16.0).paint_at(
                            ui,
                            Rect::from_center_size(
                                pos2(rect.right() - 16.0, rect.center().y),
                                Vec2::splat(16.0),
                            ),
                        );
                    } else if pinned {
                        Icon::Pin.image(palette.secondary, 13.0).paint_at(
                            ui,
                            Rect::from_center_size(
                                pos2(rect.right() - 16.0, rect.center().y),
                                Vec2::splat(13.0),
                            ),
                        );
                    }
                }

                if let Some(track) = response.dnd_release_payload::<DragTrack>() {
                    if entry.favorite {
                        if app.is_saved(&track.song.id) != Some(true) {
                            app.actions
                                .push(Action::ToggleFavorite(track.song.id.clone()));
                        }
                    } else if let Page::Playlist(id) = &entry.page
                        && entry.owned_playlist.is_some()
                    {
                        app.actions.push(Action::AddToPlaylist {
                            playlist_id: id.clone(),
                            playlist_name: entry.name.clone(),
                            songs: vec![track.song.clone()],
                        });
                    }
                }
                if response.clicked() {
                    app.actions.push(Action::Open(entry.page.clone()));
                }
                if let (Some(media), Some(playlist)) = (&entry.media, &entry.owned_playlist) {
                    egui::Popup::context_menu(&response)
                        .frame(super::widgets::menu_frame(&palette))
                        .show(|ui| {
                            super::widgets::context_menu_items(ui, app, media, Some(playlist));
                            let uri = media.uri();
                            let pinned = app.settings.pinned_contexts.contains(&uri);
                            if super::widgets::menu_item(
                                ui,
                                &palette,
                                Some(if pinned { Icon::PinOff } else { Icon::Pin }),
                                if pinned { "Unpin" } else { "Pin to top" },
                            ) {
                                if pinned {
                                    app.settings.pinned_contexts.retain(|held| held != &uri);
                                } else {
                                    app.settings.pinned_contexts.push(uri);
                                }
                                app.mark_settings_dirty();
                            }
                        });
                }
                response.on_hover_cursor(egui::CursorIcon::PointingHand);
            });
        });
}

/// The gradient tile used for the server's starred music collection.
pub fn favorites_cover(ui: &egui::Ui, rect: Rect, radius: f32) {
    super::widgets::paint_gradient_icon_cover(
        ui,
        rect,
        radius,
        "favorites-cover-gradient",
        [
            egui::Color32::from_rgb(0x45, 0x0a, 0xf5),
            egui::Color32::from_rgb(0x6a, 0x3a, 0xe8),
            egui::Color32::from_rgb(0x8e, 0x9f, 0xe5),
            egui::Color32::from_rgb(0xc4, 0xef, 0xd9),
        ],
        Icon::HeartFilled,
    );
}
