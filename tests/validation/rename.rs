use std::fs;

use folt_core::folio_dir::FolioId;
use folt_core::folio_name::FolioName;
use folt_core::front_matter;
use folt_core::history::{self, HistoryStores};
use folt_core::home::HomeLock;
use folt_core::object::{Commit, ObjectId, hash_blob};
use folt_core::placement::{PlacementKind, PlacementOps, StagedBlob};
use folt_core::placement_fs::PlacementFs;
use folt_core::refs::{Expect, Kind, RefName};
use folt_core::registry::{NewFolio, NewPlacement, Placement, PlacementPath, RegistryError};
use folt_core::rename::{self, RenameError};
use folt_core::working_state;

use crate::common::TempHome;
use crate::support::{TestFolio, metadata};

const OLD: &[u8] = b"---\r\nfolio-name: 'old' # keep\r\ntitle: keep\r\n---\r\nbody\n";
const NEW: &[u8] = b"---\r\nfolio-name: \"new\" # keep\r\ntitle: keep\r\n---\r\nbody\n";

fn name(value: &str) -> FolioName {
    FolioName::parse(value).unwrap()
}

fn setup(home: &TempHome, lock: &mut HomeLock, bytes: &[u8]) -> TestFolio {
    let mut folio = TestFolio::new(home, lock);
    folio.commit_internal(home, lock, bytes);
    folio
        .registry
        .write(lock, |tx| tx.set_name(folio.folio.id(), &name("old")))
        .unwrap();
    folio
}

fn run(
    home: &TempHome,
    folio: &mut TestFolio,
    lock: &mut HomeLock,
) -> Result<Option<ObjectId>, RenameError> {
    let stores = HistoryStores {
        objects: &folio.objects,
        refs: &folio.refs,
        object_limit: 16384,
    };
    let fs = PlacementFs::new(&home.home);
    let plan = rename::plan_rename(&name("old"), &name("new"), &fs, &stores, &folio.registry)?;
    rename::apply_rename(
        &plan,
        metadata(),
        &fs,
        &stores,
        &mut folio.registry,
        lock,
        &mut |_, _| Ok(false),
    )
}

fn copy(home: &TempHome, folio: &mut TestFolio, lock: &HomeLock, head: &str) -> Placement {
    let internal = folio.internal();
    let path = home.root().join(format!("copy-{head}.md"));
    let placement = folt_core::placement::Placement {
        path: path.clone(),
        kind: PlacementKind::Copy,
        head: head.into(),
        commit_id: internal.commit_id,
        base: None,
    };
    let written = PlacementFs::new(&home.home)
        .create(
            folio.folio.id(),
            &placement,
            &folio.objects,
            internal.base.unwrap().hash,
        )
        .unwrap();
    let row = NewPlacement {
        path: PlacementPath::resolve(path).unwrap(),
        id: folio.folio.id(),
        kind: PlacementKind::Copy,
        head: Some(head.into()),
        commit_id: internal.commit_id,
        base: written.base,
        staged: None,
    };
    folio
        .registry
        .write(lock, |tx| tx.add_placement(&row))
        .unwrap();
    folio.registry.placement_at(&row.path).unwrap().unwrap()
}

fn assert_completed(folio: &TestFolio, before: ObjectId, after: ObjectId) {
    assert_eq!(folio.registry.find_by_name(&name("old")).unwrap(), None);
    assert_eq!(
        folio.registry.find_by_name(&name("new")).unwrap(),
        Some(folio.folio.id())
    );
    assert_eq!(fs::read(folio.internal().path.to_path_buf()).unwrap(), NEW);
    let commit = folio.objects.get_commit(after, 16384).unwrap();
    assert_eq!(commit.parents, vec![before]);
    assert_eq!(commit.message, "Rename `old` to `new`");
    assert_eq!(commit.author, metadata().author);
    assert_eq!(commit.time, metadata().time);
    assert_eq!(commit.content, hash_blob(NEW));
    assert_eq!(folio.internal().commit_id, Some(after));
    assert_eq!(folio.internal().base.unwrap().hash, hash_blob(NEW));
    assert_eq!(folio.internal().staged, None);
    assert_eq!(
        folio
            .refs
            .read(Kind::Branch, &RefName::parse("main").unwrap())
            .unwrap(),
        Some(after)
    );
}

