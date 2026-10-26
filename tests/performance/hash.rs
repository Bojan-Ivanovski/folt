use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput};
use folt_core::object::hash_blob;

pub fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("hash_blob");
    for size in crate::SIZES {
        let bytes = crate::input(size);
        group.throughput(Throughput::Bytes(size as u64));
        if size == crate::SIZES[2] {
            group.sample_size(10);
        }
        group.bench_with_input(BenchmarkId::from_parameter(size), &bytes, |b, bytes| {
            b.iter(|| hash_blob(black_box(bytes)));
        });
    }
    group.finish();
}
