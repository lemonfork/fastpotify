//! Lyrics for the playing song, from OpenSubsonic or LRCLIB.
//!
//! [LRCLIB](https://lrclib.net) provides plain and LRC-synced lyrics without an
//! account or key. Fastpotify asks the active server first and uses LRCLIB only
//! when that server has no lyrics. Cache keys contain only an opaque,
//! profile-scoped media reference.
//!
//! Matching starts with an exact lookup, then ranks search results. Track
//! length is the strongest signal for distinguishing versions with the same
//! title.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use crate::api::{ApiClient, MediaId, MediaKind};

const API: &str = "https://lrclib.net/api";
/// Keep a malformed or hostile fallback service from growing the process
/// without bound. Normal LRCLIB records are tiny compared with this limit.
const MAX_LRCLIB_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
/// Lyrics do not change; a cached answer is good for this long.
const CACHE_LIFETIME: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// A candidate this far from the playing track's length is another
/// recording, however well the names match.
const MAX_DRIFT_SECS: f64 = 30.0;

/// Track metadata used for lyrics lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    /// Profile-scoped song identity; never an authenticated stream URL.
    pub media: MediaId,
    pub artist: String,
    pub title: String,
    pub album: String,
    pub duration_ms: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    /// When the line starts; `None` in lyrics that carry no timing.
    pub at_ms: Option<u32>,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lyrics {
    pub lines: Vec<Line>,
    /// Every line has a time, so the panel can follow the song.
    pub synced: bool,
    /// The database knows there are no words.
    pub instrumental: bool,
}

impl Lyrics {
    /// The line being sung at `position_ms`, or `None` before the first one
    /// starts and for lyrics without timing.
    pub fn active_line(&self, position_ms: u32) -> Option<usize> {
        if !self.synced {
            return None;
        }
        let started = self
            .lines
            .partition_point(|line| line.at_ms.is_some_and(|at| at <= position_ms));
        started.checked_sub(1)
    }
}

/// Fetches lyrics for `query`, using the active OpenSubsonic profile first and
/// the disk cache or LRCLIB fallback when necessary.
pub async fn fetch(
    client: &ApiClient,
    http: &reqwest::Client,
    cache_dir: &Path,
    query: &Query,
) -> Result<Option<Lyrics>> {
    anyhow::ensure!(
        query.media.profile == *client.profile_id()
            && query.media.kind == MediaKind::Song
            && crate::media::is_media_ref(&query.media.uri()),
        "lyrics reference does not belong to the active song profile"
    );
    let cache_path = cache_dir.join(format!("{}.json", cache_key(query)));
    if let Some(cached) = read_cache(&cache_path) {
        return Ok(cached);
    }

    let server = client
        .lyrics(optional(&query.artist), optional(&query.title))
        .await;
    if let Ok(answer) = &server
        && let Some(found) = from_server_text(&answer.text)
    {
        let found = Some(found);
        write_cache(&cache_path, &found);
        return Ok(found);
    }

    let fallback = lookup(http, query).await?;
    // Do not turn a transient server failure into a month-long negative
    // cache entry. A positive fallback is still safe to retain.
    if server.is_ok() || fallback.is_some() {
        write_cache(&cache_path, &fallback);
    }
    Ok(fallback)
}

fn optional(value: &str) -> Option<&str> {
    (!value.trim().is_empty()).then_some(value.trim())
}

fn from_server_text(text: &str) -> Option<Lyrics> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let synced = parse_lrc(text);
    if !synced.is_empty() {
        return Some(Lyrics {
            lines: synced,
            synced: true,
            instrumental: false,
        });
    }
    Some(Lyrics {
        lines: text
            .lines()
            .map(|line| Line {
                at_ms: None,
                text: line.trim_end().to_owned(),
            })
            .collect(),
        synced: false,
        instrumental: false,
    })
}

async fn lookup(http: &reqwest::Client, query: &Query) -> Result<Option<Lyrics>> {
    let title = clean_title(&query.title);
    let artist = clean_artist(&query.artist);
    if title.is_empty() || artist.is_empty() {
        return Ok(None);
    }
    let duration = (query.duration_ms / 1000).to_string();
    let mut exact = vec![
        ("artist_name", artist.as_str()),
        ("track_name", title.as_str()),
    ];
    if !query.album.trim().is_empty() {
        exact.push(("album_name", query.album.trim()));
    }
    if query.duration_ms > 0 {
        exact.push(("duration", duration.as_str()));
    }
    if let Some(record) = get::<Record>(http, "/get", &exact).await?
        && let Some(lyrics) = record.lyrics()
    {
        return Ok(Some(lyrics));
    }
    let candidates = get::<Vec<Record>>(
        http,
        "/search",
        &[
            ("artist_name", artist.as_str()),
            ("track_name", title.as_str()),
        ],
    )
    .await?
    .unwrap_or_default();
    Ok(pick(candidates, query).and_then(|record| record.lyrics()))
}

