use folt_core::checkout::{self, CheckoutStores};
use folt_core::history;
use folt_core::placement_fs::PlacementFs;
use folt_core::refs::RefName;

use crate::common::TempHome;
use crate::support::TestFolio;

#[test]
fn checks_branch_out_into_copy() {
    let home = TempHome::new();
    let mut lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let row = folio.copy(&home, &lock);
    let commit = folio.commit_object(&lock, b"branch content");
    let topic = RefName::parse("topic").unwrap();
    history::create_branch(
        folio.folio.id(),
        &topic,
        commit,
        &folio.refs,
        &mut folio.registry,
        &mut lock,
    )
    .unwrap();
    let stores = CheckoutStores {
        objects: &folio.objects,
        refs: &folio.refs,
        object_limit: 16384,
    };
    checkout::checkout_placement(
        &row,
        &topic,
        false,
        &PlacementFs::new(&home.home),
        &stores,
        &mut folio.registry,
        &lock,
    )
    .unwrap();
    let updated = folio.registry.placement_at(&row.path).unwrap().unwrap();
    assert_eq!(updated.head, "topic");
    assert_eq!(updated.commit_id, Some(commit));
    assert_eq!(
        std::fs::read(updated.path.to_path_buf()).unwrap(),
        b"branch content"
    );
}
