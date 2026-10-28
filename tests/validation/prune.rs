use folt_core::home::HomeLock;
use folt_core::object::hash_blob;
#[cfg(unix)]
use folt_core::placement::PlacementKind;
use folt_core::placement::StagedBlob;
use folt_core::placement_fs::PlacementFs;
use folt_core::prune::{self, PruneError, PruneOptions};
use folt_core::registry::{NewPlacement, Placement, PlacementPath};

use crate::common::TempHome;
use crate::support::TestFolio;

fn applying() -> PruneOptions {
    PruneOptions {
        apply: true,
        confirm_mass_deletion: false,
    }
}

fn new_row(row: &Placement) -> NewPlacement {
    NewPlacement {
        path: row.path.clone(),
        id: row.id,
        kind: row.kind,
        head: Some(row.head.clone()),
        commit_id: row.commit_id,
        base: row.base.clone(),
        staged: row.staged,
    }
}

fn twin(folio: &mut TestFolio, lock: &HomeLock, row: &Placement, path: PlacementPath) -> Placement {
    let mut twin = row.clone();
    twin.path = path;
    folio
        .registry
        .write(lock, |tx| tx.add_placement(&new_row(&twin)))
        .unwrap();
    twin
}

#[test]
fn gone_copies_drop_regardless_of_edits_or_stage_and_preserve_objects() {
    for edited in [false, true] {
        for staged in [false, true] {
            for has_twin in [false, true] {
                let home = TempHome::new();
                let lock = home.lock();
                let mut folio = TestFolio::new(&home, &lock);
                let mut row = folio.copy(&home, &lock);
                if edited {
                    std::fs::write(row.path.to_path_buf(), b"edited").unwrap();
                }
                let blob = folio
                    .objects
                    .put(&lock, folt_core::object::ObjectKind::Blob, b"stage")
                    .unwrap();
                if staged {
                    row.staged = Some(StagedBlob {
                        hash: blob,
                        size: 5,
                    });
                    folio
                        .registry
                        .write(&lock, |tx| tx.update_placement(&row))
                        .unwrap();
                }
                if has_twin {
                    let path = home.root().join("twin");
                    std::fs::copy(row.path.to_path_buf(), &path).unwrap();
                    twin(
                        &mut folio,
                        &lock,
                        &row,
                        PlacementPath::resolve(path).unwrap(),
                    );
                }
                std::fs::remove_file(row.path.to_path_buf()).unwrap();
                let fs = PlacementFs::new(&home.home);
                let dry = prune::prune_placements(
                    &mut folio.registry,
                    &fs,
                    &lock,
                    &[],
                    PruneOptions::default(),
                )
                .unwrap();
                assert!(dry.dry_run);
                assert!(folio.registry.placement_at(&row.path).unwrap().is_some());
                let mut applied =
                    prune::prune_placements(&mut folio.registry, &fs, &lock, &[], applying())
                        .unwrap();
                assert!(!applied.dry_run);
                assert_eq!(applied.dropped.len(), 1);
                assert_eq!(
                    applied.unreachable_staged_blobs,
                    usize::from(staged && !has_twin)
                );
                assert_eq!(applied.dropped[0].stage_unreachable, staged && !has_twin);
                assert!(
                    applied
                        .warnings
                        .iter()
                        .any(|warning| warning.contains("elsewhere"))
                );
                applied.dry_run = true;
                assert_eq!(dry, applied);
                assert!(folio.registry.folio_exists(row.id).unwrap());
                assert_eq!(
                    folio
                        .objects
                        .get(blob, folt_core::object::ObjectKind::Blob, 1024)
                        .unwrap(),
                    b"stage"
                );
                assert!(
                    prune::prune_placements(&mut folio.registry, &fs, &lock, &[], applying())
                        .unwrap()
                        .dropped
                        .is_empty()
                );
            }
        }
    }
}

#[test]
fn live_edited_copies_and_unusable_paths_are_kept() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let row = folio.copy(&home, &lock);
    let fs = PlacementFs::new(&home.home);
    std::fs::write(row.path.to_path_buf(), b"different").unwrap();
    let result = prune::prune_placements(&mut folio.registry, &fs, &lock, &[], applying()).unwrap();
    assert_eq!(
        result
            .kept
            .iter()
            .find(|kept| kept.path == row.path)
            .unwrap()
            .reason,
        "edited or different"
    );
    assert_eq!(std::fs::read(row.path.to_path_buf()).unwrap(), b"different");
    std::fs::remove_file(row.path.to_path_buf()).unwrap();
    std::fs::create_dir(row.path.to_path_buf()).unwrap();
    let result = prune::prune_placements(&mut folio.registry, &fs, &lock, &[], applying()).unwrap();
    assert_eq!(
        result
            .kept
            .iter()
            .find(|kept| kept.path == row.path)
            .unwrap()
            .reason,
        "unusable entry"
    );
    assert!(result.dropped.is_empty());
}

