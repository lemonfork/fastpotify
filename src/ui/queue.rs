//! The local playback queue, as a page or as a side panel.

use egui::{Align, Frame, Layout, Margin};

use crate::api::models::PlayableItem;
use crate::app::App;
use crate::model::{Action, QueueTab, RowContext, SongListMode};
use crate::player::QueueEntry;
use crate::theme::{self, Icon};

use super::widgets::{self, TrackRow};

pub fn page(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    ui.add_space(8.0);
    let queue = app.queue_view();
    let offer_save = queue.current.is_some() || !queue.rows.is_empty();
    ui.horizontal(|ui| {
        theme::text(ui, "Queue", theme::bold(28.0), palette.text);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if save_button(ui, &palette, offer_save) {
                app.actions.push(Action::SaveQueueAsPlaylist);
            }
        });
    });
    ui.add_space(12.0);
    contents(app, ui, false);
}

pub fn side_panel(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let panel = egui::Panel::right("queue-panel")
        .resizable(true)
        .default_size(app.settings.queue_width)
        .size_range(theme::SIDE_PANEL_MIN_WIDTH..=560.0)
        .show_separator_line(false)
        .frame(
            Frame::new()
                .fill(palette.panel)
                .inner_margin(Margin::symmetric(12, 12)),
        );
    let response = panel.show(ui, |ui| {
        let tab = app.queue_tab;
        let queue = app.queue_view();
        let offer_save =
            tab == QueueTab::Queue && (queue.current.is_some() || !queue.rows.is_empty());
        let mut picked = None;
        let mut close = false;
        let mut save = false;
        egui::Sides::new().shrink_left().show(
            ui,
            |ui| {
                ui.add_space(4.0);
                picked = widgets::chips(
                    ui,
                    &palette,
                    &[(QueueTab::Queue, "Queue"), (QueueTab::Recents, "Recent")],
                    tab,
                );
            },
            |ui| {
                close =
                    theme::icon_button(ui, Icon::X, 18.0, palette.secondary, palette.text, "Close")
                        .clicked();
                save = save_button(ui, &palette, offer_save);
            },
        );
        if let Some(tab) = picked {
            app.actions.push(Action::SetQueueTab(tab));
        }
        if close {
            app.actions.push(Action::ToggleQueuePanel);
        }
        if save {
            app.actions.push(Action::SaveQueueAsPlaylist);
        }
        ui.add_space(8.0);
        egui::ScrollArea::vertical()
            .id_salt("queue-panel-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| match app.queue_tab {
                QueueTab::Queue => contents(app, ui, true),
                QueueTab::Recents => recents_contents(app, ui),
            });
    });
    let width = response.response.rect.width();
    if (width - app.settings.queue_width).abs() > 1.0 {
        app.settings.queue_width = width;
        app.actions.push(Action::SettingsChanged);
    }
}

fn save_button(ui: &mut egui::Ui, palette: &crate::theme::Palette, offer: bool) -> bool {
    offer
        && theme::icon_button(
            ui,
            Icon::ListPlus,
            18.0,
            palette.secondary,
            palette.text,
            "Save as a playlist",
        )
        .clicked()
}

fn clear_button(app: &mut App, ui: &mut egui::Ui) {
    if !app.can_clear_queue() {
        return;
    }
    let palette = app.palette;
    if theme::icon_button(
        ui,
        Icon::Trash,
        18.0,
        palette.secondary,
        palette.text,
        "Clear Next up",
    )
    .clicked()
    {
        app.actions.push(Action::ClearQueue);
    }
}

fn contents(app: &mut App, ui: &mut egui::Ui, compact: bool) {
    let palette = app.palette;
    let queue = app.queue_view();
    let current = queue.current.map(|entry| PlayableItem::Track(entry.song));
    let items = queue.rows;

    if let Some(current) = &current {
        theme::text(ui, "Now playing", theme::semibold(14.0), palette.text);
        ui.add_space(4.0);
        let context = RowContext::Songs {
            songs: vec![current.as_track().clone()],
            mode: SongListMode::Finite,
        };
        widgets::track_row(
            ui,
            app,
            TrackRow {
                index: 0,
                number: Some(1),
                item: current,
                context: &context,
                show_cover: true,
                show_album: !compact,
                added_at: None,
                playlist_index: None,
                compact,
                thin: false,
                shift: 0.0,
                picked: false,
                picked_songs: &[],
            },
        );
        ui.add_space(14.0);
    }

    if items.is_empty() {
        widgets::empty_state(
            ui,
            &palette,
            Icon::ListVideo,
            "Nothing queued",
            "Songs added to Next up appear here.",
        );
        return;
    }

    let row_height = if compact {
        theme::COMPACT_ROW_HEIGHT
    } else {
        theme::ROW_HEIGHT
    };
    let manual_len = queue.manual_len.min(items.len());
    if manual_len > 0 {
        ui.horizontal(|ui| {
            theme::text(ui, "Playing next", theme::semibold(14.0), palette.text);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                clear_button(app, ui);
            });
        });
        ui.add_space(4.0);
        for index in 0..manual_len {
            queue_row(app, ui, &items, index, compact);
        }
        ui.add_space(14.0);
    }
    if items.len() > manual_len {
        theme::text(ui, "Next up", theme::semibold(14.0), palette.text);
        ui.add_space(4.0);
        widgets::virtual_rows(ui, items.len() - manual_len, row_height, |ui, index| {
            queue_row(app, ui, &items, manual_len + index, compact);
        });
    }
}

fn recents_contents(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let items = app.recents_view.clone();
    let loading = app.recents.loading;
    let error = app.recents.error.clone();
    let loaded_once = app.recents.loaded_once;

    if items.is_empty() {
        if loading || !loaded_once {
            widgets::loading_row(ui, &palette);
            return;
        }
        if let Some(error) = error {
            widgets::error_row(ui, app, &error, None);
            return;
        }
        widgets::empty_state(
            ui,
            &palette,
            Icon::Clock,
            "No recent plays",
            "Songs you finish listening to appear here.",
        );
        return;
    }

    if let Some(error) = error {
        widgets::error_row(ui, app, &error, None);
        ui.add_space(6.0);
    }
    widgets::virtual_rows(ui, items.len(), theme::COMPACT_ROW_HEIGHT, |ui, index| {
        let entry = &items[index];
        let item = PlayableItem::Track(entry.track.clone());
        let context = RowContext::Songs {
            songs: vec![entry.track.clone()],
            mode: SongListMode::Finite,
        };
        widgets::track_row(
            ui,
            app,
            TrackRow {
                index,
                number: None,
                item: &item,
                context: &context,
                show_cover: true,
                show_album: false,
                added_at: entry.played_at.as_deref(),
                playlist_index: None,
                compact: true,
                thin: false,
                shift: 0.0,
                picked: false,
                picked_songs: &[],
            },
        );
    });
    if loading {
        ui.add_space(8.0);
        widgets::loading_row(ui, &palette);
    }
}

fn queue_row(app: &mut App, ui: &mut egui::Ui, items: &[QueueEntry], index: usize, compact: bool) {
    let entry = &items[index];
    let item = PlayableItem::Track(entry.song.clone());
    let context = RowContext::Queue(entry.occurrence_id);
    widgets::track_row(
        ui,
        app,
        TrackRow {
            index,
            number: Some(index + 1),
            item: &item,
            context: &context,
            show_cover: true,
            show_album: !compact,
            added_at: None,
            playlist_index: None,
            compact,
            thin: false,
            shift: 0.0,
            picked: false,
            picked_songs: &[],
        },
    );
}
