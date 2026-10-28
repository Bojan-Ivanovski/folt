use folt_core::checkout::{self, CheckoutStores};
use folt_core::folio_dir::FolioId;
use folt_core::history::{self, HistoryStores};
use folt_core::object::{Commit, ObjectId};
use folt_core::placement::{
    Observation, Placement, PlacementError, PlacementKind, PlacementOps, Written,
};
use folt_core::placement_fs::PlacementFs;
use folt_core::placement_rules::{self, RefusalReason, RulesError};
use folt_core::refs::{Expect, Kind, RefName};
use folt_core::registry::{NewPlacement, PlacementPath};
use folt_core::store::ObjectStore;
use folt_core::working_state::{self, Edit, Staged};

use crate::common::TempHome;
use crate::support::{TestFolio, metadata};

// Observe a link without requiring platform symlink privileges; all internal-file
// operations still use the real filesystem and stores.
struct LinkFs<'a> {
    fs: PlacementFs<'a>,
    observation: Observation,
}

impl PlacementOps for LinkFs<'_> {
    fn read(&self, id: FolioId, row: &Placement) -> Result<Observation, PlacementError> {
        if row.kind == PlacementKind::Link {
            Ok(self.observation.clone())
        } else {
            self.fs.read(id, row)
        }
    }

    fn create(
        &self,
        id: FolioId,
        row: &Placement,
        objects: &ObjectStore,
        content: ObjectId,
    ) -> Result<Written, PlacementError> {
        self.fs.create(id, row, objects, content)
    }

    fn refresh(
        &self,
        id: FolioId,
        row: &Placement,
        objects: &ObjectStore,
        content: ObjectId,
        expected: Option<&Observation>,
    ) -> Result<Written, PlacementError> {
        self.fs.refresh(id, row, objects, content, expected)
    }

    fn sync_parent(&self, row: &Placement) -> Result<(), PlacementError> {
        self.fs.sync_parent(row)
    }

    fn convert(
        &self,
        id: FolioId,
        row: &Placement,
        kind: PlacementKind,
        objects: &ObjectStore,
        content: ObjectId,
        expected: &Observation,
    ) -> Result<Written, PlacementError> {
        self.fs.convert(id, row, kind, objects, content, expected)
    }

    fn normalize_link(&self, id: FolioId, row: &Placement) -> Result<(), PlacementError> {
        self.fs.normalize_link(id, row)
    }

    fn relocate(
        &self,
        id: FolioId,
        row: &Placement,
        to: &std::path::Path,
        expected: &Observation,
    ) -> Result<Written, PlacementError> {
        self.fs.relocate(id, row, to, expected)
    }

    fn unlink(
        &self,
        id: FolioId,
        row: &Placement,
        expected: &Observation,
    ) -> Result<(), PlacementError> {
        self.fs.unlink(id, row, expected)
    }

    fn hash_placement(&self, row: &Placement) -> Result<ObjectId, PlacementError> {
        self.fs.hash_placement(row)
    }
}

fn link(
    folio: &mut TestFolio,
    home: &TempHome,
    lock: &folt_core::home::HomeLock,
    name: &str,
) -> folt_core::registry::Placement {
    let path = PlacementPath::resolve(home.root().join(name)).unwrap();
    folio
        .registry
        .write(lock, |tx| {
            tx.add_placement(&NewPlacement {
                path: path.clone(),
                id: folio.folio.id(),
                kind: PlacementKind::Link,
                head: None,
                commit_id: None,
                base: None,
                staged: None,
            })
        })
        .unwrap();
    folio.registry.placement_at(&path).unwrap().unwrap()
}