#[test]
fn renames_only_main_internal_and_creates_one_commit() {
    let home = TempHome::new();
    let mut lock = home.lock();
    let mut folio = setup(&home, &mut lock, OLD);
    let before = folio.internal().commit_id.unwrap();
    let main_copy = copy(&home, &mut folio, &lock, "main");
    let feature = copy(&home, &mut folio, &lock, "feature");
    history::create_branch(
        folio.folio.id(),
        &RefName::parse("feature").unwrap(),
        before,
        &folio.refs,
        &mut folio.registry,
        &mut lock,
    )
    .unwrap();
    let after = run(&home, &mut folio, &mut lock).unwrap().unwrap();
    assert_completed(&folio, before, after);
    for row in [main_copy, feature] {
        assert_eq!(fs::read(row.path.to_path_buf()).unwrap(), OLD);
        assert_eq!(folio.registry.placement_at(&row.path).unwrap(), Some(row));
    }
    assert_eq!(
        folio
            .refs
            .read(Kind::Branch, &RefName::parse("feature").unwrap())
            .unwrap(),
        Some(before)
    );
    assert!(matches!(
        run(&home, &mut folio, &mut lock),
        Err(RenameError::UnknownName(_))
    ));
    assert_eq!(folio.internal().commit_id, Some(after));
}

#[test]
fn non_markdown_updates_only_registry_including_binary_content() {
    for bytes in [
        b"body".as_slice(),
        b"binary\0data",
        b"---\ntitle: no name\n---\nbody",
    ] {
        let home = TempHome::new();
        let mut lock = home.lock();
        let mut folio = setup(&home, &mut lock, bytes);
        let before = folio.internal();
        assert_eq!(run(&home, &mut folio, &mut lock).unwrap(), None);
        assert_eq!(
            folio.registry.folio_name(folio.folio.id()).unwrap(),
            Some(name("new"))
        );
        assert_eq!(folio.internal(), before);
        assert_eq!(fs::read(before.path.to_path_buf()).unwrap(), bytes);
    }
}

#[test]
fn internal_edits_and_stages_refuse_before_registry_change() {
    for stage in [false, true] {
        let home = TempHome::new();
        let mut lock = home.lock();
        let mut folio = setup(&home, &mut lock, OLD);
        let row = folio.internal();
        fs::write(row.path.to_path_buf(), b"user edit").unwrap();
        if stage {
            working_state::stage(
                &row,
                &PlacementFs::new(&home.home),
                &folio.objects,
                &folio.refs,
                &mut folio.registry,
                &lock,
            )
            .unwrap();
            // Even restoring the file leaves a stage that must block rename.
            fs::write(row.path.to_path_buf(), OLD).unwrap();
        }
        let error = run(&home, &mut folio, &mut lock).unwrap_err();
        assert!(matches!(error, RenameError::InternalChanged(_)));
        assert!(error.to_string().contains("commit or discard"));
        assert!(error.to_string().contains("unstage"));
        assert_eq!(
            folio.registry.folio_name(folio.folio.id()).unwrap(),
            Some(name("old"))
        );
        assert_eq!(folio.internal().commit_id, row.commit_id);
    }
}

