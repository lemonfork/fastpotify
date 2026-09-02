//! Sample data for screenshots and headless rendering tests.
//!
//! Demo mode stays offline: no credentials are restored, no Navidrome request
//! is sent, and every visible surface is filled from provider-neutral sample
//! state that matches the current application model.

use std::time::Instant;

use jiff::{SignedDuration, Timestamp};

use crate::api::{
    Album, Artist, ArtistRef, Image, MediaId, MediaKind, MusicFolder, OpenSubsonicExtension,
    Page as ApiPage, PlayHistory, Playlist, PlaylistItem, ProfileId, SearchResults,
    ServerCapabilities, Song, User, UserRef, UserRoles, VerifiedServer,
};
use crate::app::App;
use crate::backend::AuthStatus;
use crate::model::*;
use crate::player::{DemoPlayback, Playback, RepeatMode};

const DEMO_PROFILE: &str = "1c12d49145ac5cb1b31a9ef0baa6ef7372d70ee0";

const ARTISTS: &[&str] = &[
    "Bonobo",
    "Khruangbin",
    "Nils Frahm",
    "Little Simz",
    "Floating Points",
    "Jon Hopkins",
    "Sault",
    "Four Tet",
];

const ALBUMS: &[(&str, usize, u32)] = &[
    ("Fragments", 0, 2022),
    ("Mordechai", 1, 2020),
    ("All Melody", 2, 2018),
    ("Sometimes I Might Be Introvert", 3, 2021),
    ("Promises", 4, 2021),
    ("Immunity", 5, 2013),
    ("Untitled (Black Is)", 6, 2020),
    ("There Is Love in You", 7, 2010),
];

const SONGS: &[&str] = &[
    "Rosewood",
    "Otomo",
    "Shadows",
    "Tides",
    "Elysian",
    "Closer",
    "Counterpart",
    "Sapien",
    "From You",
    "Day by Day",
    "Age of Phase",
    "Polyghost",
    "Time Moves Slow",
    "August 10",
    "So Rare",
    "Fugue",
    "Encores",
    "Sunlight",
    "My Friend the Forest",
    "Kaleidoscope",
];

const PLAYLISTS: &[&str] = &[
    "Monday picks",
    "Late night focus",
    "Sunday morning",
    "Running 2026",
    "Fresh arrivals",
    "Berlin nights",
    "Dinner party",
    "Deep work",
    "Road trip",
    "Kitchen jams",
];

fn profile() -> ProfileId {
    ProfileId::new(DEMO_PROFILE)
}

fn media(kind: MediaKind, raw: impl Into<String>) -> MediaId {
    MediaId::new(profile(), kind, raw)
}

fn artist_id(index: usize) -> MediaId {
    media(MediaKind::Artist, format!("art{index}"))
}

fn album_id(index: usize) -> MediaId {
    media(MediaKind::Album, format!("alb{index}"))
}

fn song_id(index: usize) -> MediaId {
    media(MediaKind::Song, format!("trk{index}"))
}

fn playlist_id(index: usize) -> MediaId {
    media(MediaKind::Playlist, format!("pl{index}"))
}

fn cover(id: impl Into<String>, width: u32) -> Image {
    Image {
        url: crate::api::ArtworkRef::new(profile(), id).uri(),
        width: Some(width),
        height: Some(width),
    }
}

fn images(seed: u32) -> Vec<Image> {
    vec![
        cover(format!("cover-{seed}-640"), 640),
        cover(format!("cover-{seed}-300"), 300),
        cover(format!("cover-{seed}-64"), 64),
    ]
}

fn artist_ref(index: usize) -> ArtistRef {
    let id = artist_id(index);
    ArtistRef {
        id: Some(id.clone()),
        name: ARTISTS[index % ARTISTS.len()].to_string(),
        uri: Some(id.uri()),
    }
}

fn artist(index: usize) -> Artist {
    let id = artist_id(index);
    let albums: Vec<Album> = ALBUMS
        .iter()
        .enumerate()
        .filter(|(_, (_, artist_index, _))| *artist_index == index % ARTISTS.len())
        .map(|(album_index, _)| album(album_index))
        .collect();
    let starred = index == 0;
    Artist {
        id: id.clone(),
        name: ARTISTS[index % ARTISTS.len()].to_string(),
        uri: id.uri(),
        images: images(100 + index as u32),
        genres: vec!["electronic".into(), "ambient".into(), "downtempo".into()],
        album_count: albums.len() as u32,
        albums,
        starred,
        starred_at: starred.then_some("2026-08-28T09:00:00Z".into()),
    }
}

fn album(index: usize) -> Album {
    let id = album_id(index);
    let (name, artist_index, year) = ALBUMS[index % ALBUMS.len()];
    let starred = index == 0;
    Album {
        id: id.clone(),
        name: name.to_string(),
        uri: id.uri(),
        images: images(200 + index as u32),
        artists: vec![artist_ref(artist_index)],
        release_date: Some(format!("{year}-03-1{}", index % 9)),
        year: Some(year),
        genres: vec!["electronic".into(), "ambient".into()],
        total_tracks: Some(12),
        duration_ms: 42 * 60 * 1000 + index as u32 * 17_000,
        tracks: None,
        starred,
        starred_at: starred.then_some("2026-08-27T07:00:00Z".into()),
    }
}

fn song(index: usize) -> Song {
    let album_index = index % ALBUMS.len();
    let mut album = album(album_index);
    album.tracks = None;
    let id = song_id(index);
    let starred = index.is_multiple_of(3);
    Song {
        id: id.clone(),
        name: SONGS[index % SONGS.len()].to_string(),
        uri: id.uri(),
        duration_ms: 180_000 + (index as u32 * 37_000) % 240_000,
        artists: vec![artist_ref(album_index)],
        album: Some(album),
        track_number: Some((index % 12) as u32 + 1),
        disc_number: Some(1),
        year: Some(ALBUMS[album_index].2),
        genres: vec!["electronic".into(), "ambient".into()],
        content_type: Some(if index.is_multiple_of(4) {
            "audio/flac".into()
        } else {
            "audio/mpeg".into()
        }),
        suffix: Some(if index.is_multiple_of(4) {
            "flac".into()
        } else {
            "mp3".into()
        }),
        bit_rate: Some(320),
        size: Some(9_000_000 + index as u64 * 125_000),
        starred,
        starred_at: starred.then_some("2026-08-26T06:30:00Z".into()),
    }
}

fn playlist_seed_songs(index: usize) -> Vec<Song> {
    let count = 12 + index % 5;
    (0..count).map(|offset| song(index * 3 + offset)).collect()
}

fn playlist(index: usize) -> Playlist {
    let preview = playlist_seed_songs(index);
    let id = playlist_id(index);
    let server_owned = matches!(index, 0 | 4);
    Playlist {
        id: id.clone(),
        name: PLAYLISTS[index % PLAYLISTS.len()].to_string(),
        uri: id.uri(),
        description: Some(if server_owned {
            "Freshly rotated from this Navidrome library.".into()
        } else {
            "A hand-picked mix for the week.".into()
        }),
        images: images(300 + index as u32),
        owner: UserRef {
            id: Some(if server_owned {
                "navidrome".into()
            } else {
                "demo".into()
            }),
            display_name: Some(if server_owned {
                "Navidrome".into()
            } else {
                "Carmine".into()
            }),
        },
        public: Some(index.is_multiple_of(2)),
        readonly: false,
        track_count: preview.len() as u32,
        duration_ms: preview.iter().map(|track| track.duration_ms).sum(),
        created: Some(format!("2026-07-{:02}T12:00:00Z", 1 + index)),
        changed: Some(format!("2026-08-{:02}T18:30:00Z", 10 + index)),
        entries: Vec::new(),
    }
}

fn playlist_with_entries(index: usize, songs: Vec<Song>) -> Playlist {
    let mut playlist = playlist(index);
    let now = demo_now();
    playlist.entries = songs
        .into_iter()
        .enumerate()
        .map(|(row, track)| PlaylistItem {
            index: row as u32,
            added_at: Some(demo_added_at(row, now)),
            track,
        })
        .collect();
    playlist.track_count = playlist.entries.len() as u32;
    playlist.duration_ms = playlist
        .entries
        .iter()
        .map(|entry| entry.track.duration_ms)
        .sum();
    playlist
}

fn demo_now() -> Timestamp {
    "2026-09-01T00:00:00Z"
        .parse()
        .expect("the fixed demo timestamp is valid")
}

fn page<T>(items: Vec<T>) -> ApiPage<T> {
    let total = items.len() as u32;
    ApiPage {
        items,
        total: Some(total),
        limit: total.max(1),
        offset: 0,
        next: None,
    }
}

fn demo_added_at(index: usize, now: Timestamp) -> String {
    let age = match index {
        0 => SignedDuration::from_secs(30),
        1 => SignedDuration::from_mins(5),
        2 => SignedDuration::from_hours(3),
        3 => SignedDuration::from_hours(2 * 24),
        4 => SignedDuration::from_hours(2 * 7 * 24),
        _ => SignedDuration::from_hours((35 + index as i64) * 24),
    };
    (now - age).to_string()
}

