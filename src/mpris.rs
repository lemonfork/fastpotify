//! Linux desktop media controls (MPRIS) for Fastpotify.
//!
//! D-Bus runs on its own thread with a local executor and exchanges bounded
//! messages with the interface, which stays the only owner of playback
//! decisions. A slow or absent session bus therefore cannot stall audio or
//! the window.

use std::sync::mpsc::{Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use mpris_server::{LoopStatus, Metadata, PlaybackStatus, Player, Time, TrackId};
use tokio::sync::mpsc as tokio_mpsc;

use crate::media::{MediaCommand, MediaState, MediaTrack};
use crate::player::{Playback, RepeatMode};

const PLAYING_POSITION_INTERVAL: Duration = Duration::from_millis(1000);
const TRACK_OBJECT_PATH_PREFIX: &str = "/me/paolino/Fastpotify/Track/r_";

enum Update {
    State(MediaState),
    Seeked(u32),
}

pub struct MediaService {
    updates: tokio_mpsc::UnboundedSender<Update>,
    commands: Receiver<MediaCommand>,
    published: Option<MediaState>,
    last_position_update: Instant,
}

impl MediaService {
    pub fn spawn(wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (updates, update_rx) = tokio_mpsc::unbounded_channel();
        let (command_tx, commands) = std::sync::mpsc::channel();
        let wake: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(wake);
        let spawned = thread::Builder::new()
            .name("fastpotify-mpris".to_string())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        log::warn!("MPRIS runtime unavailable: {error}");
                        return;
                    }
                };
                let local = tokio::task::LocalSet::new();
                let outcome = local.block_on(&runtime, run(update_rx, command_tx, wake));
                if let Err(error) = outcome {
                    log::warn!("MPRIS is unavailable: {error}");
                }
            });
        if let Err(error) = spawned {
            log::warn!("unable to start the MPRIS thread: {error}");
        }
        Self {
            updates,
            commands,
            published: None,
            last_position_update: Instant::now() - PLAYING_POSITION_INTERVAL,
        }
    }

    pub fn drain_commands(&self) -> Vec<MediaCommand> {
        self.commands.try_iter().collect()
    }

    /// Publishes structural changes immediately; the position once a second
    /// while playing, since clients interpolate between updates.
    pub fn update(&mut self, state: MediaState) {
        let structural = self
            .published
            .as_ref()
            .is_none_or(|published| !same_except_position(published, &state));
        let position_due = state.playback != Playback::Playing
            || self.last_position_update.elapsed() >= PLAYING_POSITION_INTERVAL;
        let position_changed = self
            .published
            .as_ref()
            .is_none_or(|published| published.position_ms != state.position_ms);
        if !structural && (!position_changed || !position_due) {
            return;
        }
        if self.updates.send(Update::State(state.clone())).is_ok() {
            self.published = Some(state);
            self.last_position_update = Instant::now();
        }
    }

    pub fn seeked(&self, position_ms: u32) {
        let _ = self.updates.send(Update::Seeked(position_ms));
    }
}

fn same_except_position(left: &MediaState, right: &MediaState) -> bool {
    left.playback == right.playback
        && left.track == right.track
        && (left.volume - right.volume).abs() < 0.005
        && left.shuffle == right.shuffle
        && left.repeat == right.repeat
        && left.can_control == right.can_control
}

