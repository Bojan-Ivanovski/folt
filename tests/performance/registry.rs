use std::hint::black_box;

use criterion::Criterion;
use folt_core::folio_dir::FolioId;
use folt_core::layout::{FOLIO_LAYOUT_VERSION, OBJECT_ALGORITHM};
use folt_core::placement::PlacementKind;
use folt_core::registry::{
    FolioFormat, NewFolio, NewPlacement, PlacementPath, PlacementPrefix, Registry,
};

use crate::common::TempHome;

pub fn bench(c: &mut Criterion) {
    let home = TempHome::new();
    let lock = home.lock();
    let id = FolioId::from_bytes([1; 16]);
    let mut registry = Registry::open_or_create(&home.home, &lock).unwrap();
    let paths: Vec<_> = (0..10_000)
        .map(|n| PlacementPath::resolve(home.root().join(format!("placement-{n:05}"))).unwrap())
        .collect();
    registry
        .write(&lock, |tx| {
            tx.add_folio(&NewFolio {
                id,
                format: FolioFormat {
                    layout: FOLIO_LAYOUT_VERSION,
                    algorithm: OBJECT_ALGORITHM.into(),
                },
            })?;
            for path in &paths {
                tx.add_placement(&NewPlacement {
                    path: path.clone(),
                    id,
                    kind: PlacementKind::Copy,
                    head: None,
                    commit_id: None,
                    base: None,
                    staged: None,
                })?;
            }
            Ok(())
        })
        .unwrap();
    drop(registry);
    let prefix = PlacementPrefix::resolve(home.root()).unwrap();
    let registry = Registry::open_existing(&home.home).unwrap();
    assert_eq!(registry.placements_under(&prefix).unwrap().len(), 10_000);
    let mut group = c.benchmark_group("registry");
    group.bench_function("open_existing/10000", |b| {
        b.iter(|| Registry::open_existing(black_box(&home.home)).unwrap());
    });
    group.bench_function("placements_under/10000", |b| {
        b.iter(|| registry.placements_under(black_box(&prefix)).unwrap());
    });
    group.finish();
}
