use std::fs;

use folt_core::folio_dir::FolioId;
use folt_core::folio_name::FolioName;
use folt_core::front_matter::{Reference, SCAN_LIMIT};
use folt_core::history;
use folt_core::object::{ObjectKind, ObjectWriter, hash_blob};
use folt_core::placement::PlacementKind;
use folt_core::placement_fs::PlacementFs;
use folt_core::refs::{Kind, RefName};
use folt_core::registry::{
    FolioRow, NewFolio, NewPlacement, Placement, PlacementPath, ReferenceRow, Registry,
};
use folt_core::working_state;

use crate::common::TempHome;
use crate::support::TestFolio;

fn note(mapping: &str) -> Vec<u8> {
    format!("---\nfolio-references:\n{mapping}---\n# Body [not a reference](id:elsewhere)\n")
        .into_bytes()
}

fn markdown_copy(
    home: &TempHome,
    folio: &mut TestFolio,
    lock: &folt_core::home::HomeLock,
) -> Placement {
    let path = PlacementPath::resolve(home.root().join("note.md")).unwrap();
    if let Some(row) = folio.registry.placement_at(&path).unwrap() {
        return row;
    }
    fs::write(path.to_path_buf(), b"initial").unwrap();
    folio
        .registry
        .write(lock, |tx| {
            tx.add_placement(&NewPlacement {
                path: path.clone(),
                id: folio.folio.id(),
                kind: PlacementKind::Copy,
                head: Some("main".into()),
                commit_id: None,
                base: None,
                staged: None,
            })
        })
        .unwrap();
    folio.registry.placement_at(&path).unwrap().unwrap()
}

fn stage(home: &TempHome, folio: &mut TestFolio, lock: &folt_core::home::HomeLock, bytes: &[u8]) {
    let row = markdown_copy(home, folio, lock);
    fs::write(row.path.to_path_buf(), bytes).unwrap();
    working_state::stage(
        &row,
        &PlacementFs::new(&home.home),
        &folio.objects,
        &folio.refs,
        &mut folio.registry,
        lock,
    )
    .unwrap();
}

#[test]
fn stage_resolves_names_preserves_declared_ids_and_opaque_pins() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let child = FolioId::generate();
    let unregistered = FolioId::generate();
    folio
        .registry
        .write(&lock, |tx| {
            tx.add_folio(&NewFolio {
                id: child,
                format: folio.format.clone(),
            })?;
            tx.set_name(child, &FolioName::parse("logo-image").unwrap())?;
            Ok(())
        })
        .unwrap();
    let bytes = note(&format!(
        "  resolved:\n    name: \"logo-image\"\n    pin: \"as-written@pin\"\n  missing: {{name: later}}\n  explicit: {{id: {unregistered}, pin: 'opaque pin'}}\n"
    ));
    stage(&home, &mut folio, &lock, &bytes);
    assert_eq!(
        folio.registry.references(folio.folio.id(), "main").unwrap(),
        vec![
            ReferenceRow {
                key: "explicit".into(),
                child: unregistered.to_string(),
                child_label: "".into(),
                pin: Some("opaque pin".into())
            },
            ReferenceRow {
                key: "missing".into(),
                child: "".into(),
                child_label: "later".into(),
                pin: None
            },
            ReferenceRow {
                key: "resolved".into(),
                child: child.to_string(),
                child_label: "logo-image".into(),
                pin: Some("as-written@pin".into())
            },
        ]
    );
    assert_eq!(
        fs::read(markdown_copy(&home, &mut folio, &lock).path.to_path_buf()).unwrap(),
        bytes
    );
    let hash = markdown_copy(&home, &mut folio, &lock).staged.unwrap().hash;
    assert_eq!(hash, hash_blob(&bytes));
    assert_eq!(
        folio
            .objects
            .get_prefix(hash, ObjectKind::Blob, SCAN_LIMIT + 8)
            .unwrap(),
        bytes
    );
    // A later registration is picked up at the next stage, even if bytes match.
    folio
        .registry
        .write(&lock, |tx| {
            tx.set_name(child, &FolioName::parse("later").unwrap())?;
            Ok(())
        })
        .unwrap();
    stage(&home, &mut folio, &lock, &bytes);
    let rows = folio.registry.references(folio.folio.id(), "main").unwrap();
    assert_eq!(rows[1].child, child.to_string());
    assert_eq!(rows[2].child, "");
}