#[test]
fn edited_and_staged_copies_do_not_block_or_change() {
    let home = TempHome::new();
    let mut lock = home.lock();
    let mut folio = setup(&home, &mut lock, OLD);
    let row = copy(&home, &mut folio, &lock, "main");
    fs::write(row.path.to_path_buf(), b"edited copy").unwrap();
    working_state::stage(
        &row,
        &PlacementFs::new(&home.home),
        &folio.objects,
        &folio.refs,
        &mut folio.registry,
        &lock,
    )
    .unwrap();
    let staged = folio.registry.placement_at(&row.path).unwrap().unwrap();
    run(&home, &mut folio, &mut lock).unwrap();
    assert_eq!(fs::read(row.path.to_path_buf()).unwrap(), b"edited copy");
    assert_eq!(
        folio.registry.placement_at(&row.path).unwrap(),
        Some(staged)
    );
}

#[test]
fn staging_feature_copy_ignores_its_old_folio_name() {
    let home = TempHome::new();
    let mut lock = home.lock();
    let mut folio = setup(&home, &mut lock, OLD);
    let row = copy(&home, &mut folio, &lock, "feature");
    let before = row.commit_id.unwrap();
    history::create_branch(
        folio.folio.id(),
        &RefName::parse("feature").unwrap(),
        before,
        &folio.refs,
        &mut folio.registry,
        &mut lock,
    )
    .unwrap();
    run(&home, &mut folio, &mut lock).unwrap();
    let bytes = [OLD, b"feature edit\n"].concat();
    fs::write(row.path.to_path_buf(), &bytes).unwrap();
    working_state::stage(
        &row,
        &PlacementFs::new(&home.home),
        &folio.objects,
        &folio.refs,
        &mut folio.registry,
        &lock,
    )
    .unwrap();
    assert_eq!(
        folio
            .registry
            .placement_at(&row.path)
            .unwrap()
            .unwrap()
            .staged
            .unwrap()
            .hash,
        hash_blob(&bytes)
    );
    assert_eq!(
        folio.registry.folio_name(folio.folio.id()).unwrap(),
        Some(name("new"))
    );
}

#[test]
fn refuses_taken_and_unknown_names() {
    let home = TempHome::new();
    let mut lock = home.lock();
    let mut folio = setup(&home, &mut lock, OLD);
    let other = FolioId::generate();
    folio
        .registry
        .write(&lock, |tx| {
            tx.add_folio(&NewFolio {
                id: other,
                format: folio.format.clone(),
            })?;
            tx.set_name(other, &name("new"))?;
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        run(&home, &mut folio, &mut lock),
        Err(RenameError::Registry(RegistryError::NameTaken(_)))
    ));
    assert_eq!(fs::read(folio.internal().path.to_path_buf()).unwrap(), OLD);
    let fs = PlacementFs::new(&home.home);
    assert!(matches!(
        rename::plan_rename(
            &name("unknown"),
            &name("absent"),
            &fs,
            &folio.stores(),
            &folio.registry
        ),
        Err(RenameError::UnknownName(_))
    ));
    // An existing destination alone cannot impersonate an interrupted rename.
    assert!(matches!(
        rename::plan_rename(
            &name("unknown"),
            &name("old"),
            &fs,
            &folio.stores(),
            &folio.registry
        ),
        Err(RenameError::UnknownName(_))
    ));
}

#[test]
fn resumes_each_partial_rename_with_exactly_one_main_commit() {
    for step in 0..4 {
        let home = TempHome::new();
        let mut lock = home.lock();
        let mut folio = setup(&home, &mut lock, OLD);
        let row = folio.internal();
        let before = row.commit_id.unwrap();
        folio
            .registry
            .write(&lock, |tx| {
                tx.rename(folio.folio.id(), &name("new"))?;
                tx.set_pending_rename(folio.folio.id(), &name("old"), &name("new"))
            })
            .unwrap();
        if step >= 1 {
            fs::write(row.path.to_path_buf(), NEW).unwrap();
        }
        if step >= 2 {
            working_state::stage(
                &row,
                &PlacementFs::new(&home.home),
                &folio.objects,
                &folio.refs,
                &mut folio.registry,
                &lock,
            )
            .unwrap();
        }
        let published = if step >= 3 {
            let mut metadata = metadata();
            metadata.message = "Rename `old` to `new`".into();
            let commit = folio
                .objects
                .put_commit(
                    &lock,
                    &Commit {
                        content: hash_blob(NEW),
                        parents: vec![before],
                        author: metadata.author,
                        time: metadata.time,
                        message: metadata.message,
                        body: None,
                        group: None,
                    },
                )
                .unwrap();
            folio
                .refs
                .update(
                    &mut lock,
                    Kind::Branch,
                    &RefName::parse("main").unwrap(),
                    commit,
                    Expect::Is(before),
                )
                .unwrap();
            Some(commit)
        } else {
            None
        };
        let after = run(&home, &mut folio, &mut lock).unwrap().unwrap();
        if let Some(published) = published {
            assert_eq!(after, published);
        }
        assert_completed(&folio, before, after);
        assert!(run(&home, &mut folio, &mut lock).is_err());
        assert_eq!(folio.internal().commit_id, Some(after));
    }
}

