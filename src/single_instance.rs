//! Single-instance guard and remote-control channel.
//!
//! A second instance would duplicate the local player, MPRIS service, and tray
//! icon. A second launch raises the running instance and exits.
//!
//! Linux uses a non-queued D-Bus well-known name as the guard and MPRIS
//! `Raise` to show the running instance. D-Bus releases the name when the
//! process ends, including after a crash.
//!
//! This uses zbus's blocking API because the build includes both async-io and
//! tokio executors. The blocking connection avoids depending on either runtime
//! during the startup check.
//!
//! macOS and Windows use an exclusive loopback socket for the guard and
//! control channel. The operating system releases the port when the process
//! ends.
//!
//! On macOS and Windows, clients send one `fastpotify:<verb>` line and receive
//! one reply. Commands enter the same action queue as tray and media-key
//! events. Read commands use snapshots, so the listener thread never accesses
//! app state. Linux uses MPRIS for these controls.
//!
//! The Stream Deck plugin uses the same channel. It can set shuffle and repeat,
//! favorite the current track, and play a secret-free media reference. Clients
//! poll the current snapshot; the app does not push updates.
//!
//! Any local process can reach the port, so `play-ref` validates its free-text
//! argument here before it enters the app.

/// The name held for the lifetime of the running instance.
#[cfg(target_os = "linux")]
const INSTANCE_NAME: &str = "rocks.fastpotify.Instance";

/// The MPRIS player to ask when another instance already holds the name.
#[cfg(target_os = "linux")]
const MPRIS_NAME: &str = "org.mpris.MediaPlayer2.fastpotify";

pub enum Outcome {
    /// This process is the only instance. Hold the guard until it exits.
    Only(Guard),
    /// Another instance is running and has been asked to show its window.
    Surfaced,
}

/// What a control client asked the running instance to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlCommand {
    /// Bring the window forward, creating it if needed.
    Show,
    PlayPause,
    Play,
    Pause,
    Next,
    Previous,
    /// Milliseconds; negative seeks backwards.
    SeekBy(i64),
    /// Percentage points; negative lowers the volume.
    VolumeBy(i8),
    /// Absolute percentage.
    SetVolume(u8),
    ToggleMute,
    ToggleShuffle,
    CycleRepeat,
    /// Set shuffle explicitly, avoiding missed toggle updates.
    SetShuffle(bool),
    /// Repeat set outright, for the same reason.
    SetRepeat(crate::player::RepeatMode),
    /// Absolute position, in milliseconds.
    SeekTo(u32),
    /// Favorite the playing track, or take it back out.
    ToggleSaved,
    /// Play a validated, secret-free `fastpotify:` media reference.
    PlayRef(String),
}

/// Marks this process as the running instance until dropped.
pub struct Guard {
    #[cfg(target_os = "linux")]
    _connection: Option<mpris_server::zbus::blocking::Connection>,
    /// Filled by control clients, drained by the app every frame. On Linux
    /// the same requests arrive through MPRIS instead and this stays empty.
    commands: std::sync::Arc<std::sync::Mutex<Vec<ControlCommand>>>,
    /// Current-track snapshot for `nowplaying` requests.
    now_playing: std::sync::Arc<std::sync::Mutex<String>>,
}

impl Guard {
    /// The queue a control client's commands land in. The app drains it.
    pub fn commands(&self) -> std::sync::Arc<std::sync::Mutex<Vec<ControlCommand>>> {
        std::sync::Arc::clone(&self.commands)
    }

    /// The slot the app writes the now-playing snapshot into.
    pub fn now_playing_slot(&self) -> std::sync::Arc<std::sync::Mutex<String>> {
        std::sync::Arc::clone(&self.now_playing)
    }
}

/// Snapshot value reported when nothing is playing.
pub const NOTHING_PLAYING: &str = "stopped";

