# Performance Baseline

מסמך זה קובע את ה-perf baseline של Akiva בסוף Phase 11 — אחרי isolation מלא (worker-per-app), executor אמיתי, TaskScope, ו-`Slot.pending` HashMap. הוא משרת שתי מטרות: (1) להציג את היעדים ואת מנגנון המדידה, ו-(2) לתת **טבלאות שאפשר למלא במדידות חיות** ולחזור עליהן בכל regression.

> **State (2026-05-02).** היעדים formalized; מתודולוגיה מתועדת. **§6 (SDK/framework microbenchmarks) מלא** — 9 benches ב-`sdk-rust/tests/bench_*.rs` + `framework/tests/bench_lmk.rs`, מספרים אמיתיים. **§3-§5 עדיין TBD** — דורש browser-side perf-probe app (ראה §9). אין מספרים פיקטיביים בקובץ.

## 1. יעדים

| מטריקה | יעד | מקור |
|---|---|---|
| host call overhead (warm path) | ≤ 1ms | [ROADMAP §11.7](../ROADMAP.md), [isolation-design.md §10.5](isolation-design.md) |
| storage::read (direct readonly) | ≤ 0.5ms | ROADMAP §11.7 + [risks.md §3.2](risks.md) |
| storage::write (through kernel) | ≤ 2ms | ROADMAP §11.7 |
| frame budget @ 60Hz | 16.67ms | render contract |
| 3 apps ב-foreground | ≥ 60fps יציב | ROADMAP §11.8 |

קריטריונים פנימיים נוספים (ללא public commitment):
- spawn → first poll ≤ 0.2ms
- AKML inflate (5–15 nodes) ≤ 0.5ms
- SavedState encode/decode (5-key blob) ≤ 1ms

## 2. מתודולוגיה

### 2.1 In-process telemetry (כבר קיים)

**App-side bracketing** ב-[`runtime/web/app_worker_shell.js::chargeCpu`](../runtime/web/app_worker_shell.js) — כל wasm export call (`dispatch`/`poll_dispatch`/`get_result`/`drive_spawned`) עטוף ב-`performance.now()` ומצטבר ל-`cumulativeCpuMs`. נשלח ב-pong reply ל-`AppRegistry`.

**Host-side bracketing** ב-[`runtime/web/worker.js`](../runtime/web/worker.js) — ה-kernel HAL מתעד `hal_app_memory_current` ו-`hal_app_cpu_ms` per frame ב-`sample_app_resources_into`. ראה [`framework/src/cpu_budget.rs`](../framework/src/cpu_budget.rs) ל-rolling window.

**Public surface.** `Framework::cpu_usage(package_id) -> CpuReport` ו-`Framework::memory_usage(package_id) -> UsageReport` — ה-API שעליו ייבנה ה-perf-probe.

### 2.2 Microbenchmarks (Rust, יש להוסיף)

הרצה מתוכננת: `cargo test --release -p <crate> -- --ignored bench_ --nocapture`. ה-harness עצמו עוד לא נכתב — `tests/bench_*.rs` תיקיות ריקות. אין `criterion` ב-workspace; ה-harness המתוכנן יהיה wallclock loop פשוט (`Instant::now()`, N=10⁵-10⁶) ויספיק ל-range-of-magnitude. ראה §9.

### 2.3 End-to-end (perf-probe, יש להוסיף)

`apps/perf-probe/` — app מתוכנן שיריץ benchmark suite מתוך ה-runtime עצמו. תוצאות ל-OPFS, להוצאה דרך devtools או דרך CLI helper שיתווסף. ראה §9 לדriving plan.

## 3. Host call overhead

הנתיב: app wasm → trampoline ב-`app_worker_shell.js` → SAB `postMessage` ל-leader → `worker.js` deserialise → kernel → reverse.

| metric | p50 | p95 | p99 | target | status |
|---|---|---|---|---|---|
| no-op host call (loop, N=10000) | TBD | TBD | TBD | ≤ 1ms p95 | ⏳ |
| log emit (`log_info!`, ~80B msg) | TBD | TBD | TBD | informational | ⏳ |
| dispatch + single await + result | TBD | TBD | TBD | ≤ 1ms p95 | ⏳ |

**Predictions** (לא מדידה — analytical estimate על בסיס postMessage cost ידוע ב-Chrome ~0.05ms): כל ה-rows אמורים להיות בטווח 0.2–0.7ms. ה-p99 הוא ה-risk — postMessage tail latency יודע להגיע ל-1.5ms תחת load.

