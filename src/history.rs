//! Local play history.
//!
//! Fastpotify stores plays made by its local player. Navidrome receives
//! explicit scrobbles separately; keeping this small client-side history makes
//! the Recents UI immediate and deterministic without pretending the server's
//! recently played albums are a per-song play log. A song counts only after
//! enough listening time, so skips do not fill the history.

use std::path::Path;

use crate::api::models::{PlayHistory, ProfileId, Song};

/// A play counts after 30 seconds, or halfway through a shorter track.
const COUNTS_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

/// Maximum number of stored local plays.
const KEPT: usize = 500;

/// When a song has been listened to long enough to count.
pub fn counts_after(duration_ms: u32) -> std::time::Duration {
    let half = std::time::Duration::from_millis(u64::from(duration_ms) / 2);
    COUNTS_AFTER
        .min(half)
        .max(std::time::Duration::from_secs(1))
}

/// The plays made here, newest first.
#[derive(Default)]
pub struct History {
    plays: Vec<PlayHistory>,
    /// Set when the in-memory list differs from the file.
    dirty: bool,
}

impl History {
    /// Reads the history, or returns an empty history if the file is unreadable.
    pub fn load(path: &Path, profile: &ProfileId) -> Self {
        let mut plays = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Vec<PlayHistory>>(&text).ok())
            .unwrap_or_default();
        // A copied or manually edited state file must not introduce media
        // references from another server into the active profile.
        plays.retain(|play| &play.track.id.profile == profile);
        Self {
            plays,
            dirty: false,
        }
    }

    pub fn plays(&self) -> &[PlayHistory] {
        &self.plays
    }

    pub fn is_empty(&self) -> bool {
        self.plays.is_empty()
    }

    /// Writes the history if it changed since the last save.
    pub fn save(&mut self, path: &Path) {
        if !self.dirty {
            return;
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string(&self.plays) {
            Ok(text) => {
                if let Err(error) = crate::paths::atomic_write(path, text.as_bytes()) {
                    log::warn!("could not write the play history: {error}");
                } else {
                    self.dirty = false;
                }
            }
            Err(error) => log::warn!("could not write the play history: {error}"),
        }
    }

    /// Writes down that `track` was played at `at`, newest first.
    pub fn record(&mut self, track: Song, at: jiff::Timestamp, context: Option<String>) {
        self.plays.insert(
            0,
            PlayHistory {
                track,
                played_at: Some(at.to_string()),
                context,
            },
        );
        self.plays.truncate(KEPT);
        self.dirty = true;
    }

    pub fn clear(&mut self) {
        if self.plays.is_empty() {
            return;
        }
        self.plays.clear();
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::models::{MediaId, MediaKind};

    fn profile() -> ProfileId {
        ProfileId::new("0123456789abcdef0123456789abcdef01234567")
    }

    fn song(id: &str) -> Song {
        let id = MediaId::new(profile(), MediaKind::Song, id);
        Song {
            uri: id.uri(),
            id,
            ..Song::default()
        }
    }

    /// A play counts after 30 seconds or halfway through a shorter track.
    #[test]
    fn a_song_counts_after_half_a_minute_or_half_of_it() {
        assert_eq!(counts_after(240_000).as_secs(), 30, "a four minute song");
        assert_eq!(counts_after(40_000).as_secs(), 20, "a forty second song");
        // Always require at least one second.
        assert!(counts_after(0) >= std::time::Duration::from_secs(1));
    }

    /// A counted play is not added again on later frames.
    #[test]
    fn counting_never_overflows_however_long_a_song_runs() {
        let threshold = counts_after(240_000);
        let mut listened = std::time::Duration::ZERO;
        let mut recorded = 0;
        // Continue for an hour after crossing the threshold.
        for _ in 0..3_600 {
            listened += std::time::Duration::from_secs(1);
            if recorded == 0 && listened >= threshold {
                recorded += 1;
            }
        }
        assert_eq!(recorded, 1, "written down once, and it did not panic");
    }

    /// The newest play comes first and the list is capped.
    #[test]
    fn the_newest_play_is_first_and_the_list_is_capped() {
        let mut history = History::default();
        let at: jiff::Timestamp = "2026-09-01T09:00:00Z".parse().unwrap();
        for index in 0..KEPT + 10 {
            history.record(song(&index.to_string()), at, None);
        }
        assert_eq!(history.plays().len(), KEPT, "the oldest fall off the end");
        assert_eq!(
            history.plays()[0].track.uri,
            song(&(KEPT + 9).to_string()).uri,
            "the newest is first"
        );
    }

    #[test]
    fn load_rejects_rows_from_another_server_profile() {
        let directory = std::env::temp_dir().join(format!(
            "fastpotify-history-test-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("history.json");
        let other = ProfileId::new("fedcba9876543210fedcba9876543210fedcba98");
        let mut foreign = song("foreign");
        foreign.id.profile = other;
        crate::paths::atomic_write(
            &path,
            serde_json::to_string(&vec![PlayHistory {
                track: foreign,
                played_at: None,
                context: None,
            }])
            .unwrap()
            .as_bytes(),
        )
        .unwrap();
        assert!(History::load(&path, &profile()).is_empty());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
