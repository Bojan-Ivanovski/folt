use folt_core::object::hash_blob;
use folt_core::placement::{Observation, PlacementKind, PlacementOps, StagedBlob};
use folt_core::placement_fs::PlacementFs;
use folt_core::placement_rules::{self, PlacementFacts, RefusalReason};
use folt_core::registry::{Placement, PlacementPath};

use crate::common::TempHome;
use crate::support::TestFolio;

fn facts(row: &Placement, fs: &PlacementFs<'_>) -> PlacementFacts {
    let (id, placement) = row.to_placement();
    let observation = fs.read(id, &placement).unwrap();
    let placement_hash = matches!(observation, Observation::File { .. })
        .then(|| fs.hash_placement(&placement).unwrap());
    PlacementFacts {
        placement: row.clone(),
        observation,
        placement_hash,
        tip: None,
    }
}

#[test]
fn relocate_preserves_copy_history_edits_and_stage_and_resumes() {
    for state in 0..4 {
        for interrupted in [false, true] {
            let home = TempHome::new();
            let lock = home.lock();
            let mut folio = TestFolio::new(&home, &lock);
            let mut row = folio.copy(&home, &lock);
            if state > 0 {
                std::fs::write(row.path.to_path_buf(), b"an edited copy").unwrap();
            }
            if state > 1 {
                row.staged = Some(StagedBlob {
                    hash: hash_blob(b"staged"),
                    size: 6,
                });
            }
            if state > 2 {
                row.head = "topic".into();
                row.commit_id = Some(hash_blob(b"commit"));
            }
            folio
                .registry
                .write(&lock, |tx| tx.update_placement(&row))
                .unwrap();
            let fs = PlacementFs::new(&home.home);
            let before = std::fs::read(row.path.to_path_buf()).unwrap();
            let to = PlacementPath::resolve(home.root().join("moved.txt")).unwrap();
            let source = facts(&row, &fs);
            let target =
                placement_rules::gather_relocate_target(&row, &to, &fs, &folio.registry).unwrap();
            let plan = placement_rules::judge_relocate(&source, &target).unwrap();
            if interrupted {
                fs.relocate(
                    row.id,
                    &row.to_placement().1,
                    &to.to_path_buf(),
                    &source.observation,
                )
                .unwrap();
            }
            placement_rules::apply_relocate(&plan, &fs, &mut folio.registry, &lock).unwrap();
            assert!(!row.path.to_path_buf().exists());
            assert_eq!(std::fs::read(to.to_path_buf()).unwrap(), before);
            assert!(folio.registry.placement_at(&row.path).unwrap().is_none());
            let moved = folio.registry.placement_at(&to).unwrap().unwrap();
            assert_eq!(moved.head, row.head);
            assert_eq!(moved.commit_id, row.commit_id);
            assert_eq!(moved.staged, row.staged);
            assert_eq!(
                moved.base.as_ref().unwrap().hash,
                row.base.as_ref().unwrap().hash
            );
            assert_eq!(moved.base.as_ref().unwrap().signature.size, 7);
        }
    }
}