#[test]
fn interrupted_rename_refuses_unrelated_edits_and_stages() {
    for stage in [false, true] {
        let home = TempHome::new();
        let mut lock = home.lock();
        let mut folio = setup(&home, &mut lock, OLD);
        folio
            .registry
            .write(&lock, |tx| {
                tx.rename(folio.folio.id(), &name("new"))?;
                tx.set_pending_rename(folio.folio.id(), &name("old"), &name("new"))
            })
            .unwrap();
        let row = folio.internal();
        if stage {
            folio
                .registry
                .write(&lock, |tx| {
                    let mut changed = row.clone();
                    changed.staged = Some(StagedBlob {
                        hash: hash_blob(b"other"),
                        size: 5,
                    });
                    tx.update_placement(&changed)
                })
                .unwrap();
            fs::write(row.path.to_path_buf(), NEW).unwrap();
        } else {
            fs::write(row.path.to_path_buf(), [NEW, b"user change"].concat()).unwrap();
        }
        assert!(matches!(
            run(&home, &mut folio, &mut lock),
            Err(RenameError::InternalChanged(_))
        ));
        assert_eq!(folio.internal().commit_id, row.commit_id);
    }
}

#[test]
fn shared_guard_blocks_id_unresolved_name_and_self_references_on_live_branches() {
    // Rows on main are live through placements; topic has no placements and is
    // kept live by the caller's branch-ref check. Dead rows must allow rename.
    for (by_name, self_ref, branch, live) in [
        (false, false, "topic", true),
        (true, false, "topic", true),
        (false, false, "placed", false),
        (true, false, "placed", false),
        (false, true, "main", false),
        (true, true, "main", false),
        (false, false, "dead", false),
        (true, false, "dead", false),
    ] {
        let home = TempHome::new();
        let mut lock = home.lock();
        let mut folio = setup(&home, &mut lock, OLD);
        let id = folio.folio.id();
        let parent = if self_ref { id } else { FolioId::generate() };
        let text = format!(
            "---\nfolio-references:\n  target: {{{}}}\n---\n",
            if by_name {
                "name: old".into()
            } else {
                format!("id: {id}")
            }
        );
        let refs = front_matter::read_references(text.as_bytes()).unwrap();
        folio
            .registry
            .write(&lock, |tx| {
                if !self_ref {
                    tx.add_folio(&NewFolio {
                        id: parent,
                        format: folio.format.clone(),
                    })?;
                }
                // Build unresolved name rows deliberately, then assign the name.
                tx.clear_name(id)?;
                tx.rebuild_references(parent, branch, &refs)?;
                tx.set_name(id, &name("old"))?;
                Ok(())
            })
            .unwrap();
        if branch == "placed" {
            let path = home.root().join("parent.md");
            fs::write(&path, b"parent").unwrap();
            folio
                .registry
                .write(&lock, |tx| {
                    tx.add_placement(&NewPlacement {
                        path: PlacementPath::resolve(&path)?,
                        id: parent,
                        kind: PlacementKind::Copy,
                        head: Some(branch.into()),
                        commit_id: None,
                        base: None,
                        staged: None,
                    })
                })
                .unwrap();
        }
        let blocked = live || self_ref || branch == "placed";
        if !blocked {
            let before = folio.registry.references(parent, branch).unwrap();
            run(&home, &mut folio, &mut lock).unwrap();
            // Rename's guard is read-only, even for dead rows.
            assert_eq!(folio.registry.references(parent, branch).unwrap(), before);
            folio
                .registry
                .write(&lock, |tx| {
                    // Restore the lookup label so removal also finds unresolved
                    // old-name rows; rename deliberately left them untouched.
                    tx.set_name(id, &name("old"))?;
                    tx.remove_folio(id, &mut |_, _| Ok(false))
                })
                .unwrap();
            assert!(
                folio
                    .registry
                    .references(parent, branch)
                    .unwrap()
                    .is_empty()
            );
            continue;
        }
        let mut is_live = |p, b: &str| {
            assert_eq!(p, parent);
            assert_eq!(b, branch);
            Ok(live)
        };
        let removal = folio
            .registry
            .write(&lock, |tx| tx.remove_folio(id, &mut is_live));
        if blocked {
            let error = removal.unwrap_err();
            assert!(matches!(error, RegistryError::References(_)));
            assert!(error.to_string().contains(&parent.to_string()));
            assert!(error.to_string().contains(branch));
        }
        let stores = HistoryStores {
            objects: &folio.objects,
            refs: &folio.refs,
            object_limit: 16384,
        };
        let fs = PlacementFs::new(&home.home);
        let plan =
            rename::plan_rename(&name("old"), &name("new"), &fs, &stores, &folio.registry).unwrap();
        let error = rename::apply_rename(
            &plan,
            metadata(),
            &fs,
            &stores,
            &mut folio.registry,
            &mut lock,
            &mut is_live,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            RenameError::Registry(RegistryError::References(_))
        ));
        assert!(error.to_string().contains(&parent.to_string()));
        assert!(error.to_string().contains("target"));
        assert_eq!(folio.registry.folio_name(id).unwrap(), Some(name("old")));
        assert_eq!(fs::read(folio.internal().path.to_path_buf()).unwrap(), OLD);
    }
}

