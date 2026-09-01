//! Shared state and commands for desktop media controls.
//!
//! Platform modules translate this interface to MPRIS on Linux, System Media
//! Transport Controls on Windows, and Now Playing on macOS.

use crate::player::{Playback, RepeatMode};

/// Maximum size accepted for a media reference crossing a desktop-control
/// boundary. OpenSubsonic identifiers are strings and can be long, but a
/// bounded value keeps the loopback protocol and D-Bus metadata cheap to
/// validate.
pub const MAX_MEDIA_REF_LEN: usize = 8 * 1024;

/// Whether `reference` is a canonical, secret-free Fastpotify media reference.
///
/// The reference deliberately carries only a media kind, a non-secret profile
/// fingerprint, and a base64url-encoded server ID. In particular, URL query
/// characters are not accepted, so an authenticated OpenSubsonic stream or
/// cover URL cannot cross IPC or desktop media-control boundaries by mistake.
pub fn is_media_ref(reference: &str) -> bool {
    if reference.is_empty()
        || reference.len() > MAX_MEDIA_REF_LEN
        || !reference
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'-' | b'_'))
    {
        return false;
    }

    let Ok(id) = reference.parse::<crate::api::models::MediaId>() else {
        return false;
    };
    valid_profile(id.profile.as_str()) && !id.raw().is_empty() && id.uri() == reference
}

/// Whether `reference` is a canonical, secret-free Fastpotify artwork
/// reference suitable for publication to a desktop media service.
pub fn is_artwork_ref(reference: &str) -> bool {
    if reference.is_empty()
        || reference.len() > MAX_MEDIA_REF_LEN
        || !reference
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'-' | b'_'))
    {
        return false;
    }

    let Ok(art) = reference.parse::<crate::api::models::ArtworkRef>() else {
        return false;
    };
    valid_profile(art.profile.as_str()) && !art.id.is_empty() && art.uri() == reference
}

fn valid_profile(profile: &str) -> bool {
    profile.len() == 40
        && profile
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone, Debug, PartialEq)]
pub enum MediaCommand {
    Play,
    Pause,
    PlayPause,
    Stop,
    Next,
    Previous,
    SeekBy(i64),
    SetPosition {
        track_uri: String,
        position_ms: u32,
    },
    SetVolume(f64),
    SetShuffle(bool),
    SetRepeat(RepeatMode),
    /// A platform `OpenUri` request already reduced to a validated
    /// `fastpotify:` media reference by the platform adapter.
    OpenUri(String),
    Raise,
    Quit,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaTrack {
    /// Canonical, secret-free `fastpotify:` media reference.
    pub uri: String,
    pub title: String,
    pub artists: Vec<String>,
    pub album: String,
    /// Canonical `fastpotify-art:` reference, never a provider request URL.
    pub art_url: Option<String>,
    pub duration_ms: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MediaState {
    pub playback: Playback,
    pub track: Option<MediaTrack>,
    pub position_ms: u32,
    pub volume: f64,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub can_control: bool,
}

impl Default for MediaState {
    fn default() -> Self {
        Self {
            playback: Playback::Stopped,
            track: None,
            position_ms: 0,
            volume: 1.0,
            shuffle: false,
            repeat: RepeatMode::Off,
            can_control: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::models::{ArtworkRef, MediaId, MediaKind, ProfileId};

    fn profile() -> ProfileId {
        ProfileId::new("0123456789abcdef0123456789abcdef01234567")
    }

    #[test]
    fn canonical_media_refs_accept_arbitrary_server_ids_without_exposing_them() {
        let id = MediaId::new(profile(), MediaKind::Song, "unicode / id ?u=secret 音乐");
        let reference = id.uri();

        assert!(is_media_ref(&reference));
        assert!(!reference.contains('?'));
        assert!(!reference.contains("secret"));
        assert_eq!(reference.parse::<MediaId>().unwrap().raw(), id.raw());
    }

    #[test]
    fn media_refs_reject_urls_legacy_uris_and_noncanonical_shapes() {
        let profile = "0123456789abcdef0123456789abcdef01234567";
        assert!(!is_media_ref(
            "https://music.example/rest/stream?id=1&t=secret"
        ));
        assert!(!is_media_ref("legacy:track:old-id"));
        assert!(!is_media_ref(&format!("fastpotify:song:{profile}:")));
        assert!(!is_media_ref(&format!(
            "fastpotify:song:{profile}:c29uZw=="
        )));
        assert!(!is_media_ref("fastpotify:song:not-a-profile:c29uZw"));
        assert!(!is_media_ref(&"fastpotify:song:".repeat(MAX_MEDIA_REF_LEN)));
    }

    #[test]
    fn only_secret_free_artwork_refs_are_publishable() {
        let reference = ArtworkRef::new(profile(), "cover / 艺术").uri();
        assert!(is_artwork_ref(&reference));
        assert!(!is_artwork_ref(
            "https://music.example/rest/getCoverArt?id=cover&u=user&t=secret"
        ));
        assert!(!is_artwork_ref(&format!(
            "fastpotify-art:{}:",
            profile().as_str()
        )));
    }
}
