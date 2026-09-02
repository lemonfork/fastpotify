//! Deterministic, profile-local mixes derived from listening history.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::api::{ArtistRef, MediaId, MediaKind, PlayHistory, ProfileId, Song};

/// The number of songs shown in either Home mix.
pub(crate) const MIX_SIZE: usize = 20;

/// The current local-calendar day, suitable for a daily cache key.
pub(crate) fn local_day_key() -> String {
    jiff::Zoned::now().date().to_string()
}

/// Produces one stable mix for a day without mutating its inputs.
///
/// History is newest first. Repeated songs, recently played rows, familiar
/// artists and familiar genres all increase a candidate's weight; favorites
/// receive an additional boost. The hash-based weighted ordering keeps a mix
/// fixed for the day while letting the next day choose a different order.
pub(crate) fn generate_daily_mix(
    history: &[PlayHistory],
    favorites: &[Song],
    discovery: &[Song],
    day: &str,
    limit: usize,
) -> Vec<Song> {
    if limit == 0 {
        return Vec::new();
    }

    let listening = ListeningSignals::from_inputs(history, favorites);
    let favorite_ids: HashSet<MediaId> = favorites
        .iter()
        .filter(|song| usable_song(song))
        .map(|song| song.id.clone())
        .collect();
    let mut candidates = HashMap::<MediaId, Song>::new();

    // Keep the newest copy of a historical song. Favorites and discovery then
    // extend the pool without allowing repeated API rows into the result.
    for play in history {
        insert_candidate(&mut candidates, &play.track);
    }
    for song in favorites {
        insert_candidate(&mut candidates, song);
    }
    for song in discovery {
        insert_candidate(&mut candidates, song);
    }

    let mut ranked = candidates
        .into_values()
        .map(|song| {
            let favorite = favorite_ids.contains(&song.id);
            let weight = listening.weight(&song, favorite);
            let tie_breaker = daily_song_hash(day, &song.id);
            RankedSong {
                weighted_key: weighted_key(tie_breaker, weight),
                tie_breaker,
                song,
            }
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        left.weighted_key
            .total_cmp(&right.weighted_key)
            .then_with(|| left.tie_breaker.cmp(&right.tie_breaker))
            .then_with(|| {
                left.song
                    .id
                    .profile
                    .as_str()
                    .cmp(right.song.id.profile.as_str())
            })
            .then_with(|| left.song.id.raw().cmp(right.song.id.raw()))
    });

    let mut songs = diversify(ranked, limit);
    apply_mix_favorite_state(&mut songs, &favorite_ids);
    songs
}

fn insert_candidate(candidates: &mut HashMap<MediaId, Song>, song: &Song) {
    if usable_song(song) {
        candidates
            .entry(song.id.clone())
            .or_insert_with(|| song.clone());
    }
}

fn usable_song(song: &Song) -> bool {
    song.id.kind == MediaKind::Song && !song.id.raw().is_empty()
}

#[derive(Default)]
struct ListeningSignals {
    song_plays: HashMap<MediaId, u32>,
    song_recency: HashMap<MediaId, f64>,
    artists: HashMap<String, u32>,
    genres: HashMap<String, u32>,
    favorite_artists: HashMap<String, u32>,
    favorite_genres: HashMap<String, u32>,
}

impl ListeningSignals {
    fn from_inputs(history: &[PlayHistory], favorites: &[Song]) -> Self {
        let mut signals = Self::from_history(history);
        let mut seen = HashSet::new();
        for song in favorites {
            if !usable_song(song) || !seen.insert(song.id.clone()) {
                continue;
            }
            let mut row_artists = HashSet::new();
            for artist in &song.artists {
                for key in artist_keys(artist) {
                    if row_artists.insert(key.clone()) {
                        *signals.favorite_artists.entry(key).or_default() += 1;
                    }
                }
            }
            let mut row_genres = HashSet::new();
            for genre in &song.genres {
                if let Some(key) = normalized(genre)
                    && row_genres.insert(key.clone())
                {
                    *signals.favorite_genres.entry(key).or_default() += 1;
                }
            }
        }
        signals
    }

