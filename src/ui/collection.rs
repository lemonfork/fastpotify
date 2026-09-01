//! Playlist, album, and Favorites pages.

use std::sync::Arc;

use egui::{Align, Layout, Sense, Vec2};

use crate::api::models::{Album, MediaId, PlayableItem, Playlist, Song, pick_image};
use crate::app::App;
use crate::model::{Action, DragTrack, Loadable, Page, RowContext, SortColumn, TableSort};
use crate::theme::{self, Icon};
use crate::util;

use super::widgets::{self, TrackRow};

pub struct Hero<'a> {
    pub image: Option<&'a str>,
    pub favorite: bool,
    pub kind: &'a str,
    pub title: &'a str,
    pub description: Option<String>,
    pub byline: Vec<(String, Option<Page>)>,
    pub round: bool,
}

pub fn hero(app: &mut App, ui: &mut egui::Ui, hero: Hero<'_>) {
    let palette = app.palette;
    ui.add_space(12.0);
    let cover_size = if ui.available_width() > 720.0 {
        212.0
    } else {
        160.0
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 24.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(cover_size), Sense::hover());
        let radius = if hero.round { cover_size / 2.0 } else { 6.0 };
        widgets::paint_shadow(ui, &palette, rect, radius);
        if hero.favorite {
            super::sidebar::favorites_cover(ui, rect, radius);
        } else {
            widgets::paint_cover(
                ui,
                &palette,
                hero.image,
                rect,
                radius,
                if hero.round { Icon::User } else { Icon::Music },
            );
        }
        ui.vertical(|ui| {
            let width = ui.available_width();
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.add_space(cover_size * 0.08);
            theme::text(ui, hero.kind, theme::medium(12.5), palette.text);
            let display_title = crate::bidi::display_text(hero.title);
            let mut size = if cover_size > 200.0 { 56.0 } else { 40.0 };
            while size > 22.0
                && ui
                    .painter()
                    .layout_no_wrap(display_title.to_string(), theme::bold(size), palette.text)
                    .size()
                    .x
                    > width
            {
                size -= 6.0;
            }
            theme::text(ui, hero.title, theme::bold(size), palette.text);
            if let Some(description) = &hero.description
                && !description.is_empty()
            {
                theme::text(
                    ui,
                    description.as_str(),
                    theme::regular(13.5),
                    palette.secondary,
                );
            }
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for (index, (text, page)) in hero.byline.iter().enumerate() {
                    if index > 0 {
                        theme::text(ui, "•", theme::regular(13.5), palette.secondary);
                    }
                    if let Some(page) = page {
                        if theme::link(ui, text, theme::semibold(13.5), palette.text).clicked() {
                            app.actions.push(Action::Open(page.clone()));
                        }
                    } else {
                        theme::text(ui, text, theme::regular(13.5), palette.secondary);
                    }
                }
            });
        });
    });
    ui.add_space(20.0);
}

pub struct Actions<'a> {
    pub play_context: Option<MediaId>,
    /// Exact songs on screen. Used for sorted views and Favorites, where no
    /// synthetic server context exists.
    pub view: Option<Vec<Song>>,
    pub saved: Option<(MediaId, bool)>,
    pub saved_icons: (Icon, Icon),
    pub saved_tooltips: (&'a str, &'a str),
    pub owned_playlist: Option<Playlist>,
    pub name: &'a str,
}

