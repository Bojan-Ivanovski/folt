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

mod conversion {
    use super::*;
    use folt_core::checkout::{self, CheckoutStores};
    use folt_core::home::HomeLock;
    use folt_core::object::{ObjectId, hash_blob};
    use folt_core::placement::{PlacementError, PlacementKind, StagedBlob};
    use folt_core::placement_rules::{self, ConvertPlan, RefusalReason, RulesError};
    use folt_core::refs::RefName;
    use folt_core::registry::Placement;

    fn initial_main(folio: &mut TestFolio, lock: &mut HomeLock) -> ObjectId {
        use folt_core::refs::{Expect, Kind};
        let commit = folio.commit_object(lock, b"initial");
        folio
            .refs
            .update(
                lock,
                Kind::Branch,
                &RefName::parse("main").unwrap(),
                commit,
                Expect::Absent,
            )
            .unwrap();
        let mut internal = folio.internal();
        internal.commit_id = Some(commit);
        folio
            .registry
            .write(lock, |tx| tx.update_placement(&internal))
            .unwrap();
        commit
    }

    fn plan(
        home: &TempHome,
        folio: &TestFolio,
        row: &Placement,
        kind: PlacementKind,
    ) -> ConvertPlan {
        let fs = PlacementFs::new(&home.home);
        let facts = placement_rules::gather_convert(row, &fs, |_| Ok(&folio.refs)).unwrap();
        let internal =
            placement_rules::gather_convert(&folio.internal(), &fs, |_| Ok(&folio.refs)).unwrap();
        let target = checkout::resolve(
            &folio.refs,
            &folio.objects,
            &RefName::parse("main").unwrap(),
            16384,
        )
        .unwrap();
        placement_rules::judge_convert(&facts, kind, Some(&target), Some(&internal)).unwrap()
    }

    fn apply(
        home: &TempHome,
        lock: &HomeLock,
        folio: &mut TestFolio,
        row: &Placement,
        kind: PlacementKind,
    ) -> Placement {
        let plan = plan(home, folio, row, kind);
        placement_rules::apply_convert(
            &plan,
            &PlacementFs::new(&home.home),
            &folio.objects,
            &mut folio.registry,
            lock,
        )
        .unwrap();
        folio.registry.placement_at(&row.path).unwrap().unwrap()
    }

    fn link(home: &TempHome, lock: &HomeLock, folio: &mut TestFolio, row: &Placement) -> Placement {
        let mut row = row.clone();
        std::fs::remove_file(row.path.to_path_buf()).unwrap();
        row.kind = PlacementKind::Link;
        row.base = None;
        row.commit_id = None;
        PlacementFs::new(&home.home)
            .create(
                row.id,
                &row.to_placement().1,
                &folio.objects,
                hash_blob(b"initial"),
            )
            .unwrap();
        folio
            .registry
            .write(lock, |tx| tx.update_placement(&row))
            .unwrap();
        row
    }

