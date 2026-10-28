use folt_core::object::hash_blob;
#[cfg(unix)]
use folt_core::placement::PlacementKind;
use folt_core::placement::{Observation, PlacementOps, StagedBlob};
use folt_core::placement_fs::PlacementFs;
use folt_core::placement_rules::{self, PlacementFacts, RefusalReason};
use folt_core::registry::{Placement, PlacementPath};

use crate::common::TempHome;
use crate::support::TestFolio;

// Exercise the same aliasing as macOS's /var -> /private/var on every Unix host.
// Windows keeps the ordinary setup without requiring symlink privileges.
fn placement_home() -> TempHome {
    let home = TempHome::new();
    #[cfg(unix)]
    let home = {
        let mut home = home;
        let alias = home.root().join("alias");
        std::os::unix::fs::symlink(std::fs::canonicalize(home.root()).unwrap(), &alias).unwrap();
        home.home = folt_core::home::Home::open_or_create(alias.join("home")).unwrap();
        home
    };
    home
}

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
            let home = placement_home();
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
                folio
                    .registry
                    .write(&lock, |tx| {
                        tx.add_placement(&folt_core::registry::NewPlacement {
                            path: to.clone(),
                            id: row.id,
                            kind: row.kind,
                            head: Some(row.head.clone()),
                            commit_id: row.commit_id,
                            base: row.base.clone(),
                            staged: row.staged,
                        })
                    })
                    .unwrap();
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
    let home = placement_home();
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
                head: Some("unrelated".into()),
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
                let home = placement_home();
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
        let home = placement_home();
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
    let home = placement_home();
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
    std::fs::create_dir(from.join("nested")).unwrap();
    std::fs::write(from.join("nested/file"), b"nested content").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("../unmanaged", from.join("nested/link")).unwrap();
    let mut directory = moved.clone();
    directory.path = PlacementPath::resolve_directory(&from).unwrap();
    directory.base = None;
    let fs = PlacementFs::new(&home.home);
    let to_key = PlacementPath::resolve(&to).unwrap();
    let target =
        placement_rules::gather_relocate_target(&directory, &to_key, &fs, &folio.registry).unwrap();
    let plan = placement_rules::judge_relocate(&facts(&directory, &fs), &target).unwrap();
    placement_rules::apply_relocate(&plan, &fs, &mut folio.registry, &lock).unwrap();
    assert_eq!(std::fs::read(to.join("unmanaged")).unwrap(), b"keep me");
    assert!(!from.exists());
    assert_eq!(
        std::fs::read(to.join("nested/file")).unwrap(),
        b"nested content"
    );
    #[cfg(unix)]
    {
        assert_eq!(
            std::fs::read_link(to.join("nested/link")).unwrap(),
            std::path::Path::new("../unmanaged")
        );
        assert_eq!(std::fs::read(to.join("nested/link")).unwrap(), b"keep me");
    }
    moved.path = PlacementPath::resolve(to.join("copy.txt")).unwrap();
    assert_eq!(
        folio.registry.placement_at(&moved.path).unwrap(),
        Some(moved)
    );
}

#[test]
fn relocate_refuses_sources_and_destinations_containing_internal_paths_or_home() {
    let home = placement_home();
    let lock = home.lock();
    let folio = TestFolio::new(&home, &lock);
    let internal = folio.internal();
    let fs = PlacementFs::new(&home.home);
    let ordinary = PlacementPath::resolve_directory(home.root().join("ordinary")).unwrap();
    std::fs::create_dir(ordinary.to_path_buf()).unwrap();
    let internal_path = internal.path.to_path_buf();
    let home_path = std::fs::canonicalize(home.path()).unwrap();
    for protected in [
        internal_path.clone(),
        internal_path.parent().unwrap().to_path_buf(),
        home_path.join("folios"),
        home_path.clone(),
        home_path.parent().unwrap().to_path_buf(),
    ] {
        let protected = PlacementPath::resolve_directory(protected).unwrap();
        for source_is_protected in [false, true] {
            let mut directory = internal.clone();
            directory.kind = folt_core::placement::PlacementKind::Copy;
            directory.base = None;
            let (source, destination) = if source_is_protected {
                (&protected, &ordinary)
            } else {
                (&ordinary, &protected)
            };
            directory.path = source.clone();
            let error = placement_rules::gather_relocate_target(
                &directory,
                destination,
                &fs,
                &folio.registry,
            )
            .unwrap_err();
            assert!(matches!(
                error,
                placement_rules::RulesError::Refused(ref refusal)
                    if refusal.reason == RefusalReason::Internal
            ));
            assert_eq!(folio.internal(), internal);
            assert_eq!(std::fs::read(&internal_path).unwrap(), b"initial");
        }
    }

    // Stored internal rows remain protected even outside the current home.
    let mut former = internal.clone();
    former.path = PlacementPath::resolve(home.root().join("former-internal")).unwrap();
    let mut registry = folio.registry;
    registry
        .write(&lock, |tx| tx.move_placement(&internal.path, &former.path))
        .unwrap();
    let mut directory = former.clone();
    directory.kind = folt_core::placement::PlacementKind::Copy;
    directory.base = None;
    for source_is_protected in [false, true] {
        directory.path = if source_is_protected {
            former.path.clone()
        } else {
            ordinary.clone()
        };
        let destination = if source_is_protected {
            &ordinary
        } else {
            &former.path
        };
        assert!(matches!(
            placement_rules::gather_relocate_target(&directory, destination, &fs, &registry),
            Err(placement_rules::RulesError::Refused(ref refusal))
                if refusal.reason == RefusalReason::Internal
        ));
    }
}

