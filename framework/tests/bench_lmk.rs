//! Stone 11.8b — LMK pick_victims microbench. Decision logic is on the
//! per-frame hot path (Phase 12.3 wakes drive_lmk every frame); we want
//! the priority pass + sort to stay well under 100µs even with dozens
//! of tracked apps.
//!
//! `cargo test --release -p akiva-framework --test bench_lmk -- --ignored --nocapture`

use std::collections::HashSet;

use akiva_framework::activity_manager::{ActivityManager, LaunchMode};
use akiva_framework::activity::{Activity, ActivityContext};
use akiva_framework::compositor::FrameCommands;
use akiva_framework::lmk::{pick_victims, KillReason, LmkPolicy};
use akiva_framework::memory_manager::MemoryManager;

const WARMUP: usize = 100;
const INNER: usize = 1_000;
const SAMPLES: usize = 50;

struct Stub;
impl Activity for Stub {
    fn render(&mut self, _: &mut ActivityContext<'_>, _: &mut FrameCommands) {}
}

fn build_fixture(n_packages: usize) -> (MemoryManager, ActivityManager) {
    let mut mm = MemoryManager::new();
    let mut am = ActivityManager::new();
    for i in 0..n_packages {
        let pkg = format!("pkg{:03}", i);
        am.register(&pkg, "Main", LaunchMode::Standard, || Box::new(Stub));
        // Cross-crate visibility: the bench can't reach the in-crate
        // `force_state_for_test` test helper, so every record stays at
        // `LifecycleState::Initial` (priority Empty). The bench still
        // exercises the priority pass + sort + iterate path; just with
        // homogeneous priority — tie-break by bytes is what dominates.
        let _ = am.launch(&pkg, "Main").unwrap();
        mm.record(&pkg, ((i + 1) * 1024 * 1024) as u64);
    }
    (mm, am)
}

fn median(mut samples: Vec<u128>) -> u128 {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

#[test]
#[ignore = "microbench"]
fn bench_pick_victims_30_packages_under_threshold() {
    // Fast path — total <= threshold returns empty immediately.
    let (mm, am) = build_fixture(30);
    let policy = LmkPolicy {
        threshold_bytes: u64::MAX, // never triggers
        recovery_bytes: 0,
        max_kills_per_check: 4,
    };
    let protected: HashSet<&str> = HashSet::new();

    for _ in 0..WARMUP {
        std::hint::black_box(pick_victims(&policy, &mm, &am, &protected, KillReason::SystemPressure));
    }
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let t0 = std::time::Instant::now();
        for _ in 0..INNER {
            std::hint::black_box(pick_victims(&policy, &mm, &am, &protected, KillReason::SystemPressure));
        }
        samples.push(t0.elapsed().as_nanos() / INNER as u128);
    }
    let p50 = median(samples);
    println!("bench_pick_victims_30pkg_no_pressure  N={INNER}×{SAMPLES}  p50={p50}ns/op");
    // Threshold-check fast path should be ~10ns — sum + compare.
    assert!(p50 < 5_000, "fast path > 5µs/op — regression");
}

#[test]
#[ignore = "microbench"]
fn bench_pick_victims_30_packages_over_threshold() {
    // Real work — priority_of for every package + sort + iterate.
    let (mm, am) = build_fixture(30);
    let policy = LmkPolicy {
        threshold_bytes: 100 * 1024,
        recovery_bytes: 50 * 1024,
        max_kills_per_check: 4,
    };
    let protected: HashSet<&str> = HashSet::new();

    for _ in 0..WARMUP {
        std::hint::black_box(pick_victims(&policy, &mm, &am, &protected, KillReason::SystemPressure));
    }
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let t0 = std::time::Instant::now();
        for _ in 0..INNER {
            std::hint::black_box(pick_victims(&policy, &mm, &am, &protected, KillReason::SystemPressure));
        }
        samples.push(t0.elapsed().as_nanos() / INNER as u128);
    }
    let p50 = median(samples);
    println!("bench_pick_victims_30pkg_active  N={INNER}×{SAMPLES}  p50={p50}ns/op");
    // 30 packages × priority_of + sort. p99 budget is 100µs/op.
    assert!(p50 < 100_000, "active path > 100µs/op — regression");
}