/// One LRCLIB request. A 404 is "nobody has transcribed this" and a 400 is
/// "that is not a question I can answer"; neither is a fault worth showing.
async fn get<T: DeserializeOwned>(
    http: &reqwest::Client,
    path: &str,
    params: &[(&str, &str)],
) -> Result<Option<T>> {
    let response = http
        .get(format!("{API}{path}"))
        .query(params)
        .header("Accept", "application/json")
        .send()
        .await
        .context("cannot reach LRCLIB")?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::BAD_REQUEST {
        return Ok(None);
    }
    if !status.is_success() {
        anyhow::bail!("LRCLIB answered {status}");
    }
    ensure_declared_size(response.content_length())?;
    let mut response = response;
    let mut body = Vec::with_capacity(
        response
            .content_length()
            .unwrap_or_default()
            .min(MAX_LRCLIB_RESPONSE_BYTES as u64) as usize,
    );
    while let Some(chunk) = response
        .chunk()
        .await
        .context("cannot read the answer from LRCLIB")?
    {
        extend_bounded(&mut body, &chunk)?;
    }
    Ok(Some(
        serde_json::from_slice(&body).context("unexpected answer from LRCLIB")?,
    ))
}

fn extend_bounded(body: &mut Vec<u8>, chunk: &[u8]) -> Result<()> {
    let length = body
        .len()
        .checked_add(chunk.len())
        .context("LRCLIB answer size overflow")?;
    anyhow::ensure!(
        length <= MAX_LRCLIB_RESPONSE_BYTES,
        "LRCLIB answer exceeds the size limit"
    );
    body.extend_from_slice(chunk);
    Ok(())
}

fn ensure_declared_size(length: Option<u64>) -> Result<()> {
    anyhow::ensure!(
        length.is_none_or(|length| length <= MAX_LRCLIB_RESPONSE_BYTES as u64),
        "LRCLIB answer exceeds the size limit"
    );
    Ok(())
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Record {
    track_name: String,
    artist_name: String,
    duration: Option<f64>,
    instrumental: bool,
    synced_lyrics: Option<String>,
    plain_lyrics: Option<String>,
}

impl Record {
    fn synced(&self) -> Option<&str> {
        self.synced_lyrics
            .as_deref()
            .filter(|text| !text.trim().is_empty())
    }

    fn plain(&self) -> Option<&str> {
        self.plain_lyrics
            .as_deref()
            .filter(|text| !text.trim().is_empty())
    }

    fn lyrics(&self) -> Option<Lyrics> {
        if self.instrumental {
            return Some(Lyrics {
                instrumental: true,
                ..Lyrics::default()
            });
        }
        if let Some(synced) = self.synced() {
            let lines = parse_lrc(synced);
            if !lines.is_empty() {
                return Some(Lyrics {
                    lines,
                    synced: true,
                    instrumental: false,
                });
            }
        }
        let plain = self.plain()?;
        Some(Lyrics {
            lines: plain
                .lines()
                .map(|line| Line {
                    at_ms: None,
                    text: line.trim_end().to_string(),
                })
                .collect(),
            synced: false,
            instrumental: false,
        })
    }
}

/// How well a search result fits what is playing; `None` rules it out.
fn score(record: &Record, query: &Query) -> Option<i64> {
    if !loose_match(&record.track_name, &clean_title(&query.title)) {
        return None;
    }
    let mut score = 0;
    if loose_match(&record.artist_name, &clean_artist(&query.artist)) {
        score += 1000;
    }
    if query.duration_ms > 0
        && let Some(duration) = record.duration.filter(|duration| *duration > 0.0)
    {
        let drift = (duration - f64::from(query.duration_ms) / 1000.0).abs();
        if drift > MAX_DRIFT_SECS {
            return None;
        }
        score += ((MAX_DRIFT_SECS - drift) * 10.0) as i64;
    }
    // Timing is the point, so a synced upload wins a tie.
    if record.synced().is_some() {
        score += 200;
    } else if record.plain().is_some() {
        score += 50;
    }
    Some(score)
}

fn pick(records: Vec<Record>, query: &Query) -> Option<Record> {
    let mut best: Option<(i64, Record)> = None;
    for record in records {
        let Some(score) = score(&record, query) else {
            continue;
        };
        if best.as_ref().is_none_or(|(held, _)| score > *held) {
            best = Some((score, record));
        }
    }
    best.map(|(_, record)| record)
}

// ---- names ----------------------------------------------------------------

/// Words a title carries in brackets that a lyrics database does not.
const BRACKET_NOISE: &[&str] = &[
    "remaster",
    "remastered",
    "remix",
    "live",
    "acoustic",
    "version",
    "edit",
    "mix",
    "mono",
    "stereo",
    "deluxe",
    "bonus",
    "expanded",
    "explicit",
    "anniversary",
    "feat",
    "featuring",
    "with",
];

/// What a " - " suffix says when it is not part of the title.
const SUFFIX_NOISE: &[&str] = &[
    "remaster",
    "remastered",
    "radio edit",
    "single version",
    "album version",
    "live",
    "mono",
    "stereo",
    "rerecorded",
    "re-recorded",
];

/// Invisible characters that carry no meaning: a zero-width space, the word
/// joiner and the invisible operators, and a byte order mark. Not the
/// zero-width joiner or non-joiner, which hold emoji sequences and Indic
/// and Persian words together.
fn is_disposable(c: char) -> bool {
    matches!(c, '\u{200b}' | '\u{2060}'..='\u{2064}' | '\u{feff}')
}

/// Whether `text` contains `phrase` as whole words, case-insensitively.
fn has_phrase(text: &str, phrase: &str) -> bool {
    let words: Vec<String> = text
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|word| !word.is_empty())
        .map(|word| word.trim_matches('-').to_string())
        .collect();
    let wanted: Vec<&str> = phrase.split(' ').collect();
    words
        .windows(wanted.len())
        .any(|window| window.iter().zip(&wanted).all(|(a, b)| a == b))
}

