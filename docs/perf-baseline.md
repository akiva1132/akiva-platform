# Performance Baseline

This document fixes the perf baseline of Akiva at the close of
Phase 11 — after full isolation (worker-per-app), a real executor,
`TaskScope`, and the `Slot.pending` HashMap. It serves two purposes:
(1) state the targets and the measurement methodology, and (2) hold
**tables that can be filled with live numbers** and re-run on every
regression.

> **State (May 2026).** Targets and methodology are formalised. §6
> (SDK / framework microbenchmarks) is **complete** — 9 benches
> across `sdk-rust/tests/bench_*.rs` + `framework/tests/bench_lmk.rs`,
> with real numbers. **§3–§5 are still TBD** — they need a
> browser-side perf-probe app (see §9). No fictitious numbers are
> stored in this file.

## 1. Targets

| metric                                      | target            | source                                                                       |
|---------------------------------------------|-------------------|------------------------------------------------------------------------------|
| host call overhead (warm path)              | ≤ 1 ms            | [ROADMAP §11.7](../ROADMAP.md), [isolation-design.md §10.5](isolation-design.md) |
| `storage::read` (direct, readonly)          | ≤ 0.5 ms          | ROADMAP §11.7, [risks.md §3.2](risks.md)                                     |
| `storage::write` (kernel-routed)            | ≤ 2 ms            | ROADMAP §11.7                                                                |
| frame budget @ 60 Hz                        | 16.67 ms          | render contract                                                              |
| 3 apps in foreground                        | sustained ≥ 60 fps | ROADMAP §11.8                                                               |

Internal goals (no public commitment):

- spawn → first poll: ≤ 0.2 ms
- AKML inflate (5–15 nodes): ≤ 0.5 ms
- SavedState encode/decode (5-key blob): ≤ 1 ms

## 2. Methodology

### 2.1 In-process telemetry (already live)

**App-side bracketing** in
[`runtime/web/app_worker_shell.js::chargeCpu`](../runtime/web/app_worker_shell.js):
every wasm export call (`dispatch`, `poll_dispatch`, `get_result`,
`drive_spawned`) is wrapped in `performance.now()` and accumulates
into `cumulativeCpuMs`, sent back to the `AppRegistry` in the pong
reply.

**Host-side bracketing** in
[`runtime/web/worker.js`](../runtime/web/worker.js): the kernel HAL
records `hal_app_memory_current` and `hal_app_cpu_ms` per frame in
`sample_app_resources_into`. See
[`framework/src/cpu_budget.rs`](../framework/src/cpu_budget.rs) for
the rolling window.

**Public surface.**
`Framework::cpu_usage(package_id) -> CpuReport` and
`Framework::memory_usage(package_id) -> UsageReport` — the API the
perf-probe will consume.

### 2.2 Microbenchmarks (Rust, in tree)

```bash
cargo test --release -p akiva-sdk       --tests -- --ignored --nocapture
cargo test --release -p akiva-framework --tests -- --ignored --nocapture
```

Bench files live under `<crate>/tests/bench_*.rs`, gated behind
`#[ignore]` so a default `cargo test` stays fast. The harness is a
hand-rolled wallclock loop (`Instant::now()`, warmup → 50–100 outer
samples × INNER iterations → median ns/op). No `criterion` in the
workspace — keeps dependencies tight.

### 2.3 End-to-end (perf-probe, to add)

`apps/perf-probe/` — a regular app that runs scripted scenarios
(storage thrash, host-call burst) inside the live runtime and dumps
JSON to OPFS. Not yet written. See §9 for the driving plan.

## 3. Host call overhead

The path: app wasm → trampoline in `app_worker_shell.js` →
`postMessage` to leader (or shared-array buffer where applicable) →
`worker.js` deserialise → kernel → reverse on the way back.

| metric                                            | p50  | p95  | p99  | target            | status |
|---------------------------------------------------|------|------|------|-------------------|--------|
| no-op host call (loop, N=10 000)                  | TBD  | TBD  | TBD  | ≤ 1 ms p95        | ⏳     |
| log emit (`log_info!`, ~80 B msg)                 | TBD  | TBD  | TBD  | informational     | ⏳     |
| dispatch + single await + result                  | TBD  | TBD  | TBD  | ≤ 1 ms p95        | ⏳     |

