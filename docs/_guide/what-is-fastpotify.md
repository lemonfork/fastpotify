---
title: What is Fastpotify?
description: A small native desktop client for Navidrome and compatible OpenSubsonic servers.
nav_order: 0
---

## Your music server, native and fast

Fastpotify is a Rust and [egui](https://github.com/emilk/egui) desktop client
for Navidrome and compatible OpenSubsonic servers. It runs on Linux, macOS,
and Windows, starts quickly, and has no embedded browser engine.

![Fastpotify showing a playlist with the queue open and a track playing](/screenshot.png)

## What it does

- Streams music from the signed-in server to this computer.
- Keeps a local, authoritative queue with play-next, duplicate occurrences,
  shuffle, repeat, seek, and session restore.
- Browses artists, albums, playlists, and favorites and searches the server.
- Creates, edits, and deletes playlists and adds or removes songs.
- Provides Home cards for a personalized Daily mix and refreshable Random mix,
  alongside recently added albums, frequently played albums, and local recent
  songs.
- Supports light, dark, and system themes plus album-art accents.
- Keeps playing from the tray and integrates with desktop media controls.
- Includes the Winamp mini player, spectrum analyser, oscilloscope, playlist,
  and ten-band equalizer.

## Deliberate limits

- Fastpotify plays only audio returned by the active server. It does not
  substitute tracks from another catalogue.
- Playback is local. Navidrome's Jukebox and saved play-queue endpoints are
  not presented as speaker transfer or remote-control features.
- The first implementation does not claim gapless playback, loudness
  normalization, or an on-disk audio cache.
- Playlist reordering is available only after the complete editable playlist
  is loaded and while no sort or filter changes the row order. Fastpotify
  replaces the complete ordered list in one request and never splits a
  reorder into partially visible mutations.
- Server capabilities differ. A missing optional shelf or lyrics result is
  shown as unavailable rather than being synthesized from a private API.

Bug reports should include `fastpotify.log`, `panic.log` after a crash, the
server name/version, and reproduction steps. See the
[issue form](https://github.com/crmne/fastpotify/issues/new/choose).