/// Strips what a player adds to a title and a database leaves out:
/// "(Remastered 2011)", " - Live at Wembley", "feat. Someone".
pub fn clean_title(title: &str) -> String {
    let original: String = title.chars().filter(|c| !is_disposable(*c)).collect();
    let mut cleaned = String::with_capacity(original.len());
    let mut rest = original.as_str();
    while let Some(open) = rest.find(['(', '[']) {
        let closer = if rest[open..].starts_with('(') {
            ')'
        } else {
            ']'
        };
        let Some(close) = rest[open..].find(closer) else {
            break;
        };
        let inner = &rest[open + 1..open + close];
        let noisy = BRACKET_NOISE.iter().any(|word| has_phrase(inner, word));
        if noisy {
            cleaned.push_str(rest[..open].trim_end());
        } else {
            cleaned.push_str(&rest[..open + close + 1]);
        }
        rest = &rest[open + close + 1..];
    }
    cleaned.push_str(rest);
    let mut cleaned = cleaned.trim().to_string();
    // " - Remastered 2009" and friends, from the first dash whose tail is
    // noise; a dash inside a real title stays.
    let mut cut = None;
    for (index, _) in cleaned.match_indices(" - ") {
        let tail = &cleaned[index + 3..];
        if SUFFIX_NOISE.iter().any(|phrase| has_phrase(tail, phrase))
            || tail
                .split_whitespace()
                .next()
                .is_some_and(|word| word.len() == 4 && word.chars().all(|c| c.is_ascii_digit()))
                && has_phrase(tail, "version")
        {
            cut = Some(index);
            break;
        }
    }
    if let Some(index) = cut {
        cleaned.truncate(index);
    }
    let cleaned = strip_featuring(cleaned.trim());
    if cleaned.is_empty() {
        original.trim().to_string()
    } else {
        cleaned
    }
}

/// Everything from a standalone "feat", "ft", or "featuring" on.
fn strip_featuring(text: &str) -> String {
    let lower = text.to_lowercase();
    let mut cut = None;
    for marker in ["featuring", "feat", "ft"] {
        let mut from = 0;
        while let Some(found) = lower[from..].find(marker) {
            let start = from + found;
            let end = start + marker.len();
            let before = lower[..start].trim_end_matches(['-', '(']).trim_end();
            let preceded = before.len() < start && !before.is_empty();
            let followed = lower[end..]
                .strip_prefix('.')
                .unwrap_or(&lower[end..])
                .starts_with(' ');
            if preceded && followed {
                let at = before.len();
                cut = Some(cut.map_or(at, |held: usize| held.min(at)));
                break;
            }
            from = end;
        }
    }
    match cut {
        Some(at) => text[..at].trim().to_string(),
        None => text.trim().to_string(),
    }
}

