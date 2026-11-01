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
        matches!(folio.registry.write(&lock, |tx| tx.remove_folio(folio.folio.id(), &mut |_, _| Ok(true))), Err(RegistryError::FolioHasPlacements(id)) if id == folio.folio.id())
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
            tx.remove_folio(folio.folio.id(), &mut |_, _| Ok(true))
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

#[test]
fn names_are_unique_case_sensitive_and_released_by_removal() {
    use folt_core::folio_dir::FolioId;
    use folt_core::folio_name::FolioName;
    use folt_core::registry::{NewFolio, Registry};

    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let first = folio.folio.id();
    let second = FolioId::generate();
    let name = FolioName::parse("logo").unwrap();
    let upper = FolioName::parse("Logo").unwrap();
    folio.registry.write(&lock, |tx| {
        for id in [second, FolioId::generate(), FolioId::generate()] {
            tx.add_folio(&NewFolio { id, format: folio.format.clone() })?;
        }
        assert!(matches!(tx.rename(first, &name), Err(RegistryError::NotNamed(id)) if id == first));
        assert_eq!(tx.clear_name(first)?, None);
        assert_eq!(tx.set_name(first, &name)?, None);
        assert_eq!(tx.set_name(first, &name)?, Some(name.clone()));
        assert!(matches!(tx.set_name(second, &name), Err(RegistryError::NameTaken(taken)) if taken == name));
        assert!(matches!(tx.rename(second, &name), Err(RegistryError::NotNamed(_))));
        assert_eq!(tx.set_name(second, &upper)?, None);
        Ok(())
    }).unwrap();
    let reader = Registry::open_existing(&home.home).unwrap();
    assert_eq!(reader.find_by_name(&name).unwrap(), Some(first));
    assert_eq!(reader.find_by_name(&upper).unwrap(), Some(second));
    assert_eq!(reader.folio_name(first).unwrap(), Some(name.clone()));
    folio
        .registry
        .write(&lock, |tx| {
            assert!(matches!(
                tx.rename(second, &name),
                Err(RegistryError::NameTaken(_))
            ));
            tx.remove_folio(first, &mut |_, _| Ok(true))?;
            assert_eq!(tx.rename(second, &name)?, upper.clone());
            assert_eq!(tx.clear_name(second)?, Some(name.clone()));
            assert_eq!(tx.clear_name(second)?, None);
            assert_eq!(tx.set_name(second, &upper)?, None);
            assert_eq!(tx.set_name(second, &name)?, Some(upper.clone()));
            Ok(())
        })
        .unwrap();
    assert_eq!(reader.find_by_name(&upper).unwrap(), None);
    assert_eq!(reader.find_by_name(&name).unwrap(), Some(second));
    assert!(
        matches!(reader.folio_name(first), Err(RegistryError::FolioMissing(id)) if id == first)
    );
    for action in 0..3 {
        assert!(matches!(folio.registry.write(&lock, |tx| match action {
            0 => tx.set_name(first, &name),
            1 => tx.rename(first, &name).map(Some),
            _ => tx.clear_name(first),
        }), Err(RegistryError::FolioMissing(id)) if id == first));
    }
}

#[test]
fn replace_all_preserves_only_surviving_names() {
    use folt_core::folio_dir::FolioId;
    use folt_core::folio_name::FolioName;
    use folt_core::registry::{FolioRow, NewFolio};

    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let survivor = folio.folio.id();
    let removed = FolioId::generate();
    let added = FolioId::generate();
    let kept_name = FolioName::parse("kept").unwrap();
    let freed_name = FolioName::parse("freed").unwrap();
    folio
        .registry
        .write(&lock, |tx| {
            tx.add_folio(&NewFolio {
                id: removed,
                format: folio.format.clone(),
            })?;
            tx.set_name(survivor, &kept_name)?;
            tx.set_name(removed, &freed_name)?;
            tx.replace_all(
                &[survivor, added].map(|id| FolioRow {
                    id,
                    format: folio.format.clone(),
                }),
                &[],
            )
        })
        .unwrap();
    assert_eq!(
        folio.registry.folio_name(survivor).unwrap(),
        Some(kept_name.clone())
    );
    assert_eq!(folio.registry.folio_name(added).unwrap(), None);
    assert_eq!(
        folio.registry.find_by_name(&kept_name).unwrap(),
        Some(survivor)
    );
    assert_eq!(folio.registry.find_by_name(&freed_name).unwrap(), None);
    assert!(!folio.registry.folio_exists(removed).unwrap());
    folio
        .registry
        .write(&lock, |tx| tx.set_name(added, &freed_name))
        .unwrap();
}