/// Loopback port that marks a running instance on platforms without a bus.
/// Registered to nothing; chosen high and out of the ephemeral range.
#[cfg(not(target_os = "linux"))]
const INSTANCE_PORT: u16 = 47_113;

/// Every request and reply starts with this, so a foreign program that
/// happens to hold the port is never mistaken for Fastpotify.
#[cfg(not(target_os = "linux"))]
const PREFIX: &str = "fastpotify:";
#[cfg(not(target_os = "linux"))]
const OK_REPLY: &str = "fastpotify:ok";
#[cfg(not(target_os = "linux"))]
const NOW_REPLY: &str = "fastpotify:now ";

/// What the running instance said back.
#[cfg(not(target_os = "linux"))]
pub enum Reply {
    /// The command was accepted.
    Ok,
    /// The `nowplaying` snapshot: [`NOTHING_PLAYING`], or tab-separated
    /// `state, title, artists, album, position_ms, duration_ms, volume,
    /// shuffle, repeat, artwork_ref, favorite, device`. The final device field
    /// is retained for compatibility and is empty or `local`.
    NowPlaying(String),
}

/// Sends one verb to the running instance and reads its reply.
#[cfg(not(target_os = "linux"))]
pub fn send(verb: &str) -> std::io::Result<Reply> {
    // Validate before opening the socket as well as on the server. This keeps
    // an accidental authenticated URL out of IPC even when this library API
    // is called directly rather than through the CLI parser.
    if parse(&format!("{PREFIX}{verb}")).is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid Fastpotify control command",
        ));
    }
    send_to(INSTANCE_PORT, verb)
}

#[cfg(not(target_os = "linux"))]
fn send_to(port: u16, verb: &str) -> std::io::Result<Reply> {
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, TcpStream};
    use std::time::Duration;

    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(format!("{PREFIX}{verb}\n").as_bytes())?;
    // The listener writes one line and closes. Older instances do not reply to
    // unknown verbs, so the read times out.
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    let line = reply.lines().next().unwrap_or("");
    if line == OK_REPLY {
        Ok(Reply::Ok)
    } else if let Some(snapshot) = line.strip_prefix(NOW_REPLY) {
        Ok(Reply::NowPlaying(snapshot.to_owned()))
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the port is held by something other than Fastpotify",
        ))
    }
}

#[cfg(not(target_os = "linux"))]
pub fn acquire(waker: &crate::backend::Waker) -> Outcome {
    use std::net::{Ipv4Addr, TcpListener};
    use std::sync::{Arc, Mutex};

    let unguarded = || Guard {
        commands: Default::default(),
        now_playing: Arc::new(Mutex::new(NOTHING_PLAYING.to_owned())),
    };

    let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, INSTANCE_PORT)) {
        Ok(listener) => listener,
        Err(_) => {
            // Raise the existing instance only if the port answers as Fastpotify.
            let answered = send("show").is_ok_and(|reply| matches!(reply, Reply::Ok));
            if answered {
                return Outcome::Surfaced;
            }
            log::warn!("port {INSTANCE_PORT} is busy but not with Fastpotify; running unguarded");
            return Outcome::Only(unguarded());
        }
    };

    let guard = unguarded();
    let commands = Arc::clone(&guard.commands);
    let now_playing = Arc::clone(&guard.now_playing);
    let waker = waker.clone();
    let spawned = std::thread::Builder::new()
        .name("fastpotify-instance".to_owned())
        .spawn(move || serve(listener, &commands, &now_playing, &waker));
    if let Err(error) = spawned {
        log::warn!("cannot listen for other launches: {error}");
    }
    Outcome::Only(guard)
}

