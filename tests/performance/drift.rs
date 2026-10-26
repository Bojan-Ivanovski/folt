use std::{fs, hint::black_box};

use criterion::{BenchmarkId, Criterion};
use folt_core::folio_dir::FolioId;
use folt_core::object::hash_blob;
use folt_core::placement::{
    self, Observation, Placement, PlacementBase, PlacementKind, PlacementOps,
};
use folt_core::placement_fs::PlacementFs;

use crate::common::TempHome;

pub fn bench(c: &mut Criterion) {
    let home = TempHome::new();
    let filesystem = PlacementFs::new(&home.home);
    let id = FolioId::from_bytes([1; 16]);
    let mut group = c.benchmark_group("drift");
    for size in crate::SIZES {
        let bytes = crate::input(size);
        let path = home.root().join("working");
        fs::write(&path, &bytes).unwrap();
        let mut placement = Placement {
            path,
            kind: PlacementKind::Copy,
            head: "main".into(),
            commit_id: None,
            base: None,
        };
        let Observation::File { signature } = filesystem.read(id, &placement).unwrap() else {
            panic!("benchmark working file missing");
        };
        placement.base = Some(PlacementBase {
            hash: hash_blob(&bytes),
            signature,
        });
        if size == crate::SIZES[2] {
            group.sample_size(10);
        }
        for mismatch in [true, false] {
            placement.base.as_mut().unwrap().signature.size = size as u64 + u64::from(mismatch);
            group.bench_function(
                BenchmarkId::new(
                    if mismatch {
                        "classify_size_mismatch"
                    } else {
                        "classify_same_size"
                    },
                    size,
                ),
                |b| {
                    b.iter(|| {
                        let observation = filesystem.read(id, black_box(&placement)).unwrap();
                        let hash = if placement::needs_hash(&placement, &observation) {
                            Some(filesystem.hash_placement(&placement).unwrap())
                        } else {
                            None
                        };
                        placement::classify(&placement, &observation, hash)
                    });
                },
            );
        }
    }
    group.finish();
}