    fn from_history(history: &[PlayHistory]) -> Self {
        let mut signals = Self::default();
        for (index, play) in history.iter().enumerate() {
            let song = &play.track;
            if !usable_song(song) {
                continue;
            }
            *signals.song_plays.entry(song.id.clone()).or_default() += 1;
            // History is newest first. This small bounded addition makes a
            // recent play matter without overwhelming long-term frequency.
            *signals.song_recency.entry(song.id.clone()).or_default() +=
                1.0 / (1.0 + index as f64 / 24.0);

            let mut row_artists = HashSet::new();
            for artist in &song.artists {
                for key in artist_keys(artist) {
                    if row_artists.insert(key.clone()) {
                        *signals.artists.entry(key).or_default() += 1;
                    }
                }
            }
            let mut row_genres = HashSet::new();
            for genre in &song.genres {
                if let Some(key) = normalized(genre)
                    && row_genres.insert(key.clone())
                {
                    *signals.genres.entry(key).or_default() += 1;
                }
            }
        }
        signals
    }

    fn weight(&self, song: &Song, favorite: bool) -> f64 {
        let own_plays = self.song_plays.get(&song.id).copied().unwrap_or(0) as f64;
        let recency = self.song_recency.get(&song.id).copied().unwrap_or(0.0);
        let artist_frequency = song
            .artists
            .iter()
            .flat_map(artist_keys)
            .filter_map(|key| self.artists.get(&key))
            .copied()
            .max()
            .unwrap_or(0) as f64;
        let genre_frequency = song
            .genres
            .iter()
            .filter_map(|genre| normalized(genre))
            .filter_map(|genre| self.genres.get(&genre))
            .copied()
            .max()
            .unwrap_or(0) as f64;
        let favorite_artist_frequency = song
            .artists
            .iter()
            .flat_map(artist_keys)
            .filter_map(|key| self.favorite_artists.get(&key))
            .copied()
            .max()
            .unwrap_or(0) as f64;
        let favorite_genre_frequency = song
            .genres
            .iter()
            .filter_map(|genre| normalized(genre))
            .filter_map(|genre| self.favorite_genres.get(&genre))
            .copied()
            .max()
            .unwrap_or(0) as f64;

        4.0 + own_plays * 10.0
            + recency * 3.0
            + artist_frequency * 1.5
            + genre_frequency
            + favorite_artist_frequency * 0.75
            + favorite_genre_frequency * 0.5
            + if favorite { 24.0 } else { 0.0 }
    }
}

fn artist_keys(artist: &ArtistRef) -> Vec<String> {
    let mut keys = Vec::with_capacity(2);
    if let Some(id) = &artist.id
        && !id.raw().is_empty()
    {
        keys.push(format!("id\0{}\0{}", id.profile.as_str(), id.raw()));
    }
    if let Some(name) = normalized(&artist.name) {
        keys.push(format!("name\0{name}"));
    }
    keys
}

fn normalized(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_lowercase())
}

struct RankedSong {
    song: Song,
    weighted_key: f64,
    tie_breaker: u64,
}

/// Efraimidis-Spirakis weighted sampling without replacement, represented as
/// sortable keys. All entropy comes from the stable per-day song hash.
fn weighted_key(hash: u64, weight: f64) -> f64 {
    const TWO_TO_53: f64 = 9_007_199_254_740_992.0;
    let unit = ((hash >> 11) as f64 + 0.5) / TWO_TO_53;
    -unit.ln() / weight.max(1.0)
}

