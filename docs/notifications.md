# Notification System

> **Implementation status (May 2026, ABI 0.12.0):** the Notification API
> reached production in **Stone 14.3** (browser `Notification` IO ops
> + SDK `akiva_sdk::notification` + main-thread `cap_req`/`cap_resp`
> bridge). The earlier `system.show-toast` MVP path described in
> drafts of this doc was **never shipped**; toasts and snackbars are
> the only notification surface still missing — they belong to the
> `WindowLayer::Overlay` slot in the WindowManager but no widget /
> ABI op exists for them yet. See [§5](#5-not-yet-implemented) for
> what is still open.

## 1. Purpose

The notification system lets an Akiva app surface a message to the
user **outside the canvas of its own activity**. Three flavours were
on the original design table:

- **Toasts.** Short bottom-edge message with no interaction.
- **Persistent notifications.** A row that sits in a tray, with title /
  body / icon and optional tap-to-launch + actions.
- **Foreground notifications.** A hint to the system that a background
  process is alive and important (e.g. "Download in progress").

Of the three, the **persistent** flavour ships in Phase 14: title +
body + tag, posted through `navigator.Notification`. Toasts and
foreground notifications are deferred (see §5).

## 2. What ships today (Stone 14.3 / ABI 0.11.0+)

The active surface lives in the main thread, not the kernel: the
browser `Notification` constructor needs a `Window` context and
`Notification.requestPermission()` must be invoked from a user
gesture. The kernel relays through the `cap_req` / `cap_resp` bridge
introduced in 14.3 — the worker fires an op, the main thread invokes
the browser API, and the result rides back as an async dispatch.

### IO ops (declared in `framework/src/wire/op_kinds.rs`)

| op                              | code     | args                                               | result                  |
|---------------------------------|----------|----------------------------------------------------|-------------------------|
| `IO::NOTIFY_REQUEST_PERMISSION` | `0x010F` | `[]`                                               | `[status: u8]` (0/1/2)  |
| `IO::NOTIFY_POST`               | `0x0110` | `[title:utf8 \| body:utf8 \| tag:utf8]` length-prefixed | `[notify_id: u32 LE]` |
| `FIRE::NOTIFY_CLOSE`            | `0x000C` | `[notify_id: u32 LE]`                              | _(fire-and-forget)_     |

The status codes match `NotificationPermission` in the browser:
`0 = Default`, `1 = Granted`, `2 = Denied`. `tag` collapses
duplicates per browser convention — repeated posts with the same tag
replace the previous one in the tray.

### SDK (`sdk-rust/src/notification.rs`)

```rust
use akiva_sdk::notification::{request_permission, post, NotifyPermission};

let granted = request_permission().await?;
if matches!(granted, NotifyPermission::Granted) {
    let n = post("Build done", "calculator-2.1.akiva", "build-status").await?;
    // The handle drops -> close fires automatically. Or call
    // n.close() explicitly. Idempotent.
}
```

The `Notification` handle owns the `notify_id` and on `Drop` issues
`FIRE::NOTIFY_CLOSE`. Apps that want a sticky notification keep the
handle alive; one-shot apps let it drop.

### Capability gate

Manifest must declare `akiva.permission.NOTIFICATIONS` (declared
`Normal` protection level — install-time auto-grant). Without the
grant the SDK calls return `NotifyError::Denied` before any browser
round-trip.

### Browser-side wiring (`runtime/web/app_registry.js`)

`AppRegistry` allocates a `cap_id` per pending request, posts to the
main thread, and resolves the worker's pending future via
`completeNotifyRequestPermission` / `completeNotifyPost`. The
per-package `_disposeCapsForPackage` cleanup ensures notifications
belonging to a torn-down package don't leak — same lifecycle hook
that cleans WebSocket / WebTransport handles.

## 3. Limits we accept by design

- **No browser push.** If the user closes every Akiva tab, no
  notifications fire. We don't run a Service Worker that listens for
  pushes — there is no OS to host one.
- **In-session only.** All notifications are scoped to an active
  Akiva session. This matches the platform's "OS-on-browser" framing:
  if you aren't *inside* the OS window, there is no kernel running.
- **No grouping / channels.** Android-style notification channels are
  not modeled; one flat permission, one flat tray.
- **No actions / inline replies.** The browser `Notification` API
  supports actions only through Service Workers; we don't ship one,
  so the post-Stone-14.3 surface is title + body + tag only.

## 4. Architecture sketch (kept for reference)

The original design also imagined an in-runtime `NotificationManager`
that the Shell would render alongside the browser's native tray. The
Stone 14.3 path bypasses this: we route directly through
`navigator.Notification`. A future Shell-side tray (a "drawer of
recent notifications" UI) would still be useful but is **not**
required for the capability to work.

If we later want one:

```
App → SDK.post(...) → cap_req → main thread → browser Notification
                                            ↘ shell-side tray (future)
```

The Shell would subscribe via a broadcast intent (see
[`intents.md §13`](intents.md)) once broadcasts ship.

## 5. Not yet implemented

| feature                                    | status                                                                                       |
|--------------------------------------------|----------------------------------------------------------------------------------------------|
| Toasts (`system.show-toast(msg, dur)`)     | **not implemented.** `WindowLayer::Overlay` exists in the WindowManager but no toast op / widget renders into it. |
| Foreground / ongoing notifications         | not implemented. Browser API exposes `requireInteraction` but we don't surface it.           |
| Notification actions / inline replies      | requires a Service Worker; out of scope.                                                     |
| Shell-side notification tray               | requires broadcast intents (post-MVP). The browser tray is the current UI.                    |
| Per-app notification channels              | not modeled. Single permission, single tray.                                                  |

## 6. Dependencies before we extend further

Before we re-open this design and add a Shell-side tray:

- **Cross-app read on `StorageManager`** — Shell needs to enumerate
  notifications from other packages.
- **Broadcast intents** ([`intents.md §13`](intents.md)) — the
  `akiva.intent.action.NOTIFICATION_POSTED` mechanism the original
  design depended on.
- **Toast surface in `WindowLayer::Overlay`** — needed even
  independently of notifications, but the same render path serves
  in-app notifications too.

Without those, "real" Akiva-native notifications stay deferred to
v0.3+. The Phase 14 browser-backed implementation is the supported
surface in the meantime.