pub(crate) fn verified_server() -> VerifiedServer {
    VerifiedServer {
        profile: profile(),
        user: User {
            id: "demo".into(),
            display_name: Some("Carmine".into()),
            scrobbling_enabled: true,
            max_bit_rate: Some(320),
            roles: UserRoles {
                settings: true,
                download: true,
                playlist: true,
                cover_art: true,
                stream: true,
                ..UserRoles::default()
            },
        },
        music_folders: vec![MusicFolder {
            id: "main".into(),
            name: "Library".into(),
        }],
        capabilities: ServerCapabilities {
            protocol_version: "1.16.1".into(),
            server_type: Some("navidrome".into()),
            server_version: Some("0.54.0".into()),
            open_subsonic: true,
            extensions: vec![OpenSubsonicExtension {
                name: "formPost".into(),
                versions: vec![1],
            }],
        },
    }
}

pub fn populate(app: &mut App) {
    app.backend.set_offline(true);
    app.offline = true;
    app.login_server = "https://music.example.test".into();
    app.login_username = "demo".into();
    app.login_password.clear();
    let verified = verified_server();
    app.demo_connect(verified.clone());
    app.auth = AuthStatus::Connected(Box::new(verified));

    let songs: Vec<Song> = (0..40).map(song).collect();
    let playlists: Vec<Playlist> = (0..PLAYLISTS.len()).map(playlist).collect();
    app.library.playlists = Loadable::Loaded(playlists.clone());

    let playlist_one_songs = songs.iter().take(30).cloned().collect::<Vec<_>>();
    let playlist_zero_songs = songs.iter().rev().take(18).cloned().collect::<Vec<_>>();
    let playlist_one = playlist_with_entries(1, playlist_one_songs.clone());
    let playlist_zero = playlist_with_entries(0, playlist_zero_songs.clone());

    let mut playlist_page = PlaylistPage {
        playlist: Loadable::Loaded(playlist_one.clone()),
        ..PlaylistPage::default()
    };
    playlist_page
        .items
        .absorb(0, page(playlist_one.entries.clone()));
    app.playlist_pages
        .insert(playlist_one.id.clone(), playlist_page);

    let mut discover_page = PlaylistPage {
        playlist: Loadable::Loaded(playlist_zero.clone()),
        ..PlaylistPage::default()
    };
    discover_page
        .items
        .absorb(0, page(playlist_zero.entries.clone()));
    app.playlist_pages
        .insert(playlist_zero.id.clone(), discover_page);

    let album_tracks = songs.iter().take(12).cloned().collect::<Vec<_>>();
    let album_zero = Album {
        tracks: Some(page(album_tracks.clone())),
        ..album(0)
    };
    let mut album_page = AlbumPage {
        album: Loadable::Loaded(album_zero.clone()),
        ..AlbumPage::default()
    };
    album_page.tracks.absorb(0, page(album_tracks));
    app.album_pages.insert(album_zero.id.clone(), album_page);

    let mut artist_zero = artist(0);
    artist_zero.albums = (0..4).map(album).collect();
    artist_zero.album_count = artist_zero.albums.len() as u32;
    let mut artist_page = ArtistPage {
        artist: Loadable::Loaded(artist_zero.clone()),
        ..ArtistPage::default()
    };
    artist_page.albums.set_cached(artist_zero.albums.clone());
    app.artist_pages.insert(artist_zero.id.clone(), artist_page);

    let favorite_songs = songs
        .iter()
        .filter(|song| song.starred)
        .cloned()
        .collect::<Vec<_>>();
    app.library.favorite_songs.set_cached(favorite_songs);
    app.library.albums.set_cached((0..8).map(album).collect());
    app.library.artists.set_cached((0..8).map(artist).collect());

    app.home.requested = true;
    app.home.loaded_at = Some(Instant::now());
    app.home.recently_added = Loadable::Loaded((0..8).map(album).collect());
    app.home.frequent_albums = Loadable::Loaded((1..9).map(album).collect());
    app.home.daily_mix =
        Loadable::Loaded(songs.iter().take(crate::mixes::MIX_SIZE).cloned().collect());
    app.home.random_songs = Loadable::Loaded(
        songs
            .iter()
            .rev()
            .take(crate::mixes::MIX_SIZE)
            .cloned()
            .collect(),
    );
    app.home.random_refreshing = false;
    let home_now = demo_now();
    app.home.recently_played = Loadable::Loaded(
        songs
            .iter()
            .skip(5)
            .take(12)
            .enumerate()
            .map(|(index, track)| PlayHistory {
                track: track.clone(),
                played_at: Some(demo_added_at(index, home_now)),
                context: Some(playlist_id(1).uri()),
            })
            .collect(),
    );

    let recents_now = demo_now();
    let recents = songs
        .iter()
        .skip(2)
        .take(24)
        .enumerate()
        .map(|(index, track)| PlayHistory {
            track: track.clone(),
            played_at: Some(demo_added_at(index, recents_now)),
            context: Some(playlist_id(index % PLAYLISTS.len()).uri()),
        })
        .collect::<Vec<_>>();
    app.recents.absorb(
        0,
        ApiPage {
            items: recents.clone(),
            total: None,
            limit: recents.len() as u32,
            offset: 0,
            next: Some(recents.len() as u32),
        },
    );
    app.recents_view = recents;

    app.search.query = "Bonobo".into();
    app.search.committed = "Bonobo".into();
    app.search.results = Loadable::Loaded(SearchResults {
        tracks: Some(page(songs.iter().take(10).cloned().collect())),
        artists: Some(page((0..6).map(artist).collect())),
        albums: Some(page((0..6).map(album).collect())),
        playlists: Some(page(playlists.iter().take(6).cloned().collect())),
    });
    app.settings.search_history =
        vec!["Khruangbin".into(), "ambient".into(), "Monday picks".into()];

    app.demo_set_playback(
        crate::player::demo_snapshot(DemoPlayback {
            current: Some(songs[0].clone()),
            manual: vec![songs[12].clone(), songs[12].clone(), songs[13].clone()],
            context: songs.iter().skip(1).take(12).cloned().collect(),
            position_ms: 83_000,
            playback: Playback::Playing,
            volume: app.settings.volume,
            shuffle: false,
            repeat: RepeatMode::Off,
        }),
        Some(playlist_id(1)),
    );
    app.demo_rebuild_saved_state();
    app.open(Page::Home);
}

#[cfg(feature = "demo")]
fn show_resume(app: &mut App, advance: bool) {
    let songs = app.queue_playlist_songs();
    let index = usize::from(advance).min(songs.len().saturating_sub(1));
    let Some(current) = songs.get(index).cloned() else {
        return;
    };
    let context = Some(playlist_id(1));
    app.demo_set_playback(
        crate::player::demo_snapshot(DemoPlayback {
            current: Some(current),
            manual: Vec::new(),
            context: songs.into_iter().skip(index + 1).collect(),
            position_ms: if advance { 0 } else { 19_566 },
            playback: Playback::Paused,
            volume: app.settings.volume,
            shuffle: false,
            repeat: RepeatMode::Off,
        }),
        context,
    );
}

fn demo_page(text: &str) -> Option<Page> {
    Page::decode(text).or_else(|| {
        let (kind, raw) = text.split_once(':')?;
        match kind {
            "playlist" => Some(Page::Playlist(media(MediaKind::Playlist, raw))),
            "album" => Some(Page::Album(media(MediaKind::Album, raw))),
            "artist" => Some(Page::Artist(media(MediaKind::Artist, raw))),
            _ => None,
        }
    })
}

#[cfg(feature = "demo")]
fn sample_lyrics() -> crate::lyrics::Lyrics {
    let lines = [
        (40_000, "Streetlights blinking down the river road"),
        (46_500, "Every window holding someone's evening"),
        (53_000, "I keep the radio low so you can sleep"),
        (59_500, "Counting mile markers like a rosary"),
        (66_000, "We left the city with the tank half full"),
        (72_500, "And a map that only shows the way back"),
        (79_000, "But the night is wide and the road is long"),
        (85_500, "And there's nowhere I would rather be"),
        (92_000, "Coffee going cold in the cup holder"),
        (98_500, "Your hand asleep on the gear stick"),
        (105_000, "Somewhere past the county line"),
        (111_500, "The stars come out to see us through"),
        (118_000, "Still the night is wide and the road is long"),
        (124_500, "And there's nowhere I would rather be"),
    ];
    crate::lyrics::Lyrics {
        lines: lines
            .iter()
            .map(|(at_ms, text)| crate::lyrics::Line {
                at_ms: Some(*at_ms),
                text: (*text).to_string(),
            })
            .collect(),
        synced: true,
        instrumental: false,
    }
}

