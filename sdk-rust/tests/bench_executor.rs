//! Stone 11.8b — executor microbenchmarks.
//!
//! `cargo test --release -p akiva-sdk --test bench_executor -- --ignored --nocapture`

use std::future::Future;
use std::pin::Pin;
use std::task::Poll;

use akiva_sdk::activity::block_on_ready;
use akiva_sdk::executor::{poll_once, run_to_ready};

const WARMUP: usize = 10_000;
const INNER: usize = 100_000;
const SAMPLES: usize = 100;

fn median(mut samples: Vec<u128>) -> u128 {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

#[test]
#[ignore = "microbench"]
fn bench_run_to_ready_inline_future() {
    for _ in 0..WARMUP {
        std::hint::black_box(run_to_ready(async { 42u32 }));
    }
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let t0 = std::time::Instant::now();
        for _ in 0..INNER {
            std::hint::black_box(run_to_ready(async { 42u32 }));
        }
        samples.push(t0.elapsed().as_nanos() / INNER as u128);
    }
    let p50 = median(samples);
    println!("bench_run_to_ready_inline_future  N={INNER}×{SAMPLES}  p50={p50}ns/op");
    assert!(p50 < 5_000, "run_to_ready of inline future > 5µs/op — regression");
}

#[test]
#[ignore = "microbench"]
fn bench_poll_once_inline_future() {
    // Allocate the boxed future once outside the loop — `poll_once`
    // is the cost we measure. Re-poll a never-ready future N times:
    // the future returns Pending immediately so the body is just the
    // poll machinery itself.
    let mut fut: Pin<Box<dyn Future<Output = ()> + 'static>> =
        Box::pin(std::future::pending::<()>());
    for _ in 0..WARMUP {
        match poll_once(&mut fut) {
            Poll::Pending => {}
            Poll::Ready(_) => unreachable!(),
        }
    }
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let t0 = std::time::Instant::now();
        for _ in 0..INNER {
            std::hint::black_box(poll_once(&mut fut));
        }
        samples.push(t0.elapsed().as_nanos() / INNER as u128);
    }
    let p50 = median(samples);
    println!("bench_poll_once_inline_future  N={INNER}×{SAMPLES}  p50={p50}ns/op");
    assert!(p50 < 1_000, "poll_once > 1µs/op — regression");
}

#[test]
#[ignore = "microbench"]
fn bench_block_on_ready_inline_future() {
    // The legacy synchronous shim — same path used by save_state on the
    // sync side of the trait. Compared to `run_to_ready` to expose any
    // cost differential.
    for _ in 0..WARMUP {
        std::hint::black_box(block_on_ready(async { 7u32 }));
    }
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let t0 = std::time::Instant::now();
        for _ in 0..INNER {
            std::hint::black_box(block_on_ready(async { 7u32 }));
        }
        samples.push(t0.elapsed().as_nanos() / INNER as u128);
    }
    let p50 = median(samples);
    println!("bench_block_on_ready_inline_future  N={INNER}×{SAMPLES}  p50={p50}ns/op");
    assert!(p50 < 5_000);
}
