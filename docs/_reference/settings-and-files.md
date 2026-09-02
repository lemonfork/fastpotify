---
title: Settings & Files
description: Where Fastpotify keeps configuration, Navidrome credentials, profile state, and caches.
nav_order: 0
---

## Where things live

Fastpotify follows each platform's conventions. On Linux:

| What | Where | Safe to delete? |
| --- | --- | --- |
| Settings | `~/.config/fastpotify/settings.json` | Yes, preferences reset |
| Winamp skins | `~/.config/fastpotify/skins/` | Yes, you add them again |
| Active server credential | `~/.local/state/fastpotify/navidrome.json` | Yes, you sign in again |
| Profile session | `~/.local/state/fastpotify/profiles/<profile>/session.json` | Yes |
| Profile play history | `~/.local/state/fastpotify/profiles/<profile>/history.json` | Yes |
| Today's Daily mix | `~/.local/state/fastpotify/profiles/<profile>/daily-mix.json` | Yes, it is regenerated |
| Artwork cache | `~/.cache/fastpotify/art/` | Always |
| Lyrics cache | `~/.cache/fastpotify/lyrics/` | Always |
| Last run's log | `~/.local/state/fastpotify/fastpotify.log` | Always |
| Crash log | `~/.local/state/fastpotify/panic.log` | Always |

The credential file contains the server URL, username, and password. It is
separate from settings and caches, atomically replaced, and owner-only on
Unix. Signing out deletes it. Logs, sessions, media references, and artwork
cache keys never contain request authentication parameters.

Profile directories use a non-secret fingerprint of the normalized server
URL and username. This prevents opaque OpenSubsonic IDs from one server being
restored against another. The existing application/config directory names
remain `fastpotify` so upgrades do not create a second set of preferences.

On macOS, settings, state, and logs are in
`~/Library/Application Support/me.paolino.fastpotify` and caches in
`~/Library/Caches/me.paolino.fastpotify`. On Windows, settings are in
`%APPDATA%\paolino\fastpotify\config`, state and logs in
`%LOCALAPPDATA%\paolino\fastpotify\data`, and caches in
`%LOCALAPPDATA%\paolino\fastpotify\cache`.

## settings.json

Settings are readable JSON and are written atomically. Unknown fields from an
older release are ignored, so removing an obsolete integration does not make
the file unreadable. In the app, Settings are grouped into Account, Playback,
Appearance, Winamp skins, Equalizer, Storage, and About. Wide windows keep the
section list in a left rail; the minimum width wraps it above the active
section. Main fields include:

| Field | Default | Meaning |
| --- | --- | --- |
| `bitrate` | `320` | Preferred server transcoding ceiling in kbps; an empty transcode falls back to the original file |
| `audio_device` | system default | Local output device |
| `audio_buffer_ms` | `100` | Output buffer; lower is more responsive, higher tolerates load |
| `theme` | `dark` | `dark`, `light`, or `system` |
| `accent_from_art` | `true` | Tint pages with album art |
| `volume` | `70%` | Last local volume |
| `sidebar_visible` | `true` | Show the library sidebar |
| `sidebar_compact` | `false` | Names only in the sidebar, without covers |
| `tracklist_compact` | `false` | One-line song rows without covers |
| `keep_playing_in_background` | `true` | Closing the main window keeps playback in the tray |
| `check_for_updates` | `true` | Ask GitHub once a day for a newer release |
| `pinned_contexts` | empty | Server-scoped media references pinned in the sidebar |
| `winamp_window` | `false` | Use the Winamp mini-player window |
| `skin` | built in | Winamp skin file or folder name |
| `skin_scale` | by display | Screen pixels per skin pixel, 1 to 4 |
| `winamp_on_top` | `false` | Keep the mini player above other windows |
| `vis` | `bars` | `bars`, `scope`, or `off` |
| `eq_on` | `false` | Apply the ten-band equalizer |
| `eq_preamp_db` | `0` | Equalizer preamp in dB |
| `eq_bands_db` | ten zeros | Bands from 60 Hz to 16 kHz in dB |
| `balance` | `0` | Left/right balance from -1 to 1 |
| `mono` | `false` | Play both channels the same |

## Command line

Run `fastpotify --help` for the complete list. Starting without a subcommand
opens or raises the app. Desktop-control verbs include `play`, `pause`,
`play-pause`, `next`, `previous`, `seek`, `seek-to`, `volume`, `mute`,
`shuffle`, `repeat`, `favorite`, `play-ref`, `now-playing`, and `show`.

`play-ref` accepts only a canonical, secret-free `fastpotify:` media
reference. On Linux the running app uses MPRIS; other platforms send commands
to the existing instance over its loopback control socket.

Attach `fastpotify.log` to bug reports. It contains the last run's output and
must not contain credentials. After a crash, attach `panic.log` too.

## Demo mode

Builds made with `cargo build --features demo` accept `--demo`, sample pages,
and `--demo-shot <PATH>` for deterministic UI screenshots. Demo mode does not
connect to a server or write settings.