/// Applies `--demo-page` and `--demo-show`.
#[cfg(feature = "demo")]
pub fn apply_flags(app: &mut App, page: Option<&str>, show: Option<&str>) {
    app.settings.winamp_window = false;
    if let Some(page) = page.and_then(demo_page) {
        app.open(page);
    }
    for surface in show.unwrap_or("").split(',').map(str::trim) {
        match surface {
            "queue" => app.show_queue_panel = true,
            "recents" => {
                app.show_queue_panel = true;
                app.queue_tab = QueueTab::Recents;
            }
            "shortcuts" => app.dialog = Some(Dialog::Shortcuts),
            "create" => {
                app.dialog = Some(Dialog::CreatePlaylist {
                    name: "Autumn drives".into(),
                    public: false,
                    songs: vec![song(1)],
                })
            }
            "edit" => {
                app.dialog = Some(Dialog::EditPlaylist {
                    id: playlist_id(1),
                    name: PLAYLISTS[1].into(),
                    description: "A hand-picked mix for the week.".into(),
                    public: false,
                });
            }
            "delete" => {
                app.dialog = Some(Dialog::ConfirmDeletePlaylist {
                    id: playlist_id(1),
                    name: PLAYLISTS[1].into(),
                });
            }
            "login" => {
                app.auth = AuthStatus::SignedOut;
                app.user = None;
            }
            "settings" => app.open(Page::Settings),
            "light" => {
                app.settings.theme = crate::settings::ThemeChoice::Light;
                app.actions.push(Action::SettingsChanged);
            }
            "dark" => {
                app.settings.theme = crate::settings::ThemeChoice::Dark;
                app.actions.push(Action::SettingsChanged);
            }
            "focus" => app.settings.sidebar_visible = false,
            "resume" => show_resume(app, false),
            "resume-next" => show_resume(app, true),
            "winamp" => {
                app.settings.winamp_window = true;
                app.settings.skin = None;
            }
            "playlist" => app.settings.playlist_open = true,
            "shade" => app.settings.winamp_shaded = true,
            "playlist-shade" => app.settings.playlist_shaded = true,
            "eq" => {
                app.settings.eq_open = true;
                app.settings.eq_on = true;
                app.settings.eq_bands_db = crate::eq::PRESETS[13].bands_db;
            }
            "presets" => app.winamp.open_presets = true,
            "art" => app.settings.art_expanded = true,
            "small" => app.settings.skin_scale = Some(1),
            "compact" => {
                app.settings.sidebar_compact = true;
                app.settings.tracklist_compact = true;
            }
            "eq-shade" => {
                app.settings.eq_open = true;
                app.settings.eq_shaded = true;
            }
            "pins" => {
                app.settings.pinned_contexts = vec![playlist_id(2).uri(), playlist_id(4).uri()];
            }
            "sorted" => {
                app.table_sorts.insert(
                    Page::Playlist(playlist_id(1)),
                    TableSort {
                        column: SortColumn::Added,
                        ascending: false,
                    },
                );
            }
            "lyrics" => {
                app.lyrics = Loadable::Loaded(Some(sample_lyrics()));
                app.lyrics_following = true;
                app.show_lyrics_panel = true;
            }
            // Mixed-script titles make fallback coverage and baseline changes
            // visible in the deterministic playlist screenshot.
            "scripts" => {
                let titles = [
                    ("Fastpotify 中文测试", "Inter 与系统字体"),
                    ("夜に駆ける", "YOASOBI"),
                    ("起风了", "买辣椒也用券"),
                    ("봄여름가을겨울 (Still Life)", "BIGBANG"),
                    ("打上花火", "DAOKO, 米津玄師"),
                    ("光年之外", "G.E.M. 邓紫棋"),
                    ("밤편지", "IU"),
                    ("Lemon", "米津玄師"),
                ];
                let rename = |track: &mut Song, (title, artist): (&str, &str)| {
                    track.name = title.to_string();
                    track.artists = vec![ArtistRef {
                        id: None,
                        name: artist.to_string(),
                        uri: None,
                    }];
                };
                if let Some(page) = app.playlist_pages.get_mut(&playlist_id(1)) {
                    for (entry, names) in page.items.items.iter_mut().zip(titles) {
                        rename(&mut entry.track, names);
                    }
                    if let Some(playlist) = page.playlist.get_mut() {
                        playlist.entries.clone_from(&page.items.items);
                    }
                }
                if let Loadable::Loaded(playlists) = &mut app.library.playlists {
                    let names = ["通勤のBGM", "睡前歌单", "출근길 플레이리스트"];
                    for (playlist, name) in playlists.iter_mut().skip(3).zip(names) {
                        playlist.name = name.to_string();
                    }
                }
            }
            _ => {}
        }
    }
    if matches!(app.page(), Page::Settings) {
        app.settings.bitrate = 320;
        app.settings.audio_device = None;
        app.settings.keep_playing_in_background = true;
        app.settings.check_for_updates = true;
        app.settings.audio_buffer_ms = crate::sink::DEFAULT_BUFFER_MS;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppOptions;
    use crate::paths::AppDirs;
    use crate::settings::Settings;

    const SETTINGS_PANELS: [(&str, &str); 7] = [
        ("Account", "Signed in to your music server"),
        ("Playback", "Maximum streaming bitrate"),
        ("Appearance", "Colour from album art"),
        ("Winamp skins", "Mini player"),
        (
            "Equalizer",
            "A ten-band equalizer for playback on this computer.",
        ),
        ("Storage", "Artwork cache"),
        (
            "About",
            "Built with Rust and egui for OpenSubsonic music servers.",
        ),
    ];

    fn page_playlist(index: usize) -> Page {
        Page::Playlist(playlist_id(index))
    }

    fn page_album(index: usize) -> Page {
        Page::Album(album_id(index))
    }

    fn page_artist(index: usize) -> Page {
        Page::Artist(artist_id(index))
    }

    fn missing_playlist() -> Page {
        Page::Playlist(media(MediaKind::Playlist, "missing"))
    }

    fn frame(ctx: &egui::Context, app: &mut App) {
        frame_events(ctx, app, Vec::new());
    }

    fn frame_events(ctx: &egui::Context, app: &mut App, events: Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            app.frame_ui(ui);
        });
        output.textures_delta.clear();
    }

    fn painted_text(shapes: &[egui::epaint::ClippedShape]) -> Vec<(String, egui::Rect)> {
        fn walk(shape: &egui::epaint::Shape, text: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::epaint::Shape::Text(shape) => text.push((
                    shape.galley.job.text.clone(),
                    egui::Rect::from_min_size(shape.pos, shape.galley.size()),
                )),
                egui::epaint::Shape::Vec(shapes) => {
                    shapes.iter().for_each(|shape| walk(shape, text));
                }
                _ => {}
            }
        }

        let mut text = Vec::new();
        for clipped in shapes {
            walk(&clipped.shape, &mut text);
        }
        text
    }

    fn painted_rects(shapes: &[egui::epaint::ClippedShape]) -> Vec<egui::Rect> {
        fn walk(shape: &egui::epaint::Shape, rects: &mut Vec<egui::Rect>) {
            match shape {
                egui::epaint::Shape::Rect(shape) => rects.push(shape.rect),
                egui::epaint::Shape::Vec(shapes) => {
                    shapes.iter().for_each(|shape| walk(shape, rects));
                }
                _ => {}
            }
        }

        let mut rects = Vec::new();
        for clipped in shapes {
            walk(&clipped.shape, &mut rects);
        }
        rects
    }

    fn painted_textures(
        shapes: &[egui::epaint::ClippedShape],
    ) -> Vec<(egui::Rect, egui::TextureId)> {
        fn walk(shape: &egui::epaint::Shape, textures: &mut Vec<(egui::Rect, egui::TextureId)>) {
            match shape {
                egui::epaint::Shape::Rect(shape) => {
                    if let Some(brush) = &shape.brush {
                        textures.push((shape.rect, brush.fill_texture_id));
                    }
                }
                egui::epaint::Shape::Vec(shapes) => {
                    shapes.iter().for_each(|shape| walk(shape, textures));
                }
                _ => {}
            }
        }

        let mut textures = Vec::new();
        for clipped in shapes {
            walk(&clipped.shape, &mut textures);
        }
        textures
    }

    fn clipped_painted_text(
        shapes: &[egui::epaint::ClippedShape],
    ) -> Vec<(String, egui::Rect, egui::Rect)> {
        fn walk(
            shape: &egui::epaint::Shape,
            clip_rect: egui::Rect,
            text: &mut Vec<(String, egui::Rect, egui::Rect)>,
        ) {
            match shape {
                egui::epaint::Shape::Text(shape) => {
                    let rect = egui::Rect::from_min_size(shape.pos, shape.galley.size());
                    let visible = rect.intersect(clip_rect);
                    if visible.is_positive() {
                        text.push((shape.galley.job.text.clone(), rect, visible));
                    }
                }
                egui::epaint::Shape::Vec(shapes) => {
                    shapes.iter().for_each(|shape| walk(shape, clip_rect, text));
                }
                _ => {}
            }
        }

        let mut text = Vec::new();
        for clipped in shapes {
            walk(&clipped.shape, clipped.clip_rect, &mut text);
        }
        text
    }

    fn app_output(
        ctx: &egui::Context,
        app: &mut App,
        viewport: egui::Rect,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(viewport),
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| app.frame_ui(ui));
        output.textures_delta.clear();
        output
    }

    // `App::frame_ui` drains actions after drawing. These settings-only
    // harnesses keep the raw `Action` queue visible for tests that verify the
    // page still emits the expected commands.
    fn settings_output(
        ctx: &egui::Context,
        app: &mut App,
        viewport: egui::Rect,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(viewport),
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            ui.set_min_size(ui.available_size());
            crate::ui::settings::show(app, ui);
        });
        output.textures_delta.clear();
        output
    }

    fn click_settings(
        ctx: &egui::Context,
        app: &mut App,
        viewport: egui::Rect,
        position: egui::Pos2,
    ) {
        let pointer = |pressed| egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let _ = settings_output(
            ctx,
            app,
            viewport,
            vec![egui::Event::PointerMoved(position), pointer(true)],
        );
        let _ = settings_output(
            ctx,
            app,
            viewport,
            vec![egui::Event::PointerMoved(position), pointer(false)],
        );
    }

    fn click_app(ctx: &egui::Context, app: &mut App, viewport: egui::Rect, position: egui::Pos2) {
        let pointer = |pressed| egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let _ = app_output(
            ctx,
            app,
            viewport,
            vec![egui::Event::PointerMoved(position), pointer(true)],
        );
        let _ = app_output(
            ctx,
            app,
            viewport,
            vec![egui::Event::PointerMoved(position), pointer(false)],
        );
    }

    fn home_output(
        ctx: &egui::Context,
        app: &mut App,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| crate::ui::home::show(app, ui));
        output.textures_delta.clear();
        output
    }

    fn click_home(ctx: &egui::Context, app: &mut App, pos: egui::Pos2) {
        let pointer = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let _ = home_output(
            ctx,
            app,
            vec![egui::Event::PointerMoved(pos), pointer(true)],
        );
        let _ = home_output(
            ctx,
            app,
            vec![egui::Event::PointerMoved(pos), pointer(false)],
        );
    }

    fn mix_output(
        ctx: &egui::Context,
        app: &mut App,
        page: Page,
        events: Vec<egui::Event>,
    ) -> (Vec<(String, egui::Rect)>, Vec<egui::Rect>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| match &page {
            Page::DailyMix => crate::ui::mixes::daily(app, ui),
            Page::RandomMix => crate::ui::mixes::random(app, ui),
            page => panic!("not a Mix page: {page:?}"),
        });
        output.textures_delta.clear();

        fn collect_large_buttons(shape: &egui::epaint::Shape, buttons: &mut Vec<egui::Rect>) {
            match shape {
                egui::epaint::Shape::Circle(shape) if (shape.radius - 28.0).abs() < 0.1 => {
                    buttons.push(egui::Rect::from_center_size(
                        shape.center,
                        egui::Vec2::splat(shape.radius * 2.0),
                    ));
                }
                egui::epaint::Shape::Vec(shapes) => {
                    shapes
                        .iter()
                        .for_each(|shape| collect_large_buttons(shape, buttons));
                }
                _ => {}
            }
        }

        let text = painted_text(&output.shapes);
        let mut buttons = Vec::new();
        for clipped in &output.shapes {
            collect_large_buttons(&clipped.shape, &mut buttons);
        }
        (text, buttons)
    }

    fn click_mix(ctx: &egui::Context, app: &mut App, page: Page, pos: egui::Pos2) {
        let pointer = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        mix_output(
            ctx,
            app,
            page.clone(),
            vec![egui::Event::PointerMoved(pos), pointer(true)],
        );
        mix_output(
            ctx,
            app,
            page,
            vec![egui::Event::PointerMoved(pos), pointer(false)],
        );
    }

    fn make_app(tag: &str) -> (egui::Context, App, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "fastpotify-{tag}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let ctx = egui::Context::default();
        let waker = crate::backend::Waker::default();
        waker.attach(&ctx);
        let mut app = App::new(
            &waker,
            dirs,
            Settings::default(),
            AppOptions {
                media_controls: false,
                tray: false,
                offline: true,
            },
        );
        app.attach(&ctx);
        populate(&mut app);
        (ctx, app, root)
    }

    fn finish(mut app: App, root: std::path::PathBuf) {
        app.backend.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn documented_demo_page_shorthands_still_decode() {
        assert_eq!(demo_page("home"), Some(Page::Home));
        assert_eq!(demo_page("daily-mix"), Some(Page::DailyMix));
        assert_eq!(demo_page("random-mix"), Some(Page::RandomMix));
        assert_eq!(demo_page("playlist:pl1"), Some(page_playlist(1)));
        assert_eq!(demo_page("album:alb0"), Some(page_album(0)));
        assert_eq!(demo_page("artist:art0"), Some(page_artist(0)));
        assert_eq!(demo_page("legacy:track"), None);
    }

    #[test]
    fn home_demo_draws_mix_shortcuts_without_song_shelves() {
        let (ctx, mut app, root) = make_app("home-mixes");
        let mut daily_only = song(40);
        daily_only.name = "Daily-only shelf sentinel".into();
        let mut random_only = song(41);
        random_only.name = "Random-only shelf sentinel".into();
        app.home.daily_mix = Loadable::Loaded(vec![daily_only]);
        app.home.random_songs = Loadable::Loaded(vec![random_only]);

        let _ = home_output(&ctx, &mut app, Vec::new());
        let output = home_output(&ctx, &mut app, Vec::new());
        let text = painted_text(&output.shapes);
        assert_eq!(
            text.iter().filter(|(text, _)| text == "Daily mix").count(),
            1
        );
        assert_eq!(
            text.iter().filter(|(text, _)| text == "Random mix").count(),
            1
        );
        assert!(
            !text
                .iter()
                .any(|(text, _)| text == "Daily-only shelf sentinel")
        );
        assert!(
            !text
                .iter()
                .any(|(text, _)| text == "Random-only shelf sentinel")
        );

        let textures = painted_textures(&output.shapes);
        let gradient_for = |label: &str| {
            let label = text
                .iter()
                .find(|(painted, _)| painted == label)
                .map(|(_, rect)| *rect)
                .unwrap_or_else(|| panic!("{label} shortcut is drawn"));
            textures
                .iter()
                .filter(|(rect, _)| {
                    (rect.width() - 60.0).abs() < 0.5
                        && (rect.height() - 60.0).abs() < 0.5
                        && rect.right() <= label.left()
                        && (rect.center().y - label.center().y).abs() < 1.0
                })
                .max_by(|(left, _), (right, _)| left.right().total_cmp(&right.right()))
                .map(|(_, texture)| *texture)
                .unwrap_or_else(|| panic!("{label:?} has no textured 60px icon background"))
        };
        let daily_gradient = gradient_for("Daily mix");
        let random_gradient = gradient_for("Random mix");
        assert_ne!(
            daily_gradient, random_gradient,
            "Daily and Random mix should keep distinct gradient backgrounds"
        );

        let daily = text
            .iter()
            .find(|(text, _)| text == "Daily mix")
            .map(|(_, rect)| rect.center())
            .expect("Daily mix shortcut is drawn");
        app.actions.clear();
        click_home(&ctx, &mut app, daily);
        assert!(matches!(
            app.actions.as_slice(),
            [Action::Open(Page::DailyMix)]
        ));

        let random = text
            .iter()
            .find(|(text, _)| text == "Random mix")
            .map(|(_, rect)| rect.center())
            .expect("Random mix shortcut is drawn");
        app.actions.clear();
        click_home(&ctx, &mut app, random);
        assert!(matches!(
            app.actions.as_slice(),
            [Action::Open(Page::RandomMix)]
        ));
        finish(app, root);
    }

    #[test]
    fn random_mix_retry_only_emits_the_targeted_refresh() {
        let (ctx, mut app, root) = make_app("random-mix-retry");
        app.home.daily_mix = Loadable::Loaded(Vec::new());
        app.home.random_songs = Loadable::Failed("Random mix failed".into());
        app.actions.clear();

        let (text, _) = mix_output(&ctx, &mut app, Page::RandomMix, Vec::new());
        let retry = text
            .iter()
            .find(|(text, _)| text == "Retry")
            .map(|(_, rect)| rect.center())
            .expect("Random mix page Retry is drawn");
        click_mix(&ctx, &mut app, Page::RandomMix, retry);

        assert!(matches!(app.actions.as_slice(), [Action::RefreshRandomMix]));
        finish(app, root);
    }

    #[test]
    fn random_mix_page_refresh_only_emits_the_targeted_refresh() {
        let (ctx, mut app, root) = make_app("random-mix-refresh");
        app.actions.clear();

        let (text, _) = mix_output(&ctx, &mut app, Page::RandomMix, Vec::new());
        let refresh = text
            .iter()
            .find(|(text, _)| text == "Refresh")
            .map(|(_, rect)| rect.center())
            .expect("Random mix page Refresh is drawn");
        click_mix(&ctx, &mut app, Page::RandomMix, refresh);

        assert!(matches!(app.actions.as_slice(), [Action::RefreshRandomMix]));
        finish(app, root);
    }

    #[test]
    fn daily_mix_page_draw_and_play_use_the_same_bounded_songs() {
        let (ctx, mut app, root) = make_app("daily-mix-bound");
        let source: Vec<_> = (0..=crate::mixes::MIX_SIZE).map(song).collect();
        app.home.daily_mix = Loadable::Loaded(source.clone());
        app.actions.clear();

        let (_, buttons) = mix_output(&ctx, &mut app, Page::DailyMix, Vec::new());
        let play = buttons
            .first()
            .map(egui::Rect::center)
            .expect("Daily mix page Play button is drawn");
        click_mix(&ctx, &mut app, Page::DailyMix, play);

        match app.actions.as_slice() {
            [Action::PlaySongs { songs, index, mode }] => {
                assert_eq!(*index, 0);
                assert_eq!(*mode, SongListMode::Finite);
                assert_eq!(songs.len(), crate::mixes::MIX_SIZE);
                assert_eq!(
                    songs.iter().map(|song| &song.id).collect::<Vec<_>>(),
                    source[..crate::mixes::MIX_SIZE]
                        .iter()
                        .map(|song| &song.id)
                        .collect::<Vec<_>>()
                );
            }
            actions => panic!("Daily mix Play emitted unexpected actions: {actions:?}"),
        }
        finish(app, root);
    }

    #[test]
    fn random_mix_page_play_marks_the_song_list_for_refill() {
        let (ctx, mut app, root) = make_app("random-mix-play-mode");
        let source: Vec<_> = (0..crate::mixes::MIX_SIZE).map(song).collect();
        app.home.random_songs = Loadable::Loaded(source.clone());
        app.actions.clear();

        let (_, buttons) = mix_output(&ctx, &mut app, Page::RandomMix, Vec::new());
        let play = buttons
            .first()
            .map(egui::Rect::center)
            .expect("Random mix page Play button is drawn");
        click_mix(&ctx, &mut app, Page::RandomMix, play);

        match app.actions.as_slice() {
            [Action::PlaySongs { songs, index, mode }] => {
                assert_eq!(*index, 0);
                assert_eq!(*mode, SongListMode::RandomMix);
                assert_eq!(
                    songs.iter().map(|song| &song.id).collect::<Vec<_>>(),
                    source.iter().map(|song| &song.id).collect::<Vec<_>>()
                );
            }
            actions => panic!("Random mix Play emitted unexpected actions: {actions:?}"),
        }
        finish(app, root);
    }

    #[test]
    fn sorted_random_mix_row_play_keeps_the_refill_mode() {
        let (ctx, mut app, root) = make_app("random-mix-row-play-mode");
        let source: Vec<_> = (0..crate::mixes::MIX_SIZE).map(song).collect();
        app.home.random_songs = Loadable::Loaded(source.clone());
        app.table_sorts.insert(
            Page::RandomMix,
            TableSort {
                column: SortColumn::Title,
                ascending: true,
            },
        );
        app.actions.clear();

        let (text, _) = mix_output(&ctx, &mut app, Page::RandomMix, Vec::new());
        let first_row = source
            .iter()
            .filter_map(|song| {
                text.iter()
                    .find(|(painted, _)| painted == &song.name)
                    .map(|(_, rect)| *rect)
            })
            .min_by(|left, right| left.top().total_cmp(&right.top()))
            .expect("Random mix song rows are drawn");
        click_mix(
            &ctx,
            &mut app,
            Page::RandomMix,
            egui::pos2(16.0, first_row.center().y),
        );

        match app.actions.as_slice() {
            [
                Action::PlayFromRow {
                    context: RowContext::Songs { songs, mode },
                    index,
                    ..
                },
            ] => {
                assert_eq!(*index, 0);
                assert_eq!(*mode, SongListMode::RandomMix);
                assert_eq!(songs.len(), crate::mixes::MIX_SIZE);
            }
            actions => panic!("Random mix row Play emitted unexpected actions: {actions:?}"),
        }
        finish(app, root);
    }

    #[cfg(feature = "demo")]
    #[test]
    fn scripts_surface_contains_a_mixed_latin_and_han_title() {
        let (_ctx, mut app, root) = make_app("scripts");
        apply_flags(&mut app, Some("playlist:pl1"), Some("scripts"));
        let page = &app.playlist_pages[&playlist_id(1)];
        assert_eq!(page.items.items[0].track.name, "Fastpotify 中文测试");
        assert_eq!(
            page.playlist.get().unwrap().entries[0].track.name,
            "Fastpotify 中文测试"
        );
        finish(app, root);
    }

    #[test]
    fn backend_bootstrap_auth_cannot_replace_the_offline_demo_session() {
        let (ctx, mut app, root) = make_app("auth-race");
        let connected = match &app.auth {
            AuthStatus::Connected(server) => server.clone(),
            status => panic!("demo did not start connected: {status:?}"),
        };
        let user = app.user.clone();

        // These are the exact events queued asynchronously by a backend that
        // starts without credential restoration. Deliver them deterministically
        // after `populate` has installed the demo session, then draw frames to
        // drain any real bootstrap events which happened to race with them.
        app.demo_receive_bootstrap_auth(AuthStatus::Starting);
        app.demo_receive_bootstrap_auth(AuthStatus::SignedOut);
        for _ in 0..3 {
            frame(&ctx, &mut app);
        }

        assert_eq!(app.auth, AuthStatus::Connected(connected));
        assert_eq!(app.user, user);
        assert!(app.is_connected());
        finish(app, root);
    }

    #[test]
    fn a_toast_is_wide_enough_to_read() {
        let (ctx, mut app, root) = make_app("toast");
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        };
        app.toast("Saved");
        for _ in 0..2 {
            frame(&ctx, &mut app);
        }
        app.toasts.clear();
        app.toast("Time Moves Slow will play next");
        let mut first = ctx.run_ui(input.clone(), |ui| app.frame_ui(ui));
        first.textures_delta.clear();
        let mut output = ctx.run_ui(input, |ui| app.frame_ui(ui));
        output.textures_delta.clear();

        fn widest_toast_text(shape: &egui::epaint::Shape) -> Option<f32> {
            match shape {
                egui::epaint::Shape::Text(text)
                    if text.galley.job.text.contains("Time Moves Slow") =>
                {
                    Some(text.galley.rect.width())
                }
                egui::epaint::Shape::Vec(shapes) => {
                    shapes.iter().filter_map(widest_toast_text).next()
                }
                _ => None,
            }
        }
        let width = output
            .shapes
            .iter()
            .filter_map(|clipped| widest_toast_text(&clipped.shape))
            .next()
            .expect("the toast's text is painted");
        assert!(width > 150.0, "toast text is only {width}px wide");
        finish(app, root);
    }

    #[test]
    fn the_shortcuts_dialog_fits_a_small_window() {
        let (ctx, mut app, root) = make_app("shortcuts");
        app.dialog = Some(Dialog::Shortcuts);

        let height = 420.0;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, height),
            )),
            ..Default::default()
        };
        let mut first = ctx.run_ui(input.clone(), |ui| app.frame_ui(ui));
        first.textures_delta.clear();
        let mut output = ctx.run_ui(input, |ui| app.frame_ui(ui));
        output.textures_delta.clear();

        let dialog = app.dialog_rect.expect("the dialog drew itself");
        assert!(
            dialog.max.y <= height + 1.0,
            "the dialog runs {} pixels past the bottom",
            dialog.max.y - height
        );
        finish(app, root);
    }

    #[test]
    fn login_form_is_fully_visible_at_the_default_window_size() {
        let (ctx, mut app, root) = make_app("login-default-window");
        app.user = None;
        app.auth = AuthStatus::SignedOut;

        let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0));
        let _ = app_output(&ctx, &mut app, viewport, Vec::new());
        let output = app_output(&ctx, &mut app, viewport, Vec::new());
        let visible_text = clipped_painted_text(&output.shapes);

        for expected in [
            "Server URL",
            "Username",
            "Password",
            "Sign in",
            "Compatible with Navidrome and OpenSubsonic servers. Your password is stored only in this app's private profile.",
        ] {
            let (_, layout, visible) = visible_text
                .iter()
                .find(|(text, _, _)| text == expected)
                .unwrap_or_else(|| panic!("{expected:?} is not visible in the default window"));
            assert_eq!(
                *layout, *visible,
                "{expected:?} is clipped in the default window"
            );
        }

        finish(app, root);
    }

    #[test]
    fn login_content_stays_in_the_minimum_window_and_scrolls_to_the_bottom() {
        let (ctx, mut app, root) = make_app("login-minimum-window");
        app.user = None;
        app.login_server = "http://music.example.test".into();
        app.login_username = "listener".into();
        app.login_password = "secret".into();
        let failure = "The server rejected this sign-in attempt after returning an unexpectedly long diagnostic response. Please verify the server address, account permissions, authentication settings, reverse proxy configuration, and network connection before trying again. ".repeat(5);
        app.auth = AuthStatus::Failed(failure.clone());

        let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(760.0, 520.0));
        let pointer = egui::pos2(380.0, 260.0);
        let _ = app_output(
            &ctx,
            &mut app,
            viewport,
            vec![egui::Event::PointerMoved(pointer)],
        );
        let mut output = app_output(
            &ctx,
            &mut app,
            viewport,
            vec![egui::Event::PointerMoved(pointer)],
        );

        fn largest_panel_rect(
            shape: &egui::epaint::Shape,
            panel: egui::Color32,
        ) -> Option<egui::Rect> {
            match shape {
                egui::epaint::Shape::Rect(shape) if shape.fill == panel => Some(shape.rect),
                egui::epaint::Shape::Vec(shapes) => shapes
                    .iter()
                    .filter_map(|shape| largest_panel_rect(shape, panel))
                    .max_by(|left, right| left.area().total_cmp(&right.area())),
                _ => None,
            }
        }

        let card = output
            .shapes
            .iter()
            .filter_map(|shape| largest_panel_rect(&shape.shape, app.palette.panel))
            .max_by(|left, right| left.area().total_cmp(&right.area()))
            .expect("the login card is painted");
        assert!(
            viewport.contains_rect(card),
            "login card {card:?} exceeds viewport {viewport:?}"
        );

        let initial_text = clipped_painted_text(&output.shapes);
        assert!(
            initial_text.iter().any(|(text, _, _)| text == "Server URL"),
            "the top of the login form is not visible"
        );
        for (text, _, visible) in &initial_text {
            assert!(
                viewport.contains_rect(*visible),
                "visible login text {text:?} exceeds viewport: {visible:?}"
            );
        }

        let scroll = |delta| egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, delta),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        };
        let mut warning_seen = false;
        let mut failure_seen = false;
        let mut bottom_reached = false;
        for _ in 0..24 {
            let _ = app_output(
                &ctx,
                &mut app,
                viewport,
                vec![egui::Event::PointerMoved(pointer), scroll(-240.0)],
            );
            output = app_output(
                &ctx,
                &mut app,
                viewport,
                vec![egui::Event::PointerMoved(pointer)],
            );
            let visible_text = clipped_painted_text(&output.shapes);
            warning_seen |= visible_text.iter().any(|(text, _, _)| {
                text.contains(
                    "HTTP is not encrypted. Other people on this network may observe your music",
                )
            });
            failure_seen |= visible_text.iter().any(|(text, _, _)| text == &failure);
            for (text, _, visible) in &visible_text {
                assert!(
                    viewport.contains_rect(*visible),
                    "visible login text {text:?} exceeds viewport: {visible:?}"
                );
            }

            bottom_reached = [
                "Sign in",
                "Compatible with Navidrome and OpenSubsonic servers. Your password is stored only in this app's private profile.",
            ]
            .iter()
            .all(|expected| {
                visible_text
                    .iter()
                    .find(|(text, _, _)| text == expected)
                    .is_some_and(|(_, layout, visible)| {
                        layout == visible && viewport.contains_rect(*layout)
                    })
            });
            if bottom_reached {
                break;
            }
        }
        assert!(warning_seen, "the HTTP security warning is not reachable");
        assert!(
            failure_seen,
            "the long authentication failure is not reachable"
        );
        assert!(
            bottom_reached,
            "the sign-in button and privacy note are not reachable after scrolling"
        );

        finish(app, root);
    }

    #[test]
    fn the_narrowest_panel_keeps_its_header_on_one_row() {
        let (ctx, mut app, root) = make_app("queue-header");
        app.show_queue_panel = true;
        app.settings.queue_width = crate::theme::SIDE_PANEL_MIN_WIDTH;

        let mut placed: Vec<(String, f32)> = Vec::new();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        };
        for _ in 0..2 {
            placed.clear();
            let mut output = ctx.run_ui(input.clone(), |ui| app.frame_ui(ui));
            output.textures_delta.clear();
            fn walk(shape: &egui::epaint::Shape, placed: &mut Vec<(String, f32)>) {
                match shape {
                    egui::epaint::Shape::Text(text) => {
                        placed.push((text.galley.job.text.clone(), text.pos.y))
                    }
                    egui::epaint::Shape::Vec(shapes) => {
                        shapes.iter().for_each(|shape| walk(shape, placed))
                    }
                    _ => {}
                }
            }
            for clipped in &output.shapes {
                walk(&clipped.shape, &mut placed);
            }
        }
        let at = |label: &str| -> f32 {
            placed
                .iter()
                .find(|(text, _)| text == label)
                .unwrap_or_else(|| panic!("{label} was never drawn: {placed:?}"))
                .1
        };
        let (queue, recents) = (at("Queue"), at("Recent"));
        assert!(
            (queue - recents).abs() < 1.0,
            "Queue at {queue}, Recent at {recents}"
        );
        finish(app, root);
    }

    #[test]
    fn settings_start_on_playback_and_only_draw_the_active_panel() {
        let (ctx, mut app, root) = make_app("settings-default-panel");
        let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1240.0, 800.0));
        app.open(Page::Settings);

        let _ = app_output(&ctx, &mut app, viewport, Vec::new());
        let output = app_output(&ctx, &mut app, viewport, Vec::new());
        let text = painted_text(&output.shapes);
        let visible_text = clipped_painted_text(&output.shapes);

        let expected_rows: &[&str] = if cfg!(windows) {
            &[
                "Maximum streaming bitrate",
                "Audio output",
                "Keep music playing when the window closes",
                "Check for updates",
                "Output buffer",
                "Playback settings applied.",
            ]
        } else {
            &[
                "Maximum streaming bitrate",
                "Audio output",
                "Keep music playing when the window closes",
                "Check for updates",
                "Playback settings applied.",
            ]
        };
        for expected in expected_rows {
            let (_, layout, visible) = visible_text
                .iter()
                .find(|(painted, _, _)| painted == *expected)
                .unwrap_or_else(|| {
                    panic!("wide Playback row {expected:?} is not visible in the default window")
                });
            assert_eq!(
                *layout, *visible,
                "wide Playback row {expected:?} is clipped in the default window"
            );
        }
        if !cfg!(windows) {
            assert!(
                !text.iter().any(|(painted, _)| painted == "Output buffer"),
                "the Windows-only output buffer setting was drawn"
            );
        }

        let background_rects = painted_rects(&output.shapes);
        let widget_around = |label: &str| {
            let text_rect = text
                .iter()
                .find(|(painted, _)| painted == label)
                .map(|(_, rect)| *rect)
                .unwrap_or_else(|| panic!("wide Playback control {label:?} is not painted"));
            background_rects
                .iter()
                .filter(|rect| {
                    rect.contains_rect(text_rect)
                        && rect.width() > text_rect.width() + 3.0
                        && rect.height() > text_rect.height() + 3.0
                })
                .min_by(|left, right| left.area().total_cmp(&right.area()))
                .copied()
                .unwrap_or_else(|| {
                    panic!("wide Playback control {label:?} has no enclosing widget shape")
                })
        };
        let bitrate_buttons = [
            widget_around("Normal · 96 kbps"),
            widget_around("High · 160 kbps"),
            widget_around("Very high · 320 kbps"),
        ];
        let bitrate_centers = bitrate_buttons
            .iter()
            .map(|rect| rect.center().y)
            .collect::<Vec<_>>();
        let top_bitrate_center = bitrate_centers
            .iter()
            .copied()
            .fold(f32::INFINITY, f32::min);
        let bottom_bitrate_center = bitrate_centers
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            bottom_bitrate_center - top_bitrate_center < 1.0,
            "wide Playback bitrate choices wrapped instead of staying on one line: {bitrate_buttons:?}"
        );

        let switch_for = |label: &str| {
            let label_rect = text
                .iter()
                .find(|(painted, _)| painted == label)
                .map(|(_, rect)| *rect)
                .unwrap_or_else(|| panic!("wide Playback row {label:?} is not painted"));
            background_rects
                .iter()
                .filter(|rect| {
                    rect.width() > rect.height()
                        && rect.width() < 80.0
                        && rect.height() < 40.0
                        && rect.left() > label_rect.right()
                        && (rect.center().y - label_rect.center().y).abs() < 24.0
                })
                .copied()
                .next()
                .unwrap_or_else(|| panic!("wide Playback row {label:?} has no aligned switch"))
        };
        let mut control_right_edges = vec![
            bitrate_buttons
                .iter()
                .map(egui::Rect::right)
                .fold(f32::NEG_INFINITY, f32::max),
            widget_around("System default").right(),
            switch_for("Keep music playing when the window closes").right(),
            switch_for("Check for updates").right(),
        ];
        if cfg!(windows) {
            let buffer_buttons = [
                widget_around("200 ms"),
                widget_around("100 ms"),
                widget_around("50 ms"),
            ];
            control_right_edges.push(
                buffer_buttons
                    .iter()
                    .map(egui::Rect::right)
                    .fold(f32::NEG_INFINITY, f32::max),
            );
        }
        let leftmost_control_edge = control_right_edges
            .iter()
            .copied()
            .fold(f32::INFINITY, f32::min);
        let rightmost_control_edge = control_right_edges
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            rightmost_control_edge - leftmost_control_edge < 1.0,
            "wide Playback controls do not share a right edge: {control_right_edges:?}"
        );

        finish(app, root);
    }

    #[test]
    fn every_settings_category_switches_to_one_distinct_panel() {
        let (ctx, mut app, root) = make_app("settings-categories");
        let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0));
        app.open(Page::Settings);

        let _ = app_output(&ctx, &mut app, viewport, Vec::new());
        let mut output = app_output(&ctx, &mut app, viewport, Vec::new());

        // Start with Account so every click, including Playback, changes the
        // selection rather than exercising an already-selected navigation row.
        for index in [0, 1, 2, 3, 4, 5, 6] {
            let (label, selected_anchor) = SETTINGS_PANELS[index];
            let navigation = painted_text(&output.shapes)
                .into_iter()
                .find(|(painted, _)| painted == label)
                .map(|(_, rect)| rect.center())
                .unwrap_or_else(|| panic!("settings navigation did not draw {label:?}"));
            click_app(&ctx, &mut app, viewport, navigation);
            output = app_output(&ctx, &mut app, viewport, Vec::new());
            let text = painted_text(&output.shapes);

            let visible_panels = SETTINGS_PANELS
                .iter()
                .filter_map(|(_, anchor)| {
                    text.iter()
                        .any(|(painted, _)| painted == anchor)
                        .then_some(*anchor)
                })
                .collect::<Vec<_>>();
            assert_eq!(
                visible_panels,
                vec![selected_anchor],
                "clicking {label:?} did not show exactly its own panel"
            );
        }

        finish(app, root);
    }

    #[test]
    fn settings_navigation_is_a_wide_rail_and_wraps_at_the_minimum_window() {
        fn nav_rects(text: &[(String, egui::Rect, egui::Rect)], wide: bool) -> Vec<egui::Rect> {
            SETTINGS_PANELS
                .iter()
                .map(|(label, _)| {
                    let candidates = text.iter().filter(|(painted, _, _)| painted == label);
                    if wide {
                        candidates
                            .min_by(|(_, left, _), (_, right, _)| {
                                left.left().total_cmp(&right.left())
                            })
                            .map(|(_, layout, _)| *layout)
                    } else {
                        candidates
                            .min_by(|(_, left, _), (_, right, _)| {
                                left.top().total_cmp(&right.top())
                            })
                            .map(|(_, layout, _)| *layout)
                    }
                    .unwrap_or_else(|| panic!("settings navigation did not draw {label:?}"))
                })
                .collect()
        }

        fn assert_horizontally_unclipped(
            text: &[(String, egui::Rect, egui::Rect)],
            label: &str,
            pick_wide_navigation_copy: Option<bool>,
        ) {
            let mut candidates = text.iter().filter(|(painted, _, _)| painted == label);
            let entry = match pick_wide_navigation_copy {
                Some(true) => candidates
                    .min_by(|(_, left, _), (_, right, _)| left.left().total_cmp(&right.left())),
                Some(false) => candidates
                    .min_by(|(_, left, _), (_, right, _)| left.top().total_cmp(&right.top())),
                None => candidates.next(),
            }
            .unwrap_or_else(|| panic!("{label:?} was not visibly painted"));
            let (_, layout, visible) = entry;
            assert!(
                (layout.left() - visible.left()).abs() < 0.5
                    && (layout.right() - visible.right()).abs() < 0.5,
                "{label:?} is horizontally clipped: layout {layout:?}, visible {visible:?}"
            );
        }

        let (wide_ctx, mut wide_app, wide_root) = make_app("settings-wide-rail");
        let wide_viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0));
        wide_app.open(Page::Settings);
        let _ = app_output(&wide_ctx, &mut wide_app, wide_viewport, Vec::new());
        let wide_output = app_output(&wide_ctx, &mut wide_app, wide_viewport, Vec::new());
        let wide_text = clipped_painted_text(&wide_output.shapes);
        let wide_nav = nav_rects(&wide_text, true);
        for (label, _) in SETTINGS_PANELS {
            assert_horizontally_unclipped(&wide_text, label, Some(true));
        }
        let playback = wide_text
            .iter()
            .find(|(painted, _, _)| painted == "Maximum streaming bitrate")
            .map(|(_, layout, _)| *layout)
            .expect("wide Playback content anchor is visible");
        let rail_right = wide_nav
            .iter()
            .map(egui::Rect::right)
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            playback.left() > rail_right + 20.0,
            "wide Playback content {playback:?} does not sit to the right of the rail {wide_nav:?}"
        );
        assert_horizontally_unclipped(&wide_text, "Maximum streaming bitrate", None);
        finish(wide_app, wide_root);

        let (narrow_ctx, mut narrow_app, narrow_root) = make_app("settings-wrapped-nav");
        let narrow_viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(760.0, 520.0));
        narrow_app.open(Page::Settings);
        let _ = app_output(&narrow_ctx, &mut narrow_app, narrow_viewport, Vec::new());
        let mut narrow_output =
            app_output(&narrow_ctx, &mut narrow_app, narrow_viewport, Vec::new());
        let narrow_text = clipped_painted_text(&narrow_output.shapes);
        let narrow_nav = nav_rects(&narrow_text, false);
        for (label, _) in SETTINGS_PANELS {
            assert_horizontally_unclipped(&narrow_text, label, Some(false));
        }
        let narrow_left = narrow_nav
            .iter()
            .map(egui::Rect::left)
            .fold(f32::INFINITY, f32::min);
        let narrow_right = narrow_nav
            .iter()
            .map(egui::Rect::right)
            .fold(f32::NEG_INFINITY, f32::max);
        let narrow_top = narrow_nav
            .iter()
            .map(egui::Rect::top)
            .fold(f32::INFINITY, f32::min);
        let narrow_bottom = narrow_nav
            .iter()
            .map(egui::Rect::bottom)
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            narrow_right - narrow_left > 100.0,
            "minimum-width Settings navigation did not flow horizontally: {narrow_nav:?}"
        );
        assert!(
            narrow_bottom - narrow_top > 32.0,
            "minimum-width Settings navigation did not wrap onto multiple rows: {narrow_nav:?}"
        );
        let playback = narrow_text
            .iter()
            .find(|(painted, _, _)| painted == "Maximum streaming bitrate")
            .map(|(_, layout, _)| *layout)
            .expect("minimum-width Playback content anchor is visible");
        assert!(
            narrow_bottom < playback.top(),
            "minimum-width navigation {narrow_nav:?} is not above Playback content {playback:?}"
        );
        assert_horizontally_unclipped(&narrow_text, "Maximum streaming bitrate", None);

        click_app(
            &narrow_ctx,
            &mut narrow_app,
            narrow_viewport,
            narrow_nav[4].center(),
        );
        narrow_output = app_output(&narrow_ctx, &mut narrow_app, narrow_viewport, Vec::new());
        let mut reached_last_band = false;
        let content_pointer = egui::pos2(680.0, 400.0);
        for _ in 0..20 {
            let text = clipped_painted_text(&narrow_output.shapes);
            if let Some((_, layout, visible)) = text.iter().find(|(painted, _, _)| painted == "16K")
            {
                assert_eq!(
                    *layout, *visible,
                    "minimum-width Equalizer last band is horizontally clipped"
                );
                reached_last_band = true;
                break;
            }
            let wheel = egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -80.0),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
            };
            let _ = app_output(
                &narrow_ctx,
                &mut narrow_app,
                narrow_viewport,
                vec![egui::Event::PointerMoved(content_pointer), wheel],
            );
            narrow_output = app_output(
                &narrow_ctx,
                &mut narrow_app,
                narrow_viewport,
                vec![egui::Event::PointerMoved(content_pointer)],
            );
        }
        assert!(
            reached_last_band,
            "minimum-width Equalizer last band is not reachable by scrolling"
        );
        finish(narrow_app, narrow_root);
    }

    #[test]
    fn long_settings_choice_groups_wrap_without_covering_following_content() {
        let (ctx, mut app, root) = make_app("settings-wrapped-choice-groups");
        let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(760.0, 1100.0));
        app.open(Page::Settings);

        let _ = app_output(&ctx, &mut app, viewport, Vec::new());
        let output = app_output(&ctx, &mut app, viewport, Vec::new());
        let equalizer = painted_text(&output.shapes)
            .into_iter()
            .find(|(painted, _)| painted == "Equalizer")
            .map(|(_, rect)| rect.center())
            .expect("settings navigation draws Equalizer");
        click_app(&ctx, &mut app, viewport, equalizer);

        let output = app_output(&ctx, &mut app, viewport, Vec::new());
        let text = clipped_painted_text(&output.shapes);
        let label_rect = |label: &str| {
            let (_, layout, visible) = text
                .iter()
                .find(|(painted, _, _)| painted == label)
                .unwrap_or_else(|| panic!("minimum-width Equalizer text {label:?} is missing"));
            assert_eq!(
                *layout, *visible,
                "minimum-width Equalizer text {label:?} is clipped"
            );
            *layout
        };

        let long_preset_bottom = [
            label_rect("Laptop Speakers / Headphones"),
            label_rect("Night Listening"),
        ]
        .into_iter()
        .map(|rect| rect.bottom())
        .fold(f32::NEG_INFINITY, f32::max);
        let response_curve = label_rect("Pre");
        assert!(
            response_curve.top() > long_preset_bottom + 4.0,
            "wrapped Equalizer presets overlap the following content: \
             presets end at {long_preset_bottom}, response curve starts at {}",
            response_curve.top()
        );

        finish(app, root);
    }

    #[test]
    fn settings_playback_apply_and_account_sign_out_keep_their_actions() {
        let (ctx, mut app, root) = make_app("settings-actions");
        let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0));
        let _ = settings_output(&ctx, &mut app, viewport, Vec::new());
        let mut output = settings_output(&ctx, &mut app, viewport, Vec::new());

        app.actions.clear();
        let high_bitrate = painted_text(&output.shapes)
            .into_iter()
            .find(|(painted, _)| painted == "High · 160 kbps")
            .map(|(_, rect)| rect.center())
            .expect("Playback draws the 160 kbps choice");
        click_settings(&ctx, &mut app, viewport, high_bitrate);
        assert_eq!(app.settings.bitrate, 160);
        assert!(
            app.actions
                .iter()
                .any(|action| matches!(action, Action::SettingsChanged)),
            "changing playback quality did not queue SettingsChanged"
        );

        output = settings_output(&ctx, &mut app, viewport, Vec::new());
        let apply = painted_text(&output.shapes)
            .into_iter()
            .find(|(painted, _)| painted == "Apply and restart playback")
            .map(|(_, rect)| rect.center())
            .expect("changed playback settings do not offer Apply and restart playback");
        app.actions.clear();
        click_settings(&ctx, &mut app, viewport, apply);
        assert!(
            app.actions
                .iter()
                .any(|action| matches!(action, Action::RestartEngine)),
            "applying changed playback settings did not queue RestartEngine"
        );
        output = settings_output(&ctx, &mut app, viewport, Vec::new());
        let text = painted_text(&output.shapes);
        assert!(
            text.iter()
                .any(|(painted, _)| painted == "Playback settings applied.")
        );
        assert!(
            !text
                .iter()
                .any(|(painted, _)| painted == "Apply and restart playback")
        );

        let account = text
            .iter()
            .find(|(painted, _)| painted == "Account")
            .map(|(_, rect)| rect.center())
            .expect("settings navigation draws Account");
        click_settings(&ctx, &mut app, viewport, account);
        output = settings_output(&ctx, &mut app, viewport, Vec::new());
        let sign_out = painted_text(&output.shapes)
            .into_iter()
            .find(|(painted, _)| painted == "Sign out")
            .map(|(_, rect)| rect.center())
            .expect("Account settings draw Sign out");
        app.actions.clear();
        click_settings(&ctx, &mut app, viewport, sign_out);
        assert!(
            app.actions
                .iter()
                .any(|action| matches!(action, Action::SignOut)),
            "Account Sign out did not queue SignOut"
        );

        finish(app, root);
    }

    #[test]
    fn every_surface_renders_headless() {
        let (ctx, mut app, root) = make_app("render");

        let pages = [
            Page::Home,
            Page::Search,
            Page::Favorites,
            Page::DailyMix,
            Page::RandomMix,
            Page::Albums,
            Page::Artists,
            page_playlist(1),
            missing_playlist(),
            page_album(0),
            page_artist(0),
            Page::Queue,
            Page::Settings,
        ];
        for page in pages {
            app.open(page.clone());
            for _ in 0..3 {
                frame(&ctx, &mut app);
            }
            assert_eq!(app.page(), &page);
        }
        app.settings.sidebar_visible = false;
        frame(&ctx, &mut app);
        app.settings.sidebar_visible = true;
        app.show_queue_panel = true;
        frame(&ctx, &mut app);
        for dialog in [
            Dialog::Shortcuts,
            Dialog::CreatePlaylist {
                name: "x".into(),
                public: true,
                songs: vec![],
            },
            Dialog::EditPlaylist {
                id: playlist_id(1),
                name: "x".into(),
                description: String::new(),
                public: false,
            },
            Dialog::ConfirmDeletePlaylist {
                id: playlist_id(1),
                name: "x".into(),
            },
        ] {
            app.dialog = Some(dialog);
            frame(&ctx, &mut app);
        }
        app.settings.theme = crate::settings::ThemeChoice::Light;
        app.actions.push(Action::SettingsChanged);
        app.open(Page::Home);
        for _ in 0..3 {
            frame(&ctx, &mut app);
        }
        assert!(!app.palette.dark);
        finish(app, root);
    }

    #[test]
    fn dropping_a_song_on_a_sidebar_playlist_adds_it() {
        let (ctx, mut app, root) = make_app("drag-sidebar");
        app.open(page_playlist(1));
        for _ in 0..3 {
            frame(&ctx, &mut app);
        }

        let mut dropped = false;
        for step in 0..40 {
            let pos = egui::pos2(120.0, 120.0 + step as f32 * 15.0);
            egui::DragAndDrop::set_payload(
                &ctx,
                DragTrack {
                    song: song(0),
                    from: None,
                },
            );
            frame_events(&ctx, &mut app, vec![egui::Event::PointerMoved(pos)]);
            frame_events(
                &ctx,
                &mut app,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            assert!(!egui::DragAndDrop::has_any_payload(&ctx));
            if app.playlist_busy {
                dropped = true;
                break;
            }
        }
        assert!(dropped, "no sweep position landed on an owned playlist row");
        finish(app, root);
    }

    #[test]
    fn dragging_a_duplicate_playlist_row_reorders_its_exact_occurrence() {
        let (ctx, mut app, root) = make_app("reorder-playlist");
        let playlist = playlist_id(1);
        let page_id = Page::Playlist(playlist.clone());

        // Two rows deliberately carry the same song. Their original server
        // indexes and added-at markers are the only way to tell the
        // occurrences apart while the drag is in flight.
        {
            let page = app
                .playlist_pages
                .get_mut(&playlist)
                .expect("the demo playlist is loaded");
            assert!(page.items.is_complete());
            assert!(page.filter.is_empty());
            assert!(!app.table_sorts.contains_key(&page_id));
            let duplicate = page.items.items[5].track.clone();
            page.items.items[7].track = duplicate;
            page.items.revision = page.items.revision.wrapping_add(1);
            if let Some(metadata) = page.playlist.get_mut() {
                metadata.entries = page.items.items.clone();
            }
        }

        app.open(page_id);
        for _ in 0..3 {
            frame(&ctx, &mut app);
        }

        let before = &app.playlist_pages[&playlist].items.items;
        let source_row = 7;
        let source_index = before[source_row].index;
        let source_song = before[source_row].track.clone();
        let source_marker = before[source_row].added_at.clone();
        let twin_marker = before[5].added_at.clone();
        assert_eq!(source_song.id, before[5].track.id);
        assert_ne!(source_marker, twin_marker);
        let original_markers = before
            .iter()
            .map(|entry| entry.added_at.clone())
            .collect::<Vec<_>>();

        // The collection header height depends on fonts and platform scale,
        // so sweep through the central panel until a valid insert slot owns
        // the release. The app applies the emitted ReorderPlaylist action at
        // the end of that same frame, setting playlist_busy optimistically.
        let mut landed = false;
        'positions: for x in [520.0, 700.0, 900.0] {
            for step in 0..45 {
                let position = egui::pos2(x, 120.0 + step as f32 * 15.0);
                egui::DragAndDrop::set_payload(
                    &ctx,
                    DragTrack {
                        song: source_song.clone(),
                        from: Some((playlist.clone(), source_index)),
                    },
                );
                frame_events(&ctx, &mut app, vec![egui::Event::PointerMoved(position)]);
                frame_events(
                    &ctx,
                    &mut app,
                    vec![egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                );
                egui::DragAndDrop::clear_payload(&ctx);
                if app.playlist_busy {
                    landed = true;
                    break 'positions;
                }
            }
        }
        assert!(
            landed,
            "no complete unsorted playlist slot accepted the row"
        );

        let after = &app.playlist_pages[&playlist].items.items;
        assert_eq!(
            after.iter().map(|entry| entry.index).collect::<Vec<_>>(),
            (0..after.len() as u32).collect::<Vec<_>>()
        );
        let after_markers = after
            .iter()
            .map(|entry| entry.added_at.clone())
            .collect::<Vec<_>>();
        let destination = after_markers
            .iter()
            .position(|marker| marker == &source_marker)
            .expect("the dragged occurrence remains present");
        assert!(destination < source_row);
        let mut expected = original_markers;
        let moved = expected.remove(source_row);
        expected.insert(destination, moved);
        assert_eq!(after_markers, expected);
        assert!(after_markers.contains(&twin_marker));
        finish(app, root);
    }

    #[test]
    fn custom_sidebar_order_round_trips_through_settings() {
        let settings = Settings {
            sidebar_order: vec![playlist_id(4).uri(), playlist_id(0).uri()],
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        let restored: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.sidebar_order, settings.sidebar_order);
        let older: Settings = serde_json::from_str("{}").unwrap();
        assert!(older.sidebar_order.is_empty());
    }
}
