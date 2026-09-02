---
title: How It Connects
description: How Fastpotify authenticates to Navidrome/OpenSubsonic, streams music, and keeps credentials private.
nav_order: 1
---

## One server profile

Fastpotify connects directly to one Navidrome or OpenSubsonic-compatible
server. Sign-in needs its base URL, username, and password. The app verifies
the account, music folders, protocol version, and advertised OpenSubsonic
extensions before it saves the profile.

The password is stored only in `navidrome.json` in the platform state
directory. The file is atomically replaced and owner-only on Unix. It is not
written to settings, logs, media references, artwork keys, or desktop media
metadata. Signing out deletes it.

For every API request Fastpotify creates a fresh random salt and sends the
OpenSubsonic token `md5(password + salt)` alongside the username, client name,
API version, and JSON response format. This is the standard token
authentication defined by the
[OpenSubsonic API](https://opensubsonic.netlify.app/docs/api-reference/).

Use HTTPS whenever the server is not on a network you fully trust. Token
authentication keeps the password itself out of a request, but it does not
turn plain HTTP into an encrypted connection.

## Requests and streams

Metadata requests have a bounded timeout and a small concurrency limit. Audio
uses the server's `/rest/stream` endpoint through a separate client with no
total response timeout, because a healthy stream is intentionally long-lived.
Authenticated redirects are followed only within the configured origin.

The Random mix opened from Home uses the standard `getRandomSongs` endpoint.
The Daily mix is assembled locally from profile-scoped play history and
Favorites together with those random candidates; Fastpotify does not upload
listening history to a recommendation service. Daily mix is rebuilt
automatically for each local calendar day; refreshing Random mix is an
independent metadata request and cannot replace the queue snapshot of a mix
already playing.

When playback is explicitly started from the Random mix page, Fastpotify also
uses `getRandomSongs` to extend that playing context. It sends one continuation
request when three of the mix's own upcoming songs remain; manually queued
songs do not count toward the threshold. Only one continuation request can be
in flight at a time. Songs returned by the server are appended to the context
tail, leaving the current song and manual queue unchanged, and the same check
can fetch later batches while the server keeps returning songs. Continuation
requests do not replace the visible Random mix page, and that page's Refresh
request does not replace the playing context.

The first stream request asks the server for MP3 at the selected bitrate. If a
server reports success but the transcode ends before its first byte,
Fastpotify retries that song once with OpenSubsonic's `format=raw`. This keeps
a broken server transcoder from making the original unplayable. That fallback
play may exceed the selected bitrate because it is the original file.
The response bytes, rather than the filename or `Content-Type`, select the
container and codec. Symphonia handles the primary codec set, including ALAC;
when it can demux mono or stereo Ogg Opus but has no decoder for it, Fastpotify
uses its local pure-Rust Opus fallback. Other unsupported originals still fail
visibly instead of entering a retry loop.

Playback is local and authoritative. Fastpotify downloads bounded chunks,
decodes them off the UI thread, normalizes them to the output pipeline, applies
the equalizer, feeds the visualizers, and applies volume last. The queue and
transport state live in this local engine; OpenSubsonic's saved play-queue API
is not treated as remote playback control.

Fastpotify calls `scrobble` when a song starts and submits it once after about
30 seconds, or halfway through a shorter song. Starting a stream alone does
not make Navidrome count the play.

## Secret-free media references

OpenSubsonic IDs are arbitrary strings and can collide between servers.
Fastpotify therefore stores an opaque `fastpotify:` reference containing the
entity kind, a non-secret profile fingerprint, and an encoded server ID.
Artwork uses a separate `fastpotify-art:` reference. Neither contains the
server URL or authentication query.

Sessions, history, and disposable profile data are scoped by the same profile
fingerprint. Switching servers cannot make an ID from the previous server
open or play on the new one.

## Other connections

Fastpotify has no telemetry, analytics, or hosted service. It connects to:

- the signed-in server for metadata, artwork, lyrics, playlist changes,
  scrobbles, and audio;
- [LRCLIB](https://lrclib.net) only as a lyrics fallback when the server has
  no lyrics for the playing song; and
- GitHub once a day for update checks when that setting is enabled.

Artwork and lyrics are cached in the platform cache directory. Clearing them
never signs you out.
