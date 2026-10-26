use folt_core::registry::{PlacementPath, PlacementPrefix, RegistryError};

use crate::common::TempHome;
use crate::support::TestFolio;

#[test]
fn adds_folio_and_copy_placement() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let row = folio.copy(&home, &lock);
    assert_eq!(folio.registry.folios().unwrap(), vec![folio.folio.id()]);
    assert_eq!(
        folio.registry.placement_at(&row.path).unwrap(),
        Some(row.clone())
    );
    assert_eq!(
        folio.registry.placements_of(folio.folio.id()).unwrap(),
        vec![row]
    );
}

#[test]
fn internal_rows_are_hidden_from_user_lists() {
    let home = TempHome::new();
    let lock = home.lock();
    let folio = TestFolio::new(&home, &lock);
    assert!(
        folio
            .registry
            .placements_of(folio.folio.id())
            .unwrap()
            .is_empty()
    );
    assert!(
        folio
            .registry
            .placements_under(&PlacementPrefix::resolve(home.root()).unwrap())
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        folio
            .registry
            .placements_of_head(folio.folio.id(), "main")
            .unwrap(),
        vec![folio.internal()]
    );
}

#[test]
fn removing_folio_requires_no_user_placements() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let copy = folio.copy(&home, &lock);
    let internal = folio.internal();
    assert!(
        matches!(folio.registry.write(&lock, |tx| tx.remove_folio(folio.folio.id())), Err(RegistryError::FolioHasPlacements(id)) if id == folio.folio.id())
    );
    assert!(folio.registry.folio_exists(folio.folio.id()).unwrap());
    assert_eq!(
        folio.registry.placement_at(&copy.path).unwrap(),
        Some(copy.clone())
    );
    folio
        .registry
        .write(&lock, |tx| {
            tx.remove_placement(&copy.path)?;
            tx.remove_folio(folio.folio.id())
        })
        .unwrap();
    assert!(!folio.registry.folio_exists(folio.folio.id()).unwrap());
    for row in [copy, internal] {
        assert_eq!(folio.registry.placement_at(&row.path).unwrap(), None);
    }
}

#[test]
fn moving_prefix_preserves_row_data() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let mut row = folio.copy(&home, &lock);
    let old_dir = home.root().join("old");
    let new_dir = home.root().join("new");
    std::fs::create_dir(&old_dir).unwrap();
    std::fs::create_dir(&new_dir).unwrap();
    let old_path = PlacementPath::resolve(old_dir.join("file.txt")).unwrap();
    folio
        .registry
        .write(&lock, |tx| tx.move_placement(&row.path, &old_path))
        .unwrap();
    let old = PlacementPrefix::resolve(&old_dir).unwrap();
    let new = PlacementPrefix::resolve(&new_dir).unwrap();
    folio
        .registry
        .write(&lock, |tx| tx.move_prefix(&old, &new))
        .unwrap();
    assert_eq!(folio.registry.placement_at(&old_path).unwrap(), None);
    row.path = PlacementPath::resolve(new_dir.join("file.txt")).unwrap();
    assert_eq!(folio.registry.placement_at(&row.path).unwrap(), Some(row));
}
