//! Per-package CPU attribution (Phase 12.2).
//!
//! Tracks the cumulative CPU time each app worker has spent inside
//! the wasm dispatch / poll / drive paths, plus a smoothed rate so
//! Phase 12.3 (Low-Memory Killer) can rank packages by "weighs on
//! frame budget" without reading raw frame-by-frame deltas.
//!
//! ## Provenance
//!
//! Each app worker brackets `akiva_app_dispatch`,
//! `akiva_app_poll_dispatch`, `akiva_app_get_result`, and
//! `akiva_app_drive_spawned` with `performance.now()` and accumulates
//! the duration in a local `cumulativeCpuMs`. The pong heartbeat
//! piggybacks the value to the kernel-worker every ~500 ms, where
//! `AppRegistry` stashes it on the process record. The kernel pulls
//! the cumulative reading once per frame via `hal_app_cpu_ms` and
//! forwards `(package_id, cumulative_ms, frame_ts_ms)` here.
//!
//! Rate computation lives in this module so consumers (Phase 12.3
//! priority tiers, Phase 12.5 enforcement, future debug overlays)
//! see one consistent number — `recent_ms_per_sec` — rather than
//! every consumer rolling its own diff.
//!
//! ## Why not server-side smoothing
//!
//! An EMA-based rate would respond faster but adds tunables (alpha
//! choice). Two-snapshot windowing (newest + ~1 s ago) is exact
//! enough for ranking decisions, has no hidden state, and lets a
//! debug overlay inspect the raw inputs. We can swap in EMA later if
//! Phase 12.3's policy actually needs it.

use std::collections::HashMap;

/// Snapshot of one package's CPU usage. `cumulative_ms` is the total
/// time the app worker has spent inside wasm-export brackets since
/// the worker booted; `recent_ms_per_sec` is the rate computed over
/// roughly the last second; `peak_ms_per_sec` is the high-water mark
/// of the rate across the lifetime of this manager.
///
/// `frame_budget_fraction` is `recent_ms_per_sec / 16.667` — the
/// share of a 60 Hz frame the app would consume if this rate were
/// sustained. Values >1.0 mean "this app alone exceeds the frame
/// budget"; Phase 12.3 LMK ranking treats anything >0.5 as a hot
/// candidate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CpuReport {
    pub cumulative_ms: f64,
    pub recent_ms_per_sec: f64,
    pub peak_ms_per_sec: f64,
    pub frame_budget_fraction: f64,
}

impl CpuReport {
    /// 60 Hz frame budget — the standard reference point for
    /// `frame_budget_fraction`. Constant so consumers don't have to
    /// pick their own number.
    pub const FRAME_BUDGET_MS: f64 = 1000.0 / 60.0;

    /// True if the app is consuming more than half of a 60 Hz frame
    /// budget on average — Phase 12.3 LMK's default trigger.
    pub fn over_frame_budget(&self) -> bool {
        self.frame_budget_fraction > 0.5
    }
}

/// Window length used for the rolling rate computation. We compare
/// the newest cumulative reading with one taken roughly this many
/// milliseconds ago; values within the window scale by the actual
/// elapsed time, not this constant.
///
/// 1 second is short enough that a sudden burst (bad render frame
/// loop, JIT warmup) shows up immediately, long enough that a single
/// dispatch's noise doesn't dominate.
const RATE_WINDOW_MS: f64 = 1000.0;

#[derive(Debug, Clone, Copy)]
struct CpuEntry {
    /// Latest cumulative reading from the host.
    cumulative_ms: f64,
    /// Frame timestamp of `cumulative_ms` (i.e. when the kernel
    /// observed it).
    cumulative_ts_ms: f64,
    /// "Window-base" snapshot — at least `RATE_WINDOW_MS` older than
    /// `cumulative_ts_ms` once the window fills. Used as the
    /// denominator/numerator pair for the rate calculation.
    base_cumulative_ms: f64,
    base_ts_ms: f64,
    /// High-water mark of `recent_ms_per_sec` across the lifetime of
    /// the entry.
    peak_ms_per_sec: f64,
}

#[derive(Debug, Default)]
pub struct CpuBudget {
    by_package: HashMap<String, CpuEntry>,
}

impl CpuBudget {
    pub fn new() -> Self {
        Self { by_package: HashMap::new() }
    }