/// Answers control clients until the listener closes. One request line and
/// one reply line per connection.
#[cfg(not(target_os = "linux"))]
fn serve(
    listener: std::net::TcpListener,
    commands: &std::sync::Mutex<Vec<ControlCommand>>,
    now_playing: &std::sync::Mutex<String>,
    waker: &crate::backend::Waker,
) {
    use std::io::Write;
    use std::time::Duration;

    let queue = |command| {
        commands
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(command);
        waker.wake();
    };

    for mut stream in listener.incoming().flatten() {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let Some(line) = read_line(&mut stream) else {
            continue;
        };
        match parse(&line) {
            Some(Request::Command(command)) => {
                let _ = stream.write_all(format!("{OK_REPLY}\n").as_bytes());
                queue(command);
            }
            Some(Request::NowPlaying) => {
                let snapshot = now_playing
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone();
                let snapshot = publishable_now_playing(&snapshot);
                let _ = stream.write_all(format!("{NOW_REPLY}{snapshot}\n").as_bytes());
            }
            // Not our client; say nothing and hang up.
            None => {}
        }
    }
}

/// Keeps the raw snapshot layout compatible while ensuring the two fields
/// that used to carry provider URLs and remote devices are safe to publish.
#[cfg(not(target_os = "linux"))]
fn publishable_now_playing(snapshot: &str) -> String {
    let mut fields: Vec<&str> = snapshot.split('\t').collect();
    if let Some(artwork) = fields.get_mut(9)
        && !artwork.is_empty()
        && !crate::media::is_artwork_ref(artwork)
    {
        *artwork = "";
    }
    if let Some(device) = fields.get_mut(11)
        && *device != "local"
    {
        *device = "";
    }
    fields.join("\t")
}

/// A parsed request line: a command for the app, or a read the listener
/// answers itself.
#[cfg(not(target_os = "linux"))]
enum Request {
    Command(ControlCommand),
    NowPlaying,
}

#[cfg(not(target_os = "linux"))]
fn parse(line: &str) -> Option<Request> {
    let verb = line.trim_end().strip_prefix(PREFIX)?;
    let (verb, argument) = match verb.split_once(' ') {
        Some((verb, argument)) => (verb, Some(argument.trim())),
        None => (verb, None),
    };
    let command = match (verb, argument) {
        ("show", None) => ControlCommand::Show,
        ("playpause", None) => ControlCommand::PlayPause,
        ("play", None) => ControlCommand::Play,
        ("pause", None) => ControlCommand::Pause,
        ("next", None) => ControlCommand::Next,
        ("previous", None) => ControlCommand::Previous,
        ("seek-by", Some(ms)) => ControlCommand::SeekBy(ms.parse().ok()?),
        ("seek-to", Some(ms)) => ControlCommand::SeekTo(ms.parse().ok()?),
        ("volume-by", Some(delta)) => ControlCommand::VolumeBy(delta.parse().ok()?),
        ("volume-set", Some(volume)) => ControlCommand::SetVolume(volume.parse().ok()?),
        ("mute", None) => ControlCommand::ToggleMute,
        ("shuffle", None) => ControlCommand::ToggleShuffle,
        ("shuffle-set", Some("on")) => ControlCommand::SetShuffle(true),
        ("shuffle-set", Some("off")) => ControlCommand::SetShuffle(false),
        ("repeat", None) => ControlCommand::CycleRepeat,
        // Match explicitly because `RepeatMode::from_api` maps unknown values
        // to `off`; control clients should reject them.
        ("repeat-set", Some("off")) => ControlCommand::SetRepeat(crate::player::RepeatMode::Off),
        ("repeat-set", Some("context")) => {
            ControlCommand::SetRepeat(crate::player::RepeatMode::Context)
        }
        ("repeat-set", Some("track")) => {
            ControlCommand::SetRepeat(crate::player::RepeatMode::Track)
        }
        ("favorite-toggle" | "save-toggle", None) => ControlCommand::ToggleSaved,
        ("play-ref" | "play-uri", Some(reference)) => {
            ControlCommand::PlayRef(media_ref(reference)?)
        }
        ("nowplaying", None) => return Some(Request::NowPlaying),
        _ => return None,
    };
    Some(Request::Command(command))
}

