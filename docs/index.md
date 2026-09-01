---
layout: home
title: Fastpotify
description: A fast, native Navidrome and OpenSubsonic client for Linux, macOS, and Windows, written in Rust.
permalink: /
hero:
  name: Fastpotify
  text: Your music server, native and fast
  tagline: A lightweight Navidrome and OpenSubsonic client with local playback and library access for Linux, macOS, and Windows.
  actions:
    - theme: brand
      text: Download
      link: /download/
    - theme: alt
      text: What is Fastpotify?
      link: /what-is-fastpotify/
    - theme: alt
      text: GitHub
      link: https://github.com/crmne/fastpotify
  image:
    src: /screenshot.png
    alt: "Fastpotify showing the Late night focus playlist with the queue panel open, a track playing, and the library in the sidebar"
    width: 1894
    height: 1037

features:
  - icon: ⚡
    title: Lightweight
    details: A native binary with no browser engine. It starts in well under a second and typically uses 100–250 MB of RAM.
  - icon: 🔊
    title: Local playback
    details: Stream from your server to this computer, with output-device controls, a local queue, shuffle, repeat, and an equalizer.
  - icon: 📚
    title: Library and search
    details: Browse playlists, favorites, albums, and artists. Search your server and edit playlists you own.
  - icon: 🎨
    title: Themes
    details: Use light, dark, or system mode. Pages and the player bar can take their colour from the album art.
  - icon: 📻
    title: Winamp mini player
    details: Ctrl+M (Cmd+Shift+M on macOS) opens a small player for classic Winamp 2 skins, with a spectrum analyser, equalizer, and playlist.
    link: /winamp/
    link_text: See it in action
  - icon: ⌨️
    title: Desktop controls
    details: Keyboard shortcuts, MPRIS media controls on Linux, and a tray option that keeps music playing after you close the window.
  - icon: 🔓
    title: Open source
    details: MIT-licensed Rust built with egui and the documented OpenSubsonic API. The docs explain its connections and stored credentials.
    link: https://github.com/crmne/fastpotify
    link_text: Read the source
---

## Winamp mini player

Load a classic `.wsz` skin from the
[Winamp Skin Museum](https://skins.webamp.org). The mini player includes an
analyser, equalizer, playlist, shade modes, and integer pixel scaling.

![The mini player wearing the built-in skin](/assets/images/winamp.png)

<style>
  /* The hero image slot is sized for a square logo; the screenshot needs the
     room. Page-scoped overrides, so the theme stays untouched. */
  .VPHero .image-container {
    width: 100% !important;
    height: auto !important;
    transform: none !important;
  }
  .VPHero .image-src {
    position: relative !important;
    top: auto !important;
    left: auto !important;
    transform: none !important;
    width: 100% !important;
    height: auto !important;
    max-width: 100% !important;
    max-height: none !important;
    padding: 0 !important;
    border-radius: 12px;
    box-shadow: 0 12px 48px rgba(0, 0, 0, 0.45);
  }
  @media (max-width: 959px) {
    .VPHero .image {
      margin: 0 0 24px !important;
    }
  }
</style>