    /// Record a fresh `(cumulative_ms, frame_ts_ms)` pair. The first
    /// call for a package primes both the `cumulative` and `base`
    /// snapshots; subsequent calls advance `cumulative` immediately
    /// and slide `base` forward when it ages past the window.
    ///
    /// Idempotent: re-recording the same `(cumulative_ms, ts)` is
    /// cheap (no allocation; rate lands at zero, which is correct
    /// for "no work since last sample").
    pub fn record(&mut self, package_id: &str, cumulative_ms: f64, frame_ts_ms: f64) {
        let entry = self.by_package.entry(package_id.to_string()).or_insert(CpuEntry {
            cumulative_ms,
            cumulative_ts_ms: frame_ts_ms,
            base_cumulative_ms: cumulative_ms,
            base_ts_ms: frame_ts_ms,
            peak_ms_per_sec: 0.0,
        });
        // Slide the window base forward when it's old enough that the
        // gap between newest and base exceeds the target window. If
        // we slid every frame, the rate would be one-frame-noisy; if
        // we never slid, the rate would average over the whole boot.
        if frame_ts_ms - entry.base_ts_ms >= RATE_WINDOW_MS {
            entry.base_cumulative_ms = entry.cumulative_ms;
            entry.base_ts_ms = entry.cumulative_ts_ms;
        }
        entry.cumulative_ms = cumulative_ms;
        entry.cumulative_ts_ms = frame_ts_ms;
        let dt = entry.cumulative_ts_ms - entry.base_ts_ms;
        if dt > 0.0 {
            let rate = (entry.cumulative_ms - entry.base_cumulative_ms) * 1000.0 / dt;
            // Negative rates are nonsense (cumulative would have to
            // decrease); clamp at zero so a noisy host reading
            // doesn't poison `peak_ms_per_sec`.
            let rate = rate.max(0.0);
            if rate > entry.peak_ms_per_sec {
                entry.peak_ms_per_sec = rate;
            }
        }
    }

    /// Snapshot for `package_id`. Returns `None` if no host sample
    /// has landed yet; otherwise computes the rate from the current
    /// (cumulative, base) pair and folds in the frame-budget
    /// fraction.
    pub fn usage(&self, package_id: &str) -> Option<CpuReport> {
        let entry = self.by_package.get(package_id)?;
        let dt = entry.cumulative_ts_ms - entry.base_ts_ms;
        let recent = if dt > 0.0 {
            ((entry.cumulative_ms - entry.base_cumulative_ms) * 1000.0 / dt).max(0.0)
        } else {
            0.0
        };
        Some(CpuReport {
            cumulative_ms: entry.cumulative_ms,
            recent_ms_per_sec: recent,
            peak_ms_per_sec: entry.peak_ms_per_sec,
            frame_budget_fraction: recent / CpuReport::FRAME_BUDGET_MS,
        })
    }

    /// Drop accounting for `package_id`. Symmetric with
    /// `MemoryManager::forget`; called on uninstall or when Phase 12.4
    /// (suspension) decides to start fresh on revival. Returns the
    /// final cumulative reading for log breadcrumbs.
    pub fn forget(&mut self, package_id: &str) -> Option<f64> {
        self.by_package.remove(package_id).map(|e| e.cumulative_ms)
    }