#[test]
fn missing_parent_and_internal_rows_are_preserved() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let row = folio.copy(&home, &lock);
    let parent = home.root().join("offline");
    std::fs::create_dir(&parent).unwrap();
    let key = PlacementPath::resolve(parent.join("copy")).unwrap();
    let offline = twin(&mut folio, &lock, &row, key);
    std::fs::remove_dir(&parent).unwrap();
    let internal = folio.internal();
    std::fs::remove_file(internal.path.to_path_buf()).unwrap();
    let fs = PlacementFs::new(&home.home);
    let result = prune::prune_placements(&mut folio.registry, &fs, &lock, &[], applying()).unwrap();
    assert!(result.dropped.is_empty());
    assert_eq!(
        result
            .kept
            .iter()
            .find(|kept| kept.path == offline.path)
            .unwrap()
            .reason,
        "parent directory missing"
    );
    assert_eq!(folio.internal(), internal);
}

#[test]
fn mass_deletion_requires_confirmation_and_counts_every_scoped_row() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let row = folio.copy(&home, &lock);
    for index in 0..4 {
        twin(
            &mut folio,
            &lock,
            &row,
            PlacementPath::resolve(home.root().join(format!("gone-{index}"))).unwrap(),
        );
    }
    std::fs::remove_file(row.path.to_path_buf()).unwrap();
    let before = folio.registry.all_placements().unwrap();
    let fs = PlacementFs::new(&home.home);
    let dry = prune::prune_placements(
        &mut folio.registry,
        &fs,
        &lock,
        &[],
        PruneOptions::default(),
    )
    .unwrap();
    assert_eq!(dry.counts.in_scope, 6);
    assert_eq!(dry.counts.dropped, 5);
    let error =
        prune::prune_placements(&mut folio.registry, &fs, &lock, &[], applying()).unwrap_err();
    assert!(matches!(
        error,
        PruneError::ConfirmationRequired {
            dropped: 5,
            in_scope: 6
        }
    ));
    assert!(error.to_string().contains("5 of 6"));
    assert_eq!(folio.registry.all_placements().unwrap(), before);
    let mut options = applying();
    options.confirm_mass_deletion = true;
    let mut result =
        prune::prune_placements(&mut folio.registry, &fs, &lock, &[], options).unwrap();
    result.dry_run = true;
    assert_eq!(dry, result);
    assert_eq!(folio.registry.all_placements().unwrap(), [folio.internal()]);
}

#[test]
fn scoped_prune_includes_nested_directory_markers_but_not_similar_prefixes() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let row = folio.copy(&home, &lock);
    let old = home.root().join("old");
    std::fs::create_dir_all(old.join("nested")).unwrap();
    std::fs::create_dir(home.root().join("old-notes")).unwrap();
    let nested = twin(
        &mut folio,
        &lock,
        &row,
        PlacementPath::resolve(old.join("nested/__folio__.md")).unwrap(),
    );
    let sibling = twin(
        &mut folio,
        &lock,
        &row,
        PlacementPath::resolve(home.root().join("old-notes/file")).unwrap(),
    );
    let fs = PlacementFs::new(&home.home);
    let scope = [PlacementPath::resolve_directory(&old).unwrap()];
    let result =
        prune::prune_placements(&mut folio.registry, &fs, &lock, &scope, applying()).unwrap();
    assert_eq!(result.counts.in_scope, 1);
    assert_eq!(result.dropped[0].path, nested.path);
    assert_eq!(
        folio.registry.placement_at(&sibling.path).unwrap(),
        Some(sibling)
    );
    std::fs::remove_file(row.path.to_path_buf()).unwrap();
    let result = prune::prune_placements(
        &mut folio.registry,
        &fs,
        &lock,
        std::slice::from_ref(&row.path),
        applying(),
    )
    .unwrap();
    assert_eq!(result.dropped[0].path, row.path);
}

