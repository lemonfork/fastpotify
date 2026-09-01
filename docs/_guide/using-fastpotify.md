---
title: Everyday Use
description: Favorites, playlists, the local queue, and play history.
nav_order: 3
---

## Favorites and playlists

The sidebar keeps Favorites and your server playlists within reach. Pin an
album, artist, or playlist to keep it at the top. Sidebar ordering is local UI
personalization; it does not reorder anything on the server.

Favoriting a song, album, or artist updates the interface immediately and
then sends `star` or `unstar` to the server. If the request fails, Fastpotify
shows an actionable error and reconciles the affected view.

Owned playlists can be created, renamed, described, made public or private,
and deleted. Songs can be added by menu or drag, removed, and dragged into a
new order while the playlist is shown in its original unfiltered order.
Fastpotify sends the complete ordered song list in one replacement request,
so duplicate occurrences keep their exact positions. Read-only server
playlists do not show editing controls.

## Queue

The queue belongs to the local player, not a remote device. Manually queued
songs play before the remaining album or playlist context. The same song may
be queued more than once; each occurrence remains distinct. See
[The Queue's Rules](/queue/) for the complete contract.

## Recent songs and scrobbles

Fastpotify records a song locally after about 30 seconds, or halfway through a
shorter song. Paused time and seeking do not count. The same threshold submits
one OpenSubsonic scrobble to the server, while the start is announced with a
non-submission scrobble.

The local list is stored in the active profile's `history.json` and appears in
the queue panel's Recents tab and on Home. Settings → Storage shows its
location and can clear it.
