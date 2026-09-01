//! Artist details and albums exposed by OpenSubsonic.

use crate::api::models::{MediaId, pick_image};
use crate::app::App;
use crate::model::{Action, Loadable, Page};
use crate::theme::{self, Icon};
use crate::util;

use super::collection::{Hero, hero};
use super::widgets;

pub fn show(app: &mut App, ui: &mut egui::Ui, id: &MediaId) {
    let Some(page) = app.artist_pages.remove(id) else {
        app.ensure_loaded(Page::Artist(id.clone()));
        return;
    };
    let palette = app.palette;
    match &page.artist {
        Loadable::Loaded(artist) => {
            let mut byline = Vec::new();
            if artist.album_count > 0 {
                byline.push((
                    format!(
                        "{} album{}",
                        util::format_count(artist.album_count as u64),
                        if artist.album_count == 1 { "" } else { "s" }
                    ),
                    None,
                ));
            }
            if !artist.genres.is_empty() {
                byline.push((
                    artist
                        .genres
                        .iter()
                        .take(3)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", "),
                    None,
                ));
            }
            hero(
                app,
                ui,
                Hero {
                    image: pick_image(&artist.images, 300),
                    favorite: false,
                    kind: "Artist",
                    title: &artist.name,
                    description: None,
                    byline,
                    round: true,
                },
            );
            let favorite = app.is_saved(&artist.id).unwrap_or(artist.starred);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 18.0;
                if app.play_pending(&artist.id) {
                    theme::circle_spinner(ui, 56.0, palette.accent, palette.on_accent, "Starting…");
                } else if theme::circle_button(
                    ui,
                    Icon::PlayFilled,
                    56.0,
                    palette.accent,
                    palette.accent_hover,
                    palette.on_accent,
                    "Play",
                )
                .clicked()
                {
                    app.actions.push(Action::PlayContext {
                        context: artist.id.clone(),
                        offset: None,
                        offset_index: None,
                    });
                }
                if theme::pill_button(
                    ui,
                    &palette,
                    if favorite {
                        "In Favorites"
                    } else {
                        "Add to Favorites"
                    },
                    false,
                )
                .clicked()
                {
                    app.actions.push(Action::ToggleFavorite(artist.id.clone()));
                }
                let more = theme::icon_button(
                    ui,
                    Icon::Ellipsis,
                    26.0,
                    palette.secondary,
                    palette.text,
                    "More",
                );
                egui::Popup::menu(&more)
                    .frame(widgets::menu_frame(&palette))
                    .show(|ui| {
                        widgets::context_menu_items(ui, app, &artist.id, None);
                    });
            });
            ui.add_space(20.0);

            theme::section_title(ui, &palette, "Albums");
            ui.add_space(8.0);
            widgets::grid(ui, |ui| {
                for album in &page.albums.items {
                    let subtitle = album.year_label().unwrap_or_else(|| "Album".to_string());
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
            if page.albums.loading {
                widgets::loading_row(ui, &palette);
            } else if let Some(error) = &page.albums.error {
                widgets::error_row(ui, app, error, Some(Page::Artist(id.clone())));
            } else if page.albums.items.is_empty() && page.albums.loaded_once {
                widgets::empty_state(
                    ui,
                    &palette,
                    Icon::Disc,
                    "No albums",
                    "This server did not return any albums for the artist.",
                );
            } else {
                widgets::load_more_when_near_end(
                    ui,
                    app,
                    Page::Artist(id.clone()),
                    page.albums.can_load_more(),
                );
            }
        }
        Loadable::Loading | Loadable::NotLoaded => {
            ui.add_space(40.0);
            widgets::loading_row(ui, &palette);
        }
        Loadable::Failed(error) => {
            let error = error.clone();
            ui.add_space(40.0);
            widgets::error_row(ui, app, &error, Some(Page::Artist(id.clone())));
        }
    }
    app.artist_pages.insert(id.clone(), page);
}