pub fn actions_row(
    app: &mut App,
    ui: &mut egui::Ui,
    actions: Actions<'_>,
    filter: Option<&mut String>,
) {
    let palette = app.palette;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 18.0;
        if let Some(context) = &actions.play_context {
            let now_playing_here =
                app.playing_context_id() == Some(context) && app.believed_playing();
            let icon = if now_playing_here {
                Icon::PauseFilled
            } else {
                Icon::PlayFilled
            };
            if app.play_pending(context) {
                theme::circle_spinner(ui, 56.0, palette.accent, palette.on_accent, "Starting…");
            } else if theme::circle_button(
                ui,
                icon,
                56.0,
                palette.accent,
                palette.accent_hover,
                palette.on_accent,
                if now_playing_here { "Pause" } else { "Play" },
            )
            .clicked()
            {
                if now_playing_here {
                    app.actions.push(Action::TogglePlay);
                } else if let Some(songs) = actions.view.clone() {
                    app.actions.push(Action::PlaySongs { songs, index: 0 });
                } else {
                    app.actions.push(Action::PlayContext {
                        context: context.clone(),
                        offset: None,
                        offset_index: None,
                    });
                }
            }
            // A sorted or filtered view is an explicit local song list. Do
            // not turn its Shuffle button into a different server context.
            if actions.view.is_none() {
                let shuffling_here =
                    app.playing_context_id() == Some(context) && app.playing_context_shuffle();
                if theme::icon_button(
                    ui,
                    Icon::Shuffle,
                    26.0,
                    if shuffling_here {
                        palette.accent
                    } else {
                        palette.secondary
                    },
                    palette.text,
                    if shuffling_here {
                        "Shuffle off"
                    } else {
                        "Shuffle play"
                    },
                )
                .clicked()
                {
                    if app.playing_context_id() == Some(context) {
                        app.actions.push(Action::SetShuffle(!shuffling_here));
                    } else {
                        app.actions.push(Action::ShufflePlay(context.clone()));
                    }
                }
            }
        } else if let Some(songs) = actions.view.clone()
            && !songs.is_empty()
            && theme::circle_button(
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
            app.actions.push(Action::PlaySongs { songs, index: 0 });
        }
        if let Some((id, saved)) = &actions.saved {
            let (icon, tooltip, color) = if *saved {
                (
                    actions.saved_icons.1,
                    actions.saved_tooltips.1,
                    palette.accent,
                )
            } else {
                (
                    actions.saved_icons.0,
                    actions.saved_tooltips.0,
                    palette.secondary,
                )
            };
            if theme::icon_button(ui, icon, 26.0, color, palette.text, tooltip).clicked() {
                app.actions.push(Action::ToggleFavorite(id.clone()));
            }
        }
        if let Some(context) = &actions.play_context {
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
                    widgets::context_menu_items(ui, app, context, actions.owned_playlist.as_ref());
                });
        }
        if let Some(filter) = filter {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                widgets::search_field(
                    ui,
                    &palette,
                    egui::Id::new(("collection-filter", actions.name)),
                    filter,
                    "Filter",
                    220.0,
                );
            });
        }
    });
    ui.add_space(14.0);
}

/// A song and the optional date it was added or starred.
pub type TableItem = (PlayableItem, Option<String>, Option<u32>);

pub struct Table<'a> {
    pub items: &'a [TableItem],
    pub context: RowContext,
    pub show_album: bool,
    pub show_cover: bool,
    pub show_added: bool,
    pub page: Page,
    pub loading: bool,
    pub error: Option<&'a str>,
    pub can_load_more: bool,
    pub filter: &'a str,
    pub items_revision: u64,
}

#[derive(Clone)]
pub struct TableCache {
    pub sort: Option<TableSort>,
    pub needle: String,
    pub items_revision: u64,
    pub visible: Arc<[usize]>,
    pub view_songs: Option<Arc<[Song]>>,
}

pub fn prepare_table_view(
    ui: &mut egui::Ui,
    page: &Page,
    items: &[TableItem],
    needle: &str,
    sort: Option<TableSort>,
    items_revision: u64,
) -> Arc<TableCache> {
    let cache_id = egui::Id::new("table-view-cache").with(page);
    let cached = ui.data(|data| data.get_temp::<Arc<TableCache>>(cache_id));
    if let Some(cache) = cached.filter(|cache| {
        cache.sort == sort && cache.needle == needle && cache.items_revision == items_revision
    }) {
        return cache;
    }
    let visible = view_indices(items, needle, sort);
    let view_songs = (!needle.is_empty() || sort.is_some()).then(|| {
        visible
            .iter()
            .map(|&index| items[index].0.as_track().clone())
            .collect::<Arc<[Song]>>()
    });
    let cache = Arc::new(TableCache {
        sort,
        needle: needle.to_string(),
        items_revision,
        visible: visible.into(),
        view_songs,
    });
    ui.data_mut(|data| data.insert_temp(cache_id, Arc::clone(&cache)));
    cache
}

