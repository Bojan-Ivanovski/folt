use folt_core::placement::{Drift, Observation, PlacementOps, classify};
use folt_core::placement_fs::PlacementFs;

use crate::common::TempHome;
use crate::support::TestFolio;

#[test]
fn copy_is_in_sync_with_its_base() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let row = folio.copy(&home, &lock);
    let (id, placement) = row.to_placement();
    let fs = PlacementFs::new(&home.home);
    let observation = fs.read(id, &placement).unwrap();
    let hash = fs.hash_placement(&placement).unwrap();
    assert_eq!(
        classify(&placement, &observation, Some(hash)),
        Drift::InSync
    );
    assert_eq!(std::fs::read(&placement.path).unwrap(), b"initial");
}

#[test]
fn edited_then_missing_copy_reports_drift() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let row = folio.copy(&home, &lock);
    let (id, placement) = row.to_placement();
    let fs = PlacementFs::new(&home.home);
    // Same length: detecting this edit requires a hash, not metadata timing.
    std::fs::write(&placement.path, b"changed").unwrap();
    let observation = fs.read(id, &placement).unwrap();
    assert_eq!(
        classify(
            &placement,
            &observation,
            Some(fs.hash_placement(&placement).unwrap())
        ),
        Drift::PlacementEdited
    );
    std::fs::remove_file(&placement.path).unwrap();
    let observation = fs.read(id, &placement).unwrap();
    assert_eq!(observation, Observation::Missing);
    assert_eq!(classify(&placement, &observation, None), Drift::Gone);
}

#[cfg(unix)]
#[test]
fn link_points_to_internal_working_file() {
    use folt_core::object::ObjectKind;
    use folt_core::placement::{Placement, PlacementKind};
    let home = TempHome::new();
    let lock = home.lock();
    let folio = TestFolio::new(&home, &lock);
    let blob = folio
        .objects
        .put(&lock, ObjectKind::Blob, b"initial")
        .unwrap();
    let placement = Placement {
        path: home.root().join("link.txt"),
        kind: PlacementKind::Link,
        head: "main".into(),
        commit_id: None,
        base: None,
    };
    let fs = PlacementFs::new(&home.home);
    assert_eq!(
        fs.create(folio.folio.id(), &placement, &folio.objects, blob)
            .unwrap()
            .base,
        None
    );
    assert_eq!(
        std::fs::read_link(&placement.path).unwrap(),
        fs.link_target(folio.folio.id())
    );
    assert_eq!(
        fs.read(folio.folio.id(), &placement).unwrap(),
        Observation::IntactLink
    );
    assert_eq!(std::fs::read(&placement.path).unwrap(), b"initial");
}