    fn assert_no_temporary(home: &TempHome) {
        for entry in std::fs::read_dir(home.root()).unwrap() {
            assert!(
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".folt-")
            );
        }
    }

    #[test]
    fn converts_link_to_copy_of_main_with_mode() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;
        let home = TempHome::new();
        let mut lock = home.lock();
        let mut folio = TestFolio::new(&home, &lock);
        let tip = folio.commit_internal(&home, &mut lock, b"main content");
        let copy = folio.copy(&home, &lock);
        let row = link(&home, &lock, &mut folio, &copy);
        let internal = folio.internal();
        #[cfg(unix)]
        std::fs::set_permissions(
            internal.path.to_path_buf(),
            std::fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        let updated = apply(&home, &lock, &mut folio, &row, PlacementKind::Copy);
        assert_eq!(updated.kind, PlacementKind::Copy);
        assert_eq!(updated.head, "main");
        assert_eq!(updated.commit_id, Some(tip));
        assert_eq!(
            updated.base.as_ref().unwrap().hash,
            hash_blob(b"main content")
        );
        assert_eq!(updated.staged, None);
        assert!(
            !std::fs::symlink_metadata(updated.path.to_path_buf())
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read(updated.path.to_path_buf()).unwrap(),
            b"main content"
        );
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(updated.path.to_path_buf())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert_eq!(folio.internal(), internal);
        assert_no_temporary(&home);
    }

    #[test]
    fn converts_clean_and_behind_copies_to_links() {
        for behind in [false, true] {
            let home = TempHome::new();
            let mut lock = home.lock();
            let mut folio = TestFolio::new(&home, &lock);
            let first = initial_main(&mut folio, &mut lock);
            let mut row = folio.copy(&home, &lock);
            row.commit_id = Some(first);
            folio
                .registry
                .write(&lock, |tx| tx.update_placement(&row))
                .unwrap();
            if behind {
                folio.commit_internal(&home, &mut lock, b"new main");
            }
            let updated = apply(&home, &lock, &mut folio, &row, PlacementKind::Link);
            assert_eq!(updated.kind, PlacementKind::Link);
            assert_eq!(updated.head, "main");
            assert_eq!(updated.commit_id, None);
            assert_eq!(updated.base, None);
            assert_eq!(updated.staged, None);
            assert_eq!(
                std::fs::read_link(updated.path.to_path_buf()).unwrap(),
                PlacementFs::new(&home.home).link_target(row.id)
            );
            assert_eq!(
                std::fs::read(updated.path.to_path_buf()).unwrap(),
                if behind {
                    b"new main".as_slice()
                } else {
                    b"initial".as_slice()
                }
            );
            assert_no_temporary(&home);
        }
    }

    #[test]
    fn resumes_both_directions_without_replacing_entry_again() {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        for kind in [PlacementKind::Link, PlacementKind::Copy] {
            let home = TempHome::new();
            let mut lock = home.lock();
            let mut folio = TestFolio::new(&home, &lock);
            initial_main(&mut folio, &mut lock);
            let mut row = folio.copy(&home, &lock);
            if kind == PlacementKind::Copy {
                row = link(&home, &lock, &mut folio, &row);
            }
            let fs = PlacementFs::new(&home.home);
            let observation = fs.read(row.id, &row.to_placement().1).unwrap();
            fs.convert(
                row.id,
                &row.to_placement().1,
                kind,
                &folio.objects,
                hash_blob(b"initial"),
                &observation,
            )
            .unwrap();
            assert_eq!(
                folio.registry.placement_at(&row.path).unwrap().unwrap(),
                row
            );
            #[cfg(unix)]
            let before = std::fs::symlink_metadata(row.path.to_path_buf()).unwrap();
            let updated = apply(&home, &lock, &mut folio, &row, kind);
            #[cfg(unix)]
            {
                let after = std::fs::symlink_metadata(row.path.to_path_buf()).unwrap();
                assert_eq!(before.ino(), after.ino());
            }
            assert_eq!(std::fs::read(row.path.to_path_buf()).unwrap(), b"initial");
            assert_eq!(updated.kind, kind);
            if kind == PlacementKind::Copy {
                assert_eq!(updated.base.unwrap().hash, hash_blob(b"initial"));
            }
            assert_no_temporary(&home);
        }
    }

    #[test]
    fn resumes_link_to_copy_with_unpublished_temporary() {
        let home = TempHome::new();
        let mut lock = home.lock();
        let mut folio = TestFolio::new(&home, &lock);
        initial_main(&mut folio, &mut lock);
        let row = folio.copy(&home, &lock);
        let row = link(&home, &lock, &mut folio, &row);
        // Simulate stopping after preparation, before the atomic rename.
        let temporary = home.root().join(".folt-convert-interrupted");
        std::fs::write(&temporary, b"initial").unwrap();
        let fs = PlacementFs::new(&home.home);
        assert_eq!(
            fs.read(row.id, &row.to_placement().1).unwrap(),
            Observation::IntactLink
        );
        let updated = apply(&home, &lock, &mut folio, &row, PlacementKind::Copy);
        assert_eq!(updated.kind, PlacementKind::Copy);
        assert_eq!(std::fs::read(row.path.to_path_buf()).unwrap(), b"initial");
        std::fs::remove_file(temporary).unwrap();
        assert_no_temporary(&home);
    }

    #[cfg(windows)]
    #[test]
    fn replaces_windows_link_with_open_delete_shared_handle() {
        use std::os::windows::fs::OpenOptionsExt;

        let home = TempHome::new();
        let mut lock = home.lock();
        let mut folio = TestFolio::new(&home, &lock);
        initial_main(&mut folio, &mut lock);
        let row = folio.copy(&home, &lock);
        let row = link(&home, &lock, &mut folio, &row);
        // Unlinking first would leave this entry delete-pending while the
        // handle is open, preventing the subsequent rename from publishing.
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1 | 0x2 | 0x4) // READ | WRITE | DELETE
            .custom_flags(0x0020_0000) // FILE_FLAG_OPEN_REPARSE_POINT
            .open(row.path.to_path_buf())
            .unwrap();
        let fs = PlacementFs::new(&home.home);
        fs.convert(
            row.id,
            &row.to_placement().1,
            PlacementKind::Copy,
            &folio.objects,
            hash_blob(b"initial"),
            &Observation::IntactLink,
        )
        .unwrap();
        assert!(matches!(
            fs.read(row.id, &row.to_placement().1).unwrap(),
            Observation::File { .. }
        ));
        let updated = apply(&home, &lock, &mut folio, &row, PlacementKind::Copy);
        assert_eq!(updated.kind, PlacementKind::Copy);
        assert_eq!(std::fs::read(row.path.to_path_buf()).unwrap(), b"initial");
        drop(held);
        assert_no_temporary(&home);
    }

    #[cfg(windows)]
    #[test]
    fn failed_windows_replacement_preserves_link_and_can_retry() {
        use std::os::windows::fs::OpenOptionsExt;

        let home = TempHome::new();
        let mut lock = home.lock();
        let mut folio = TestFolio::new(&home, &lock);
        initial_main(&mut folio, &mut lock);
        let row = folio.copy(&home, &lock);
        let row = link(&home, &lock, &mut folio, &row);
        // Open the link itself, allowing reads/writes but denying deletion.
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1 | 0x2) // FILE_SHARE_READ | FILE_SHARE_WRITE
            .custom_flags(0x0020_0000) // FILE_FLAG_OPEN_REPARSE_POINT
            .open(row.path.to_path_buf())
            .unwrap();
        let fs = PlacementFs::new(&home.home);
        assert!(matches!(
            fs.convert(
                row.id,
                &row.to_placement().1,
                PlacementKind::Copy,
                &folio.objects,
                hash_blob(b"initial"),
                &Observation::IntactLink,
            ),
            Err(PlacementError::OperationFailed { .. })
        ));
        assert_eq!(
            fs.read(row.id, &row.to_placement().1).unwrap(),
            Observation::IntactLink
        );
        assert_eq!(
            folio.registry.placement_at(&row.path).unwrap().unwrap(),
            row
        );
        assert_no_temporary(&home);
        drop(held);
        let updated = apply(&home, &lock, &mut folio, &row, PlacementKind::Copy);
        assert_eq!(updated.kind, PlacementKind::Copy);
        assert_eq!(std::fs::read(row.path.to_path_buf()).unwrap(), b"initial");
        assert_no_temporary(&home);
    }

    #[test]
    fn refuses_every_unsafe_state_without_changing_the_row() {
        for case in 0..10 {
            let home = TempHome::new();
            let mut lock = home.lock();
            let mut folio = TestFolio::new(&home, &lock);
            initial_main(&mut folio, &mut lock);
            let mut row = folio.copy(&home, &lock);
            let mut kind = PlacementKind::Link;
            let reason = match case {
                0 => {
                    row = folio.internal();
                    RefusalReason::Internal
                }
                1 => {
                    row.head = "topic".into();
                    RefusalReason::CopyBranch
                }
                2 => {
                    std::fs::remove_file(row.path.to_path_buf()).unwrap();
                    RefusalReason::ConvertGone
                }
                3 => {
                    std::fs::remove_file(row.path.to_path_buf()).unwrap();
                    std::fs::create_dir(row.path.to_path_buf()).unwrap();
                    RefusalReason::NotAFile
                }
                4 => {
                    std::fs::remove_file(row.path.to_path_buf()).unwrap();
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(home.root().join("other"), row.path.to_path_buf())
                        .unwrap();
                    #[cfg(windows)]
                    std::os::windows::fs::symlink_file(
                        home.root().join("other"),
                        row.path.to_path_buf(),
                    )
                    .unwrap();
                    RefusalReason::OtherLink
                }
                5 => {
                    std::fs::write(row.path.to_path_buf(), b"changed").unwrap();
                    RefusalReason::Edited
                }
                6 => {
                    row.staged = Some(StagedBlob {
                        hash: hash_blob(b"initial"),
                        size: 7,
                    });
                    RefusalReason::Staged
                }
                7 => {
                    row = link(&home, &lock, &mut folio, &row);
                    kind = PlacementKind::Copy;
                    std::fs::write(folio.internal().path.to_path_buf(), b"changed").unwrap();
                    RefusalReason::InternalEdited
                }
                8 => {
                    row = link(&home, &lock, &mut folio, &row);
                    kind = PlacementKind::Copy;
                    std::fs::remove_file(row.path.to_path_buf()).unwrap();
                    std::fs::write(row.path.to_path_buf(), b"changed").unwrap();
                    RefusalReason::Edited
                }
                _ => {
                    kind = PlacementKind::Copy;
                    RefusalReason::SameKind
                }
            };
            folio
                .registry
                .write(&lock, |tx| tx.update_placement(&row))
                .unwrap();
            let fs = PlacementFs::new(&home.home);
            // Non-main copies are refused before their branch's tip matters.
            let facts = placement_rules::gather_convert(&row, &fs, |_| Ok(&folio.refs)).unwrap();
            let internal =
                placement_rules::gather_convert(&folio.internal(), &fs, |_| Ok(&folio.refs))
                    .unwrap();
            let target = checkout::resolve(
                &folio.refs,
                &folio.objects,
                &RefName::parse("main").unwrap(),
                16384,
            )
            .unwrap();
            assert_eq!(
                placement_rules::judge_convert(&facts, kind, Some(&target), Some(&internal))
                    .unwrap_err()
                    .reason,
                reason,
                "case {case}"
            );
            assert_eq!(
                folio.registry.placement_at(&row.path).unwrap().unwrap(),
                row
            );
            assert_no_temporary(&home);
        }
    }

    #[test]
    fn discard_checkout_allows_conversion_after_copy_or_link_side_edits() {
        for link_side in [false, true] {
            let home = TempHome::new();
            let mut lock = home.lock();
            let mut folio = TestFolio::new(&home, &lock);
            initial_main(&mut folio, &mut lock);
            let mut row = folio.copy(&home, &lock);
            if link_side {
                row = link(&home, &lock, &mut folio, &row);
            }
            std::fs::write(row.path.to_path_buf(), b"changed").unwrap();
            let discard_row = if link_side {
                folio.internal()
            } else {
                row.clone()
            };
            let stores = CheckoutStores {
                objects: &folio.objects,
                refs: &folio.refs,
                object_limit: 16384,
            };
            checkout::checkout_placement(
                &discard_row,
                &RefName::parse("main").unwrap(),
                true,
                &PlacementFs::new(&home.home),
                &stores,
                &mut folio.registry,
                &lock,
            )
            .unwrap();
            let row = folio.registry.placement_at(&row.path).unwrap().unwrap();
            let kind = if link_side {
                PlacementKind::Copy
            } else {
                PlacementKind::Link
            };
            apply(&home, &lock, &mut folio, &row, kind);
            assert_eq!(std::fs::read(row.path.to_path_buf()).unwrap(), b"initial");
        }
    }

    #[test]
    fn rechecks_registry_and_both_files_before_conversion() {
        for case in 0..4 {
            let home = TempHome::new();
            let mut lock = home.lock();
            let mut folio = TestFolio::new(&home, &lock);
            initial_main(&mut folio, &mut lock);
            let mut row = folio.copy(&home, &lock);
            if case >= 2 {
                row = link(&home, &lock, &mut folio, &row);
            }
            let kind = if case >= 2 {
                PlacementKind::Copy
            } else {
                PlacementKind::Link
            };
            let plan = plan(&home, &folio, &row, kind);
            let reason = match case {
                0 => {
                    let mut changed = row.clone();
                    changed.head = "topic".into();
                    folio
                        .registry
                        .write(&lock, |tx| tx.update_placement(&changed))
                        .unwrap();
                    RefusalReason::RegistryChanged
                }
                1 | 2 => {
                    std::fs::write(row.path.to_path_buf(), b"changed").unwrap();
                    RefusalReason::PlacementChanged
                }
                _ => {
                    let mut internal = folio.internal();
                    internal.base = None;
                    folio
                        .registry
                        .write(&lock, |tx| tx.update_placement(&internal))
                        .unwrap();
                    RefusalReason::RegistryChanged
                }
            };
            let result = placement_rules::apply_convert(
                &plan,
                &PlacementFs::new(&home.home),
                &folio.objects,
                &mut folio.registry,
                &lock,
            );
            assert!(
                matches!(result, Err(RulesError::Refused(refusal)) if refusal.reason == reason)
            );
            assert_no_temporary(&home);
        }
    }

    #[test]
    fn failed_conversion_cleans_temporary_and_preserves_entry() {
        let home = TempHome::new();
        let mut lock = home.lock();
        let mut folio = TestFolio::new(&home, &lock);
        initial_main(&mut folio, &mut lock);
        let row = folio.copy(&home, &lock);
        let fs = PlacementFs::new(&home.home);
        let (_, placement) = row.to_placement();
        let expected = fs.read(row.id, &placement).unwrap();
        std::fs::write(&placement.path, b"changed").unwrap();
        assert!(matches!(
            fs.convert(
                row.id,
                &placement,
                PlacementKind::Link,
                &folio.objects,
                hash_blob(b"initial"),
                &expected
            ),
            Err(PlacementError::PlacementChanged)
        ));
        assert_eq!(std::fs::read(&placement.path).unwrap(), b"changed");
        assert_no_temporary(&home);
        let row = link(&home, &lock, &mut folio, &row);
        let (_, placement) = row.to_placement();
        let expected = fs.read(row.id, &placement).unwrap();
        assert!(
            fs.convert(
                row.id,
                &placement,
                PlacementKind::Copy,
                &folio.objects,
                ObjectId::from_bytes([0; 32]),
                &expected
            )
            .is_err()
        );
        assert_eq!(
            fs.read(row.id, &placement).unwrap(),
            Observation::IntactLink
        );
        assert_eq!(std::fs::read(&placement.path).unwrap(), b"initial");
        assert_no_temporary(&home);
        std::fs::remove_file(&placement.path).unwrap();
        std::fs::write(&placement.path, b"changed").unwrap();
        assert!(matches!(
            fs.convert(
                row.id,
                &placement,
                PlacementKind::Copy,
                &folio.objects,
                hash_blob(b"initial"),
                &expected
            ),
            Err(PlacementError::PlacementChanged)
        ));
        assert_eq!(std::fs::read(&placement.path).unwrap(), b"changed");
        assert_no_temporary(&home);
    }
}