pub fn table(app: &mut App, ui: &mut egui::Ui, table: Table<'_>) {
    let palette = app.palette;
    let needle = table.filter.trim().to_lowercase();
    let sort = app.table_sorts.get(&table.page).copied();
    let entry = prepare_table_view(
        ui,
        &table.page,
        table.items,
        &needle,
        sort,
        table.items_revision,
    );
    let thin = app.settings.tracklist_compact;
    let show_cover = !thin && table.show_cover;
    let row_height = if thin {
        theme::THIN_ROW_HEIGHT
    } else {
        theme::ROW_HEIGHT
    };
    if !table.items.is_empty()
        && let Some(column) = widgets::table_header(
            ui,
            &palette,
            table.show_album,
            table.show_added,
            show_cover,
            sort,
        )
    {
        let next = match sort {
            Some(sort) if sort.column == column && sort.ascending => Some(TableSort {
                column,
                ascending: false,
            }),
            Some(sort) if sort.column == column => None,
            Some(_) if column == SortColumn::Index => None,
            _ => Some(TableSort {
                column,
                ascending: column != SortColumn::Index,
            }),
        };
        if let Some(sort) = next {
            app.table_sorts.insert(table.page.clone(), sort);
        } else {
            app.table_sorts.remove(&table.page);
        }
        app.note_session_change();
    }
    let context = if let Some(songs) = &entry.view_songs {
        match &table.context {
            RowContext::Context { context, .. } => RowContext::View {
                songs: songs.to_vec(),
                context: context.clone(),
            },
            _ => RowContext::Songs(songs.to_vec()),
        }
    } else {
        table.context.clone()
    };
    let transformed = sort.is_some() || !needle.is_empty();
    // The OpenSubsonic reorder operation replaces the complete playlist, so
    // accept a row drop only while the visible order is exactly the complete
    // server order. The payload carries the original row index to distinguish
    // duplicate songs.
    let reorder_playlist = (!transformed && !table.can_load_more)
        .then(|| match &table.context {
            RowContext::Context {
                editable_playlist: Some(id),
                ..
            } if table.items.iter().all(|(_, _, index)| index.is_some()) => Some(id.clone()),
            _ => None,
        })
        .flatten();
    let list_top = ui.cursor().top();
    let reorder_slot = reorder_playlist.as_ref().and_then(|playlist_id| {
        let track = egui::DragAndDrop::payload::<DragTrack>(ui.ctx())?;
        let (origin, _) = track.from.as_ref()?;
        if origin != playlist_id {
            return None;
        }
        let position = ui
            .ctx()
            .pointer_latest_pos()
            .filter(|position| ui.clip_rect().contains(*position))?;
        let row = (position.y - list_top) / row_height;
        (row >= 0.0 && row <= entry.visible.len() as f32)
            .then(|| (row.round() as usize).min(entry.visible.len()))
    });
    let view = format!("{sort:?}|{needle}|{}", entry.visible.len());
    app.keep_picked_rows_for(&table.page, &view);
    let picked = app.picked_rows(&table.page).cloned().unwrap_or_default();
    let picked_songs: Vec<Song> = picked
        .iter()
        .filter_map(|row| entry.visible.get(*row))
        .filter_map(|index| table.items.get(*index))
        .map(|(item, _, _)| item.as_track().clone())
        .collect();
    let rows = entry.visible.len();
    let mut pick = None;
    widgets::virtual_rows(ui, rows, row_height, |ui, row| {
        let index = entry.visible[row];
        let (item, added_at, playlist_index) = &table.items[index];
        let shift = ui.ctx().animate_value_with_time(
            ui.id().with(("table-reorder-shift", row)),
            match reorder_slot {
                Some(slot) if row < slot => -4.0,
                Some(_) => 4.0,
                None => 0.0,
            },
            0.12,
        );
        if let Some(asked) = widgets::track_row(
            ui,
            app,
            TrackRow {
                index: if transformed { row } else { index },
                number: Some(if transformed { row + 1 } else { index + 1 }),
                item,
                context: &context,
                show_cover,
                show_album: table.show_album,
                added_at: added_at.as_deref(),
                playlist_index: *playlist_index,
                compact: false,
                thin,
                shift,
                picked: picked.contains(&row),
                picked_songs: &picked_songs,
            },
        ) {
            pick = Some((row, asked));
        }
    });
    if let Some((row, asked)) = pick {
        app.pick_row(&table.page, &view, row, asked, rows);
    }
    if !picked.is_empty() && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        app.clear_picked_rows();
    }
    if let Some(slot) = reorder_slot {
        let y = list_top + slot as f32 * row_height;
        ui.painter().hline(
            ui.max_rect().x_range().shrink(8.0),
            y,
            egui::Stroke::new(2.0, palette.accent),
        );
        if ui.input(|input| input.pointer.any_released())
            && let Some(track) = egui::DragAndDrop::take_payload::<DragTrack>(ui.ctx())
            && let Some((playlist_id, source_index)) = track.from.clone()
        {
            let mut order = table
                .items
                .iter()
                .filter_map(|(_, _, index)| *index)
                .collect::<Vec<_>>();
            if order.len() == table.items.len()
                && let Some(source_row) = order.iter().position(|index| *index == source_index)
            {
                let moved = order.remove(source_row);
                let destination = if slot > source_row { slot - 1 } else { slot };
                if destination != source_row {
                    order.insert(destination.min(order.len()), moved);
                    app.actions.push(Action::ReorderPlaylist {
                        playlist_id,
                        ordered_row_indices: order,
                    });
                }
            }
        }
    }
    if table.loading {
        ui.add_space(8.0);
        widgets::loading_row(ui, &palette);
    }
    if let Some(error) = table.error {
        ui.add_space(8.0);
        widgets::error_row(ui, app, error, Some(table.page.clone()));
    }
    if table.items.is_empty() && !table.loading && table.error.is_none() {
        widgets::empty_state(
            ui,
            &palette,
            Icon::Music,
            "Nothing here yet",
            "Songs added on your server appear here.",
        );
    } else if entry.visible.is_empty()
        && !needle.is_empty()
        && table.can_load_more
        && !table.loading
    {
        app.actions.push(Action::LoadMore(table.page));
    } else {
        widgets::load_more_when_near_end(
            ui,
            app,
            table.page,
            table.can_load_more && !table.loading,
        );
    }
}