#[test]
fn links_share_internal_edit_stage_commit_and_discard() {
    let home = TempHome::new();
    let mut lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let link = link(&mut folio, &home, &lock, "link");
    let other = self::link(&mut folio, &home, &lock, "other");
    let fs = LinkFs {
        fs: PlacementFs::new(&home.home),
        observation: Observation::IntactLink,
    };
    let internal = folio.internal();
    std::fs::write(internal.path.to_path_buf(), b"shared edit").unwrap();
    let status = placement_rules::gather_status(
        &[link.clone(), other.clone()],
        &fs,
        |_| Ok(&folio.refs),
        &folio.registry,
    )
    .unwrap();
    assert_eq!(status[0].edit, Edit::Edited);
    assert_eq!(status[0].staged, Staged::None);
    assert_eq!(status[0].drift, folt_core::placement::Drift::InSync);
    working_state::stage(&link, &fs, &folio.objects, &mut folio.registry, &lock).unwrap();
    let status = placement_rules::gather_status(
        &[link.clone(), other.clone()],
        &fs,
        |_| Ok(&folio.refs),
        &folio.registry,
    )
    .unwrap();
    assert!(
        status
            .iter()
            .all(|status| status.edit == Edit::Edited && status.staged == Staged::Staged)
    );
    working_state::unstage(&other, &fs, &mut folio.registry, &lock).unwrap();
    assert_eq!(folio.internal().staged, None);
    working_state::stage(&link, &fs, &folio.objects, &mut folio.registry, &lock).unwrap();
    let stores = HistoryStores {
        objects: &folio.objects,
        refs: &folio.refs,
        object_limit: 16384,
    };
    let facts = history::gather_commit(&link, &fs, &stores, &folio.registry).unwrap();
    assert_eq!(facts.working.placement, folio.internal());
    let plan = history::judge_commit(&facts).unwrap();
    let commit = history::apply_commit(
        &plan,
        metadata(),
        &fs,
        &stores,
        &mut folio.registry,
        &mut lock,
    )
    .unwrap();
    assert_eq!(folio.internal().commit_id, Some(commit));
    assert_eq!(folio.internal().staged, None);
    assert_eq!(
        folio.registry.placement_at(&link.path).unwrap().unwrap(),
        link
    );
    let status = placement_rules::gather_status(
        std::slice::from_ref(&link),
        &fs,
        |_| Ok(&folio.refs),
        &folio.registry,
    )
    .unwrap();
    assert_eq!(status[0].edit, Edit::Clean);
    assert_eq!(status[0].staged, Staged::None);

    std::fs::write(internal.path.to_path_buf(), b"discard this").unwrap();
    working_state::stage(&link, &fs, &folio.objects, &mut folio.registry, &lock).unwrap();
    let stores = CheckoutStores {
        objects: &folio.objects,
        refs: &folio.refs,
        object_limit: 16384,
    };
    let main = RefName::parse("main").unwrap();
    assert!(
        checkout::checkout_placement(
            &link,
            &main,
            false,
            &fs,
            &stores,
            &mut folio.registry,
            &lock
        )
        .is_err()
    );
    history::create_branch(
        &RefName::parse("topic").unwrap(),
        commit,
        &folio.refs,
        &mut lock,
    )
    .unwrap();
    assert!(
        checkout::checkout_placement(
            &link,
            &RefName::parse("topic").unwrap(),
            true,
            &fs,
            &stores,
            &mut folio.registry,
            &lock
        )
        .is_err()
    );
    checkout::checkout_placement(&link, &main, true, &fs, &stores, &mut folio.registry, &lock)
        .unwrap();
    assert_eq!(
        std::fs::read(internal.path.to_path_buf()).unwrap(),
        b"shared edit"
    );
    assert_eq!(folio.internal().staged, None);
}

