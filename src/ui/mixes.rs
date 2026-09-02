//! Daily and server-random mix collection pages.

use crate::api::models::{PlayableItem, Song};
use crate::app::App;
use crate::model::{Action, Loadable, Page, RowContext};
use crate::theme::{self, Icon};
use crate::util;

use super::collection::{self, Actions, AuxiliaryAction, Hero, Table, TableItem};
use super::widgets;

#[derive(Clone, Copy, PartialEq, Eq)]
enum MixKind {
    Daily,
    Random,
}

impl MixKind {
    fn page(self) -> Page {
        match self {
            Self::Daily => Page::DailyMix,
            Self::Random => Page::RandomMix,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Daily => "Daily mix",
            Self::Random => "Random mix",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Daily => {
                "Made for today from your listening history, favorites, artists, and genres."
            }
            Self::Random => "A fresh selection from your Navidrome server.",
        }
    }
}

pub fn daily(app: &mut App, ui: &mut egui::Ui) {
    let songs = app.home.daily_mix.clone();
    let revision = app.home.daily_mix_revision;
    show(app, ui, MixKind::Daily, songs, revision, false);
}

pub fn random(app: &mut App, ui: &mut egui::Ui) {
    let songs = app.home.random_songs.clone();
    let revision = app.home.random_mix_revision;
    let refreshing = app.home.random_refreshing;
    show(app, ui, MixKind::Random, songs, revision, refreshing);
}

fn show(
    app: &mut App,
    ui: &mut egui::Ui,
    kind: MixKind,
    songs: Loadable<Vec<Song>>,
    revision: u64,
    refreshing: bool,
) {
    let palette = app.palette;
    match songs {
        Loadable::Loaded(mut songs) => {
            songs.truncate(crate::mixes::MIX_SIZE);
            loaded(app, ui, kind, songs, revision, refreshing);
        }
        Loadable::Loading | Loadable::NotLoaded => {
            mix_hero(app, ui, kind, &[]);
            if kind == MixKind::Random {
                actions(app, ui, kind, Vec::new(), true, None);
            }
            widgets::loading_row(ui, &palette);
        }
        Loadable::Failed(message) => {
            mix_hero(app, ui, kind, &[]);
            if kind == MixKind::Random {
                random_error(app, ui, &message);
            } else {
                widgets::error_row(ui, app, &message, Some(Page::DailyMix));
            }
        }
    }
}

fn loaded(
    app: &mut App,
    ui: &mut egui::Ui,
    kind: MixKind,
    songs: Vec<Song>,
    revision: u64,
    refreshing: bool,
) {
    mix_hero(app, ui, kind, &songs);

    let page = kind.page();
    let filter_id = egui::Id::new(("mix-filter", &page));
    let mut filter = ui
        .data(|data| data.get_temp::<String>(filter_id))
        .unwrap_or_default();
    let items: Vec<TableItem> = songs
        .iter()
        .cloned()
        .map(|song| (PlayableItem::Track(song), None, None))
        .collect();
    let table_view = collection::prepare_table_view(
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
    actions(
        app,
        ui,
        kind,
        visible_songs.clone(),
        refreshing,
        Some(&mut filter),
    );
    ui.data_mut(|data| data.insert_temp(filter_id, filter.clone()));

    if items.is_empty() {
        let (icon, title, body) = match kind {
            MixKind::Daily => (
                Icon::Sparkles,
                "No Daily mix yet",
                "Play or favorite some music first, then today's mix will appear here.",
            ),
            MixKind::Random => (
                Icon::Shuffle,
                "No songs in this mix",
                "Refresh to ask your server for another Random mix.",
            ),
        };
        widgets::empty_state(ui, &app.palette, icon, title, body);
        return;
    }

    collection::table(
        app,
        ui,
        Table {
            items: &items,
            context: RowContext::Songs(visible_songs),
            show_album: true,
            show_cover: true,
            show_added: false,
            page,
            loading: false,
            error: None,
            can_load_more: false,
            filter: &filter,
            items_revision: revision,
        },
    );
}

fn mix_hero(app: &mut App, ui: &mut egui::Ui, kind: MixKind, songs: &[Song]) {
    let count = songs.len();
    let duration: u64 = songs.iter().map(|song| song.duration_ms as u64).sum();
    let mut byline = Vec::new();
    if let Some(user) = &app.user {
        byline.push((user.name().to_string(), None));
    }
    if count > 0 {
        byline.push((
            format!(
                "{} songs, {}",
                util::format_count(count as u64),
                util::format_total_ms(duration)
            ),
            None,
        ));
    }
    collection::hero(
        app,
        ui,
        Hero {
            image: songs.first().and_then(|song| song.image(300)),
            favorite: false,
            kind: "Mix",
            title: kind.title(),
            description: Some(kind.description().to_string()),
            byline,
            round: false,
        },
    );
}

fn actions(
    app: &mut App,
    ui: &mut egui::Ui,
    kind: MixKind,
    songs: Vec<Song>,
    refreshing: bool,
    filter: Option<&mut String>,
) {
    let auxiliary = (kind == MixKind::Random).then_some(AuxiliaryAction {
        icon: Icon::Refresh,
        label: "Refresh",
        tooltip: if refreshing {
            "Refreshing…"
        } else {
            "Generate another Random mix"
        },
        loading: refreshing,
        action: Action::RefreshRandomMix,
    });
    collection::actions_row(
        app,
        ui,
        Actions {
            play_context: None,
            view: Some(songs),
            saved: None,
            saved_icons: (Icon::Heart, Icon::HeartFilled),
            saved_tooltips: ("", ""),
            owned_playlist: None,
            name: kind.title(),
            auxiliary,
        },
        filter,
    );
}

fn random_error(app: &mut App, ui: &mut egui::Ui, message: &str) {
    let palette = app.palette;
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        theme::icon(ui, Icon::CircleAlert, 16.0, palette.danger);
        theme::text(ui, message, theme::regular(13.0), palette.secondary);
        if theme::soft_button(ui, &palette, Some(Icon::Refresh), "Retry", false).clicked() {
            app.actions.push(Action::RefreshRandomMix);
        }
    });
}