fn daily_song_hash(day: &str, id: &MediaId) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for part in [
        b"fastpotify-daily-mix-v1".as_slice(),
        day.as_bytes(),
        id.profile.as_str().as_bytes(),
        id.raw().as_bytes(),
    ] {
        for byte in (part.len() as u64)
            .to_le_bytes()
            .into_iter()
            .chain(part.iter().copied())
        {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    // Finalize FNV-1a so small changes in an ISO date affect every output bit.
    hash ^= hash >> 30;
    hash = hash.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    hash ^= hash >> 27;
    hash = hash.wrapping_mul(0x94d0_49bb_1331_11eb);
    hash ^ (hash >> 31)
}

fn primary_artist(song: &Song) -> Option<String> {
    song.artists.first().and_then(|artist| {
        artist
            .id
            .as_ref()
            .filter(|id| !id.raw().is_empty())
            .map(|id| format!("id\0{}\0{}", id.profile.as_str(), id.raw()))
            .or_else(|| normalized(&artist.name).map(|name| format!("name\0{name}")))
    })
}

fn apply_mix_favorite_state(songs: &mut [Song], favorite_ids: &HashSet<MediaId>) {
    for song in songs {
        let favorite = favorite_ids.contains(&song.id);
        song.starred = favorite;
        if !favorite {
            song.starred_at = None;
        }
        if let Some(album) = song.album.as_mut() {
            album.starred = false;
            album.starred_at = None;
        }
    }
}

fn diversify(ranked: Vec<RankedSong>, limit: usize) -> Vec<Song> {
    let artist_cap = limit.div_ceil(5).max(2).min(limit);
    let mut artist_counts = HashMap::<String, usize>::new();
    let mut selected = Vec::with_capacity(limit.min(ranked.len()));
    let mut deferred = Vec::new();

    for ranked_song in ranked {
        if selected.len() == limit {
            break;
        }
        if let Some(artist) = primary_artist(&ranked_song.song) {
            let count = artist_counts.get(&artist).copied().unwrap_or(0);
            if count >= artist_cap {
                deferred.push(ranked_song.song);
                continue;
            }
            artist_counts.insert(artist, count + 1);
        }
        selected.push(ranked_song.song);
    }

    // A small or single-artist library should still receive a full mix.
    selected.extend(deferred.into_iter().take(limit - selected.len()));
    selected
}

/// Persistence helper for the stable daily mix snapshot.
pub(crate) struct DailyMixCache;

#[derive(Serialize, Deserialize)]
struct StoredDailyMix {
    day: String,
    songs: Vec<Song>,
}

impl DailyMixCache {
    /// Reads today's mix. Missing, stale, malformed, foreign-profile and
    /// otherwise invalid files are cache misses.
    pub(crate) fn load(
        path: &Path,
        day: &str,
        profile: &ProfileId,
        limit: usize,
    ) -> Option<Vec<Song>> {
        let bytes = std::fs::read(path).ok()?;
        let stored: StoredDailyMix = serde_json::from_slice(&bytes).ok()?;
        if stored.day != day || stored.songs.is_empty() {
            return None;
        }
        if stored.songs.iter().any(|song| {
            song.id.kind != MediaKind::Song
                || song.id.raw().is_empty()
                || &song.id.profile != profile
        }) {
            return None;
        }
        if limit == 0 {
            return Some(Vec::new());
        }

        let mut seen = HashSet::new();
        let mut songs = Vec::with_capacity(limit.min(stored.songs.len()));
        for song in stored.songs {
            if seen.insert(song.id.clone()) {
                songs.push(song);
                if songs.len() == limit {
                    break;
                }
            }
        }
        apply_mix_favorite_state(&mut songs, &HashSet::new());
        Some(songs)
    }

    /// Atomically stores every generated song together with its local day.
    pub(crate) fn save(path: &Path, day: &str, songs: &[Song]) -> io::Result<()> {
        if songs.is_empty() {
            return Ok(());
        }
        let bytes = serde_json::to_vec(&StoredDailyMix {
            day: day.to_owned(),
            songs: songs.to_vec(),
        })
        .map_err(io::Error::other)?;
        crate::paths::atomic_write(path, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> ProfileId {
        ProfileId::new("0123456789abcdef0123456789abcdef01234567")
    }

    fn other_profile() -> ProfileId {
        ProfileId::new("fedcba9876543210fedcba9876543210fedcba98")
    }

    fn song(id: &str, artist: &str, genre: &str) -> Song {
        song_for(profile(), id, artist, genre)
    }

    fn song_for(profile: ProfileId, id: &str, artist: &str, genre: &str) -> Song {
        let media_id = MediaId::new(profile.clone(), MediaKind::Song, id);
        let artist_id = MediaId::new(
            profile,
            MediaKind::Artist,
            format!("artist-{}", artist.to_lowercase()),
        );
        Song {
            uri: media_id.uri(),
            id: media_id,
            name: id.to_owned(),
            artists: vec![ArtistRef {
                id: Some(artist_id),
                name: artist.to_owned(),
                uri: None,
            }],
            genres: vec![genre.to_owned()],
            ..Song::default()
        }
    }

    fn play(track: Song) -> PlayHistory {
        PlayHistory {
            track,
            played_at: None,
            context: None,
        }
    }

    fn ids(songs: &[Song]) -> Vec<&str> {
        songs.iter().map(|song| song.id.raw()).collect()
    }

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "fastpotify-daily-mix-test-{}-{}",
                std::process::id(),
                rand::random::<u64>()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_same_day_is_stable_and_a_new_day_changes_the_order() {
        let discovery = (0..24)
            .map(|index| song(&format!("song-{index}"), "artist", "rock"))
            .collect::<Vec<_>>();
        let first = generate_daily_mix(&[], &[], &discovery, "2026-09-02", 20);
        let repeated = generate_daily_mix(&[], &[], &discovery, "2026-09-02", 20);
        let tomorrow = generate_daily_mix(&[], &[], &discovery, "2026-09-03", 20);
        assert_eq!(first, repeated);
        assert_ne!(ids(&first), ids(&tomorrow));
    }

    #[test]
    fn listening_and_favorite_signals_each_increase_weight() {
        let frequent = song("frequent", "Known Artist", "rock");
        let once = song("once", "Other Artist", "jazz");
        let recent = song("recent", "Recency Artist", "folk");
        let older = song("older", "Recency Artist", "folk");
        let same_artist = song("same-artist", "Known Artist", "ambient");
        let same_genre = song("same-genre", "Unknown Artist", "rock");
        let unrelated = song("unrelated", "Unknown Artist", "classical");
        let history = vec![
            play(recent.clone()),
            play(frequent.clone()),
            play(frequent.clone()),
            play(frequent.clone()),
            play(once.clone()),
            play(older.clone()),
        ];
        let signals = ListeningSignals::from_history(&history);

        assert!(signals.weight(&frequent, false) > signals.weight(&once, false));
        assert!(signals.weight(&recent, false) > signals.weight(&older, false));
        assert!(signals.weight(&same_artist, false) > signals.weight(&unrelated, false));
        assert!(signals.weight(&same_genre, false) > signals.weight(&unrelated, false));
        assert!(signals.weight(&unrelated, true) > signals.weight(&unrelated, false));
    }

    #[test]
    fn favorite_artists_and_genres_gently_guide_discovery() {
        let favorite = song("favorite", "Loved Artist", "dream pop");
        let same_artist = song("same-artist", "Loved Artist", "ambient");
        let same_genre = song("same-genre", "Someone Else", "dream pop");
        let unrelated = song("unrelated", "Someone Else", "classical");
        let signals = ListeningSignals::from_inputs(&[], &[favorite]);

        assert!(signals.weight(&same_artist, false) > signals.weight(&unrelated, false));
        assert!(signals.weight(&same_genre, false) > signals.weight(&unrelated, false));
    }

    #[test]
    fn results_are_unique_bounded_and_artist_diverse() {
        let mut discovery = (0..12)
            .map(|index| song(&format!("popular-{index}"), "Popular", "pop"))
            .collect::<Vec<_>>();
        discovery.extend((0..12).map(|index| {
            song(
                &format!("varied-{index}"),
                &format!("Artist {index}"),
                "pop",
            )
        }));
        discovery.push(discovery[0].clone());

        let mix = generate_daily_mix(&[], &[], &discovery, "2026-09-02", 10);
        let unique = mix
            .iter()
            .map(|song| song.id.clone())
            .collect::<HashSet<_>>();
        let popular = mix
            .iter()
            .filter(|song| song.artists[0].name == "Popular")
            .count();
        assert_eq!(mix.len(), 10);
        assert_eq!(unique.len(), mix.len());
        assert!(popular <= 2, "the 20% primary-artist cap applies");
    }

    #[test]
    fn a_small_single_artist_pool_is_backfilled() {
        let discovery = (0..8)
            .map(|index| song(&format!("song-{index}"), "Only Artist", "rock"))
            .collect::<Vec<_>>();
        let mix = generate_daily_mix(&[], &[], &discovery, "2026-09-02", 5);
        assert_eq!(mix.len(), 5);
    }

    #[test]
    fn empty_history_falls_back_to_discovery() {
        let discovery = vec![song("one", "A", "rock"), song("two", "B", "jazz")];
        let mix = generate_daily_mix(&[], &[], &discovery, "2026-09-02", MIX_SIZE);
        assert_eq!(mix.len(), discovery.len());
        assert_eq!(ids(&mix).into_iter().collect::<HashSet<_>>().len(), 2);
        assert!(generate_daily_mix(&[], &[], &discovery, "2026-09-02", 0).is_empty());
    }

    #[test]
    fn generated_mix_ignores_stale_favorite_flags_from_history() {
        let mut stale = song("stale", "A", "rock");
        stale.starred = true;
        stale.starred_at = Some("2026-09-01T00:00:00Z".into());

        let mix = generate_daily_mix(&[play(stale)], &[], &[], "2026-09-02", 1);

        assert_eq!(mix.len(), 1);
        assert!(!mix[0].starred);
        assert_eq!(mix[0].starred_at, None);
    }

    #[test]
    fn a_cache_from_another_profile_is_rejected() {
        let directory = TempDir::new();
        let path = directory.0.join("daily-mix.json");
        DailyMixCache::save(
            &path,
            "2026-09-02",
            &[song_for(other_profile(), "foreign", "A", "rock")],
        )
        .unwrap();
        assert!(DailyMixCache::load(&path, "2026-09-02", &profile(), MIX_SIZE).is_none());
    }

    #[test]
    fn stale_and_non_song_cache_rows_are_rejected() {
        let directory = TempDir::new();
        let path = directory.0.join("daily-mix.json");
        DailyMixCache::save(&path, "2026-09-01", &[song("old", "A", "rock")]).unwrap();
        assert!(DailyMixCache::load(&path, "2026-09-02", &profile(), MIX_SIZE).is_none());

        let mut album = song("not-a-song", "A", "rock");
        album.id.kind = MediaKind::Album;
        DailyMixCache::save(&path, "2026-09-02", &[album]).unwrap();
        assert!(DailyMixCache::load(&path, "2026-09-02", &profile(), MIX_SIZE).is_none());
    }

    #[test]
    fn old_and_malformed_cache_files_are_ignored() {
        let directory = TempDir::new();
        let path = directory.0.join("daily-mix.json");
        crate::paths::atomic_write(
            &path,
            serde_json::to_vec(&vec![song("old", "A", "rock")])
                .unwrap()
                .as_slice(),
        )
        .unwrap();
        assert!(DailyMixCache::load(&path, "2026-09-02", &profile(), MIX_SIZE).is_none());

        crate::paths::atomic_write(&path, b"{broken json").unwrap();
        assert!(DailyMixCache::load(&path, "2026-09-02", &profile(), MIX_SIZE).is_none());
    }

    #[test]
    fn cache_round_trip_is_atomic_deduplicated_and_truncated() {
        let directory = TempDir::new();
        let path = directory.0.join("daily-mix.json");
        let first = song("one", "A", "rock");
        let input = vec![
            first.clone(),
            first,
            song("two", "B", "jazz"),
            song("three", "C", "pop"),
        ];
        DailyMixCache::save(&path, "2026-09-02", &input).unwrap();

        let stored: StoredDailyMix =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            stored.songs.len(),
            input.len(),
            "save keeps the full snapshot"
        );
        let loaded = DailyMixCache::load(&path, "2026-09-02", &profile(), 2).unwrap();
        assert_eq!(ids(&loaded), vec!["one", "two"]);
        assert!(
            !std::fs::read_dir(&directory.0)
                .unwrap()
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        );
    }

    #[test]
    fn cached_mix_drops_stale_favorite_flags() {
        let directory = TempDir::new();
        let path = directory.0.join("daily-mix.json");
        let mut stale = song("one", "A", "rock");
        stale.starred = true;
        stale.starred_at = Some("2026-09-01T00:00:00Z".into());
        DailyMixCache::save(&path, "2026-09-02", &[stale]).unwrap();

        let loaded = DailyMixCache::load(&path, "2026-09-02", &profile(), MIX_SIZE).unwrap();

        assert_eq!(loaded.len(), 1);
        assert!(!loaded[0].starred);
        assert_eq!(loaded[0].starred_at, None);
    }

    #[test]
    fn local_day_keys_are_iso_dates() {
        local_day_key().parse::<jiff::civil::Date>().unwrap();
    }
}