#[test]
fn file_reappearing_or_row_changing_after_gather_is_kept() {
    for changed_row in [false, true] {
        let home = TempHome::new();
        let lock = home.lock();
        let mut folio = TestFolio::new(&home, &lock);
        let mut row = folio.copy(&home, &lock);
        std::fs::remove_file(row.path.to_path_buf()).unwrap();
        let fs = PlacementFs::new(&home.home);
        let plan = prune::gather(&folio.registry, &fs, &[]).unwrap();
        assert_eq!(plan.report().dropped.len(), 1);
        if changed_row {
            row.head = "changed".into();
            folio
                .registry
                .write(&lock, |tx| tx.update_placement(&row))
                .unwrap();
        } else {
            std::fs::write(row.path.to_path_buf(), b"reappeared").unwrap();
        }
        let result = prune::apply(&plan, &fs, &mut folio.registry, &lock, false).unwrap();
        assert!(result.dropped.is_empty());
        assert_eq!(
            folio.registry.placement_at(&row.path).unwrap(),
            Some(row.clone())
        );
        if changed_row {
            assert!(
                result
                    .kept
                    .iter()
                    .any(|kept| kept.reason.contains("registry changed"))
            );
        } else {
            assert_eq!(
                std::fs::read(row.path.to_path_buf()).unwrap(),
                b"reappeared"
            );
        }
    }
}

#[test]
fn interrupted_move_row_sets_prune_to_one_identical_surviving_set() {
    for renamed in [false, true] {
        let home = TempHome::new();
        let lock = home.lock();
        let mut folio = TestFolio::new(&home, &lock);
        let mut row = folio.copy(&home, &lock);
        row.staged = Some(StagedBlob {
            hash: hash_blob(b"stage"),
            size: 5,
        });
        folio
            .registry
            .write(&lock, |tx| tx.update_placement(&row))
            .unwrap();
        let destination = twin(
            &mut folio,
            &lock,
            &row,
            PlacementPath::resolve(home.root().join("destination")).unwrap(),
        );
        if renamed {
            std::fs::rename(row.path.to_path_buf(), destination.path.to_path_buf()).unwrap();
        }
        let fs = PlacementFs::new(&home.home);
        let result =
            prune::prune_placements(&mut folio.registry, &fs, &lock, &[], applying()).unwrap();
        assert_eq!(result.dropped.len(), 1);
        assert_eq!(result.unreachable_staged_blobs, 0);
        let (gone, live) = if renamed {
            (row, destination)
        } else {
            (destination, row)
        };
        assert!(folio.registry.placement_at(&gone.path).unwrap().is_none());
        assert_eq!(
            folio.registry.placement_at(&live.path).unwrap(),
            Some(live.clone())
        );
        assert_eq!(std::fs::read(live.path.to_path_buf()).unwrap(), b"initial");
    }
}

#[cfg(unix)]
#[test]
fn deleted_links_drop_and_mismatching_files_or_links_are_kept() {
    for replacement in [0, 1, 2] {
        let home = TempHome::new();
        let lock = home.lock();
        let mut folio = TestFolio::new(&home, &lock);
        let mut row = folio.internal();
        row.kind = PlacementKind::Link;
        row.path = PlacementPath::resolve(home.root().join("link")).unwrap();
        row.base = None;
        folio
            .registry
            .write(&lock, |tx| tx.add_placement(&new_row(&row)))
            .unwrap();
        std::os::unix::fs::symlink(folio.internal().path.to_path_buf(), row.path.to_path_buf())
            .unwrap();
        std::fs::remove_file(row.path.to_path_buf()).unwrap();
        if replacement == 1 {
            std::fs::write(row.path.to_path_buf(), b"foreign").unwrap();
        }
        if replacement == 2 {
            std::os::unix::fs::symlink("foreign", row.path.to_path_buf()).unwrap();
        }
        let fs = PlacementFs::new(&home.home);
        let result =
            prune::prune_placements(&mut folio.registry, &fs, &lock, &[], applying()).unwrap();
        assert_eq!(result.dropped.len(), usize::from(replacement == 0));
        if replacement != 0 {
            assert_eq!(
                result
                    .kept
                    .iter()
                    .find(|kept| kept.path == row.path)
                    .unwrap()
                    .reason,
                "a different file than expected"
            );
            assert!(std::fs::symlink_metadata(row.path.to_path_buf()).is_ok());
        }
    }
}

#[test]
fn a_parent_replaced_by_a_file_reports_a_read_error_and_keeps_the_row() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let row = folio.copy(&home, &lock);
    let parent = home.root().join("parent");
    std::fs::create_dir(&parent).unwrap();
    let nested = twin(
        &mut folio,
        &lock,
        &row,
        PlacementPath::resolve(parent.join("child")).unwrap(),
    );
    std::fs::remove_dir(&parent).unwrap();
    std::fs::write(&parent, b"a file").unwrap();
    let fs = PlacementFs::new(&home.home);
    let result = prune::prune_placements(&mut folio.registry, &fs, &lock, &[], applying()).unwrap();
    assert!(result.dropped.is_empty());
    let reason = &result
        .kept
        .iter()
        .find(|kept| kept.path == nested.path)
        .unwrap()
        .reason;
    assert!(reason.contains("read error"), "{reason}");
    assert_eq!(std::fs::read(parent).unwrap(), b"a file");
}