#[cfg(unix)]
#[test]
fn unresolvable_link_target_can_be_observed_and_forgotten() {
    let home = placement_home();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let row = link(&mut folio, &home, &lock);
    let personal = home.root().join("personal.txt");
    std::fs::write(&personal, b"personal content").unwrap();
    std::fs::remove_file(row.path.to_path_buf()).unwrap();
    let target = std::path::PathBuf::from("personal.txt/child");
    std::os::unix::fs::symlink(&target, row.path.to_path_buf()).unwrap();
    let fs = PlacementFs::new(&home.home);
    let gathered =
        placement_rules::gather(std::slice::from_ref(&row), &fs, |_| Ok(&folio.refs)).unwrap();
    assert_eq!(
        gathered[0].observation,
        Observation::OtherLink {
            target: target.clone()
        }
    );
    assert_eq!(
        placement_rules::judge_remove(&gathered[0], true, false)
            .unwrap_err()
            .reason,
        RefusalReason::LinkReplaced
    );
    let plan = placement_rules::judge_remove(&gathered[0], false, false).unwrap();
    placement_rules::apply_remove(&plan, &fs, &mut folio.registry, &lock).unwrap();
    assert!(folio.registry.placement_at(&row.path).unwrap().is_none());
    assert_eq!(std::fs::read_link(row.path.to_path_buf()).unwrap(), target);
    assert_eq!(std::fs::read(personal).unwrap(), b"personal content");
    assert_eq!(
        std::fs::read(folio.internal().path.to_path_buf()).unwrap(),
        b"initial"
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
fn missing_link_never_adopts_an_independent_link() {
    let home = placement_home();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let row = link(&mut folio, &home, &lock);
    let fs = PlacementFs::new(&home.home);
    let to = PlacementPath::resolve(home.root().join("independent-link")).unwrap();
    let target = placement_rules::gather_relocate_target(&row, &to, &fs, &folio.registry).unwrap();
    let original_plan = placement_rules::judge_relocate(&facts(&row, &fs), &target).unwrap();
    std::fs::remove_file(row.path.to_path_buf()).unwrap();
    std::os::unix::fs::symlink(fs.link_target(row.id), to.to_path_buf()).unwrap();

    let target = placement_rules::gather_relocate_target(&row, &to, &fs, &folio.registry).unwrap();
    assert_eq!(target.observation, Observation::IntactLink);
    assert!(!target.pending);
    assert_eq!(
        placement_rules::judge_relocate(&facts(&row, &fs), &target)
            .unwrap_err()
            .reason,
        RefusalReason::Gone
    );
    assert!(
        placement_rules::apply_relocate(&original_plan, &fs, &mut folio.registry, &lock).is_err()
    );
    assert_eq!(
        folio.registry.placement_at(&row.path).unwrap(),
        Some(row.clone())
    );
    assert!(folio.registry.placement_at(&to).unwrap().is_none());
    assert_eq!(
        std::fs::read_link(to.to_path_buf()).unwrap(),
        fs.link_target(row.id)
    );
    assert_eq!(std::fs::read(to.to_path_buf()).unwrap(), b"initial");
}

#[cfg(unix)]
#[test]
fn link_relocate_and_remove_address_only_the_link_row() {
    let home = placement_home();
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
    let home = placement_home();
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
    let home = placement_home();
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
    let home = placement_home();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let copy = folio.copy(&home, &lock);
    let internal = folio.internal();
    #[cfg(unix)]
    let link = link(&mut folio, &home, &lock);
    let old_home = home.path().with_file_name("old-home");
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
            std::fs::canonicalize(link.path.to_path_buf()).unwrap(),
            std::fs::canonicalize(internal.path.to_path_buf()).unwrap()
        );
    }
}

