---
title: Getting Started
description: Install Fastpotify, connect a Navidrome/OpenSubsonic server, and start local playback.
nav_order: 2
---

## Install

The [Download page](/download/) has installers and archives for macOS,
Windows, and Linux.

Or build from source with the Rust version pinned by the repository:

```sh
git clone https://github.com/crmne/fastpotify
cd fastpotify
cargo install --path .
```

On Linux, install the GUI and audio development packages. On Arch:

```sh
sudo pacman -S --needed alsa-lib libpulse libxkbcommon wayland
```

On Debian or Ubuntu:

```sh
sudo apt install libasound2-dev libpulse-dev libxkbcommon-dev libwayland-dev libgl1-mesa-dev
```

Fastpotify uses system fonts for scripts its interface font does not cover.
Install Noto and Noto CJK on Linux if titles appear as empty boxes.

## Connect your server

Start Fastpotify and enter:

1. the Navidrome/OpenSubsonic base URL, including `https://` and any base
   path;
2. your username; and
3. your password.

Fastpotify verifies the server before it saves the credentials. Prefer HTTPS;
plain HTTP should be limited to a network you fully trust. The normalized
server URL cannot contain embedded credentials, a query, or a fragment.

The app stores one active profile in the platform state directory
(`~/.local/state/fastpotify/navidrome.json` on Linux). Signing out removes it.
There is no browser approval and no separate playback authorization.

## Basics

- **Double-click a song to play immediately.** Playback and the queue update
  optimistically; network and decode work stays off the UI thread.
- **Closing the window can keep music playing.** The menu-bar or tray menu
  offers Show Main Window, Previous, Play/Pause, Next, and Quit, with
  separators around the playback controls. This is configurable in Settings.
- **Use Space for play/pause, Ctrl+F or `/` for search, and Q for the queue.**
  Ctrl+/ shows all shortcuts.
- **Right-click rows and cards** to queue, favorite, add to a playlist, or
  copy the app's secret-free media reference.
- **Output-device, buffer, bitrate, theme, EQ, and Winamp controls** live in
  Settings.