#[test]
fn relocate_refuses_existing_and_registered_destinations_and_changed_facts() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let row = folio.copy(&home, &lock);
    let fs = PlacementFs::new(&home.home);
    let source = facts(&row, &fs);
    let to = PlacementPath::resolve(home.root().join("destination")).unwrap();
    for directory in [false, true] {
        if directory {
            std::fs::create_dir(to.to_path_buf()).unwrap();
        } else {
            std::fs::write(to.to_path_buf(), b"occupied").unwrap();
        }
        let target =
            placement_rules::gather_relocate_target(&row, &to, &fs, &folio.registry).unwrap();
        assert_eq!(
            placement_rules::judge_relocate(&source, &target)
                .unwrap_err()
                .reason,
            RefusalReason::DestinationExists
        );
        assert!(
            fs.relocate(
                row.id,
                &row.to_placement().1,
                &to.to_path_buf(),
                &source.observation
            )
            .is_err()
        );
        if directory {
            std::fs::remove_dir(to.to_path_buf()).unwrap();
        } else {
            std::fs::remove_file(to.to_path_buf()).unwrap();
        }
    }
    let mut other = row.clone();
    other.path = to.clone();
    folio
        .registry
        .write(&lock, |tx| {
            tx.add_placement(&folt_core::registry::NewPlacement {
                path: to.clone(),
                id: row.id,
                kind: row.kind,
                head: None,
                commit_id: None,
                base: row.base.clone(),
                staged: None,
            })
        })
        .unwrap();
    let target = placement_rules::gather_relocate_target(&row, &to, &fs, &folio.registry).unwrap();
    assert_eq!(
        placement_rules::judge_relocate(&source, &target)
            .unwrap_err()
            .reason,
        RefusalReason::DestinationRegistered
    );
    folio
        .registry
        .write(&lock, |tx| tx.remove_placement(&other.path))
        .unwrap();
    let target = placement_rules::gather_relocate_target(&row, &to, &fs, &folio.registry).unwrap();
    let plan = placement_rules::judge_relocate(&source, &target).unwrap();
    std::fs::write(row.path.to_path_buf(), b"changed").unwrap();
    assert!(placement_rules::apply_relocate(&plan, &fs, &mut folio.registry, &lock).is_err());
    assert_eq!(folio.registry.placement_at(&row.path).unwrap(), Some(row));
    assert!(!to.to_path_buf().exists());
}

#[test]
fn remove_copy_checks_edits_stage_discard_and_forget() {
    for state in 0..4 {
        for delete_file in [false, true] {
            for discard in [false, true] {
                let home = TempHome::new();
                let lock = home.lock();
                let mut folio = TestFolio::new(&home, &lock);
                let mut row = folio.copy(&home, &lock);
                if state == 1 {
                    std::fs::write(row.path.to_path_buf(), b"edited").unwrap();
                }
                if state == 2 {
                    row.staged = Some(StagedBlob {
                        hash: hash_blob(b"initial"),
                        size: 7,
                    });
                    folio
                        .registry
                        .write(&lock, |tx| tx.update_placement(&row))
                        .unwrap();
                }
                if state == 3 {
                    std::fs::remove_file(row.path.to_path_buf()).unwrap();
                }
                let fs = PlacementFs::new(&home.home);
                let plan = placement_rules::judge_remove(&facts(&row, &fs), delete_file, discard);
                if delete_file && !discard && (state == 1 || state == 2) {
                    assert_eq!(
                        plan.unwrap_err().reason,
                        if state == 1 {
                            RefusalReason::Edited
                        } else {
                            RefusalReason::Staged
                        }
                    );
                    assert_eq!(
                        folio.registry.placement_at(&row.path).unwrap(),
                        Some(row.clone())
                    );
                    assert!(row.path.to_path_buf().exists());
                } else {
                    placement_rules::apply_remove(&plan.unwrap(), &fs, &mut folio.registry, &lock)
                        .unwrap();
                    assert!(folio.registry.placement_at(&row.path).unwrap().is_none());
                    assert_eq!(row.path.to_path_buf().exists(), !delete_file && state != 3);
                }
                assert!(folio.internal().path.to_path_buf().exists());
            }
        }
    }
}

#[test]
fn remove_rechecks_and_resumes_after_unlink() {
    for interrupted in [false, true] {
        let home = TempHome::new();
        let lock = home.lock();
        let mut folio = TestFolio::new(&home, &lock);
        let row = folio.copy(&home, &lock);
        let fs = PlacementFs::new(&home.home);
        let source = facts(&row, &fs);
        let plan = placement_rules::judge_remove(&source, true, false).unwrap();
        if interrupted {
            fs.unlink(row.id, &row.to_placement().1, &source.observation)
                .unwrap();
            placement_rules::apply_remove(&plan, &fs, &mut folio.registry, &lock).unwrap();
            assert!(folio.registry.placement_at(&row.path).unwrap().is_none());
        } else {
            std::fs::write(row.path.to_path_buf(), b"changed").unwrap();
            assert!(placement_rules::apply_remove(&plan, &fs, &mut folio.registry, &lock).is_err());
            assert_eq!(std::fs::read(row.path.to_path_buf()).unwrap(), b"changed");
        }
    }
}

