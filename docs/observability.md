# Observability & Debugging

> **Implementation status (May 2026, ABI 0.12.0):** logging is live —
> apps call `log_*!` macros that route through
> `host_fire(FIRE::LOG, …)` and surface in the browser console with
> the `[package-id]` prefix. The WIT `interface log` shape sketched
> in [`abi.md §4.7`](abi.md) is the post-Component-Model target, not
> the current ABI v0 path. Trace spans, structured JSON sinks, dev
> overlays, and crash-toast are **not yet implemented** — see
> [§9](#9-not-yet-implemented).

## 1. Purpose

A wasm app runs inside a sandbox. Without deliberate observability
infrastructure, debugging it on Akiva is miserable. This doc fixes
the minimum bar — what is mandatory from v0.1 — and tracks where the
post-MVP improvements need to land.

## 2. Principles

1. **Every app log reaches the developer with no setup.** A `log_info!`
   inside an `Activity::on_event` lands in the browser console
   immediately, with the package id as a prefix.
2. **Traces are first-class — eventually.** `span_begin` /
   `span_end` will surface as `performance.mark` / `performance.measure`
   so the Chrome DevTools Performance panel can read them. *Not yet
   wired*; see §9.
3. **The runtime logs in the same channel.** Kernel and framework
   messages share the format with app messages; the developer reads
   one stream.

## 3. Current ABI (v0, raw)

App-side macros forward through the unified-ABI `host_fire` import:

```rust
use akiva_sdk::prelude::*;

log_trace!("frame {} drawn", n);   // dropped in release host
log_debug!("counter = {}", v);     // dropped in release host
log_info!("user tapped {}", tag);
log_warn!("retrying after {} failures", k);
log_error!("giving up: {err}");
```

These expand to `host_fire(FIRE::LOG, [level | msg_utf8])`, where
`level` is one of (`sdk-rust/src/log.rs`):

| const            | value | semantics                                                  |
|------------------|------:|------------------------------------------------------------|
| `LEVEL_TRACE`    | `0`   | very verbose; default-off in release host builds.          |
| `LEVEL_DEBUG`    | `1`   | dev-time detail; default-off in release host builds.       |
| `LEVEL_INFO`     | `2`   | the audible default level.                                 |
| `LEVEL_WARN`     | `3`   | noteworthy but recoverable.                                |
| `LEVEL_ERROR`    | `4`   | failure that needs developer attention.                    |

Note this is **five levels**, not four — `TRACE` is below `DEBUG`.

The kernel side decodes the args in `host.rs::dispatch_fire_log` and
calls a single `_log(level, message)` callback configured by the
platform. The browser HAL implementation lives in
`runtime/web/worker.js`; it routes by level into the corresponding
`console.{trace,debug,info,warn,error}` and prefixes each line with
`[<package-id>]`.

The kernel and framework write through the same `_log` callback,
prefixed with `kernel` / `framework.<subsystem>` rather than a
package id, so the order of lines reflects the actual sequence on
the worker.

## 4. Sinks (current and planned)

| Sink                  | Audience          | Status                                                                                                       |
|-----------------------|-------------------|--------------------------------------------------------------------------------------------------------------|
| Browser console       | Developer         | **Live.** Routed by level. The default browser filter chip at "Info" hides trace/debug noise.                |
| In-app log viewer     | End user / QA     | **Not implemented.** A Shell "Developer → Logs" view that tails an in-memory ring is on the v0.3 wishlist.   |
| Structured trace      | Host tooling      | **Not implemented.** `performance.mark` integration is the planned target for `span_*`; no ABI op exists yet. |
| `.akiva/logs/*.jsonl` | CI                | **Not implemented.** No dev-server file sink today; would land alongside `akiva devserver` enhancements.     |

## 5. Format

The current line format is plain text, owned by the host:

```
[com.akiva.calculator] user tapped digit 7
```

The structured-JSON layout shown below is the **post-MVP target** for
when we either ship the in-app log viewer or wire the file sink:

```json
{ "ts": 1763890240123, "level": "info", "src": "com.akiva.calculator",
  "msg": "user tapped digit 7", "span_id": 42 }
```

Apps don't have to do anything for this transition — the SDK already
treats messages as opaque strings, so the kernel can switch to JSON
encoding without breaking them.

## 6. Developer flags (planned)

| URL flag             | Effect                                                              | Status                  |
|----------------------|---------------------------------------------------------------------|-------------------------|
| `?akiva_debug=1`     | Forces `LEVEL_DEBUG` audible (host-side).                          | not implemented         |
| `?akiva_trace=1`     | Activates `span_begin` / `span_end` (otherwise no-op).             | not implemented         |
| `?akiva_devtools=1`  | Mounts an FPS / per-app memory / live-activities overlay in `tab.js`. | not implemented         |

These are listed so the design intent stays explicit; the URL parser
in `runtime/web/tab.js` does not currently look at any of them.

## 7. Panic and crash signalling

Today: when an app's wasm module traps, the watchdog in
`runtime/web/app_registry.js` logs an `[error]` line through the
same `_log` channel and tears down the worker. The activity is
moved to the `Crashed` lifecycle state on the framework side
(`activity-model.md §7`). **No user-facing crash toast** is
rendered yet — that needs the toast surface listed in
[`notifications.md §5`](notifications.md). For now, the developer
sees the failure in console; the user sees the activity vanish.

## 8. Performance monitoring

What ships today:

- The kernel pumps a frame budget (`16.6 ms` at 60 Hz). Frames that
  exceed it are not currently logged automatically — the
  per-transition diagnostic infrastructure used for the recent
  animation work is *opt-in*, kept off the hot path, and removed
  after debugging.

What's planned for v0.3+:

- Per-app metrics — memory in use, time-in-`on_event`,
  time-in-`on_render` — exposed through a single "metrics" IO op so
  a Shell-side overlay or external tooling can pull them.
- Export to Chrome DevTools Performance via `performance.mark` (same
  hook the eventual `span_*` ops will use).

## 9. Not yet implemented

| feature                                       | what it would unlock                                          | dep / blocker                                              |
|-----------------------------------------------|---------------------------------------------------------------|------------------------------------------------------------|
| `span_begin` / `span_end` ops                 | structured timing visible in DevTools                         | new fire op + worker-side `performance.mark` plumbing      |
| In-app log viewer                             | Shell-side log triage on a deployed device                    | Shell-side widget + cross-package log read API             |
| Structured JSON line format                   | `jq`-able CI logs, replayable sessions                        | Host-side encoder; no ABI change required                  |
| `?akiva_debug=1` / `?akiva_trace=1` flags     | Toggle verbosity / span emission without a rebuild            | URL parser in `tab.js`; level threshold in `worker.js`     |
| `?akiva_devtools=1` overlay                  | FPS, memory-per-app, live activities                          | Reuses the planned metrics IO op                           |
| Frame-budget overrun auto-log                 | Catch jank in CI, not by hand                                 | Cheap; just an `if (dt > 16.6) log_warn!(…)` in the kernel  |
| Crash toast                                   | User-visible failure indication                               | Toast surface in `WindowLayer::Overlay`                    |
| `.akiva/logs/<session>.jsonl` file sink       | Persistent CI trail                                           | dev-server file write surface                              |

## 10. Explicitly out of scope

- Remote logging (sending logs to a server). Plain HTTP is available
  through the network capability if an app chooses to ship its own.
- Replay-from-event-stream debugging. Possible long-term, no design.
- Persistent crash-report aggregation. Same — out of scope until we
  have multiple devices in the field.

None of those is required to develop on Akiva; everything in §9 is
the next-step list, everything here is permanently optional.