fn view_indices(items: &[TableItem], needle: &str, sort: Option<TableSort>) -> Vec<usize> {
    let mut visible: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, (item, _, _))| {
            if needle.is_empty() {
                return true;
            }
            let track = item.as_track();
            format!(
                "{} {} {}",
                track.name,
                track.artist_names(),
                track
                    .album
                    .as_ref()
                    .map(|album| album.name.as_str())
                    .unwrap_or("")
            )
            .to_lowercase()
            .contains(needle)
        })
        .map(|(index, _)| index)
        .collect();
    if let Some(sort) = sort {
        visible.sort_by(|a, b| {
            let (index_a, index_b) = (*a, *b);
            let (item_a, added_a, _) = &items[index_a];
            let (item_b, added_b, _) = &items[index_b];
            let song_a = item_a.as_track();
            let song_b = item_b.as_track();
            let ordering = match sort.column {
                SortColumn::Title => song_a.name.to_lowercase().cmp(&song_b.name.to_lowercase()),
                SortColumn::Album => song_a
                    .album
                    .as_ref()
                    .map(|album| album.name.to_lowercase())
                    .cmp(&song_b.album.as_ref().map(|album| album.name.to_lowercase())),
                SortColumn::Added => added_a.cmp(added_b),
                SortColumn::Duration => song_a.duration_ms.cmp(&song_b.duration_ms),
                SortColumn::Index => index_a.cmp(&index_b),
            };
            if sort.ascending {
                ordering
            } else {
                ordering.reverse()
            }
        });
    }
    visible
}