#[test]
fn rehome_refuses_mixed_internal_homes_without_writes() {
    let home = placement_home();
    let lock = home.lock();
    let mut first = TestFolio::new(&home, &lock);
    let second = TestFolio::new(&home, &lock);
    let internal = first.internal();
    let old_home = home.path().with_file_name("old-home");
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
        PlacementPath::resolve(fs.link_target(second.folio.id()))
            .unwrap()
            .to_path_buf()
    );
}

#[test]
fn rehome_after_renaming_and_reopening_the_home() {
    use folt_core::home::Home;
    use folt_core::registry::Registry;
    use folt_core::store::ObjectStore;
    let home = placement_home();
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
fn rehome_repairs_relative_links_after_renaming_the_home() {
    use folt_core::home::Home;
    use folt_core::registry::Registry;
    use folt_core::store::ObjectStore;

    let home = placement_home();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let link = link(&mut folio, &home, &lock);
    let internal = folio.internal();
    let target = std::path::PathBuf::from("home/folios")
        .join(link.id.to_string())
        .join("folio.main");
    std::fs::remove_file(link.path.to_path_buf()).unwrap();
    std::os::unix::fs::symlink(&target, link.path.to_path_buf()).unwrap();
    let fs = PlacementFs::new(&home.home);
    assert_eq!(
        fs.read(link.id, &link.to_placement().1).unwrap(),
        Observation::IntactLink
    );
    drop(folio);
    drop(lock);

    let new_path = home.root().join("moved-home");
    std::fs::rename(home.path(), &new_path).unwrap();
    let moved_home = Home::open_or_create(new_path).unwrap();
    let lock = moved_home.try_lock().unwrap();
    let mut registry = Registry::open_or_create(&moved_home, &lock).unwrap();
    let row = registry.folio_row(link.id).unwrap().unwrap();
    let folio = folt_core::folio_dir::validate(moved_home.path(), &row).unwrap();
    let objects = ObjectStore::open(&folio, &row.format).unwrap();
    let fs = PlacementFs::new(&moved_home);
    let facts = facts(&link, &fs);
    assert!(placement_rules::judge_repair_link(&facts).is_ok());
    assert_eq!(facts.observation, Observation::OtherLink { target });
    for _ in 0..2 {
        placement_rules::rehome(home.path(), &fs, &objects, &mut registry, &lock).unwrap();
        assert_eq!(
            fs.read(link.id, &link.to_placement().1).unwrap(),
            Observation::IntactLink
        );
        assert_eq!(
            registry.internal_placement(link.id).unwrap().unwrap().path,
            PlacementPath::resolve(fs.link_target(link.id)).unwrap()
        );
        assert!(registry.placement_at(&internal.path).unwrap().is_none());
        assert_eq!(
            registry.placement_at(&link.path).unwrap(),
            Some(link.clone())
        );
    }
}

#[cfg(unix)]
#[test]
fn rehome_refuses_deliberate_retargets_before_rewriting_any_row() {
    for (elsewhere, relative) in [(false, false), (true, false), (false, true), (true, true)] {
        let home = placement_home();
        let lock = home.lock();
        let mut folio = TestFolio::new(&home, &lock);
        let link = link(&mut folio, &home, &lock);
        let internal = folio.internal();
        let old_home = home.path().with_file_name("old-home");
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
        let target = if relative {
            target.strip_prefix(home.root()).unwrap().to_path_buf()
        } else {
            target
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

#[cfg(unix)]
fn relocate_relative_link(from: &str, to: &str, relative_prefix: &str) {
    let home = placement_home();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let mut row = link(&mut folio, &home, &lock);
    let source = home.root().join(from);
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::remove_file(row.path.to_path_buf()).unwrap();
    let relative = std::path::Path::new(relative_prefix)
        .join("home/folios")
        .join(row.id.to_string())
        .join("folio.main");
    std::os::unix::fs::symlink(&relative, &source).unwrap();
    let from = PlacementPath::resolve(source).unwrap();
    folio
        .registry
        .write(&lock, |tx| tx.move_placement(&row.path, &from))
        .unwrap();
    row.path = from;
    let fs = PlacementFs::new(&home.home);
    assert_eq!(facts(&row, &fs).observation, Observation::IntactLink);
    let destination = home.root().join(to);
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    let to = PlacementPath::resolve(destination).unwrap();
    let target = placement_rules::gather_relocate_target(&row, &to, &fs, &folio.registry).unwrap();
    let plan = placement_rules::judge_relocate(&facts(&row, &fs), &target).unwrap();
    placement_rules::apply_relocate(&plan, &fs, &mut folio.registry, &lock).unwrap();
    let moved = folio.registry.placement_at(&to).unwrap().unwrap();
    assert_eq!(facts(&moved, &fs).observation, Observation::IntactLink);
    assert!(std::fs::read_link(to.to_path_buf()).unwrap().is_absolute());
    assert!(!row.path.to_path_buf().exists());
    assert!(folio.registry.placement_at(&row.path).unwrap().is_none());
}

#[cfg(unix)]
#[test]
fn relative_link_move_to_deeper_directory_stays_intact() {
    relocate_relative_link("link.txt", "deeper/directory/link", "");
}

#[cfg(unix)]
#[test]
fn relative_link_move_to_shallower_directory_stays_intact() {
    relocate_relative_link("deeper/directory/link", "link.txt", "../../");
}

#[cfg(unix)]
#[test]
fn relative_link_move_to_same_depth_stays_intact() {
    relocate_relative_link("source/link", "destination/link", "../");
}

#[cfg(any(unix, windows))]
fn test_symlink(target: &std::path::Path, path: &std::path::Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, path).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(target, path).unwrap();
}

#[cfg(any(unix, windows))]
#[test]
fn directory_move_preserves_relative_and_absolute_links_and_mixed_kinds() {
    use folt_core::placement::PlacementKind;

    for mixed in [false, true] {
        let home = placement_home();
        let lock = home.lock();
        let mut folio = TestFolio::new(&home, &lock);
        let fs = PlacementFs::new(&home.home);
        let from = home.root().join("source");
        std::fs::create_dir_all(from.join("nested")).unwrap();
        if let Err(error) = fs.probe_link(&from.join("probe")) {
            eprintln!("skipping directory move with links: {error}");
            return;
        }
        let content = folio
            .objects
            .put(&lock, folt_core::object::ObjectKind::Blob, b"initial")
            .unwrap();
        let mut rows = Vec::new();
        for (name, kind) in [
            ("relative", PlacementKind::Link),
            ("copy", PlacementKind::Copy),
            ("nested/absolute", PlacementKind::Link),
        ] {
            if kind == PlacementKind::Copy && !mixed {
                continue;
            }
            let mut row = folio.internal();
            row.path = PlacementPath::resolve(from.join(name)).unwrap();
            row.kind = kind;
            let written = fs
                .create(row.id, &row.to_placement().1, &folio.objects, content)
                .unwrap();
            row.base = written.base;
            if name == "relative" {
                std::fs::remove_file(row.path.to_path_buf()).unwrap();
                let target = std::path::Path::new("../home/folios")
                    .join(row.id.to_string())
                    .join("folio.main");
                test_symlink(&target, &row.path.to_path_buf());
            }
            folio
                .registry
                .write(&lock, |tx| {
                    tx.add_placement(&folt_core::registry::NewPlacement {
                        path: row.path.clone(),
                        id: row.id,
                        kind: row.kind,
                        head: Some(row.head.clone()),
                        commit_id: row.commit_id,
                        base: row.base.clone(),
                        staged: row.staged,
                    })
                })
                .unwrap();
            rows.push(row);
        }
        let mut directory = rows[0].clone();
        directory.path = PlacementPath::resolve_directory(&from).unwrap();
        directory.kind = PlacementKind::Copy;
        directory.base = None;
        let destination = home.root().join("deeper/directory/moved");
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        let to = PlacementPath::resolve_directory(&destination).unwrap();
        let target =
            placement_rules::gather_relocate_target(&directory, &to, &fs, &folio.registry).unwrap();
        let plan = placement_rules::judge_relocate(&facts(&directory, &fs), &target).unwrap();
        placement_rules::apply_relocate(&plan, &fs, &mut folio.registry, &lock).unwrap();
        for old in rows {
            let mut expected = old.clone();
            expected.path = PlacementPath::resolve(
                destination.join(old.path.to_path_buf().strip_prefix(&from).unwrap()),
            )
            .unwrap();
            assert!(folio.registry.placement_at(&old.path).unwrap().is_none());
            assert_eq!(
                folio.registry.placement_at(&expected.path).unwrap(),
                Some(expected.clone())
            );
            assert_eq!(
                std::fs::read(expected.path.to_path_buf()).unwrap(),
                b"initial"
            );
            if expected.kind == PlacementKind::Link {
                assert_eq!(facts(&expected, &fs).observation, Observation::IntactLink);
                assert!(
                    std::fs::read_link(expected.path.to_path_buf())
                        .unwrap()
                        .is_absolute()
                );
            }
        }
        assert!(!from.exists());
    }
}

#[cfg(any(unix, windows))]
#[test]
fn absolute_link_move_does_not_replace_the_link() {
    use folt_core::placement::PlacementKind;

    let home = placement_home();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let fs = PlacementFs::new(&home.home);
    let mut row = folio.internal();
    row.kind = PlacementKind::Link;
    row.base = None;
    row.path = PlacementPath::resolve(home.root().join("link")).unwrap();
    if let Err(error) = fs.probe_link(&row.path.to_path_buf()) {
        eprintln!("skipping absolute link move: {error}");
        return;
    }
    fs.create(
        row.id,
        &row.to_placement().1,
        &folio.objects,
        hash_blob(b"initial"),
    )
    .unwrap();
    folio
        .registry
        .write(&lock, |tx| {
            tx.add_placement(&folt_core::registry::NewPlacement {
                path: row.path.clone(),
                id: row.id,
                kind: row.kind,
                head: Some(row.head.clone()),
                commit_id: row.commit_id,
                base: row.base.clone(),
                staged: row.staged,
            })
        })
        .unwrap();
    let before = std::fs::read_link(row.path.to_path_buf()).unwrap();
    #[cfg(unix)]
    let inode = {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(row.path.to_path_buf())
            .unwrap()
            .ino()
    };
    let destination = home.root().join("deeper/moved");
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    let to = PlacementPath::resolve(destination).unwrap();
    let target = placement_rules::gather_relocate_target(&row, &to, &fs, &folio.registry).unwrap();
    let plan = placement_rules::judge_relocate(&facts(&row, &fs), &target).unwrap();
    placement_rules::apply_relocate(&plan, &fs, &mut folio.registry, &lock).unwrap();
    let moved = folio.registry.placement_at(&to).unwrap().unwrap();
    assert_eq!(facts(&moved, &fs).observation, Observation::IntactLink);
    assert_eq!(std::fs::read_link(to.to_path_buf()).unwrap(), before);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            std::fs::symlink_metadata(to.to_path_buf()).unwrap().ino(),
            inode
        );
    }
}

#[cfg(unix)]
#[test]
fn relocating_other_links_refuses_without_normalizing() {
    let home = placement_home();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let row = link(&mut folio, &home, &lock);
    let fs = PlacementFs::new(&home.home);
    let to = PlacementPath::resolve(home.root().join("moved")).unwrap();
    let target = placement_rules::gather_relocate_target(&row, &to, &fs, &folio.registry).unwrap();
    let plan = placement_rules::judge_relocate(&facts(&row, &fs), &target).unwrap();
    let other = std::path::Path::new("personal-file");
    std::fs::remove_file(row.path.to_path_buf()).unwrap();
    std::os::unix::fs::symlink(other, row.path.to_path_buf()).unwrap();
    assert_eq!(
        facts(&row, &fs).observation,
        Observation::OtherLink {
            target: other.into()
        }
    );
    assert_eq!(
        placement_rules::judge_relocate(&facts(&row, &fs), &target)
            .unwrap_err()
            .reason,
        RefusalReason::LinkReplaced
    );
    assert!(placement_rules::apply_relocate(&plan, &fs, &mut folio.registry, &lock).is_err());
    assert_eq!(std::fs::read_link(row.path.to_path_buf()).unwrap(), other);
    assert_eq!(folio.registry.placement_at(&row.path).unwrap(), Some(row));
    assert!(folio.registry.placement_at(&to).unwrap().is_none());
}
