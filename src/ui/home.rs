//! Stable shelves backed by the user's server and local listening history.

use egui::{CornerRadius, Rect, Sense, Vec2, pos2, vec2};

use crate::api::models::{Album, MediaId, PlayHistory, PlayableItem, Song, pick_image};
use crate::app::App;
use crate::model::{Action, Loadable, Page, RowContext};
use crate::theme::{self, Icon};

use super::widgets::{self, TrackRow};

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    ui.add_space(6.0);
    theme::text(ui, crate::util::greeting(), theme::bold(30.0), palette.text);
    ui.add_space(12.0);
    quick_access(app, ui);
    ui.add_space(16.0);

    recently_played(app, ui);
    album_shelf(
        app,
        ui,
        "recently-added",
        "Recently added",
        app.home.recently_added.clone(),
    );
    album_shelf(
        app,
        ui,
        "frequent-albums",
        "Played often",
        app.home.frequent_albums.clone(),
    );
    song_list(app, ui, "A random mix", app.home.random_songs.clone(), 20);
}

struct Tile {
    image: Option<String>,
    name: String,
    page: Page,
    play_context: Option<MediaId>,
    favorite: bool,
}

fn quick_access(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let mut tiles = vec![Tile {
        image: None,
        name: "Favorites".to_string(),
        page: Page::Favorites,
        play_context: None,
        favorite: true,
    }];
    if let Some(playlists) = app.library.playlists.get() {
        tiles.extend(playlists.iter().take(7).map(|playlist| Tile {
            image: pick_image(&playlist.images, 64).map(str::to_string),
            name: playlist.name.clone(),
            page: Page::Playlist(playlist.id.clone()),
            play_context: Some(playlist.id.clone()),
            favorite: false,
        }));
    }

    let available = ui.available_width();
    let columns = ((available / 300.0).floor() as usize).clamp(2, 4);
    let gap = 10.0;
    let tile_width = (available - gap * (columns as f32 - 1.0)) / columns as f32;
    for row in 0..tiles.len().div_ceil(columns) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for column in 0..columns {
                let Some(tile) = tiles.get(row * columns + column) else {
                    break;
                };
                let (rect, response) =
                    ui.allocate_exact_size(vec2(tile_width, 60.0), Sense::click());
                if ui.is_rect_visible(rect) {
                    let hovered = ui.rect_contains_pointer(rect);
                    let fill = if hovered {
                        palette.surface_hover
                    } else {
                        palette.surface
                    };
                    ui.painter().rect_filled(rect, CornerRadius::same(6), fill);
                    let cover = Rect::from_min_size(rect.min, Vec2::splat(60.0));
                    if tile.favorite {
                        super::sidebar::favorites_cover(ui, cover, 6.0);
                    } else {
                        widgets::paint_cover(
                            ui,
                            &palette,
                            tile.image.as_deref(),
                            cover,
                            6.0,
                            Icon::Music,
                        );
                    }
                    let play_room = if hovered && tile.play_context.is_some() {
                        52.0
                    } else {
                        12.0
                    };
                    let text_rect = Rect::from_min_max(
                        pos2(cover.right() + 12.0, rect.top()),
                        pos2(rect.right() - play_room, rect.bottom()),
                    );
                    crate::bidi::paint_line(
                        &ui.painter().with_clip_rect(text_rect),
                        text_rect.left(),
                        text_rect.right(),
                        rect.center().y,
                        &tile.name,
                        theme::bold(14.5),
                        palette.text,
                    );
                    if hovered && let Some(context) = &tile.play_context {
                        let button = Rect::from_center_size(
                            pos2(rect.right() - 28.0, rect.center().y),
                            Vec2::splat(40.0),
                        );
                        let mut child =
                            ui.new_child(egui::UiBuilder::new().max_rect(button).layout(
                                egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                            ));
                        if theme::circle_button(
                            &mut child,
                            Icon::PlayFilled,
                            40.0,
                            palette.accent,
                            palette.accent_hover,
                            palette.on_accent,
                            "Play",
                        )
                        .clicked()
                        {
                            app.actions.push(Action::PlayContext {
                                context: context.clone(),
                                offset: None,
                                offset_index: None,
                            });
                        }
                    }
                }
                if response
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    app.actions.push(Action::Open(tile.page.clone()));
                }
            }
        });
    }
}