#[test]
fn directory_move_preserves_files_and_moves_registry_prefix() {
    use folt_core::registry::{MoveResult, PlacementPrefix};
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let row = folio.copy(&home, &lock);
    let from = home.root().join("directory");
    let to = home.root().join("moved-directory");
    std::fs::create_dir(&from).unwrap();
    std::fs::rename(row.path.to_path_buf(), from.join("copy.txt")).unwrap();
    let mut moved = row.clone();
    moved.path = PlacementPath::resolve(from.join("copy.txt")).unwrap();
    folio
        .registry
        .write(&lock, |tx| tx.move_placement(&row.path, &moved.path))
        .unwrap();
    std::fs::write(from.join("unmanaged"), b"keep me").unwrap();
    let old = PlacementPrefix::resolve(&from).unwrap();
    let new = PlacementPrefix::resolve(&to).unwrap();
    let mut directory = row.to_placement().1;
    directory.path = from.clone();
    directory.base = None;
    let fs = PlacementFs::new(&home.home);
    fs.relocate(row.id, &directory, &to, &Observation::NotAFile)
        .unwrap();
    assert_eq!(
        folio
            .registry
            .write(&lock, |tx| tx.move_prefix(&old, &new))
            .unwrap(),
        MoveResult::Moved
    );
    assert_eq!(std::fs::read(to.join("unmanaged")).unwrap(), b"keep me");
    moved.path = PlacementPath::resolve(to.join("copy.txt")).unwrap();
    assert_eq!(
        folio.registry.placement_at(&moved.path).unwrap(),
        Some(moved)
    );
}

#[cfg(unix)]
fn link(folio: &mut TestFolio, home: &TempHome, lock: &folt_core::home::HomeLock) -> Placement {
    let row = folt_core::registry::NewPlacement {
        path: PlacementPath::resolve(home.root().join("link.txt")).unwrap(),
        id: folio.folio.id(),
        kind: PlacementKind::Link,
        head: None,
        commit_id: None,
        base: None,
        staged: None,
    };
    std::os::unix::fs::symlink(folio.internal().path.to_path_buf(), row.path.to_path_buf())
        .unwrap();
    folio
        .registry
        .write(lock, |tx| tx.add_placement(&row))
        .unwrap();
    folio.registry.placement_at(&row.path).unwrap().unwrap()
}

#[cfg(unix)]
#[test]
fn link_relocate_and_remove_address_only_the_link_row() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let row = link(&mut folio, &home, &lock);
    let internal = folio.internal();
    let fs = PlacementFs::new(&home.home);
    let to = PlacementPath::resolve(home.root().join("moved-link")).unwrap();
    let target = placement_rules::gather_relocate_target(&row, &to, &fs, &folio.registry).unwrap();
    let plan = placement_rules::judge_relocate(&facts(&row, &fs), &target).unwrap();
    placement_rules::apply_relocate(&plan, &fs, &mut folio.registry, &lock).unwrap();
    let moved = folio.registry.placement_at(&to).unwrap().unwrap();
    assert_eq!(
        std::fs::read_link(to.to_path_buf()).unwrap(),
        internal.path.to_path_buf()
    );
    let plan = placement_rules::judge_remove(&facts(&moved, &fs), true, false).unwrap();
    placement_rules::apply_remove(&plan, &fs, &mut folio.registry, &lock).unwrap();
    assert!(!to.to_path_buf().exists());
    assert_eq!(folio.internal(), internal);
    assert_eq!(
        std::fs::read(internal.path.to_path_buf()).unwrap(),
        b"initial"
    );
}