/// Players report collaborations in ways a database does not file them, and
/// LRCLIB itself sometimes stores "TOOL;Tool" for one artist.
pub fn clean_artist(artist: &str) -> String {
    let original: String = artist.chars().filter(|c| !is_disposable(*c)).collect();
    let cleaned = strip_featuring(&original);
    let cleaned = cleaned.split(';').next().unwrap_or_default().trim();
    if cleaned.is_empty() {
        original.trim().to_string()
    } else {
        cleaned.to_string()
    }
}

/// Lowercase, accents folded, punctuation gone, one space between words.
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        let folded = fold(c);
        if folded == '\'' || folded == '’' || folded == '`' {
            continue;
        }
        if folded == '&' {
            for word in [' ', 'a', 'n', 'd', ' '] {
                push_normalized(&mut out, &mut space, word);
            }
            continue;
        }
        push_normalized(&mut out, &mut space, folded);
    }
    out.trim().to_string()
}

fn push_normalized(out: &mut String, space: &mut bool, c: char) {
    if c.is_alphanumeric() {
        out.extend(c.to_lowercase());
        *space = false;
    } else if !*space {
        out.push(' ');
        *space = true;
    }
}

/// The plain letter behind the Latin accents titles most often carry.
fn fold(c: char) -> char {
    match c {
        'À'..='Å' | 'à'..='å' => 'a',
        'Ç' | 'ç' => 'c',
        'È'..='Ë' | 'è'..='ë' => 'e',
        'Ì'..='Ï' | 'ì'..='ï' => 'i',
        'Ñ' | 'ñ' => 'n',
        'Ò'..='Ö' | 'Ø' | 'ò'..='ö' | 'ø' => 'o',
        'Ù'..='Ü' | 'ù'..='ü' => 'u',
        'Ý' | 'ý' | 'ÿ' => 'y',
        'ß' => 's',
        _ => c,
    }
}

fn loose_match(left: &str, right: &str) -> bool {
    let a = normalize(left);
    let b = normalize(right);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a == b || a.contains(&b) || b.contains(&a)
}

// ---- LRC ------------------------------------------------------------------

/// Parses `[mm:ss.xx]` lines. A line may open with several stamps when the
/// same words repeat; tags such as `[ar:...]` carry no digits and are
/// skipped. The result is sorted by time.
pub fn parse_lrc(text: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    for raw in text.lines() {
        let mut rest = raw.trim_start();
        let mut times = Vec::new();
        while let Some(stamp) = leading_stamp(rest) {
            times.push(stamp.0);
            rest = &rest[stamp.1..];
        }
        if times.is_empty() {
            continue;
        }
        let body = rest.trim();
        for at_ms in times {
            lines.push(Line {
                at_ms: Some(at_ms),
                text: body.to_string(),
            });
        }
    }
    lines.sort_by_key(|line| line.at_ms);
    lines
}

/// A timestamp at the head of `text`: its milliseconds and its length.
fn leading_stamp(text: &str) -> Option<(u32, usize)> {
    let inner = text.strip_prefix('[')?;
    let close = inner.find(']')?;
    let stamp = &inner[..close];
    let (minutes, seconds) = stamp.split_once(':')?;
    let (seconds, fraction) = match seconds.find(['.', ':']) {
        Some(dot) => (&seconds[..dot], Some(&seconds[dot + 1..])),
        None => (seconds, None),
    };
    let minutes: u32 = minutes.parse().ok()?;
    let seconds: u32 = seconds.parse().ok()?;
    let fraction_ms = match fraction {
        Some(digits) if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) => {
            let value: u32 = digits.chars().take(3).collect::<String>().parse().ok()?;
            value * 10u32.pow(3u32.saturating_sub(digits.len().min(3) as u32))
        }
        Some(_) => return None,
        None => 0,
    };
    Some((minutes * 60_000 + seconds * 1_000 + fraction_ms, close + 2))
}

// ---- cache ------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct Cached {
    found: Option<Lyrics>,
}