fn total_duration(items: &[TableItem]) -> u64 {
    items
        .iter()
        .map(|(item, _, _)| item.duration_ms() as u64)
        .sum()
}

pub fn playlist(app: &mut App, ui: &mut egui::Ui, id: &MediaId) {
    let Some(mut page) = app.playlist_pages.remove(id) else {
        app.ensure_loaded(Page::Playlist(id.clone()));
        return;
    };
    let palette = app.palette;
    match &page.playlist {
        Loadable::Loaded(playlist) => {
            let items: Vec<TableItem> = page
                .items
                .items
                .iter()
                .map(|item| {
                    (
                        PlayableItem::Track(item.playable().clone()),
                        item.added_at.clone(),
                        Some(item.index),
                    )
                })
                .collect();
            let count = playlist.track_total().max(items.len() as u32);
            let count_text = if page.items.is_complete() {
                format!(
                    "{} songs, {}",
                    util::format_count(count as u64),
                    util::format_total_ms(total_duration(&items))
                )
            } else {
                format!("{} songs", util::format_count(count as u64))
            };
            let mut byline = Vec::new();
            if !playlist.owner_name().is_empty() {
                byline.push((playlist.owner_name().to_string(), None));
            }
            byline.push((count_text, None));
            hero(
                app,
                ui,
                Hero {
                    image: pick_image(&playlist.images, 300),
                    favorite: false,
                    kind: "Playlist",
                    title: &playlist.name,
                    description: playlist.description.clone(),
                    byline,
                    round: false,
                },
            );
            let owned = app
                .user
                .as_ref()
                .is_some_and(|user| user.roles.playlist && playlist.owned_by(&user.id));
            let needle = page.filter.trim().to_lowercase();
            let page_id = Page::Playlist(id.clone());
            let sort = app.table_sorts.get(&page_id).copied();
            let table_view =
                prepare_table_view(ui, &page_id, &items, &needle, sort, page.items.revision);
            actions_row(
                app,
                ui,
                Actions {
                    play_context: Some(playlist.id.clone()),
                    view: table_view.view_songs.as_ref().map(|songs| songs.to_vec()),
                    saved: None,
                    saved_icons: (Icon::Heart, Icon::HeartFilled),
                    saved_tooltips: ("", ""),
                    owned_playlist: owned.then(|| playlist.clone()),
                    name: &playlist.name,
                },
                Some(&mut page.filter),
            );
            table(
                app,
                ui,
                Table {
                    items: &items,
                    context: RowContext::Context {
                        context: playlist.id.clone(),
                        editable_playlist: owned.then(|| playlist.id.clone()),
                    },
                    show_album: true,
                    show_cover: true,
                    show_added: true,
                    page: page_id,
                    loading: page.items.loading,
                    error: page.items.error.as_deref(),
                    can_load_more: page.items.can_load_more(),
                    filter: &page.filter,
                    items_revision: page.items.revision,
                },
            );
        }
        Loadable::Loading | Loadable::NotLoaded => {
            ui.add_space(40.0);
            widgets::loading_row(ui, &palette);
        }
        Loadable::Failed(error) => {
            let error = error.clone();
            ui.add_space(40.0);
            widgets::error_row(ui, app, &error, Some(Page::Playlist(id.clone())));
        }
    }
    app.playlist_pages.insert(id.clone(), page);
}

