use criterion::{criterion_group, criterion_main};

#[allow(dead_code)]
#[path = "../common/mod.rs"]
mod common;
mod drift;
mod hash;
mod registry;
mod store;

const SIZES: [usize; 3] = [4 * 1024, 1024 * 1024, 64 * 1024 * 1024];

// A fixed xorshift stream keeps content and compression decisions stable across runs.
fn input(size: usize) -> Vec<u8> {
    let mut state = 0x4f1b_bcdd_1234_5678_u64;
    (0..size)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

criterion_group!(
    benches,
    hash::bench,
    store::bench,
    drift::bench,
    registry::bench,
    registry::bench_names
);
criterion_main!(benches);