#[test]
fn stage_replaces_only_the_selected_branch_and_unstage_keeps_edges() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    stage(
        &home,
        &mut folio,
        &lock,
        &note("  main: {name: main-target}\n"),
    );
    let main = folio.registry.references(folio.folio.id(), "main").unwrap();
    let mut copy = markdown_copy(&home, &mut folio, &lock);
    copy.head = "topic".into();
    folio
        .registry
        .write(&lock, |tx| tx.update_placement(&copy))
        .unwrap();
    let fs = PlacementFs::new(&home.home);
    for (key, count) in [("old", 2), ("new", 1)] {
        let content = if count == 2 {
            note("  old: {name: old-target}\n  extra: {name: extra-target}\n")
        } else {
            note("  new: {name: new-target}\n")
        };
        fs::write(copy.path.to_path_buf(), content).unwrap();
        copy = folio.registry.placement_at(&copy.path).unwrap().unwrap();
        working_state::stage(
            &copy,
            &fs,
            &folio.objects,
            &folio.refs,
            &mut folio.registry,
            &lock,
        )
        .unwrap();
        let rows = folio
            .registry
            .references(folio.folio.id(), "topic")
            .unwrap();
        assert_eq!(rows.len(), count);
        assert!(rows.iter().any(|r| r.key == key));
        assert_eq!(
            folio.registry.references(folio.folio.id(), "main").unwrap(),
            main
        );
    }
    copy = folio.registry.placement_at(&copy.path).unwrap().unwrap();
    working_state::unstage(&copy, &fs, &mut folio.registry, &lock).unwrap();
    assert_eq!(
        folio
            .registry
            .references(folio.folio.id(), "topic")
            .unwrap()[0]
            .key,
        "new"
    );
}

#[test]
fn refused_stages_preserve_placement_and_all_previous_rows() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    stage(&home, &mut folio, &lock, &note("  kept: {name: target}\n"));
    let before = markdown_copy(&home, &mut folio, &lock);
    let rows = folio.registry.references(folio.folio.id(), "main").unwrap();
    let id = FolioId::generate();
    for (content, problem) in [
        (b"---\nfolio-references:\n".to_vec(), "no closing delimiter"),
        (note("  logo: {name: a, name: b}\n"), "duplicate"),
        (note("  logo: {name: a}\n  logo: {name: b}\n"), "duplicate"),
        (note("  logo: {name: a, path: b}\n"), "unsupported"),
        (
            b"---\n'folio-references': {}\n---\n".to_vec(),
            "quoted or complex",
        ),
        (
            note(&format!("  logo: {{name: a, id: {id}}}\n")),
            "invalid value",
        ),
        (note("  logo: {pin: a}\n"), "invalid value"),
        (note("  logo: {}\n"), "invalid value"),
        (note("  logo: image\n"), "invalid value"),
        (note(&format!("  logo: id:{id}\n")), "invalid value"),
        // A valid edge inserted before this invalid name must roll back too.
        (
            note("  first: {name: valid}\n  bad-alias: {name: 'bad name'}\n"),
            "bad-alias",
        ),
        (note("  bad-alias: {name: .invalid}\n"), "bad-alias"),
    ] {
        fs::write(before.path.to_path_buf(), content).unwrap();
        let error = working_state::stage(
            &before,
            &PlacementFs::new(&home.home),
            &folio.objects,
            &folio.refs,
            &mut folio.registry,
            &lock,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains(problem), "{error}");
        assert_eq!(markdown_copy(&home, &mut folio, &lock), before);
        assert_eq!(
            folio.registry.references(folio.folio.id(), "main").unwrap(),
            rows
        );
    }
}