fn recently_played(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let history: Vec<PlayHistory> = match app.home.recently_played.clone() {
        Loadable::Loaded(history) => history,
        Loadable::Loading | Loadable::NotLoaded => {
            widgets::shelf(ui, &palette, "recently-played", "Recently played", |ui| {
                widgets::loading_row(ui, &palette)
            });
            return;
        }
        Loadable::Failed(message) => {
            widgets::shelf(ui, &palette, "recently-played", "Recently played", |ui| {
                widgets::error_row(ui, app, &message, Some(Page::Home));
            });
            return;
        }
    };
    let mut seen = std::collections::HashSet::new();
    let history: Vec<_> = history
        .into_iter()
        .filter(|entry| seen.insert(entry.track.id.clone()))
        .take(16)
        .collect();
    if history.is_empty() {
        return;
    }
    widgets::shelf(ui, &palette, "recently-played", "Recently played", |ui| {
        for entry in &history {
            song_card(app, ui, &entry.track);
        }
    });
}

fn album_shelf(
    app: &mut App,
    ui: &mut egui::Ui,
    id: &'static str,
    title: &'static str,
    albums: Loadable<Vec<Album>>,
) {
    let palette = app.palette;
    let albums = match albums {
        Loadable::Loaded(albums) => albums,
        Loadable::Loading | Loadable::NotLoaded => {
            widgets::shelf(ui, &palette, id, title, |ui| {
                widgets::loading_row(ui, &palette)
            });
            return;
        }
        Loadable::Failed(message) => {
            widgets::shelf(ui, &palette, id, title, |ui| {
                widgets::error_row(ui, app, &message, Some(Page::Home));
            });
            return;
        }
    };
    if albums.is_empty() {
        return;
    }
    widgets::shelf(ui, &palette, id, title, |ui| {
        for album in &albums {
            let subtitle = crate::api::models::join_names(
                album.artists.iter().map(|artist| artist.name.as_str()),
            );
            let card = widgets::card(
                ui,
                app,
                pick_image(&album.images, 300),
                &album.name,
                &subtitle,
                false,
                true,
            );
            if card.play {
                app.actions.push(Action::PlayContext {
                    context: album.id.clone(),
                    offset: None,
                    offset_index: None,
                });
            }
            if card.clicked {
                app.actions
                    .push(Action::Open(Page::Album(album.id.clone())));
            }
        }
    });
}

fn song_card(app: &mut App, ui: &mut egui::Ui, song: &Song) {
    let card = widgets::card(
        ui,
        app,
        song.image(300),
        &song.name,
        &song.artist_names(),
        false,
        true,
    );
    if card.play {
        app.actions.push(Action::PlaySongs {
            songs: vec![song.clone()],
            index: 0,
        });
    }
    if card.clicked
        && let Some(album) = &song.album
    {
        app.actions
            .push(Action::Open(Page::Album(album.id.clone())));
    }
}

fn song_list(
    app: &mut App,
    ui: &mut egui::Ui,
    title: &str,
    songs: Loadable<Vec<Song>>,
    limit: usize,
) {
    let palette = app.palette;
    let songs = match songs {
        Loadable::Loaded(songs) => songs,
        Loadable::Loading | Loadable::NotLoaded => {
            theme::section_title(ui, &palette, title);
            widgets::loading_row(ui, &palette);
            return;
        }
        Loadable::Failed(message) => {
            theme::section_title(ui, &palette, title);
            widgets::error_row(ui, app, &message, Some(Page::Home));
            return;
        }
    };
    if songs.is_empty() {
        return;
    }
    theme::section_title(ui, &palette, title);
    ui.add_space(4.0);
    let context = RowContext::Songs(songs.clone());
    let items: Vec<PlayableItem> = songs.into_iter().map(PlayableItem::Track).collect();
    for (index, item) in items.iter().take(limit).enumerate() {
        widgets::track_row(
            ui,
            app,
            TrackRow {
                index,
                number: None,
                item,
                context: &context,
                show_cover: !app.settings.tracklist_compact,
                show_album: true,
                added_at: None,
                playlist_index: None,
                compact: false,
                thin: app.settings.tracklist_compact,
                shift: 0.0,
                picked: false,
                picked_songs: &[],
            },
        );
    }
    ui.add_space(16.0);
}
