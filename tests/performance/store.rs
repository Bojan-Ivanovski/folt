use std::{fs, hint::black_box, io};

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput};
use folt_core::folio_dir::{Durability, FolioSpec, create_folio};
use folt_core::layout::{FOLIO_LAYOUT_VERSION, OBJECT_ALGORITHM};
use folt_core::object::{ObjectKind, hash_blob};
use folt_core::registry::FolioFormat;
use folt_core::store::ObjectStore;

use crate::common::TempHome;

pub fn bench(c: &mut Criterion) {
    let home = TempHome::new();
    let lock = home.lock();
    let folio = create_folio(
        home.path(),
        &lock,
        FolioSpec { content: vec![] },
        Durability::None,
    )
    .unwrap();
    let store = ObjectStore::open(
        &folio,
        &FolioFormat {
            layout: FOLIO_LAYOUT_VERSION,
            algorithm: OBJECT_ALGORITHM.into(),
        },
    )
    .unwrap();

    for size in crate::SIZES {
        let bytes = crate::input(size);
        let id = hash_blob(&bytes);
        let source = home.root().join("source");
        fs::write(&source, &bytes).unwrap();
        let stored = folio.path().join("blobs").join(format!("{id}.blob"));
        let large = size == crate::SIZES[2];
        let mut group = c.benchmark_group("store_write");
        // Writes include file and directory syncs, so tolerate more runner noise.
        group.noise_threshold(0.20);
        group.throughput(Throughput::Bytes(size as u64));
        if large {
            group.sample_size(10);
        }
        group.bench_function(
            BenchmarkId::new(if large { "put_file" } else { "put" }, size),
            |b| {
                b.iter_batched(
                    || {
                        // Force a fresh write, without timing cleanup or filling the disk.
                        match fs::remove_file(&stored) {
                            Ok(()) => {}
                            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                            Err(e) => panic!("remove benchmark object: {e}"),
                        }
                    },
                    |()| {
                        if large {
                            store.put_file(&lock, ObjectKind::Blob, black_box(&source))
                        } else {
                            store.put(&lock, ObjectKind::Blob, black_box(&bytes))
                        }
                        .unwrap()
                    },
                    BatchSize::PerIteration,
                );
            },
        );
        group.finish();

        store.put(&lock, ObjectKind::Blob, &bytes).unwrap();
        let mut group = c.benchmark_group("store_read");
        group.throughput(Throughput::Bytes(size as u64));
        if large {
            group.sample_size(10);
        }
        group.bench_function(
            BenchmarkId::new(if large { "get_to" } else { "get" }, size),
            |b| {
                b.iter(|| {
                    if large {
                        store
                            .get_to(black_box(id), ObjectKind::Blob, io::sink())
                            .unwrap();
                    } else {
                        black_box(
                            store
                                .get(black_box(id), ObjectKind::Blob, size as u64)
                                .unwrap(),
                        );
                    }
                });
            },
        );
        group.finish();
        fs::remove_file(stored).unwrap();
    }
}