#[test]
fn copy_commit_preserves_internal_edits_and_stale_stage() {
    let home = TempHome::new();
    let mut lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let link = link(&mut folio, &home, &lock, "link");
    let copy = folio.copy(&home, &lock);
    let fs = LinkFs {
        fs: PlacementFs::new(&home.home),
        observation: Observation::IntactLink,
    };
    std::fs::write(copy.path.to_path_buf(), b"copy edit").unwrap();
    working_state::stage(&copy, &fs, &folio.objects, &mut folio.registry, &lock).unwrap();
    let copy = folio.registry.placement_at(&copy.path).unwrap().unwrap();
    let internal = folio.internal();
    std::fs::write(internal.path.to_path_buf(), b"link edit").unwrap();
    let stores = HistoryStores {
        objects: &folio.objects,
        refs: &folio.refs,
        object_limit: 16384,
    };
    let facts = history::gather_commit(&copy, &fs, &stores, &folio.registry).unwrap();
    assert!(matches!(
        history::judge_commit(&facts),
        Err(history::Refusal::InternalEdited(_))
    ));
    working_state::stage(&link, &fs, &folio.objects, &mut folio.registry, &lock).unwrap();
    std::fs::write(internal.path.to_path_buf(), b"initial").unwrap();
    let facts = history::gather_commit(&copy, &fs, &stores, &folio.registry).unwrap();
    assert_eq!(
        working_state::judge_working(facts.internal.as_ref().unwrap()).edit,
        Edit::Clean
    );
    assert!(matches!(
        history::judge_commit(&facts),
        Err(history::Refusal::InternalEdited(_))
    ));
    assert_eq!(
        folio.registry.placement_at(&copy.path).unwrap().unwrap(),
        copy
    );
    let facts = history::gather_commit(&link, &fs, &stores, &folio.registry).unwrap();
    let plan = history::judge_commit(&facts).unwrap();
    history::apply_commit(
        &plan,
        metadata(),
        &fs,
        &stores,
        &mut folio.registry,
        &mut lock,
    )
    .unwrap();
    let facts = history::gather_commit(&copy, &fs, &stores, &folio.registry).unwrap();
    assert!(matches!(
        history::judge_commit(&facts),
        Err(history::Refusal::BranchMoved { .. })
    ));
    // A copy must first catch up with main, then it can commit normally.
    let checkout = CheckoutStores {
        objects: &folio.objects,
        refs: &folio.refs,
        object_limit: 16384,
    };
    checkout::checkout_placement(
        &link,
        &RefName::parse("main").unwrap(),
        true,
        &fs,
        &checkout,
        &mut folio.registry,
        &lock,
    )
    .unwrap();
    checkout::checkout_placement(
        &copy,
        &RefName::parse("main").unwrap(),
        true,
        &fs,
        &checkout,
        &mut folio.registry,
        &lock,
    )
    .unwrap();
    let copy = folio.registry.placement_at(&copy.path).unwrap().unwrap();
    std::fs::write(copy.path.to_path_buf(), b"new copy edit").unwrap();
    working_state::stage(&copy, &fs, &folio.objects, &mut folio.registry, &lock).unwrap();
    let copy = folio.registry.placement_at(&copy.path).unwrap().unwrap();
    let facts = history::gather_commit(&copy, &fs, &stores, &folio.registry).unwrap();
    let plan = history::judge_commit(&facts).unwrap();
    history::apply_commit(
        &plan,
        metadata(),
        &fs,
        &stores,
        &mut folio.registry,
        &mut lock,
    )
    .unwrap();
}