pub fn album(app: &mut App, ui: &mut egui::Ui, id: &MediaId) {
    let Some(page) = app.album_pages.remove(id) else {
        app.ensure_loaded(Page::Album(id.clone()));
        return;
    };
    let palette = app.palette;
    match &page.album {
        Loadable::Loaded(album) => {
            album_hero(app, ui, album, &page.tracks);
            let items: Vec<TableItem> = page
                .tracks
                .items
                .iter()
                .cloned()
                .map(|mut song| {
                    if song.album.is_none() {
                        song.album = Some(Album {
                            id: album.id.clone(),
                            name: album.name.clone(),
                            uri: album.uri.clone(),
                            images: album.images.clone(),
                            artists: album.artists.clone(),
                            ..Album::default()
                        });
                    }
                    (PlayableItem::Track(song), None, None)
                })
                .collect();
            let saved = app.is_saved(&album.id).unwrap_or(album.starred);
            let page_id = Page::Album(id.clone());
            let sort = app.table_sorts.get(&page_id).copied();
            let table_view =
                prepare_table_view(ui, &page_id, &items, "", sort, page.tracks.revision);
            actions_row(
                app,
                ui,
                Actions {
                    play_context: Some(album.id.clone()),
                    view: table_view.view_songs.as_ref().map(|songs| songs.to_vec()),
                    saved: Some((album.id.clone(), saved)),
                    saved_icons: (Icon::Heart, Icon::HeartFilled),
                    saved_tooltips: ("Add to Favorites", "Remove from Favorites"),
                    owned_playlist: None,
                    name: &album.name,
                },
                None,
            );
            table(
                app,
                ui,
                Table {
                    items: &items,
                    context: RowContext::Context {
                        context: album.id.clone(),
                        editable_playlist: None,
                    },
                    show_album: false,
                    show_cover: false,
                    show_added: false,
                    page: page_id,
                    loading: page.tracks.loading,
                    error: page.tracks.error.as_deref(),
                    can_load_more: page.tracks.can_load_more(),
                    filter: "",
                    items_revision: page.tracks.revision,
                },
            );
        }
        Loadable::Loading | Loadable::NotLoaded => {
            ui.add_space(40.0);
            widgets::loading_row(ui, &palette);
        }
        Loadable::Failed(error) => {
            let error = error.clone();
            ui.add_space(40.0);
            widgets::error_row(ui, app, &error, Some(Page::Album(id.clone())));
        }
    }
    app.album_pages.insert(id.clone(), page);
}

fn album_hero(
    app: &mut App,
    ui: &mut egui::Ui,
    album: &Album,
    tracks: &crate::model::PagedList<Song>,
) {
    let mut byline: Vec<(String, Option<Page>)> = album
        .artists
        .iter()
        .map(|artist| (artist.name.clone(), artist.id.clone().map(Page::Artist)))
        .collect();
    if let Some(year) = album.year_label() {
        byline.push((year, None));
    }
    let count = album.track_total().max(tracks.items.len() as u32);
    let duration: u64 = tracks
        .items
        .iter()
        .map(|track| track.duration_ms as u64)
        .sum();
    byline.push((
        if tracks.is_complete() {
            format!("{count} songs, {}", util::format_total_ms(duration))
        } else {
            format!("{count} songs")
        },
        None,
    ));
    hero(
        app,
        ui,
        Hero {
            image: pick_image(&album.images, 300),
            favorite: false,
            kind: "Album",
            title: &album.name,
            description: None,
            byline,
            round: false,
        },
    );
}