#[test]
fn empty_binary_and_plain_non_markdown_stages_clear_the_branch() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    for bytes in [
        b"".as_slice(),
        b"plain text [body](id:target)",
        b"\0binary",
        b"---\nfolio-references: {bad: short}\n---\n\0",
    ] {
        stage(&home, &mut folio, &lock, &note("  old: {name: target}\n"));
        stage(&home, &mut folio, &lock, bytes);
        assert!(
            folio
                .registry
                .references(folio.folio.id(), "main")
                .unwrap()
                .is_empty()
        );
    }
    // An ordinary non-Markdown copy without a leading delimiter has no refs.
    let copy = folio.copy(&home, &lock);
    fs::write(copy.path.to_path_buf(), b"plain txt").unwrap();
    working_state::stage(
        &copy,
        &PlacementFs::new(&home.home),
        &folio.objects,
        &folio.refs,
        &mut folio.registry,
        &lock,
    )
    .unwrap();
    assert!(
        folio
            .registry
            .references(folio.folio.id(), "main")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn staging_content_equal_to_base_still_rebuilds_references() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let bytes = note("  kept: {name: target}\n");
    let mut row = markdown_copy(&home, &mut folio, &lock);
    row.base = folio.internal().base;
    row.base.as_mut().unwrap().hash = hash_blob(&bytes);
    folio
        .registry
        .write(&lock, |tx| tx.update_placement(&row))
        .unwrap();
    stage(&home, &mut folio, &lock, &bytes);
    assert_eq!(markdown_copy(&home, &mut folio, &lock).staged, None);
    assert_eq!(
        folio.registry.references(folio.folio.id(), "main").unwrap()[0].key,
        "kept"
    );
}

#[test]
fn removal_checks_ids_across_branches_and_unresolved_current_names() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let parent = FolioId::generate();
    let target = folio.folio.id();
    let reference = Reference {
        alias: "alias".into(),
        name: None,
        id: Some(target),
        pin: None,
    };
    folio
        .registry
        .write(&lock, |tx| {
            tx.add_folio(&NewFolio {
                id: parent,
                format: folio.format.clone(),
            })?;
            tx.rebuild_references(parent, "other-branch", &[reference])?;
            Ok(())
        })
        .unwrap();
    let error = folio
        .registry
        .write(&lock, |tx| tx.remove_folio(target, &mut |_, _| Ok(true)))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(&parent.to_string())
            && error.contains("other-branch")
            && error.contains("alias")
    );
    let reference = Reference {
        alias: "unresolved".into(),
        name: Some("current-name".into()),
        id: None,
        pin: None,
    };
    folio
        .registry
        .write(&lock, |tx| {
            tx.delete_branch_references(parent, "other-branch")?;
            tx.rebuild_references(parent, "main", &[reference])?;
            tx.set_name(target, &FolioName::parse("current-name").unwrap())?;
            Ok(())
        })
        .unwrap();
    let error = folio
        .registry
        .write(&lock, |tx| tx.remove_folio(target, &mut |_, _| Ok(true)))
        .unwrap_err()
        .to_string();
    assert!(error.contains(&parent.to_string()) && error.contains("unresolved"));
    // An unresolved old label no longer points at a renamed target. Outgoing
    // rows on every branch disappear with the removed parent.
    folio
        .registry
        .write(&lock, |tx| {
            tx.rename(target, &FolioName::parse("new-name").unwrap())?;
            let outgoing = Reference {
                alias: "outgoing".into(),
                name: None,
                id: Some(parent),
                pin: None,
            };
            for branch in ["main", "topic"] {
                tx.rebuild_references(target, branch, std::slice::from_ref(&outgoing))?;
            }
            tx.remove_folio(target, &mut |_, _| Ok(true))
        })
        .unwrap();
    for branch in ["main", "topic"] {
        assert!(
            folio
                .registry
                .references(target, branch)
                .unwrap()
                .is_empty()
        );
    }
    assert_eq!(folio.registry.references(parent, "main").unwrap().len(), 1);
}