#[test]
fn destination_with_old_internal_name_without_pending_record_is_unknown() {
    let home = TempHome::new();
    let mut lock = home.lock();
    let mut folio = setup(&home, &mut lock, OLD);
    folio
        .registry
        .write(&lock, |tx| tx.rename(folio.folio.id(), &name("new")))
        .unwrap();
    let before = folio.internal();
    let tip = folio
        .refs
        .read(Kind::Branch, &RefName::parse("main").unwrap())
        .unwrap();
    assert!(matches!(
        run(&home, &mut folio, &mut lock),
        Err(RenameError::UnknownName(_))
    ));
    assert_eq!(
        folio.registry.folio_name(folio.folio.id()).unwrap(),
        Some(name("new"))
    );
    assert_eq!(folio.internal(), before);
    assert_eq!(fs::read(before.path.to_path_buf()).unwrap(), OLD);
    assert_eq!(
        folio
            .refs
            .read(Kind::Branch, &RefName::parse("main").unwrap())
            .unwrap(),
        tip
    );
    assert_eq!(
        folio.registry.pending_rename(folio.folio.id()).unwrap(),
        None
    );
}

#[test]
fn completion_clears_pending_record() {
    for retry in [false, true] {
        let home = TempHome::new();
        let mut lock = home.lock();
        let mut folio = setup(&home, &mut lock, OLD);
        if retry {
            folio
                .registry
                .write(&lock, |tx| {
                    tx.rename(folio.folio.id(), &name("new"))?;
                    tx.set_pending_rename(folio.folio.id(), &name("old"), &name("new"))
                })
                .unwrap();
        }
        run(&home, &mut folio, &mut lock).unwrap().unwrap();
        assert_eq!(
            folio.registry.pending_rename(folio.folio.id()).unwrap(),
            None
        );
    }
}