/// Validates a secret-free media reference received over the local control
/// port. Authenticated stream and artwork URLs are rejected by construction.
#[cfg(not(target_os = "linux"))]
fn media_ref(text: &str) -> Option<String> {
    crate::media::is_media_ref(text).then(|| text.to_owned())
}

/// Reads up to the first newline. A line too long to be one of ours, or any
/// read error, disqualifies the client.
#[cfg(not(target_os = "linux"))]
fn read_line(stream: &mut std::net::TcpStream) -> Option<String> {
    use std::io::Read;
    let mut buffer = [0u8; crate::media::MAX_MEDIA_REF_LEN + 64];
    let mut filled = 0;
    loop {
        if filled == buffer.len() {
            return None;
        }
        match stream.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => {
                filled += read;
                if buffer[..filled].contains(&b'\n') {
                    break;
                }
            }
            Err(_) => return None,
        }
    }
    let line = buffer[..filled].split(|&byte| byte == b'\n').next()?;
    String::from_utf8(line.to_vec()).ok()
}

#[cfg(target_os = "linux")]
pub fn acquire(_waker: &crate::backend::Waker) -> Outcome {
    use mpris_server::zbus::blocking::Connection;
    use mpris_server::zbus::fdo::{RequestNameFlags, RequestNameReply};

    let guard = |connection: Option<Connection>| Guard {
        _connection: connection,
        commands: Default::default(),
        now_playing: std::sync::Arc::new(std::sync::Mutex::new(NOTHING_PLAYING.to_owned())),
    };

    let connection = match Connection::session() {
        Ok(connection) => connection,
        Err(error) => {
            // No session bus at all: nothing to coordinate through, so run.
            log::debug!("no session bus, running unguarded: {error}");
            return Outcome::Only(guard(None));
        }
    };

    // Holding the D-Bus name marks this process as the running instance.
    // `NameTaken` is the normal second-launch result.
    match connection.request_name_with_flags(INSTANCE_NAME, RequestNameFlags::DoNotQueue.into()) {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {
            Outcome::Only(guard(Some(connection)))
        }
        Ok(_) | Err(mpris_server::zbus::Error::NameTaken) => {
            if !raise_running_instance(&connection) {
                log::warn!(
                    "Fastpotify is already running but did not answer; not starting a second copy"
                );
            }
            Outcome::Surfaced
        }
        Err(error) => {
            log::warn!("cannot check for a running instance, starting anyway: {error}");
            Outcome::Only(guard(None))
        }
    }
}

