//! Stone 11.8b — AKML parser microbench. The full `inflate_*` path
//! requires a `Ui` context (Canvas + Context) which is heavyweight to
//! set up just for timing; we bench `parse` (XML-ish → `Element` tree)
//! which is the dominant cost of `inflate_str` minus the `Ui` walk.
//!
//! `cargo test --release -p akiva-sdk --test bench_akml_parse -- --ignored --nocapture`

use akiva_sdk::akml::parse;

const LAYOUT: &str = r#"<?akml version="1.0"?>
<VStack padding="24" gap="16" cross="stretch">
    <Text text="@string/app_name" size="28" color="@color/title_fg" />
    <Text text="{greeting}, world!" size="36" color="@color/greeting_fg" />
    <Text text="Clicks: {count}" size="18" color="@color/body_fg" />
    <HStack gap="12">
        <Button id="greet" label="@string/cta_greet" onClick="greet" flex="1" />
        <Button id="reset" label="@string/cta_reset" onClick="reset" />
    </HStack>
</VStack>"#;

const WARMUP: usize = 1_000;
const INNER: usize = 5_000;
const SAMPLES: usize = 50;

fn median(mut samples: Vec<u128>) -> u128 {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

#[test]
#[ignore = "microbench"]
fn bench_akml_parse_compose_template() {
    // Sanity — the layout is the one shipped in cli/src/templates/compose_style.rs.
    for _ in 0..WARMUP {
        std::hint::black_box(parse(LAYOUT).unwrap());
    }
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let t0 = std::time::Instant::now();
        for _ in 0..INNER {
            std::hint::black_box(parse(LAYOUT).unwrap());
        }
        samples.push(t0.elapsed().as_nanos() / INNER as u128);
    }
    let p50 = median(samples);
    println!("bench_akml_parse_compose_template  N={INNER}×{SAMPLES}  p50={p50}ns/op");
    // Compose-style template parses every frame in worst-case apps; if
    // we exceed 50µs/op, that's >0.3% of frame budget per inflate.
    assert!(p50 < 50_000, "parse > 50µs/op — regression");
}