pub fn favorites(app: &mut App, ui: &mut egui::Ui) {
    let songs = app.library.favorite_songs.items.clone();
    let total = app
        .library
        .favorite_songs
        .total
        .unwrap_or(songs.len() as u32);
    let complete = app.library.favorite_songs.is_complete();
    let loading = app.library.favorite_songs.loading;
    let error = app.library.favorite_songs.error.clone();
    let can_load_more = app.library.favorite_songs.can_load_more();
    let revision = app.library.favorite_songs.revision;
    let items: Vec<TableItem> = songs
        .iter()
        .cloned()
        .map(|song| {
            let starred_at = song.starred_at.clone();
            (PlayableItem::Track(song), starred_at, None)
        })
        .collect();
    let count_text = if complete {
        format!(
            "{} songs, {}",
            util::format_count(total as u64),
            util::format_total_ms(total_duration(&items))
        )
    } else {
        format!("{} songs", util::format_count(total as u64))
    };
    let user = app
        .user
        .as_ref()
        .map(|user| user.name().to_string())
        .unwrap_or_default();
    hero(
        app,
        ui,
        Hero {
            image: None,
            favorite: true,
            kind: "Collection",
            title: "Favorites",
            description: None,
            byline: vec![(user, None), (count_text, None)],
            round: false,
        },
    );
    let filter_id = egui::Id::new("favorites-filter");
    let mut filter = ui
        .data(|data| data.get_temp::<String>(filter_id))
        .unwrap_or_default();
    let page = Page::Favorites;
    let table_view = prepare_table_view(
        ui,
        &page,
        &items,
        &filter.trim().to_lowercase(),
        app.table_sorts.get(&page).copied(),
        revision,
    );
    let visible_songs: Vec<Song> = table_view
        .visible
        .iter()
        .map(|&index| items[index].0.as_track().clone())
        .collect();
    actions_row(
        app,
        ui,
        Actions {
            play_context: None,
            view: Some(visible_songs.clone()),
            saved: None,
            saved_icons: (Icon::Heart, Icon::HeartFilled),
            saved_tooltips: ("", ""),
            owned_playlist: None,
            name: "Favorites",
        },
        Some(&mut filter),
    );
    ui.data_mut(|data| data.insert_temp(filter_id, filter.clone()));
    table(
        app,
        ui,
        Table {
            items: &items,
            context: RowContext::Songs(visible_songs),
            show_album: true,
            show_cover: true,
            show_added: true,
            page,
            loading,
            error: error.as_deref(),
            can_load_more,
            filter: &filter,
            items_revision: revision,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::models::{ArtistRef, MediaKind, ProfileId};

    fn song(index: usize, title: &str, artist: &str) -> Song {
        let profile = ProfileId::new("0123456789abcdef0123456789abcdef01234567");
        Song {
            id: MediaId::new(profile.clone(), MediaKind::Song, format!("song-{index}")),
            uri: MediaId::new(profile.clone(), MediaKind::Song, format!("song-{index}")).uri(),
            name: title.to_string(),
            artists: vec![ArtistRef {
                id: Some(MediaId::new(
                    profile,
                    MediaKind::Artist,
                    format!("artist-{index}"),
                )),
                name: artist.to_string(),
                uri: None,
            }],
            duration_ms: (index as u32 + 1) * 60_000,
            ..Song::default()
        }
    }

    #[test]
    fn filtered_sorted_view_uses_music_metadata() {
        let items = vec![
            (PlayableItem::Track(song(0, "Beta", "Two")), None, None),
            (PlayableItem::Track(song(1, "Alpha", "One")), None, None),
        ];
        assert_eq!(view_indices(&items, "two", None), vec![0]);
        assert_eq!(
            view_indices(
                &items,
                "",
                Some(TableSort {
                    column: SortColumn::Title,
                    ascending: true,
                })
            ),
            vec![1, 0]
        );
    }

    #[test]
    fn sorted_playlist_rows_keep_their_server_indices() {
        let items = vec![
            (PlayableItem::Track(song(0, "Beta", "Two")), None, Some(41)),
            (PlayableItem::Track(song(1, "Alpha", "One")), None, Some(7)),
        ];
        let visible = view_indices(
            &items,
            "",
            Some(TableSort {
                column: SortColumn::Title,
                ascending: true,
            }),
        );
        assert_eq!(visible, vec![1, 0]);
        assert_eq!(items[visible[0]].2, Some(7));
        assert_eq!(items[visible[1]].2, Some(41));
    }
}
