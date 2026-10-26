use folt_core::history::{self, HistoryStores};
use folt_core::object::{Commit, Tag};
use folt_core::placement::PlacementOps;
use folt_core::placement_fs::PlacementFs;
use folt_core::refs::{Expect, Kind, RefName};
use folt_core::working_state;

use crate::common::TempHome;
use crate::support::{TestFolio, metadata};

#[test]
fn commit_publishes_content_ref_and_working_base() {
    let home = TempHome::new();
    let mut lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let first = folio.commit_internal(&home, &mut lock, b"first");
    let second = folio.commit_internal(&home, &mut lock, b"second");
    let commit = folio.objects.get_commit(second, 16384).unwrap();
    assert_eq!(commit.parents, vec![first]);
    assert_eq!(
        folio
            .refs
            .read(Kind::Branch, &RefName::parse("main").unwrap())
            .unwrap(),
        Some(second)
    );
    let row = folio.internal();
    assert_eq!(row.commit_id, Some(second));
    assert_eq!(row.staged, None);
    assert_eq!(row.base.unwrap().hash, commit.content);
}

#[test]
fn resumes_commit_published_before_registry_update() {
    let home = TempHome::new();
    let mut lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let row = folio.internal();
    std::fs::write(row.path.to_path_buf(), b"staged").unwrap();
    let fs = PlacementFs::new(&home.home);
    working_state::stage(&row, &fs, &folio.objects, &mut folio.registry, &lock).unwrap();
    let row = folio.internal();
    let metadata = metadata();
    let published = folio
        .objects
        .put_commit(
            &lock,
            &Commit {
                content: row.staged.unwrap().hash,
                parents: vec![],
                author: metadata.author.clone(),
                time: metadata.time,
                message: metadata.message.clone(),
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
            published,
            Expect::Absent,
        )
        .unwrap();
    let stores = HistoryStores {
        objects: &folio.objects,
        refs: &folio.refs,
        object_limit: 16384,
    };
    let facts = history::gather_commit(&row, &fs, &stores, &folio.registry).unwrap();
    let plan = history::judge_commit(&facts).unwrap();
    assert_eq!(plan.resumed_commit(), Some(published));
    assert_eq!(
        history::apply_commit(
            &plan,
            metadata,
            &fs,
            &stores,
            &mut folio.registry,
            &mut lock
        )
        .unwrap(),
        published
    );
    assert_eq!(folio.internal().commit_id, Some(published));
    assert_eq!(folio.internal().staged, None);
}

#[test]
fn reset_moves_tip_and_restores_working_content() {
    let home = TempHome::new();
    let mut lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let first = folio.commit_internal(&home, &mut lock, b"first");
    folio.commit_internal(&home, &mut lock, b"second");
    let fs = PlacementFs::new(&home.home);
    let stores = HistoryStores {
        objects: &folio.objects,
        refs: &folio.refs,
        object_limit: 16384,
    };
    let main = RefName::parse("main").unwrap();
    let facts = history::gather_reset(
        folio.folio.id(),
        &main,
        first,
        &fs,
        &stores,
        &folio.registry,
    )
    .unwrap();
    let plan = history::judge_reset(&facts, false).unwrap();
    history::apply_reset(&plan, &fs, &stores, &mut folio.registry, &mut lock).unwrap();
    assert_eq!(folio.refs.read(Kind::Branch, &main).unwrap(), Some(first));
    let row = folio.internal();
    assert_eq!(row.commit_id, Some(first));
    assert_eq!(std::fs::read(row.path.to_path_buf()).unwrap(), b"first");
    assert_eq!(
        fs.hash_placement(&row.to_placement().1).unwrap(),
        row.base.unwrap().hash
    );
}

#[test]
fn deletes_unused_branch_but_keeps_main_with_internal_placement() {
    let home = TempHome::new();
    let mut lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let commit = folio.commit_internal(&home, &mut lock, b"first");
    let topic = RefName::parse("topic").unwrap();
    history::create_branch(&topic, commit, &folio.refs, &mut lock).unwrap();
    history::delete_branch(
        folio.folio.id(),
        &topic,
        commit,
        &folio.refs,
        &folio.registry,
        &mut lock,
    )
    .unwrap();
    assert_eq!(folio.refs.read(Kind::Branch, &topic).unwrap(), None);
    assert!(matches!(
        history::delete_branch(
            folio.folio.id(),
            &RefName::parse("main").unwrap(),
            commit,
            &folio.refs,
            &folio.registry,
            &mut lock
        ),
        Err(history::HistoryError::Refused(
            history::Refusal::BranchInUse(_)
        ))
    ));
}

#[test]
fn lightweight_and_annotated_tags_round_trip() {
    let home = TempHome::new();
    let mut lock = home.lock();
    let folio = TestFolio::new(&home, &lock);
    let commit = folio.commit_object(&lock, b"content");
    let light = RefName::parse("v1").unwrap();
    assert_eq!(
        history::create_tag(&light, commit, None, &folio.stores(), &mut lock).unwrap(),
        commit
    );
    let name = RefName::parse("v2").unwrap();
    let meta = metadata();
    let annotation = Tag {
        target: commit,
        name: name.to_string(),
        tagger: meta.author,
        time: meta.time,
        message: "release".into(),
    };
    let tagged =
        history::create_tag(&name, commit, Some(&annotation), &folio.stores(), &mut lock).unwrap();
    assert_eq!(folio.objects.get_tag(tagged, 16384).unwrap(), annotation);
    assert_eq!(folio.refs.read(Kind::Tag, &name).unwrap(), Some(tagged));
    history::delete_tag(&light, commit, &folio.refs, &mut lock).unwrap();
    assert_eq!(folio.refs.read(Kind::Tag, &light).unwrap(), None);
    assert_eq!(folio.refs.read(Kind::Tag, &name).unwrap(), Some(tagged));
}
