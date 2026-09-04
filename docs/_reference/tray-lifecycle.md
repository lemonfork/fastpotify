---
title: Tray Lifecycle
description: Tray ownership, startup states, and native menu actions.
nav_order: 3
---

## Ownership and states

`App` owns one `TrayService`. The service's state machine owns the native host
and its action channel; platform adapters only register, update, and
release native resources. Hiding a window or switching main/mini presentation
does not start or stop the tray.

| State | Meaning | What happens next |
| --- | --- | --- |
| `Stopped` | No active tray or pending actions | Explicit `start()` |
| `Stopping` | Native shutdown is pending; actions are disconnected | Wait for host exit before starting a replacement |
| `Running` | Native host is available | Menu actions and playback-label updates |
| `Unavailable` | Native creation or operation failed | Explicit `start()` retries |

Only `is_available()` allows closing the window into the tray. A configured
but uncreated or failed tray must not leave the listener with no way back.
Playback updates are remembered before startup or while unavailable, so a later
start uses the current Play/Pause label.
On macOS, raising or switching the window explicitly retries an unavailable
tray; an already-running tray remains untouched.

`App::new` prepares the service; `App::attach` starts it once the first native
window is ready. All platforms use the same calling sequence:

```rust
let mut tray = TrayService::new(move || waker.wake());
tray.start()?;             // Native window is ready; AppKit runs on its main thread.
tray.set_playing(playing)?;
for action in tray.drain_actions() {
    // Translate TrayAction to an application Action, then apply after drawing.
}
tray.stop();              // Application shutdown; Drop also releases resources.
```

Repeated starts and stops are idempotent. Reattaching a window calls `start()`
again without replacing a running tray.
Windows/Linux shutdown can be asynchronous: `start()` returns an error while
`state()` is `Stopping`, rather than registering overlapping native hosts.
After failed operation cleanup, the state becomes `Unavailable` for retry;
after an explicit stop it becomes `Stopped`.

## Window modes and platform hosts

macOS keeps the same window and AppKit loop for the main and mini players.
The window changes size, chrome, opacity, and level, while the status item
stays registered. The loop ends only at application exit, so the tray has no
separate suspend/resume protocol or native-item recreation workaround.
When leaving native fullscreen for mini mode, window sizing waits for
AppKit's completed-exit notification. The requested fullscreen flag clears
earlier than the animation finishes and cannot safely gate style changes.

Windows and Linux host their trays on independent loops, so ending a window
loop leaves them `Running`. Shutdown asks those loops to stop without waiting
for desktop work on the UI thread. AppKit resources are released on the main
thread.

## Actions

Native callbacks use a single `ActionSender` to enqueue a `TrayAction` and
wake the application. They never mutate playback or window state directly.
`App` owns the only mapping to application actions:

- `ShowMainWindow` restores the main window, including from mini mode.
- `ShowMiniPlayer` opens mini mode, or raises the existing mini window.
- `ShowCurrentWindow` handles a Dock click without changing main/mini mode.
- `ToggleWindowVisibility` preserves Linux's tray-icon click behavior.
- `PlayPause`, `Next`, `Previous`, and `Quit` use the normal application actions.

Explicit window-mode requests are resolved when application actions are
applied, so repeated or queued menu clicks do not accidentally toggle modes.

Stopping or failing drops the action channel, including queued clicks; a fresh start gets
a new one. Native menu IDs include an owner token, so a delayed event from
an old menu cannot control a replacement service. The native library's
process-global callback is registered once and dispatches to the active owner.

## Regression coverage

The state-machine tests use controlled host outcomes to cover failure,
recovery, idempotence, playback state, shutdown, and obsolete callbacks.
`tests/tray_macos.rs` runs real AppKit on the process main thread to verify
native item identity across repeated starts and mode changes, resource release,
and close-to-tray with an actually available host.
Window-mode tests separately verify no close command during a macOS mode
switch and preservation of the main and mini window geometry.
