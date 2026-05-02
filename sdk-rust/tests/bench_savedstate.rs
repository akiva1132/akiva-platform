//! Stone 11.8b — SavedState encode / decode microbenchmark.
//!
//! Run with `cargo test --release -p akiva-sdk --test bench_savedstate
//! -- --ignored --nocapture`. The `#[ignore]` gate keeps it out of the
//! default test loop so `cargo test` stays fast.
//!
//! Methodology: warm up; then 100 outer samples × `INNER` inner iters
//! per sample; report median ns/op (p50). No allocation in the inner
//! loop except the `encode` Vec, which is the cost we're measuring.
//!
//! Output is plain text — `perf-baseline.md` tables get filled in by
//! hand from the captured numbers.

use akiva_sdk::SavedState;

const WARMUP: usize = 10_000;
const INNER: usize = 10_000;
const SAMPLES: usize = 100;

fn build_blob() -> SavedState {
    let mut s = SavedState::default();
    s.put_bool("dirty", true);
    s.put_int("count", 42);
    s.put_float("zoom", 1.25);
    s.put_string("title", "performance baseline".to_string());
    s.put_bytes("payload", vec![0u8; 64]);
    s
}

fn median(mut samples: Vec<u128>) -> u128 {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

#[test]
#[ignore = "microbench — run explicitly with --ignored"]
fn bench_savedstate_encode() {
    let s = build_blob();
    for _ in 0..WARMUP {
        std::hint::black_box(s.encode());
    }
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let t0 = std::time::Instant::now();
        for _ in 0..INNER {
            std::hint::black_box(s.encode());
        }
        samples.push(t0.elapsed().as_nanos() / INNER as u128);
    }
    let p50 = median(samples);
    println!("bench_savedstate_encode  N={INNER}×{SAMPLES}  p50={p50}ns/op");
    // Loose ceiling — sanity, not a real perf gate. Real numbers go in
    // docs/perf-baseline.md §6.
    assert!(p50 < 50_000, "encode exceeded 50µs/op — investigate");
}

#[test]
#[ignore = "microbench — run explicitly with --ignored"]
fn bench_savedstate_decode() {
    let s = build_blob();
    let bytes = s.encode();
    for _ in 0..WARMUP {
        std::hint::black_box(SavedState::decode(&bytes).unwrap());
    }
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let t0 = std::time::Instant::now();
        for _ in 0..INNER {
            std::hint::black_box(SavedState::decode(&bytes).unwrap());
        }
        samples.push(t0.elapsed().as_nanos() / INNER as u128);
    }
    let p50 = median(samples);
    println!("bench_savedstate_decode  N={INNER}×{SAMPLES}  p50={p50}ns/op");
    assert!(p50 < 50_000, "decode exceeded 50µs/op — investigate");
}

#[test]
#[ignore = "microbench — run explicitly with --ignored"]
fn bench_savedstate_roundtrip() {
    let s = build_blob();
    for _ in 0..WARMUP {
        let b = s.encode();
        std::hint::black_box(SavedState::decode(&b).unwrap());
    }
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let t0 = std::time::Instant::now();
        for _ in 0..INNER {
            let b = s.encode();
            std::hint::black_box(SavedState::decode(&b).unwrap());
        }
        samples.push(t0.elapsed().as_nanos() / INNER as u128);
    }
    let p50 = median(samples);
    println!("bench_savedstate_roundtrip  N={INNER}×{SAMPLES}  p50={p50}ns/op");
    assert!(p50 < 100_000, "roundtrip exceeded 100µs/op — investigate");
}