#[cfg(unix)]
#[test]
fn detached_file_is_never_deleted_even_with_discard() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let row = link(&mut folio, &home, &lock);
    std::fs::remove_file(row.path.to_path_buf()).unwrap();
    std::fs::write(row.path.to_path_buf(), b"personal file").unwrap();
    let fs = PlacementFs::new(&home.home);
    let source = facts(&row, &fs);
    assert!(placement_rules::judge_remove(&source, true, true).is_err());
    assert!(placement_rules::judge_repair_link(&source).is_err());
    let plan = placement_rules::judge_remove(&source, false, false).unwrap();
    placement_rules::apply_remove(&plan, &fs, &mut folio.registry, &lock).unwrap();
    assert_eq!(
        std::fs::read(row.path.to_path_buf()).unwrap(),
        b"personal file"
    );
}

#[cfg(unix)]
#[test]
fn repair_accepts_only_the_same_folios_former_home() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let row = link(&mut folio, &home, &lock);
    let fs = PlacementFs::new(&home.home);
    for target in [
        home.root().join("unrelated"),
        home.root().join("old/folios/another/folio.main"),
    ] {
        std::fs::remove_file(row.path.to_path_buf()).unwrap();
        std::os::unix::fs::symlink(&target, row.path.to_path_buf()).unwrap();
        let error = placement_rules::judge_repair_link(&facts(&row, &fs)).unwrap_err();
        assert!(error.to_string().contains("#33"));
        assert_eq!(std::fs::read_link(row.path.to_path_buf()).unwrap(), target);
    }
    let target = home
        .root()
        .join("old/folios")
        .join(row.id.to_string())
        .join("folio.main");
    std::fs::remove_file(row.path.to_path_buf()).unwrap();
    std::os::unix::fs::symlink(target, row.path.to_path_buf()).unwrap();
    let plan = placement_rules::judge_repair_link(&facts(&row, &fs)).unwrap();
    placement_rules::apply_repair_link(&plan, &fs, &folio.objects, &folio.registry, &lock).unwrap();
    placement_rules::apply_repair_link(&plan, &fs, &folio.objects, &folio.registry, &lock).unwrap();
    assert_eq!(
        fs.read(row.id, &row.to_placement().1).unwrap(),
        Observation::IntactLink
    );
    assert_eq!(folio.registry.placement_at(&row.path).unwrap(), Some(row));
}

#[test]
fn rehome_rewrites_internal_rows_and_leaves_copies_untouched() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let copy = folio.copy(&home, &lock);
    let internal = folio.internal();
    #[cfg(unix)]
    let link = link(&mut folio, &home, &lock);
    let old_home = home.root().join("old-home");
    let old_path = old_home
        .join("folios")
        .join(internal.id.to_string())
        .join("folio.main");
    std::fs::create_dir_all(old_path.parent().unwrap()).unwrap();
    let old_key = PlacementPath::resolve(&old_path).unwrap();
    folio
        .registry
        .write(&lock, |tx| tx.move_placement(&internal.path, &old_key))
        .unwrap();
    #[cfg(unix)]
    {
        std::fs::remove_file(link.path.to_path_buf()).unwrap();
        std::os::unix::fs::symlink(&old_path, link.path.to_path_buf()).unwrap();
    }
    std::fs::remove_dir_all(&old_home).unwrap();
    let fs = PlacementFs::new(&home.home);
    for _ in 0..2 {
        placement_rules::rehome(&old_home, &fs, &folio.objects, &mut folio.registry, &lock)
            .unwrap();
        assert_eq!(folio.internal(), internal);
        assert_eq!(
            folio.registry.placement_at(&copy.path).unwrap(),
            Some(copy.clone())
        );
        assert_eq!(std::fs::read(copy.path.to_path_buf()).unwrap(), b"initial");
        #[cfg(unix)]
        assert_eq!(
            std::fs::read_link(link.path.to_path_buf()).unwrap(),
            internal.path.to_path_buf()
        );
    }
}