## 4. Storage operations

מאז Stone 11.6.7 #3 יש שני נתיבים:
- **Direct readonly** — app worker מחזיק `SyncAccessHandle` cache per-key ב-`pkgs/<packageId>/`. cache hit מתחזיר sync ללא round-trip.
- **Kernel-routed** — fallback על miss + כל ה-writes.

| operation | p50 | p95 | target | status |
|---|---|---|---|---|
| read (direct, cache hit) | TBD | TBD | ≤ 0.5ms | ⏳ |
| read (direct, cache miss → warm) | TBD | TBD | n/a | ⏳ |
| read (kernel-routed) | TBD | TBD | n/a | ⏳ |
| write (kernel-routed) | TBD | TBD | ≤ 2ms | ⏳ |
| remove | TBD | TBD | n/a | ⏳ |

**Predictions.** [`docs/risks.md`](risks.md) §3.2 ציפתה ל-`~0.1ms` direct read ו-`~2ms` kernel write. אם המדידה מאשרת — היעד נופל. אם direct miss הולך מעל 3ms (OPFS lock contention), שווה להוסיף background pre-open של handles נפוצים.

## 5. Frame budget @ 60Hz, 3 apps foreground

**Setup מתוכנן.** Shell + 2 apps נוספים ב-foreground. 3 workers, 3 SAB rings. Capture: `perf-probe` overlay למשך 60 שניות (טרם נבנה).

| מצב | mean fps | p1 fps | dropped frames / minute | status |
|---|---|---|---|---|
| idle (no input) | TBD | TBD | TBD | ⏳ |
| continuous tap on calculator | TBD | TBD | TBD | ⏳ |
| continuous drag on tic-tac-toe | TBD | TBD | TBD | ⏳ |
| 3 apps animating concurrently | TBD | TBD | TBD | ⏳ |

**Acceptance.** ה-exit criterion הוא "60fps יציב". ההגדרה המתועדת: p1 ≥ 55fps & dropped frames/min ≤ 15 על Chrome 125+ ב-Linux x86_64 desktop. mobile baseline נדחה ל-Phase 12 (ראה §9).

## 6. SDK / framework microbenchmarks

ה-harness יושב ב-`<crate>/tests/bench_*.rs`, gated `#[ignore]` so default `cargo test` stays fast. הרצה:

```bash
cargo test --release -p akiva-sdk       --tests -- --ignored --nocapture
cargo test --release -p akiva-framework --tests -- --ignored --nocapture
```

מתודולוגיה: warmup → 50–100 outer samples × INNER inner iters → median ns/op (p50). ראה כל קובץ bench לפרטים.

**Numbers** (Linux x86_64, Ryzen 7 5800X, `cargo test --release`, captured 2026-05-02):

| benchmark | iters | p50 ns/op | μs/op | source |
|---|---|---|---|---|
| `SavedState::encode` (5 keys, mixed types, ~64B payload) | 10k×100 | 321 | 0.32 | `sdk-rust/tests/bench_savedstate.rs` |
| `SavedState::decode` (same blob) | 10k×100 | 543 | 0.54 | ↑ |
| `SavedState` encode+decode roundtrip | 10k×100 | 892 | 0.89 | ↑ |
| `executor::poll_once` (Pending future, no work) | 100k×100 | 3 | <0.01 | `sdk-rust/tests/bench_executor.rs` |
| `executor::run_to_ready` (inline Ready) | 100k×100 | 10 | 0.01 | ↑ |
| `activity::block_on_ready` (legacy sync shim, inline Ready) | 100k×100 | 8 | <0.01 | ↑ |
| `akml::parse` (compose-style template, 6 elements) | 5k×50 | 4 638 | 4.64 | `sdk-rust/tests/bench_akml_parse.rs` |
| `lmk::pick_victims` fast path (30 pkgs, no pressure) | 1k×50 | 61 | 0.06 | `framework/tests/bench_lmk.rs` |
| `lmk::pick_victims` active path (30 pkgs, over threshold) | 1k×50 | 8 684 | 8.68 | ↑ |