**Predictions** (analytical, not measured, based on documented
Chrome `postMessage` cost ~0.05 ms): every row should land in
0.2–0.7 ms. The risk is p99 — `postMessage` tail latency can spike
to ~1.5 ms under load.

## 4. Storage operations

Since Stone 11.6.7 #3 there are two paths:

- **Direct readonly** — the app worker holds a per-key
  `SyncAccessHandle` cache rooted at `pkgs/<packageId>/`. A cache
  hit returns synchronously, no round-trip.
- **Kernel-routed** — fallback on miss, plus all writes.

| operation                                  | p50  | p95  | target | status |
|--------------------------------------------|------|------|--------|--------|
| read (direct, cache hit)                   | TBD  | TBD  | ≤ 0.5 ms | ⏳    |
| read (direct, cache miss → warm)           | TBD  | TBD  | n/a    | ⏳     |
| read (kernel-routed)                       | TBD  | TBD  | n/a    | ⏳     |
| write (kernel-routed)                      | TBD  | TBD  | ≤ 2 ms | ⏳     |
| remove                                     | TBD  | TBD  | n/a    | ⏳     |

**Predictions.** [`docs/risks.md §3.2`](risks.md) projected ~0.1 ms
for direct read and ~2 ms for a kernel-routed write. If
measurement confirms, the target is met. If a direct-read miss
exceeds 3 ms (OPFS lock contention), it's worth pre-opening
frequently-used handles in the background.

## 5. Frame budget @ 60 Hz, 3 apps in foreground

**Setup (planned).** Shell + 2 additional apps in foreground —
3 workers, 3 SAB rings. Capture: `perf-probe` overlay for 60
seconds (overlay not yet built).

| scenario                                       | mean fps | p1 fps | dropped frames / minute | status |
|------------------------------------------------|----------|--------|-------------------------|--------|
| idle (no input)                                | TBD      | TBD    | TBD                     | ⏳     |
| continuous tap on calculator                   | TBD      | TBD    | TBD                     | ⏳     |
| continuous drag on tic-tac-toe                 | TBD      | TBD    | TBD                     | ⏳     |
| 3 apps animating concurrently                  | TBD      | TBD    | TBD                     | ⏳     |

**Acceptance.** The exit criterion is "sustained 60 fps". The
working definition: p1 ≥ 55 fps and dropped frames / minute ≤ 15 on
Chrome 125+ (Linux x86_64 desktop). The mobile baseline (Pixel 7,
iPhone 14) is deferred — see §9.

## 6. SDK / framework microbenchmarks

The harness lives in `<crate>/tests/bench_*.rs`, gated `#[ignore]`
so default `cargo test` stays fast:

```bash
cargo test --release -p akiva-sdk       --tests -- --ignored --nocapture
cargo test --release -p akiva-framework --tests -- --ignored --nocapture
```

Methodology: warmup → 50–100 outer samples × INNER inner iters →
median ns/op (p50). See each bench file for specifics.

**Numbers** (Linux x86_64, Ryzen 7 5800X, `cargo test --release`,
captured 2026-05-02):

| benchmark                                                   | iters     | p50 ns/op | μs/op | source                                          |
|-------------------------------------------------------------|-----------|-----------|-------|-------------------------------------------------|
| `SavedState::encode` (5 keys, mixed types, ~64 B payload)   | 10k × 100 | 321       | 0.32  | `sdk-rust/tests/bench_savedstate.rs`            |
| `SavedState::decode` (same blob)                            | 10k × 100 | 543       | 0.54  | ↑                                               |
| `SavedState` encode + decode roundtrip                      | 10k × 100 | 892       | 0.89  | ↑                                               |
| `executor::poll_once` (Pending future, no work)             | 100k × 100| 3         | <0.01 | `sdk-rust/tests/bench_executor.rs`              |
| `executor::run_to_ready` (inline Ready)                     | 100k × 100| 10        | 0.01  | ↑                                               |
| `activity::block_on_ready` (legacy sync shim, inline Ready) | 100k × 100| 8         | <0.01 | ↑                                               |
| `akml::parse` (compose-style template, 6 elements)          | 5k × 50   | 4 638     | 4.64  | `sdk-rust/tests/bench_akml_parse.rs`            |
| `lmk::pick_victims` fast path (30 pkgs, no pressure)        | 1k × 50   | 61        | 0.06  | `framework/tests/bench_lmk.rs`                  |
| `lmk::pick_victims` active path (30 pkgs, over threshold)   | 1k × 50   | 8 684     | 8.68  | ↑                                               |