#[test]
fn resumes_commit_through_link_and_rechecks_selected_link() {
    let home = TempHome::new();
    let mut lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let link = link(&mut folio, &home, &lock, "link");
    let mut fs = LinkFs {
        fs: PlacementFs::new(&home.home),
        observation: Observation::IntactLink,
    };
    std::fs::write(folio.internal().path.to_path_buf(), b"staged").unwrap();
    working_state::stage(&link, &fs, &folio.objects, &mut folio.registry, &lock).unwrap();
    let internal = folio.internal();
    let meta = metadata();
    let published = folio
        .objects
        .put_commit(
            &lock,
            &Commit {
                content: internal.staged.unwrap().hash,
                parents: vec![],
                author: meta.author,
                time: meta.time,
                message: meta.message,
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
    let facts = history::gather_commit(&link, &fs, &stores, &folio.registry).unwrap();
    let plan = history::judge_commit(&facts).unwrap();
    assert_eq!(plan.resumed_commit(), Some(published));
    fs.observation = Observation::OtherLink {
        target: "elsewhere".into(),
    };
    let error = history::apply_commit(
        &plan,
        metadata(),
        &fs,
        &stores,
        &mut folio.registry,
        &mut lock,
    )
    .unwrap_err();
    assert!(error.to_string().contains("link was replaced"));
    assert_eq!(folio.internal(), internal);
    fs.observation = Observation::IntactLink;
    let mut changed_link = link.clone();
    changed_link.kind = PlacementKind::Copy;
    folio
        .registry
        .write(&lock, |tx| tx.update_placement(&changed_link))
        .unwrap();
    let error = history::apply_commit(
        &plan,
        metadata(),
        &fs,
        &stores,
        &mut folio.registry,
        &mut lock,
    )
    .unwrap_err();
    assert!(
        matches!(error, history::HistoryError::Refused(history::Refusal::RegistryChanged(ref path)) if *path == link.path.to_path_buf())
    );
    assert_eq!(folio.internal(), internal);
    folio
        .registry
        .write(&lock, |tx| tx.update_placement(&link))
        .unwrap();
    assert_eq!(
        history::apply_commit(
            &plan,
            metadata(),
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
fn replaced_links_refuse_all_working_operations_without_changing_rows() {
    let home = TempHome::new();
    let mut lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    folio.commit_internal(&home, &mut lock, b"base");
    let link = link(&mut folio, &home, &lock, "link");
    let internal = folio.internal();
    let real = PlacementFs::new(&home.home);
    let file = real.read(internal.id, &internal.to_placement().1).unwrap();
    for observation in [
        file,
        Observation::OtherLink {
            target: "elsewhere".into(),
        },
    ] {
        let fs = LinkFs {
            fs: PlacementFs::new(&home.home),
            observation,
        };
        let error = working_state::stage(&link, &fs, &folio.objects, &mut folio.registry, &lock)
            .unwrap_err();
        assert!(
            matches!(error, RulesError::Refused(ref refusal) if refusal.reason == RefusalReason::LinkReplaced)
        );
        assert!(error.to_string().contains("kind-change command (#33)"));
        assert!(working_state::unstage(&link, &fs, &mut folio.registry, &lock).is_err());
        assert!(working_state::gather(&link, &fs, &folio.refs, &folio.registry).is_err());
        assert!(
            placement_rules::gather_status(
                std::slice::from_ref(&link),
                &fs,
                |_| Ok(&folio.refs),
                &folio.registry
            )
            .is_err()
        );
        assert!(history::gather_commit(&link, &fs, &folio.stores(), &folio.registry).is_err());
        let stores = CheckoutStores {
            objects: &folio.objects,
            refs: &folio.refs,
            object_limit: 16384,
        };
        assert!(
            checkout::checkout_placement(
                &link,
                &RefName::parse("main").unwrap(),
                true,
                &fs,
                &stores,
                &mut folio.registry,
                &lock
            )
            .is_err()
        );
        assert_eq!(folio.internal(), internal);
        assert_eq!(
            folio.registry.placement_at(&link.path).unwrap().unwrap(),
            link
        );
    }
}

#[cfg(unix)]
#[test]
fn writes_and_commits_through_real_symlink_and_refuses_replacement() {
    let home = TempHome::new();
    let mut lock = home.lock();
    let mut folio = TestFolio::new(&home, &lock);
    let link = link(&mut folio, &home, &lock, "link");
    let fs = PlacementFs::new(&home.home);
    fs.create(
        link.id,
        &link.to_placement().1,
        &folio.objects,
        folt_core::object::hash_blob(b"initial"),
    )
    .unwrap();
    std::fs::write(link.path.to_path_buf(), b"through symlink").unwrap();
    working_state::stage(&link, &fs, &folio.objects, &mut folio.registry, &lock).unwrap();
    let stores = HistoryStores {
        objects: &folio.objects,
        refs: &folio.refs,
        object_limit: 16384,
    };
    let facts = history::gather_commit(&link, &fs, &stores, &folio.registry).unwrap();
    let plan = history::judge_commit(&facts).unwrap();
    history::apply_commit(
        &plan,
        metadata(),
        &fs,
        &stores,
        &mut folio.registry,
        &mut lock,
    )
    .unwrap();
    assert_eq!(
        fs.read(link.id, &link.to_placement().1).unwrap(),
        Observation::IntactLink
    );
    std::fs::remove_file(link.path.to_path_buf()).unwrap();
    std::fs::write(link.path.to_path_buf(), b"replacement").unwrap();
    let internal = folio.internal();
    let error =
        working_state::stage(&link, &fs, &folio.objects, &mut folio.registry, &lock).unwrap_err();
    assert!(error.to_string().contains("link was replaced"));
    assert_eq!(folio.internal(), internal);
    assert_eq!(
        std::fs::read(internal.path.to_path_buf()).unwrap(),
        b"through symlink"
    );
}