async fn run(
    mut updates: tokio_mpsc::UnboundedReceiver<Update>,
    commands: Sender<MediaCommand>,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
) -> mpris_server::zbus::Result<()> {
    let player = Player::builder("fastpotify")
        .identity("Fastpotify")
        .desktop_entry(desktop_entry())
        .can_raise(true)
        .can_quit(true)
        .can_control(true)
        .can_play(true)
        .can_pause(true)
        .can_go_next(true)
        .can_go_previous(true)
        .can_seek(true)
        .supported_uri_schemes(vec!["fastpotify".to_string()])
        .build()
        .await?;

    let send = {
        let commands = commands.clone();
        let wake = wake.clone();
        move |command: MediaCommand| {
            if commands.send(command).is_ok() {
                wake();
            }
        }
    };
    {
        let send = send.clone();
        player.connect_play(move |_| send(MediaCommand::Play));
    }
    {
        let send = send.clone();
        player.connect_pause(move |_| send(MediaCommand::Pause));
    }
    {
        let send = send.clone();
        player.connect_play_pause(move |_| send(MediaCommand::PlayPause));
    }
    {
        let send = send.clone();
        player.connect_stop(move |_| send(MediaCommand::Stop));
    }
    {
        let send = send.clone();
        player.connect_next(move |_| send(MediaCommand::Next));
    }
    {
        let send = send.clone();
        player.connect_previous(move |_| send(MediaCommand::Previous));
    }
    {
        let send = send.clone();
        player.connect_seek(move |_, offset| send(MediaCommand::SeekBy(offset.as_millis())));
    }
    {
        let send = send.clone();
        player.connect_set_position(move |_, track_id, position| {
            if let Some(media_ref) = media_ref_from_object_path(track_id.as_str()) {
                send(MediaCommand::SetPosition {
                    track_uri: media_ref,
                    position_ms: position.as_millis().max(0) as u32,
                });
            }
        });
    }
    {
        let send = send.clone();
        player.connect_set_volume(move |_, volume| send(MediaCommand::SetVolume(volume)));
    }
    {
        let send = send.clone();
        player.connect_set_shuffle(move |_, shuffle| send(MediaCommand::SetShuffle(shuffle)));
    }
    {
        let send = send.clone();
        player.connect_set_loop_status(move |_, status| {
            send(MediaCommand::SetRepeat(match status {
                LoopStatus::None => RepeatMode::Off,
                LoopStatus::Track => RepeatMode::Track,
                LoopStatus::Playlist => RepeatMode::Context,
            }))
        });
    }
    {
        let send = send.clone();
        player.connect_open_uri(move |_, uri| {
            if crate::media::is_media_ref(uri) {
                send(MediaCommand::OpenUri(uri.to_string()));
            }
        });
    }
    {
        let send = send.clone();
        player.connect_raise(move |_| send(MediaCommand::Raise));
    }
    {
        let send = send.clone();
        player.connect_quit(move |_| send(MediaCommand::Quit));
    }

    let server = player.run();
    let apply = async {
        let mut published: Option<MediaState> = None;
        while let Some(update) = updates.recv().await {
            match update {
                Update::Seeked(position_ms) => {
                    let _ = player.seeked(Time::from_millis(position_ms as i64)).await;
                }
                Update::State(state) => {
                    let previous = published.as_ref();
                    if previous.is_none_or(|p| p.playback != state.playback) {
                        let _ = player
                            .set_playback_status(playback_status(state.playback))
                            .await;
                    }
                    if previous.is_none_or(|p| p.track != state.track) {
                        let _ = player.set_metadata(metadata(state.track.as_ref())).await;
                        let _ = player.set_can_seek(state.track.is_some()).await;
                    }
                    if previous.is_none_or(|p| (p.volume - state.volume).abs() >= 0.005) {
                        let _ = player.set_volume(state.volume).await;
                    }
                    if previous.is_none_or(|p| p.shuffle != state.shuffle) {
                        let _ = player.set_shuffle(state.shuffle).await;
                    }
                    if previous.is_none_or(|p| p.repeat != state.repeat) {
                        let _ = player.set_loop_status(loop_status(state.repeat)).await;
                    }
                    player.set_position(Time::from_millis(state.position_ms as i64));
                    published = Some(state);
                }
            }
        }
    };
    tokio::select! {
        _ = server => {}
        _ = apply => {}
    }
    Ok(())
}

fn playback_status(playback: Playback) -> PlaybackStatus {
    match playback {
        Playback::Playing => PlaybackStatus::Playing,
        Playback::Paused | Playback::Loading => PlaybackStatus::Paused,
        Playback::Stopped => PlaybackStatus::Stopped,
    }
}

fn loop_status(mode: RepeatMode) -> LoopStatus {
    match mode {
        RepeatMode::Off => LoopStatus::None,
        RepeatMode::Context => LoopStatus::Playlist,
        RepeatMode::Track => LoopStatus::Track,
    }
}