**Status.** ה-7 sdk benchmarks + 2 framework benchmarks סוגרים את §6 של ה-baseline. כל ערכי p50 יציבים בין הרצות (CV ~5–10%, נמדד ידנית). אין hot-spot ש-justifies optimization מיידית: AKML parse ב-4.6µs ו-LMK active ב-8.7µs הם הכי גבוהים — שניהם מתחת ל-1% מ-frame budget.

**מה לא בטרם נבדק.** AKML `inflate_*` בנפרד מ-parse (דורש Ui setup heavyweight); `Canvas::draw_text` op encode (דורש Canvas instance heavyweight); host call latency דורש browser-side measurement (ראה §3 + §9). ה-`Slot.pending` HashMap micro-bench נדחה — זה primitive HashMap operation ו-Rust's `HashMap` performance characteristics ידועים.

## 7. Phase-11 isolation impact (qualitative)

עד שיש מספרים, להלן מה שצפוי / כבר נצפה qualitatively במהלך הפיתוח:

**Wins.**
- Crash domain אמיתי — app שעשה `panic!()` קוסם רק את ה-worker שלו. ב-Stone 11.3 זה היה מפיל את ה-tab. **verified ב-fault-containment tests** (Stone 11.5).
- Watchdog enforcement — `loop {}` ב-`on_pointer` נהרג ב-≤ 1.5s על-ידי ה-host. **verified ב-watchdog tests**.
- Direct readonly storage — pathology של "כל read עובר 4 hops" נסגרה ב-Stone 11.6.7 #3.

**Regressions לבחינה.**
- Host call overhead — postMessage boundary שלא היה קיים ב-single-worker baseline. צפוי ~10–20% תוספת. צריך מדידה כדי לאשר.
- Boot time — pre-warm של workers + SAB allocation. Stone 11.6.7 #1 (lazy spawn) פתר חלקית. צריך מדידת cold-launch.
- Memory baseline — worker overhead + SAB ring + saved-state buffer per active app. צריך לכמת ב-Phase 12.1.

## 8. איך לשחזר (כשה-harnesses קיימים)

```bash
# Microbenchmarks (Rust) — אחרי שייכתבו tests/bench_*.rs
cd /root/workspace/akiva-platform
cargo test --release -p akiva-sdk -- --ignored bench_ --nocapture
cargo test --release -p akiva-framework -- --ignored bench_ --nocapture

# Browser-side timings — אחרי ש-perf-probe ייכתב
akiva run                                    # builds + serves on :8080
# In browser: open shell → launch "perf-probe" → click "Run".
# Results land at OPFS path /perf-probe/<utc>.json.

# Frame budget under load
akiva run
# Open 3 tabs to localhost:8080. Tab 1 stays on Shell; tabs 2/3 each
# launch a different app. Per-tab FPS overlay: Ctrl+Shift+F (planned).
# Sample 60s; record p1/p50/p95.
```

## 9. Open work

חלק מה-tooling קיים, חלק עדיין דרוש. הסטטוס המעודכן:

1. ✅ **`tests/bench_*.rs`** ב-`sdk-rust` ו-`framework` — מילא §6. 9 benches עם median over 50–100 outer samples; CV נמדד ~5-10%.
2. **`apps/perf-probe/`** — app רגיל שמריץ scripted scenarios (storage thrash, host-call burst) ומפיק JSON. ~2 ימים. ימלא §3, §4.
3. **FPS overlay ב-`runtime/web/`** — toggle כדי שדוגמת §5 תיהיה ניתנת ל-capture. ~½ יום. ימלא §5.
4. **CI regression job** — `cargo test --release --ignored bench_` ב-PR, store JSON artifact, השוואה ל-baseline. **dependency:** §2-3. dropped עד שיש contributors חיצוניים.
5. **Mobile baseline** (Pixel 7, iPhone 14) — דחוי ל-Phase 12 כשה-suspension policy יציב את ה-memory pressure על mobile.

## 10. References

- [`docs/isolation-design.md`](isolation-design.md) — perf targets formalized ב-§10.5
- [`docs/risks.md`](risks.md) §3.2 — direct readonly storage rationale
- [`framework/src/cpu_budget.rs`](../framework/src/cpu_budget.rs) — rolling-window CPU accounting
- [`framework/src/memory_manager.rs`](../framework/src/memory_manager.rs) — per-app byteLength sampling
- [`runtime/web/app_worker_shell.js`](../runtime/web/app_worker_shell.js) — `chargeCpu` brackets
- ROADMAP exit criteria: §11.7 (host/storage), §11.8 (frame/fps)