**Status.** The 7 SDK benches + 2 framework benches close §6 of the
baseline. All p50 values are stable across runs (CV ~5–10%, hand-
measured). Nothing in this set justifies immediate optimisation: the
two highest costs (AKML parse at 4.6 µs, LMK active path at 8.7 µs)
are each below 1 % of a 16.67 ms frame budget.

**Not yet measured.** AKML `inflate_*` separately from parse
(requires heavyweight `Ui` setup); `Canvas::draw_text` op encoding
(requires a heavyweight `Canvas` instance); host-call latency
(needs browser-side measurement — see §3 + §9). The `Slot.pending`
HashMap microbench is intentionally skipped — it is a primitive
`HashMap` op and Rust's stdlib characteristics there are well known.

## 7. Phase-11 isolation impact (qualitative)

Until §3–§5 fill in, here is what was observed qualitatively during
development:

**Wins.**

- Real crash domain — an app that calls `panic!()` only takes down
  its own worker. In Stone 11.3 this would have killed the tab.
  Verified by the fault-containment tests added in Stone 11.5.
- Watchdog enforcement — a `loop {}` inside `on_pointer` is killed
  in ≤ 1.5 s by the host. Verified by the watchdog tests.
- Direct-readonly storage — the "every read traverses 4 hops"
  pathology was closed in Stone 11.6.7 #3.

**Regressions to investigate.**

- Host-call overhead — `postMessage` boundary that did not exist
  in the single-worker baseline. Expected to add ~10–20 %; needs
  measurement to confirm.
- Boot time — pre-warm of workers + SAB allocation. Stone 11.6.7
  #1 (lazy spawn) addressed part of it. Cold-launch numbers still
  pending.
- Memory baseline — worker overhead + SAB ring + saved-state
  buffer per active app. To be quantified in Phase 12.1.

## 8. How to reproduce (once the harnesses exist)

```bash
# Rust microbenchmarks
cd <repo-root>
cargo test --release -p akiva-sdk       -- --ignored bench_ --nocapture
cargo test --release -p akiva-framework -- --ignored bench_ --nocapture

# Browser-side timings — once perf-probe is written
akiva run                                    # builds + serves on :8080
# In the browser: shell → launch "perf-probe" → click "Run".
# Results land at OPFS path /perf-probe/<utc>.json.

# Frame budget under load
akiva run
# Open 3 tabs to localhost:8080. Tab 1 stays on Shell; tabs 2/3 each
# launch a different app. Per-tab FPS overlay: Ctrl+Shift+F (planned).
# Sample 60 s; record p1 / p50 / p95.
```

## 9. Open work

Some of the tooling exists; some still needs to be built:

1. ✅ **`tests/bench_*.rs`** in `sdk-rust` and `framework` — fills §6.
   9 benches with median over 50–100 outer samples; CV measured at
   ~5–10 %.
2. **`apps/perf-probe/`** — a regular app that runs scripted
   scenarios (storage thrash, host-call burst) and emits JSON.
   Estimated ~2 days. Fills §3 and §4.
3. **FPS overlay in `runtime/web/`** — togglable so the §5 sampling
   can be captured. Estimated ~½ day. Fills §5.
4. **CI regression job** — `cargo test --release --ignored bench_`
   on PRs, store JSON artifact, compare against baseline. Depends
   on §2 and §3. Deferred until external contributors arrive.
5. **Mobile baseline** (Pixel 7, iPhone 14) — deferred to Phase 12,
   when the suspension policy stabilises memory pressure on mobile.

## 10. References

- [`docs/isolation-design.md`](isolation-design.md) — perf targets formalised in §10.5
- [`docs/risks.md §3.2`](risks.md) — direct-readonly storage rationale
- [`framework/src/cpu_budget.rs`](../framework/src/cpu_budget.rs) — rolling-window CPU accounting
- [`framework/src/memory_manager.rs`](../framework/src/memory_manager.rs) — per-app `byteLength` sampling
- [`runtime/web/app_worker_shell.js`](../runtime/web/app_worker_shell.js) — `chargeCpu` brackets
- ROADMAP exit criteria: §11.7 (host / storage), §11.8 (frame / fps)
