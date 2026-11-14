//! Benchmark harness (placeholder; real measurements after P0).

use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn scrub_latency_placeholder(c: &mut Criterion) {
    c.bench_function("scrub_latency_placeholder", |b| b.iter(|| black_box(1 + 1)));
}

criterion_group!(benches, scrub_latency_placeholder);
criterion_main!(benches);