fn metadata(track: Option<&MediaTrack>) -> Metadata {
    let Some(track) = track else {
        return Metadata::new();
    };
    let mut builder = Metadata::builder()
        .title(track.title.clone())
        .length(Time::from_millis(track.duration_ms as i64));
    if let Some(media_ref) = publishable_media_ref(&track.uri) {
        builder = builder.url(media_ref);
        if let Some(track_id) = object_path_for(&track.uri) {
            builder = builder.trackid(track_id);
        }
    }
    if !track.artists.is_empty() {
        builder = builder.artist(track.artists.clone());
    }
    if !track.album.is_empty() {
        builder = builder.album(track.album.clone());
    }
    // `fastpotify-art:` is an internal opaque reference, not a URL desktop
    // clients can dereference. Authenticated cover URLs must never leave the
    // Navidrome client, so omit mpris:artUrl until a verified cached file URI
    // is available.
    builder.build()
}

fn publishable_media_ref(value: &str) -> Option<String> {
    crate::media::is_media_ref(value).then(|| value.to_owned())
}

/// MPRIS track IDs are D-Bus object paths rather than arbitrary strings. Hex
/// encodes the complete opaque reference into one reversible path component;
/// no provider URI is inferred when a SetPosition call comes back.
fn object_path_for(media_ref: &str) -> Option<TrackId> {
    if !crate::media::is_media_ref(media_ref) {
        return None;
    }
    let mut encoded = String::with_capacity(media_ref.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in media_ref.bytes() {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    TrackId::try_from(format!("{TRACK_OBJECT_PATH_PREFIX}{encoded}")).ok()
}

fn media_ref_from_object_path(path: &str) -> Option<String> {
    let encoded = path.strip_prefix(TRACK_OBJECT_PATH_PREFIX)?;
    if encoded.is_empty() || encoded.len() % 2 != 0 {
        return None;
    }
    let mut decoded = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.as_bytes().chunks_exact(2) {
        decoded.push(hex_nibble(pair[0])? << 4 | hex_nibble(pair[1])?);
    }
    let media_ref = String::from_utf8(decoded).ok()?;
    crate::media::is_media_ref(&media_ref).then_some(media_ref)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

/// The desktop entry's name: inside a Flatpak the entry is exported under
/// the app id, and a desktop looking it up by the plain name finds nothing.
fn desktop_entry() -> &'static str {
    if std::path::Path::new("/.flatpak-info").exists() {
        "rocks.fastpotify.Fastpotify"
    } else {
        "fastpotify"
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
    fn opaque_object_paths_round_trip_without_reconstructing_a_provider_uri() {
        let media_ref = MediaId::new(profile(), MediaKind::Song, "opaque:/? 音乐").uri();
        let path = object_path_for(&media_ref).unwrap();
        assert_eq!(
            media_ref_from_object_path(path.as_str()).as_deref(),
            Some(media_ref.as_str())
        );
    }

    #[test]
    fn legacy_provider_uris_and_malformed_paths_are_rejected() {
        assert!(object_path_for("legacy:track:old-id").is_none());
        assert!(media_ref_from_object_path(TRACK_OBJECT_PATH_PREFIX).is_none());
        assert!(
            media_ref_from_object_path(&format!("{TRACK_OBJECT_PATH_PREFIX}not_hex")).is_none()
        );
    }

    #[test]
    fn only_secret_free_media_references_are_publishable_as_metadata_urls() {
        let media_ref = MediaId::new(profile(), MediaKind::Song, "track-1").uri();
        let art_ref = ArtworkRef::new(profile(), "cover-1").uri();
        assert_eq!(
            publishable_media_ref(&media_ref).as_deref(),
            Some(media_ref.as_str())
        );
        let metadata = metadata(Some(&MediaTrack {
            uri: media_ref,
            art_url: Some(art_ref),
            ..MediaTrack::default()
        }));
        assert!(metadata.art_url().is_none());

        let stream = "https://music.example/rest/stream?id=1&u=user&t=secret";
        assert!(publishable_media_ref(stream).is_none());
    }
}