    /// Iterator over `(package_id, cumulative_ms, recent_ms_per_sec)`
    /// triples. Order is HashMap insertion (i.e. unspecified).
    pub fn iter(&self) -> impl Iterator<Item = (&str, f64, f64)> + '_ {
        self.by_package.iter().map(|(k, e)| {
            let dt = e.cumulative_ts_ms - e.base_ts_ms;
            let rate = if dt > 0.0 {
                ((e.cumulative_ms - e.base_cumulative_ms) * 1000.0 / dt).max(0.0)
            } else {
                0.0
            };
            (k.as_str(), e.cumulative_ms, rate)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_record_returns_a_zero_rate_report() {
        // No prior reading → rate is 0 by construction (window dt = 0).
        let mut b = CpuBudget::new();
        b.record("com.akiva.app", 5.0, 100.0);
        let r = b.usage("com.akiva.app").expect("just recorded");
        assert_eq!(r.cumulative_ms, 5.0);
        assert_eq!(r.recent_ms_per_sec, 0.0);
        assert_eq!(r.peak_ms_per_sec, 0.0);
    }

    #[test]
    fn unknown_package_returns_none() {
        let b = CpuBudget::new();
        assert!(b.usage("not.installed").is_none());
    }

    #[test]
    fn rate_reflects_recent_growth_within_window() {
        // 100 ms of CPU consumed in ~500 ms wall time → 200 ms/sec.
        // The base snapshot is still the original 0 ms because the
        // window hasn't elapsed yet.
        let mut b = CpuBudget::new();
        b.record("a", 0.0, 0.0);
        b.record("a", 100.0, 500.0);
        let r = b.usage("a").unwrap();
        assert!((r.recent_ms_per_sec - 200.0).abs() < 0.001,
            "expected ~200 ms/sec, got {}", r.recent_ms_per_sec);
        assert_eq!(r.cumulative_ms, 100.0);
    }

    #[test]
    fn window_slides_after_one_second_of_samples() {
        // After the window threshold elapses the base advances,
        // dropping older samples from the rate denominator. A spike
        // from frame 0–500 ms should fall out by frame 1500 ms if no
        // new work happened.
        let mut b = CpuBudget::new();
        b.record("a", 0.0, 0.0);
        b.record("a", 100.0, 500.0);   // spike: 200 ms/sec rate
        b.record("a", 100.0, 1500.0);  // window slides; idle ms
        b.record("a", 100.0, 2600.0);  // window slides again
        let r = b.usage("a").unwrap();
        // After the window slides past the spike the rate falls to
        // zero (no new cumulative growth in the new window).
        assert!(r.recent_ms_per_sec < 1.0,
            "expected rate near 0 after spike falls out of window, got {}",
            r.recent_ms_per_sec);
        // Peak is preserved across window slides.
        assert!(r.peak_ms_per_sec > 100.0,
            "peak should remember the spike");
    }

    #[test]
    fn frame_budget_fraction_is_rate_over_60hz_budget() {
        // 8.33 ms/sec → 0.5 of a 16.67 ms frame budget. Easy mental
        // anchor for the LMK threshold.
        let mut b = CpuBudget::new();
        b.record("a", 0.0, 0.0);
        b.record("a", 4.165, 500.0);  // 8.33 ms/sec
        let r = b.usage("a").unwrap();
        assert!((r.frame_budget_fraction - 0.5).abs() < 0.01,
            "expected ~0.5 frame budget fraction, got {}",
            r.frame_budget_fraction);
        assert!(r.over_frame_budget() == false,
            "exactly half-budget shouldn't trip the LMK threshold");
    }

    #[test]
    fn over_frame_budget_trips_above_half() {
        let mut b = CpuBudget::new();
        b.record("a", 0.0, 0.0);
        b.record("a", 10.0, 500.0);  // 20 ms/sec → 1.2x budget
        let r = b.usage("a").unwrap();
        assert!(r.over_frame_budget(),
            "20 ms/sec is more than half a 16.67 ms frame budget");
    }

    #[test]
    fn forget_returns_final_cumulative_and_evicts() {
        let mut b = CpuBudget::new();
        b.record("a", 7.0, 0.0);
        b.record("a", 99.0, 500.0);
        assert_eq!(b.forget("a"), Some(99.0));
        assert!(b.usage("a").is_none());
    }

    #[test]
    fn forget_on_unknown_package_is_silent_none() {
        let mut b = CpuBudget::new();
        assert_eq!(b.forget("never.tracked"), None);
    }

    #[test]
    fn negative_delta_reads_clamp_to_zero_rate() {
        // Cumulative shouldn't decrease in practice (host accumulator
        // is monotonic), but a worker reset between samples would
        // make `cumulative` drop. Treat as a fresh start: rate clamps
        // at zero so peak doesn't get poisoned.
        let mut b = CpuBudget::new();
        b.record("a", 1000.0, 0.0);
        b.record("a", 50.0, 500.0);  // worker restarted, cumulative reset
        let r = b.usage("a").unwrap();
        assert_eq!(r.recent_ms_per_sec, 0.0);
        assert_eq!(r.peak_ms_per_sec, 0.0);
    }

    #[test]
    fn iter_yields_every_tracked_package() {
        let mut b = CpuBudget::new();
        b.record("a", 0.0, 0.0);
        b.record("a", 50.0, 500.0);
        b.record("b", 0.0, 0.0);
        b.record("b", 100.0, 500.0);
        let mut entries: Vec<(String, f64, f64)> = b
            .iter()
            .map(|(id, cum, rate)| (id.to_string(), cum, rate))
            .collect();
        entries.sort_by(|x, y| x.0.cmp(&y.0));
        assert_eq!(entries[0].0, "a");
        assert_eq!(entries[0].1, 50.0);
        assert!((entries[0].2 - 100.0).abs() < 0.001);
        assert_eq!(entries[1].0, "b");
        assert_eq!(entries[1].1, 100.0);
        assert!((entries[1].2 - 200.0).abs() < 0.001);
    }
}