#[test]
fn different_pending_rename_refuses_and_removal_clears_record() {
    let home = TempHome::new();
    let mut lock = home.lock();
    let mut folio = setup(&home, &mut lock, OLD);
    let id = folio.folio.id();
    folio
        .registry
        .write(&lock, |tx| {
            tx.set_pending_rename(id, &name("previous"), &name("old"))?;
            assert_eq!(
                tx.pending_rename(id)?,
                Some((name("previous"), name("old")))
            );
            Ok(())
        })
        .unwrap();
    let before = folio.internal();
    let error = run(&home, &mut folio, &mut lock).unwrap_err();
    assert!(matches!(error, RenameError::PendingRename { .. }));
    assert!(
        error
            .to_string()
            .contains("finish pending rename `previous` to `old` first")
    );
    assert_eq!(folio.registry.folio_name(id).unwrap(), Some(name("old")));
    assert_eq!(
        folio.registry.pending_rename(id).unwrap(),
        Some((name("previous"), name("old")))
    );
    assert_eq!(folio.internal(), before);
    assert_eq!(fs::read(before.path.to_path_buf()).unwrap(), OLD);
    assert_eq!(
        folio
            .refs
            .read(Kind::Branch, &RefName::parse("main").unwrap())
            .unwrap(),
        before.commit_id
    );
    folio
        .registry
        .write(&lock, |tx| tx.remove_folio(id, &mut |_, _| Ok(false)))
        .unwrap();
    assert_eq!(folio.registry.pending_rename(id).unwrap(), None);
}

#[test]
fn apply_records_pending_rename_before_file_rewrite_failure() {
    let home = TempHome::new();
    let mut lock = home.lock();
    let mut folio = setup(&home, &mut lock, OLD);
    let id = folio.folio.id();
    let row = folio.internal();
    let text = format!("---\nfolio-references: {{target: {{id: {id}}}}}\n---\n");
    let references = front_matter::read_references(text.as_bytes()).unwrap();
    folio
        .registry
        .write(&lock, |tx| tx.rebuild_references(id, "dead", &references))
        .unwrap();
    let stores = HistoryStores {
        objects: &folio.objects,
        refs: &folio.refs,
        object_limit: 16384,
    };
    let fs = PlacementFs::new(&home.home);
    let plan =
        rename::plan_rename(&name("old"), &name("new"), &fs, &stores, &folio.registry).unwrap();
    let mut checked = false;
    let error = rename::apply_rename(
        &plan,
        metadata(),
        &fs,
        &stores,
        &mut folio.registry,
        &mut lock,
        &mut |_, _| {
            // Inject a working-file change after planning but before the rewrite,
            // leaving the successfully committed registry transaction in place.
            checked = true;
            std::fs::write(row.path.to_path_buf(), b"intervening edit").unwrap();
            Ok(false)
        },
    )
    .unwrap_err();
    assert!(checked);
    assert!(matches!(error, RenameError::Io(_)));
    assert_eq!(folio.registry.folio_name(id).unwrap(), Some(name("new")));
    assert_eq!(
        folio.registry.pending_rename(id).unwrap(),
        Some((name("old"), name("new")))
    );
    assert_eq!(folio.internal(), row);
    assert_eq!(
        folio
            .refs
            .read(Kind::Branch, &RefName::parse("main").unwrap())
            .unwrap(),
        row.commit_id
    );
    assert!(matches!(
        run(&home, &mut folio, &mut lock),
        Err(RenameError::InternalChanged(_))
    ));
    std::fs::write(row.path.to_path_buf(), OLD).unwrap();
    let after = run(&home, &mut folio, &mut lock).unwrap().unwrap();
    assert_completed(&folio, row.commit_id.unwrap(), after);
    assert_eq!(folio.registry.pending_rename(id).unwrap(), None);
}
