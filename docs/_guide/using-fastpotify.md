---
title: Everyday Use
description: Daily and Random mixes, favorites, playlists, the local queue, and play history.
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

## Daily and Random mixes

Home's **Daily mix** card opens its song-list page. The mix is generated
automatically once per local calendar day. Fastpotify weights the songs
listened to long enough to enter local history, how often their artists and
genres recur, and songs saved to Favorites. It also blends in candidates from
the server's random-song endpoint so a short history still produces a useful
mix. The generated order is stored for the active profile, so reopening the
app on the same day keeps the same mix.

Home's **Random mix** card opens a separate list sourced directly from
OpenSubsonic `getRandomSongs`. The Refresh button on that page requests a new
set without reloading the Daily mix or other Home sections. Refreshing only
changes the Random mix page; a mix already playing keeps its existing queue.

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
location and can clear it. Clearing history also discards that day's cached
Daily mix; Favorites and fresh random candidates can still supply its songs.
