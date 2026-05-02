//! Per-package memory accounting (Phase 12.1).
//!
//! Tracks current and peak `WebAssembly.Memory.buffer.byteLength` for
//! each app worker the host has reported. The numbers feed
//! [`crate::Framework::memory_usage`], which folds in the manifest's
//! `runtime.max_memory_mb` limit so callers see one rounded report.
//!
//! ## Provenance
//!
//! The framework doesn't read wasm memory directly — that lives in
//! peer workers in production. The host (`worker.js` / `AppRegistry`)
//! samples each app's `byteLength` and pushes it through a HAL import
//! once per frame; the kernel forwards the sample to
//! [`MemoryManager::record`]. Phase 12.3 (Low-Memory Killer) reads
//! [`MemoryManager`] to make eviction decisions, and Phase 12.5
//! enforces the manifest limit by trapping any worker that exceeds
//! the budget.
//!
//! The structure is deliberately thin: a `HashMap<package_id, Entry>`
//! with `current` and `peak` counters. No timestamps, no histograms —
//! Phase 12.2 (CPU attribution) will introduce a richer accounting
//! shape if it needs one.

use std::collections::HashMap;

/// Snapshot of one package's memory state. `current` is the most
/// recent host-reported `byteLength`; `peak` is the high-water mark
/// across the lifetime of this manager (cleared by [`MemoryManager::forget`]).
/// `limit` comes from the package's manifest (`runtime.max_memory_mb`)
/// converted to bytes — `u64::MAX` if no manifest limit is on file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageReport {
    pub current_bytes: u64,
    pub peak_bytes: u64,
    pub limit_bytes: u64,
}

impl UsageReport {
    /// Convenience: is the package over its manifest budget right now?
    /// Phase 12.5 turns this into an enforcement trap; Phase 12.3 uses
    /// the same predicate for kill-policy ranking.
    pub fn over_budget(&self) -> bool {
        self.current_bytes > self.limit_bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Entry {
    current: u64,
    peak: u64,
}

#[derive(Debug, Default)]
pub struct MemoryManager {
    by_package: HashMap<String, Entry>,
}

impl MemoryManager {
    pub fn new() -> Self {
        Self { by_package: HashMap::new() }
    }

    /// Record a fresh `byteLength` reading. `current` is replaced;
    /// `peak` advances monotonically. Idempotent re-recordings of the
    /// same value are cheap (no allocation after the first insert).
    pub fn record(&mut self, package_id: &str, bytes: u64) {
        match self.by_package.get_mut(package_id) {
            Some(entry) => {
                entry.current = bytes;
                if bytes > entry.peak {
                    entry.peak = bytes;
                }
            }
            None => {
                self.by_package.insert(
                    package_id.to_string(),
                    Entry { current: bytes, peak: bytes },
                );
            }
        }
    }

    /// Last reported `byteLength`, or `None` if the host hasn't
    /// pushed a sample for `package_id` yet.
    pub fn current(&self, package_id: &str) -> Option<u64> {
        self.by_package.get(package_id).map(|e| e.current)
    }

    /// High-water mark since the last `forget`/install, or `None` if
    /// no samples have been recorded.
    pub fn peak(&self, package_id: &str) -> Option<u64> {
        self.by_package.get(package_id).map(|e| e.peak)
    }

    /// Drop accounting for `package_id`. Called on uninstall or when
    /// the worker is terminated and Phase 12.4 (suspension) decides
    /// not to retain the prior peak. Returning the dropped entry's
    /// `(current, peak)` lets a caller log a final reading without a
    /// second lookup.
    pub fn forget(&mut self, package_id: &str) -> Option<(u64, u64)> {
        self.by_package
            .remove(package_id)
            .map(|e| (e.current, e.peak))
    }

    /// Total `current_bytes` across every tracked package. Useful for
    /// Phase 12.3's system-wide pressure check.
    pub fn total_current(&self) -> u64 {
        self.by_package.values().map(|e| e.current).sum()
    }

    /// Iterator over `(package_id, current, peak)` triples. Order is
    /// HashMap-insertion (i.e. unspecified) — callers that need a
    /// stable order should sort by id.
    pub fn iter(&self) -> impl Iterator<Item = (&str, u64, u64)> {
        self.by_package
            .iter()
            .map(|(k, e)| (k.as_str(), e.current, e.peak))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_then_current_reads_back_the_value() {
        let mut m = MemoryManager::new();
        m.record("com.akiva.hello", 1_048_576);
        assert_eq!(m.current("com.akiva.hello"), Some(1_048_576));
    }

    #[test]
    fn unknown_package_returns_none() {
        let m = MemoryManager::new();
        assert_eq!(m.current("not.installed"), None);
        assert_eq!(m.peak("not.installed"), None);
    }

    #[test]
    fn peak_advances_monotonically_through_growth_and_shrinkage() {
        // Wasm memory only grows, but a future suspension/revival cycle
        // (Phase 12.4) might let `current` drop while peak stays high.
        // Test the contract is monotonic-peak, replace-current.
        let mut m = MemoryManager::new();
        m.record("com.akiva.calc", 100);
        m.record("com.akiva.calc", 500);
        m.record("com.akiva.calc", 200);
        assert_eq!(m.current("com.akiva.calc"), Some(200));
        assert_eq!(m.peak("com.akiva.calc"), Some(500));
    }

    #[test]
    fn forget_returns_final_reading_and_evicts_entry() {
        let mut m = MemoryManager::new();
        m.record("com.akiva.uninstalled", 42);
        m.record("com.akiva.uninstalled", 99);
        let final_pair = m.forget("com.akiva.uninstalled");
        assert_eq!(final_pair, Some((99, 99)));
        assert_eq!(m.current("com.akiva.uninstalled"), None);
    }

    #[test]
    fn forget_on_unknown_package_is_a_silent_none() {
        let mut m = MemoryManager::new();
        assert_eq!(m.forget("never.tracked"), None);
    }

    #[test]
    fn total_current_sums_across_packages() {
        let mut m = MemoryManager::new();
        m.record("a", 1000);
        m.record("b", 2000);
        m.record("c", 500);
        // After a re-record, total uses the latest values, not a sum
        // of every reading ever taken.
        m.record("a", 100);
        assert_eq!(m.total_current(), 100 + 2000 + 500);
    }

    #[test]
    fn iter_yields_every_tracked_package() {
        let mut m = MemoryManager::new();
        m.record("a", 1);
        m.record("b", 2);
        let mut seen: Vec<(String, u64, u64)> = m
            .iter()
            .map(|(id, c, p)| (id.to_string(), c, p))
            .collect();
        seen.sort_by(|x, y| x.0.cmp(&y.0));
        assert_eq!(seen, vec![
            ("a".to_string(), 1, 1),
            ("b".to_string(), 2, 2),
        ]);
    }

    #[test]
    fn over_budget_reports_when_current_exceeds_limit() {
        let report = UsageReport {
            current_bytes: 17 * 1024 * 1024,
            peak_bytes: 17 * 1024 * 1024,
            limit_bytes: 16 * 1024 * 1024,
        };
        assert!(report.over_budget());
    }

    #[test]
    fn over_budget_false_when_within_limit() {
        let report = UsageReport {
            current_bytes: 8 * 1024 * 1024,
            peak_bytes: 12 * 1024 * 1024,
            limit_bytes: 16 * 1024 * 1024,
        };
        assert!(!report.over_budget());
    }
}