#[test]
fn rehome_refuses_mixed_internal_homes_without_writes() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut first = TestFolio::new(&home, &lock);
    let second = TestFolio::new(&home, &lock);
    let internal = first.internal();
    let old_home = home.root().join("old-home");
    let path = old_home
        .join("folios")
        .join(internal.id.to_string())
        .join("folio.main");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let old = PlacementPath::resolve(path).unwrap();
    first
        .registry
        .write(&lock, |tx| tx.move_placement(&internal.path, &old))
        .unwrap();
    let fs = PlacementFs::new(&home.home);
    assert!(
        placement_rules::rehome(&old_home, &fs, &first.objects, &mut first.registry, &lock)
            .is_err()
    );
    assert_eq!(first.internal().path, old);
    assert_eq!(
        second.internal().path.to_path_buf(),
        fs.link_target(second.folio.id())
    );
}

#[test]
fn rehome_after_renaming_and_reopening_the_home() {
    use folt_core::home::Home;
    use folt_core::registry::Registry;
    use folt_core::store::ObjectStore;
    let home = TempHome::new();
    let lock = home.lock();
    let mut first = TestFolio::new(&home, &lock);
    let second = TestFolio::new(&home, &lock);
    let copy = first.copy(&home, &lock);
    let internals = [first.internal(), second.internal()];
    #[cfg(unix)]
    let link = link(&mut first, &home, &lock);
    drop(first);
    drop(second);
    drop(lock);
    let new_path = home.root().join("moved-home");
    std::fs::rename(home.path(), &new_path).unwrap();
    let moved_home = Home::open_or_create(new_path).unwrap();
    let lock = moved_home.try_lock().unwrap();
    let mut registry = Registry::open_or_create(&moved_home, &lock).unwrap();
    let row = registry.folio_row(copy.id).unwrap().unwrap();
    let folio = folt_core::folio_dir::validate(moved_home.path(), &row).unwrap();
    let objects = ObjectStore::open(&folio, &row.format).unwrap();
    let fs = PlacementFs::new(&moved_home);
    placement_rules::rehome(home.path(), &fs, &objects, &mut registry, &lock).unwrap();
    for mut internal in internals {
        let old = internal.path.clone();
        internal.path = PlacementPath::resolve(fs.link_target(internal.id)).unwrap();
        assert_eq!(
            registry.internal_placement(internal.id).unwrap(),
            Some(internal)
        );
        assert!(registry.placement_at(&old).unwrap().is_none());
    }
    assert_eq!(registry.placement_at(&copy.path).unwrap(), Some(copy));
    #[cfg(unix)]
    assert_eq!(
        fs.read(link.id, &link.to_placement().1).unwrap(),
        Observation::IntactLink
    );
}

#[cfg(unix)]
#[test]
fn rehome_refuses_deliberate_retargets_before_rewriting_any_row() {
    for elsewhere in [false, true] {
        let home = TempHome::new();
        let lock = home.lock();
        let mut folio = TestFolio::new(&home, &lock);
        let link = link(&mut folio, &home, &lock);
        let internal = folio.internal();
        let old_home = home.root().join("old-home");
        let old_path = old_home
            .join("folios")
            .join(link.id.to_string())
            .join("folio.main");
        std::fs::create_dir_all(old_path.parent().unwrap()).unwrap();
        let old = PlacementPath::resolve(old_path).unwrap();
        folio
            .registry
            .write(&lock, |tx| tx.move_placement(&internal.path, &old))
            .unwrap();
        let target = if elsewhere {
            home.root()
                .join("another-home/folios")
                .join(link.id.to_string())
                .join("folio.main")
        } else {
            home.root().join("personal.txt")
        };
        std::fs::remove_file(link.path.to_path_buf()).unwrap();
        std::os::unix::fs::symlink(&target, link.path.to_path_buf()).unwrap();
        let fs = PlacementFs::new(&home.home);
        let error =
            placement_rules::rehome(&old_home, &fs, &folio.objects, &mut folio.registry, &lock)
                .unwrap_err();
        assert!(error.to_string().contains("#33"));
        assert_eq!(folio.internal().path, old);
        assert_eq!(std::fs::read_link(link.path.to_path_buf()).unwrap(), target);
    }
}