#[test]
fn replace_all_keeps_surviving_parents_even_when_their_children_are_removed() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let survivor = folio.folio.id();
    let removed = FolioId::generate();
    folio
        .registry
        .write(&lock, |tx| {
            tx.add_folio(&NewFolio {
                id: removed,
                format: folio.format.clone(),
            })?;
            for (parent, child) in [(survivor, removed), (removed, survivor)] {
                let r = Reference {
                    alias: "edge".into(),
                    name: None,
                    id: Some(child),
                    pin: Some("pin".into()),
                };
                for branch in ["main", "topic"] {
                    tx.rebuild_references(parent, branch, std::slice::from_ref(&r))?;
                }
            }
            tx.replace_all(
                &[FolioRow {
                    id: survivor,
                    format: folio.format.clone(),
                }],
                &[],
            )
        })
        .unwrap();
    for branch in ["main", "topic"] {
        let rows = folio.registry.references(survivor, branch).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].child, removed.to_string());
        assert!(
            folio
                .registry
                .references(removed, branch)
                .unwrap()
                .is_empty()
        );
    }
    let reopened = Registry::open_existing(&home.home).unwrap();
    assert_eq!(reopened.references(survivor, "main").unwrap().len(), 1);
}

#[test]
fn deleting_branch_keeps_rows_until_dead_referrers_are_purged() {
    let home = TempHome::new();
    let mut lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let content = note("  edge: {name: target}\n");
    let commit = folio.commit_internal(&home, &mut lock, &content);
    stage(&home, &mut folio, &lock, &content);
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
    let other_parent = FolioId::generate();
    folio
        .registry
        .write(&lock, |tx| {
            tx.add_folio(&NewFolio {
                id: other_parent,
                format: folio.format.clone(),
            })?;
            let r = Reference {
                alias: "edge".into(),
                name: Some("target".into()),
                id: None,
                pin: None,
            };
            for parent in [folio.folio.id(), other_parent] {
                tx.rebuild_references(parent, "topic", std::slice::from_ref(&r))?;
            }
            Ok(())
        })
        .unwrap();
    let target = FolioId::generate();
    folio
        .registry
        .write(&lock, |tx| {
            tx.add_folio(&NewFolio {
                id: target,
                format: folio.format.clone(),
            })?;
            tx.rebuild_references(
                folio.folio.id(),
                "topic",
                &[Reference {
                    alias: "target".into(),
                    name: None,
                    id: Some(target),
                    pin: Some("opaque".into()),
                }],
            )
        })
        .unwrap();
    let snapshot = folio
        .registry
        .references(folio.folio.id(), "topic")
        .unwrap();
    // A read-only registry proves neither deletion path attempts cache writes.
    let read_only = Registry::open_existing(&home.home).unwrap();
    // A stale ref expectation must not touch the cache.
    let stale = folio.commit_object(&lock, b"stale");
    assert!(
        history::delete_branch(
            folio.folio.id(),
            &topic,
            stale,
            &folio.refs,
            &read_only,
            &mut lock
        )
        .is_err()
    );
    assert_eq!(folio.refs.read(Kind::Branch, &topic).unwrap(), Some(commit));
    assert_eq!(
        folio
            .registry
            .references(folio.folio.id(), "topic")
            .unwrap(),
        snapshot
    );
    let error = folio
        .registry
        .write(&lock, |tx| {
            tx.remove_folio(target, &mut |parent, branch| {
                assert_eq!(parent, folio.folio.id());
                Ok(folio
                    .refs
                    .read(Kind::Branch, &RefName::parse(branch).unwrap())
                    .unwrap()
                    .is_some())
            })
        })
        .unwrap_err()
        .to_string();
    assert!(error.contains(&folio.folio.id().to_string()) && error.contains("topic"));
    // Successful deletion also leaves the cache untouched.
    history::delete_branch(
        folio.folio.id(),
        &topic,
        commit,
        &folio.refs,
        &read_only,
        &mut lock,
    )
    .unwrap();
    assert_eq!(folio.refs.read(Kind::Branch, &topic).unwrap(), None);
    assert_eq!(
        folio
            .registry
            .references(folio.folio.id(), "topic")
            .unwrap(),
        snapshot
    );
    folio
        .registry
        .write(&lock, |tx| {
            tx.remove_folio(target, &mut |_, branch| {
                Ok(folio
                    .refs
                    .read(Kind::Branch, &RefName::parse(branch).unwrap())
                    .unwrap()
                    .is_some())
            })
        })
        .unwrap();
    assert!(
        folio
            .registry
            .references(folio.folio.id(), "topic")
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        folio
            .registry
            .references(folio.folio.id(), "main")
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        folio
            .registry
            .references(other_parent, "topic")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn stored_prefix_is_bounded_for_both_codecs_and_trusts_the_stored_hash() {
    let home = TempHome::new();
    let lock = home.lock();
    let folio = TestFolio::new(&home, &lock);
    for compressed in [false, true] {
        for length in [0, 5, SCAN_LIMIT + 8, SCAN_LIMIT * 3] {
            let bytes = vec![b'x'; length];
            let mut writer =
                ObjectWriter::new(Vec::new(), ObjectKind::Blob, length as u64, compressed).unwrap();
            writer.write_body(&bytes).unwrap();
            let (stored, id) = writer.finish().unwrap();
            let path = folio.folio.path().join("blobs").join(format!("{id}.blob"));
            fs::write(&path, &stored).unwrap();
            let limit = SCAN_LIMIT + 8;
            assert_eq!(
                folio
                    .objects
                    .get_prefix(id, ObjectKind::Blob, limit)
                    .unwrap(),
                bytes[..length.min(limit)]
            );
            assert!(
                folio
                    .objects
                    .get_prefix(id, ObjectKind::Commit, limit)
                    .is_err()
            );
            // Place the same bytes under a different hash: the prefix reader
            // must trust the address rather than verify it.
            let wrong = hash_blob(b"different address");
            fs::write(
                folio
                    .folio
                    .path()
                    .join("blobs")
                    .join(format!("{wrong}.blob")),
                stored,
            )
            .unwrap();
            assert_eq!(
                folio
                    .objects
                    .get_prefix(wrong, ObjectKind::Blob, 3)
                    .unwrap(),
                bytes[..length.min(3)]
            );
        }
    }
}

#[test]
fn closing_delimiter_crossing_scan_boundary_is_read_from_the_stored_blob() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let mut bytes = b"---\r\nfolio-references: {edge: {name: target}}\r\n#".to_vec();
    bytes.resize(SCAN_LIMIT - 3, b'x');
    bytes.extend_from_slice(b"\r\n---\r\nbody");
    stage(&home, &mut folio, &lock, &bytes);
    assert_eq!(
        folio
            .registry
            .references(folio.folio.id(), "main")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn selected_copy_extension_controls_reference_parsing() {
    for (extension, bytes, count) in [
        ("txt", note("  edge: {name: target}\n"), 0),
        ("txt", b"---\nfolio-references: malformed".to_vec(), 0),
        ("MD", note("  edge: {name: target}\n"), 1),
        ("markdown", note("  edge: {name: target}\n"), 1),
    ] {
        let home = TempHome::new();
        let lock = home.lock();
        let mut folio = TestFolio::new(&home, &lock);
        let mut row = markdown_copy(&home, &mut folio, &lock);
        let old = row.path.clone();
        row.path = PlacementPath::resolve(home.root().join(format!("x.{extension}"))).unwrap();
        fs::rename(old.to_path_buf(), row.path.to_path_buf()).unwrap();
        folio
            .registry
            .write(&lock, |tx| {
                tx.remove_placement(&old)?;
                tx.add_placement(&NewPlacement {
                    path: row.path.clone(),
                    id: row.id,
                    kind: row.kind,
                    head: Some(row.head.clone()),
                    commit_id: None,
                    base: None,
                    staged: None,
                })
            })
            .unwrap();
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
            folio.registry.references(row.id, "main").unwrap().len(),
            count
        );
        // Selecting folio.main itself always clears the cache without parsing.
        let internal = folio.internal();
        fs::write(internal.path.to_path_buf(), b"---\nmalformed").unwrap();
        working_state::stage(
            &internal,
            &PlacementFs::new(&home.home),
            &folio.objects,
            &folio.refs,
            &mut folio.registry,
            &lock,
        )
        .unwrap();
        assert!(
            folio
                .registry
                .references(row.id, "main")
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn placement_without_ref_keeps_referrer_live_and_dead_name_rows_are_purged() {
    let home = TempHome::new();
    let lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let parent = folio.folio.id();
    let target = FolioId::generate();
    let mut copy = folio.copy(&home, &lock);
    copy.head = "staged-only".into();
    let name = FolioName::parse("target").unwrap();
    folio
        .registry
        .write(&lock, |tx| {
            tx.update_placement(&copy)?;
            tx.add_folio(&NewFolio {
                id: target,
                format: folio.format.clone(),
            })?;
            tx.rebuild_references(
                parent,
                "staged-only",
                &[Reference {
                    alias: "unresolved".into(),
                    name: Some("target".into()),
                    id: None,
                    pin: None,
                }],
            )?;
            tx.set_name(target, &name)?;
            Ok(())
        })
        .unwrap();
    assert!(
        folio
            .refs
            .read(Kind::Branch, &RefName::parse("staged-only").unwrap())
            .unwrap()
            .is_none()
    );
    let error = folio
        .registry
        .write(&lock, |tx| tx.remove_folio(target, &mut |_, _| Ok(false)))
        .unwrap_err()
        .to_string();
    assert!(error.contains(&parent.to_string()) && error.contains("staged-only"));
    assert_eq!(
        folio
            .registry
            .references(parent, "staged-only")
            .unwrap()
            .len(),
        1
    );
    folio
        .registry
        .write(&lock, |tx| {
            tx.remove_placement(&copy.path)?;
            tx.remove_folio(target, &mut |_, _| Ok(false))
        })
        .unwrap();
    assert!(
        folio
            .registry
            .references(parent, "staged-only")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn stage_purges_only_own_dead_branches_and_unstage_does_not_purge() {
    let home = TempHome::new();
    let mut lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let commit = folio.commit_object(&lock, b"initial");
    history::create_branch(
        folio.folio.id(),
        &RefName::parse("live-ref").unwrap(),
        commit,
        &folio.refs,
        &mut folio.registry,
        &mut lock,
    )
    .unwrap();
    let other = FolioId::generate();
    let mut copy = folio.copy(&home, &lock);
    copy.head = "live-placement".into();
    folio
        .registry
        .write(&lock, |tx| {
            tx.update_placement(&copy)?;
            tx.add_folio(&NewFolio {
                id: other,
                format: folio.format.clone(),
            })?;
            let reference = Reference {
                alias: "edge".into(),
                name: Some("target".into()),
                id: None,
                pin: None,
            };
            for branch in ["dead", "live-ref", "live-placement"] {
                tx.rebuild_references(folio.folio.id(), branch, std::slice::from_ref(&reference))?;
            }
            tx.rebuild_references(other, "dead", &[reference])
        })
        .unwrap();
    working_state::unstage(
        &copy,
        &PlacementFs::new(&home.home),
        &mut folio.registry,
        &lock,
    )
    .unwrap();
    assert_eq!(
        folio
            .registry
            .references(folio.folio.id(), "dead")
            .unwrap()
            .len(),
        1
    );
    stage(&home, &mut folio, &lock, &note("  new: {name: target}\n"));
    assert!(
        folio
            .registry
            .references(folio.folio.id(), "dead")
            .unwrap()
            .is_empty()
    );
    for branch in ["live-ref", "live-placement"] {
        assert_eq!(
            folio
                .registry
                .references(folio.folio.id(), branch)
                .unwrap()
                .len(),
            1
        );
    }
    assert_eq!(folio.registry.references(other, "dead").unwrap().len(), 1);
}

#[test]
fn recreated_branch_does_not_inherit_cached_edges() {
    let home = TempHome::new();
    let mut lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let commit = folio.commit_object(&lock, b"initial");
    let topic = RefName::parse("topic").unwrap();
    let id = folio.folio.id();
    history::create_branch(
        id,
        &topic,
        commit,
        &folio.refs,
        &mut folio.registry,
        &mut lock,
    )
    .unwrap();
    let target = FolioId::generate();
    folio
        .registry
        .write(&lock, |tx| {
            tx.add_folio(&NewFolio {
                id: target,
                format: folio.format.clone(),
            })?;
            tx.rebuild_references(
                id,
                "topic",
                &[Reference {
                    alias: "edge".into(),
                    name: None,
                    id: Some(target),
                    pin: None,
                }],
            )
        })
        .unwrap();
    // Creating an existing branch must not purge its live rows.
    assert!(
        history::create_branch(
            id,
            &topic,
            commit,
            &folio.refs,
            &mut folio.registry,
            &mut lock
        )
        .is_err()
    );
    assert_eq!(folio.registry.references(id, "topic").unwrap().len(), 1);
    history::delete_branch(id, &topic, commit, &folio.refs, &folio.registry, &mut lock).unwrap();
    assert_eq!(folio.registry.references(id, "topic").unwrap().len(), 1);
    // Even a dead name must retain its cache when target validation fails.
    let missing = hash_blob(b"missing branch target");
    let error = history::create_branch(
        id,
        &topic,
        missing,
        &folio.refs,
        &mut folio.registry,
        &mut lock,
    )
    .unwrap_err();
    assert!(
        matches!(error, history::HistoryError::Ref(folt_core::refs::RefError::MissingTarget(actual)) if actual == missing)
    );
    assert_eq!(folio.registry.references(id, "topic").unwrap().len(), 1);
    history::create_branch(
        id,
        &topic,
        commit,
        &folio.refs,
        &mut folio.registry,
        &mut lock,
    )
    .unwrap();
    assert!(folio.registry.references(id, "topic").unwrap().is_empty());
    folio
        .registry
        .write(&lock, |tx| tx.remove_folio(target, &mut |_, _| Ok(true)))
        .unwrap();
}

#[test]
fn create_branch_keeps_staged_before_ref_edges_on_failure_and_success() {
    let home = TempHome::new();
    let mut lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let parent = folio.folio.id();
    let target = FolioId::generate();
    let topic = RefName::parse("topic").unwrap();
    let mut copy = markdown_copy(&home, &mut folio, &lock);
    copy.head = "topic".into();
    folio
        .registry
        .write(&lock, |tx| {
            tx.update_placement(&copy)?;
            tx.add_folio(&NewFolio {
                id: target,
                format: folio.format.clone(),
            })
        })
        .unwrap();
    fs::write(
        copy.path.to_path_buf(),
        note(&format!("  edge: {{id: {target}}}\n")),
    )
    .unwrap();
    working_state::stage(
        &copy,
        &PlacementFs::new(&home.home),
        &folio.objects,
        &folio.refs,
        &mut folio.registry,
        &lock,
    )
    .unwrap();
    let rows = folio.registry.references(parent, "topic").unwrap();
    assert_eq!(rows.len(), 1);
    let missing = hash_blob(b"nonexistent commit");
    let wrong_kind = folio
        .objects
        .put(&lock, ObjectKind::Blob, b"not a commit")
        .unwrap();
    for commit in [missing, wrong_kind] {
        let error = history::create_branch(
            parent,
            &topic,
            commit,
            &folio.refs,
            &mut folio.registry,
            &mut lock,
        )
        .unwrap_err();
        if commit == missing {
            assert!(
                matches!(error, history::HistoryError::Ref(folt_core::refs::RefError::MissingTarget(id)) if id == missing)
            );
        } else {
            assert!(
                matches!(error, history::HistoryError::Ref(folt_core::refs::RefError::WrongKind { id, actual: ObjectKind::Blob }) if id == wrong_kind)
            );
        }
        assert_eq!(folio.registry.references(parent, "topic").unwrap(), rows);
        assert_eq!(folio.refs.read(Kind::Branch, &topic).unwrap(), None);
        let error = folio
            .registry
            .write(&lock, |tx| tx.remove_folio(target, &mut |_, _| Ok(false)))
            .unwrap_err()
            .to_string();
        assert!(error.contains("referenced by") && error.contains("topic"));
    }
    let commit = folio.commit_object(&lock, b"initial");
    history::create_branch(
        parent,
        &topic,
        commit,
        &folio.refs,
        &mut folio.registry,
        &mut lock,
    )
    .unwrap();
    assert_eq!(folio.refs.read(Kind::Branch, &topic).unwrap(), Some(commit));
    assert_eq!(folio.registry.references(parent, "topic").unwrap(), rows);
    assert!(
        folio
            .registry
            .write(&lock, |tx| tx.remove_folio(target, &mut |_, _| Ok(false)))
            .is_err()
    );
}