/// Asks the running instance to show its window, retrying briefly because it
/// may still be registering MPRIS when this launch arrives.
#[cfg(target_os = "linux")]
fn raise_running_instance(connection: &mpris_server::zbus::blocking::Connection) -> bool {
    for attempt in 0..10 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
        let raised = connection.call_method(
            Some(MPRIS_NAME),
            "/org/mpris/MediaPlayer2",
            Some("org.mpris.MediaPlayer2"),
            "Raise",
            &(),
        );
        if raised.is_ok() {
            return true;
        }
    }
    false
}

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    use super::*;
    use crate::api::models::{MediaId, MediaKind, ProfileId};
    use crate::player::RepeatMode;

    fn command(line: &str) -> Option<ControlCommand> {
        match parse(line) {
            Some(Request::Command(command)) => Some(command),
            _ => None,
        }
    }

    fn song_ref(id: &str) -> String {
        MediaId::new(
            ProfileId::new("0123456789abcdef0123456789abcdef01234567"),
            MediaKind::Song,
            id,
        )
        .uri()
    }

    #[test]
    fn parses_every_control_verb() {
        // #given / #when / #then
        assert_eq!(command("fastpotify:show\n"), Some(ControlCommand::Show));
        assert_eq!(
            command("fastpotify:playpause"),
            Some(ControlCommand::PlayPause)
        );
        assert_eq!(command("fastpotify:play"), Some(ControlCommand::Play));
        assert_eq!(command("fastpotify:pause"), Some(ControlCommand::Pause));
        assert_eq!(command("fastpotify:next"), Some(ControlCommand::Next));
        assert_eq!(
            command("fastpotify:previous"),
            Some(ControlCommand::Previous)
        );
        assert_eq!(
            command("fastpotify:seek-by -10000"),
            Some(ControlCommand::SeekBy(-10_000))
        );
        assert_eq!(
            command("fastpotify:volume-by +5"),
            Some(ControlCommand::VolumeBy(5))
        );
        assert_eq!(
            command("fastpotify:volume-set 40"),
            Some(ControlCommand::SetVolume(40))
        );
        assert_eq!(command("fastpotify:mute"), Some(ControlCommand::ToggleMute));
        assert_eq!(
            command("fastpotify:shuffle"),
            Some(ControlCommand::ToggleShuffle)
        );
        assert_eq!(
            command("fastpotify:repeat"),
            Some(ControlCommand::CycleRepeat)
        );
        assert_eq!(
            command("fastpotify:shuffle-set on"),
            Some(ControlCommand::SetShuffle(true))
        );
        assert_eq!(
            command("fastpotify:shuffle-set off"),
            Some(ControlCommand::SetShuffle(false))
        );
        assert_eq!(
            command("fastpotify:repeat-set track"),
            Some(ControlCommand::SetRepeat(RepeatMode::Track))
        );
        assert_eq!(
            command("fastpotify:repeat-set context"),
            Some(ControlCommand::SetRepeat(RepeatMode::Context))
        );
        assert_eq!(
            command("fastpotify:repeat-set off"),
            Some(ControlCommand::SetRepeat(RepeatMode::Off))
        );
        assert_eq!(
            command("fastpotify:seek-to 90000"),
            Some(ControlCommand::SeekTo(90_000))
        );
        assert_eq!(
            command("fastpotify:favorite-toggle"),
            Some(ControlCommand::ToggleSaved)
        );
        assert_eq!(
            command("fastpotify:save-toggle"),
            Some(ControlCommand::ToggleSaved)
        );
        let reference = song_ref("track-1");
        assert_eq!(
            command(&format!("fastpotify:play-ref {reference}")),
            Some(ControlCommand::PlayRef(reference.clone()))
        );
        // Older local-control clients may keep using the old verb, but the
        // argument is still required to be a new secret-free reference.
        assert_eq!(
            command(&format!("fastpotify:play-uri {reference}")),
            Some(ControlCommand::PlayRef(reference))
        );
        assert!(matches!(
            parse("fastpotify:nowplaying"),
            Some(Request::NowPlaying)
        ));
    }

    #[test]
    fn rejects_lines_that_are_not_ours() {
        assert!(parse("GET / HTTP/1.1").is_none());
        assert!(parse("fastpotify:frobnicate").is_none());
        assert!(parse("fastpotify:seek-by soon").is_none());
        assert!(parse("fastpotify:volume-set 999").is_none());
        assert!(parse("fastpotify:next please").is_none());
        assert!(parse("").is_none());
    }

    /// Free-text control arguments are validated before reaching the app.
    #[test]
    fn refuses_urls_secrets_and_legacy_provider_uris() {
        // #given / #when / #then
        assert!(command("fastpotify:play-ref http://example.com/pwn").is_none());
        assert!(
            command("fastpotify:play-ref https://music.example/rest/stream?id=1&u=user&t=secret")
                .is_none()
        );
        assert!(command("fastpotify:play-ref legacy:track:old-id").is_none());
        assert!(command("fastpotify:play-ref ../../etc/passwd").is_none());
        assert!(command("fastpotify:play-ref fastpotify:song:bad-profile:c29uZw").is_none());
        assert!(command("fastpotify:play-ref").is_none());
        assert!(command("fastpotify:play-uri").is_none());
        assert!(
            command(&format!(
                "fastpotify:play-ref {}",
                "x".repeat(crate::media::MAX_MEDIA_REF_LEN + 1)
            ))
            .is_none()
        );
        assert!(command("fastpotify:devices").is_none());
        assert!(command("fastpotify:transfer local").is_none());
        // A word that is not one of the three is refused rather than read
        // as `off`, which is what `RepeatMode::from_api` would have done.
        assert!(command("fastpotify:repeat-set sometimes").is_none());
        assert!(command("fastpotify:shuffle-set maybe").is_none());
        assert!(command("fastpotify:seek-to -1").is_none());
    }

    #[test]
    fn now_playing_never_publishes_authenticated_urls_or_remote_devices() {
        let snapshot = concat!(
            "playing\tTitle\tArtist\tAlbum\t1\t2\t50\toff\toff\t",
            "https://music.example/rest/getCoverArt?id=1&u=user&t=secret",
            "\tyes\tKitchen"
        );
        let public = publishable_now_playing(snapshot);
        let fields: Vec<_> = public.split('\t').collect();
        assert_eq!(fields.len(), 12);
        assert_eq!(fields[9], "");
        assert_eq!(fields[10], "yes");
        assert_eq!(fields[11], "");

        let art = crate::api::models::ArtworkRef::new(
            ProfileId::new("0123456789abcdef0123456789abcdef01234567"),
            "cover-1",
        )
        .uri();
        let safe = format!("playing\tT\tA\tB\t1\t2\t50\toff\toff\t{art}\tyes\tlocal");
        assert_eq!(publishable_now_playing(&safe), safe);
        assert_eq!(publishable_now_playing("stopped"), "stopped");
    }

    /// Socket commands reach the queue and reads return published snapshots.
    #[test]
    fn a_client_reaches_the_command_queue_and_the_snapshot() {
        use std::net::{Ipv4Addr, TcpListener};
        use std::sync::{Arc, Mutex};

        // #given
        let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, 0)) {
            Ok(listener) => listener,
            // Some managed sandboxes deny even loopback listeners. That is an
            // host restriction, not a regression in the IPC contract.
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("a loopback port: {error}"),
        };
        let port = listener.local_addr().expect("a bound address").port();
        let commands: Arc<Mutex<Vec<ControlCommand>>> = Default::default();
        let now_playing = Arc::new(Mutex::new("playing\tGo\tThe Band".to_owned()));
        let served = {
            let commands = Arc::clone(&commands);
            let now_playing = Arc::clone(&now_playing);
            let waker = crate::backend::Waker::default();
            std::thread::spawn(move || serve(listener, &commands, &now_playing, &waker))
        };

        // #when
        let accepted = send_to(port, "next").expect("a reply");
        let volume = send_to(port, "volume-by -5").expect("a reply");
        let favorited = send_to(port, "favorite-toggle").expect("a reply");
        let snapshot = send_to(port, "nowplaying").expect("a reply");
        let refused = send_to(port, "frobnicate");

        // #then
        assert!(matches!(accepted, Reply::Ok));
        assert!(matches!(volume, Reply::Ok));
        assert!(matches!(favorited, Reply::Ok));
        match snapshot {
            Reply::NowPlaying(line) => assert_eq!(line, "playing\tGo\tThe Band"),
            _ => panic!("nowplaying answered with something else"),
        }
        // An unknown verb gets no reply at all, so the client sees a closed
        // connection rather than a command it never sent being obeyed.
        assert!(refused.is_err());
        assert_eq!(
            *commands.lock().expect("the queue"),
            vec![
                ControlCommand::Next,
                ControlCommand::VolumeBy(-5),
                ControlCommand::ToggleSaved,
            ]
        );

        drop(served);
    }
}