fn cache_key(query: &Query) -> String {
    let digest = Sha1::digest(query.media.uri().as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn read_cache(path: &Path) -> Option<Option<Lyrics>> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    if modified.elapsed().unwrap_or(CACHE_LIFETIME) >= CACHE_LIFETIME {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<Cached>(&text)
        .ok()
        .map(|cached| cached.found)
}

fn write_cache(path: &Path, found: &Option<Lyrics>) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string(&Cached {
        found: found.clone(),
    }) {
        let _ = crate::paths::atomic_write(path, text.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media(profile: &str, id: &str) -> MediaId {
        MediaId::new(crate::auth::ProfileId::new(profile), MediaKind::Song, id)
    }

    #[test]
    fn lrclib_response_body_has_declared_and_chunked_size_boundaries() {
        ensure_declared_size(None).unwrap();
        ensure_declared_size(Some(MAX_LRCLIB_RESPONSE_BYTES as u64)).unwrap();
        assert!(ensure_declared_size(Some(MAX_LRCLIB_RESPONSE_BYTES as u64 + 1)).is_err());

        let mut body = Vec::new();
        let first_chunk = vec![b'x'; MAX_LRCLIB_RESPONSE_BYTES / 2];
        let second_chunk = vec![b'x'; MAX_LRCLIB_RESPONSE_BYTES / 2];
        extend_bounded(&mut body, &first_chunk).unwrap();
        extend_bounded(&mut body, &second_chunk).unwrap();
        assert_eq!(body.len(), MAX_LRCLIB_RESPONSE_BYTES);
        assert!(extend_bounded(&mut body, b"x").is_err());
        assert_eq!(body.len(), MAX_LRCLIB_RESPONSE_BYTES);
    }

    #[test]
    fn titles_lose_what_a_database_leaves_out() {
        assert_eq!(clean_title("Song (Remastered 2011)"), "Song");
        assert_eq!(clean_title("Song - Live at Wembley"), "Song");
        assert_eq!(clean_title("Song - 2009 Remaster"), "Song");
        assert_eq!(clean_title("Song (feat. Someone)"), "Song");
        assert_eq!(clean_title("Song feat. Someone"), "Song");
        assert_eq!(clean_title("Song ft. Someone"), "Song");
        assert_eq!(clean_title("Song (Part One)"), "Song (Part One)");
        assert_eq!(clean_title("Hyphen - Ated"), "Hyphen - Ated");
        assert_eq!(clean_title("(Remastered)"), "(Remastered)", "never empty");
        assert_eq!(clean_title("Feat\u{200b}ure"), "Feature");
    }

    #[test]
    fn artists_keep_their_first_name() {
        assert_eq!(clean_artist("TOOL;Tool"), "TOOL");
        assert_eq!(clean_artist("Artist feat. Guest"), "Artist");
        assert_eq!(clean_artist("Artist (feat. Guest)"), "Artist");
        assert_eq!(clean_artist("Beyoncé"), "Beyoncé");
    }

    #[test]
    fn matching_is_loose_about_case_accents_and_punctuation() {
        assert!(loose_match("Beyoncé", "beyonce"));
        assert!(loose_match("Rock & Roll", "rock and roll"));
        assert!(loose_match("Don't Stop", "dont stop"));
        assert!(loose_match("Song (Live)", "Song"));
        assert!(!loose_match("Something", "Else"));
        assert!(!loose_match("", "Else"));
    }

    fn record(track: &str, artist: &str, duration: f64, synced: bool) -> Record {
        Record {
            track_name: track.into(),
            artist_name: artist.into(),
            duration: Some(duration),
            instrumental: false,
            synced_lyrics: synced.then(|| "[00:01.00] a".to_string()),
            plain_lyrics: Some("a".to_string()),
        }
    }

    #[test]
    fn the_closest_length_wins_and_synced_breaks_ties() {
        let query = Query {
            media: media("0123456789abcdef0123456789abcdef01234567", "song-id"),
            artist: "Artist".into(),
            title: "Song".into(),
            album: String::new(),
            duration_ms: 200_000,
        };
        let picked = pick(
            vec![
                record("Song", "Artist", 260.0, true),
                record("Song", "Artist", 201.0, false),
                record("Song", "Artist", 202.0, true),
                record("Other", "Artist", 200.0, true),
            ],
            &query,
        )
        .unwrap();
        assert_eq!(picked.duration, Some(202.0));
        assert!(pick(vec![record("Song", "Artist", 400.0, true)], &query).is_none());
    }

    #[test]
    fn lrc_lines_parse_and_sort() {
        let lines = parse_lrc(
            "[ar:Someone]\n[00:12.50]First\n[00:05]Early\n[01:00.1][02:00.123]Twice\n\nNo stamp\n",
        );
        let times: Vec<u32> = lines.iter().filter_map(|line| line.at_ms).collect();
        assert_eq!(times, vec![5_000, 12_500, 60_100, 120_123]);
        assert_eq!(lines[0].text, "Early");
        assert_eq!(lines[2].text, "Twice");
        assert_eq!(lines[3].text, "Twice");
    }

    #[test]
    fn the_active_line_is_the_last_one_started() {
        let lyrics = Lyrics {
            lines: parse_lrc("[00:05]a\n[00:10]b\n[00:15]c"),
            synced: true,
            instrumental: false,
        };
        assert_eq!(lyrics.active_line(0), None);
        assert_eq!(lyrics.active_line(5_000), Some(0));
        assert_eq!(lyrics.active_line(12_000), Some(1));
        assert_eq!(lyrics.active_line(99_000), Some(2));
        let plain = Lyrics {
            lines: vec![Line::default()],
            synced: false,
            instrumental: false,
        };
        assert_eq!(plain.active_line(5_000), None);
    }

    #[test]
    fn records_turn_into_lyrics() {
        let mut instrumental = record("Song", "Artist", 100.0, false);
        instrumental.instrumental = true;
        assert!(instrumental.lyrics().unwrap().instrumental);
        let synced = record("Song", "Artist", 100.0, true).lyrics().unwrap();
        assert!(synced.synced);
        let plain = record("Song", "Artist", 100.0, false).lyrics().unwrap();
        assert!(!plain.synced);
        assert_eq!(plain.lines.len(), 1);
        let nothing = Record::default();
        assert!(nothing.lyrics().is_none());
    }

    #[test]
    fn server_text_supports_plain_and_lrc_without_a_supplier_parser() {
        let plain = from_server_text("first\nsecond").unwrap();
        assert!(!plain.synced);
        assert_eq!(plain.lines[1].text, "second");

        let synced = from_server_text("[00:01.00]first\n[00:02.50]second").unwrap();
        assert!(synced.synced);
        assert_eq!(synced.lines[1].at_ms, Some(2_500));
        assert!(from_server_text("  ").is_none());
    }

    #[test]
    fn lyric_cache_keys_are_profile_scoped_and_secret_free() {
        let query = |profile| Query {
            media: media(profile, "same raw id"),
            artist: "Artist".into(),
            title: "Song".into(),
            album: "Album".into(),
            duration_ms: 180_000,
        };
        let first = query("0123456789abcdef0123456789abcdef01234567");
        let second = query("fedcba9876543210fedcba9876543210fedcba98");
        assert_ne!(cache_key(&first), cache_key(&second));
        assert_eq!(cache_key(&first).len(), 40);
        assert!(!cache_key(&first).contains("same raw id"));
    }

    #[test]
    fn fetch_rejects_lyrics_from_another_profile_before_any_network_lookup() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let credentials =
            crate::auth::Credentials::new("https://music.example.test", "alice", "secret").unwrap();
        let client = ApiClient::with_default_activity(credentials).unwrap();
        let http = reqwest::Client::new();
        let cache_dir = std::env::temp_dir().join(format!(
            "fastpotify-lyrics-test-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let query = Query {
            media: media("fedcba9876543210fedcba9876543210fedcba98", "song"),
            artist: "Artist".into(),
            title: "Song".into(),
            album: String::new(),
            duration_ms: 180_000,
        };
        let error = runtime
            .block_on(fetch(&client, &http, &cache_dir, &query))
            .unwrap_err()
            .to_string();
        assert!(error.contains("active song profile"));
    }

    #[test]
    fn fetch_rejects_non_song_media_references() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let credentials =
            crate::auth::Credentials::new("https://music.example.test", "alice", "secret").unwrap();
        let client = ApiClient::with_default_activity(credentials).unwrap();
        let http = reqwest::Client::new();
        let cache_dir = std::env::temp_dir().join(format!(
            "fastpotify-lyrics-test-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let query = Query {
            media: MediaId::new(client.profile_id().clone(), MediaKind::Album, "album"),
            artist: "Artist".into(),
            title: "Song".into(),
            album: String::new(),
            duration_ms: 180_000,
        };
        let error = runtime
            .block_on(fetch(&client, &http, &cache_dir, &query))
            .unwrap_err()
            .to_string();
        assert!(error.contains("active song profile"));
    }
}
