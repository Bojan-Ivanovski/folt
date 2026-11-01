use folt_core::folio_dir::{Durability, FolioDir, FolioSpec, create_folio};
use folt_core::history::{self, CommitMetadata, HistoryStores};
use folt_core::home::HomeLock;
use folt_core::layout::{FOLIO_LAYOUT_VERSION, OBJECT_ALGORITHM};
use folt_core::object::{Author, Commit, CommitTime, ObjectId, ObjectKind, hash_blob};
use folt_core::placement::{Observation, PlacementBase, PlacementKind, PlacementOps};
use folt_core::placement_fs::PlacementFs;
use folt_core::refs::RefStore;
use folt_core::registry::{
    self, FolioFormat, NewFolio, NewPlacement, Placement, PlacementPath, Registry,
};
use folt_core::store::ObjectStore;
use folt_core::working_state;

use crate::common::TempHome;

pub struct TestFolio {
    pub folio: FolioDir,
    pub format: FolioFormat,
    pub objects: ObjectStore,
    pub refs: RefStore,
    pub registry: Registry,
}

impl TestFolio {
    pub fn new(home: &TempHome, lock: &HomeLock) -> Self {
        let folio = create_folio(
            home.path(),
            lock,
            FolioSpec {
                content: b"initial".to_vec(),
            },
            Durability::None,
        )
        .unwrap();
        let format = FolioFormat {
            layout: FOLIO_LAYOUT_VERSION,
            algorithm: OBJECT_ALGORITHM.into(),
        };
        let objects = ObjectStore::open(&folio, &format).unwrap();
        let refs = RefStore::open(&folio, &format).unwrap();
        let mut registry = Registry::open_or_create(&home.home, lock).unwrap();
        let mut internal = registry::internal_placement(&home.home, folio.id()).unwrap();
        let fs = PlacementFs::new(&home.home);
        let temporary = folt_core::placement::Placement {
            path: internal.path.to_path_buf(),
            kind: PlacementKind::Internal,
            head: "main".into(),
            commit_id: None,
            base: None,
        };
        let Observation::File { signature } = fs.read(folio.id(), &temporary).unwrap() else {
            panic!("internal file missing")
        };
        internal.base = Some(PlacementBase {
            hash: hash_blob(b"initial"),
            signature,
        });
        registry
            .write(lock, |tx| {
                tx.add_folio(&NewFolio {
                    id: folio.id(),
                    format: format.clone(),
                })?;
                tx.add_placement(&internal)
            })
            .unwrap();
        Self {
            folio,
            format,
            objects,
            refs,
            registry,
        }
    }

    pub fn internal(&self) -> Placement {
        self.registry
            .internal_placement(self.folio.id())
            .unwrap()
            .unwrap()
    }

    pub fn stores(&self) -> HistoryStores<'_> {
        HistoryStores {
            objects: &self.objects,
            refs: &self.refs,
            object_limit: 16 * 1024,
        }
    }

    pub fn commit_object(&self, lock: &HomeLock, content: &[u8]) -> ObjectId {
        let blob = self.objects.put(lock, ObjectKind::Blob, content).unwrap();
        self.objects
            .put_commit(
                lock,
                &Commit {
                    content: blob,
                    parents: vec![],
                    author: Author::anonymous(),
                    time: CommitTime::from_unix_seconds(0).unwrap(),
                    message: "fixture".into(),
                    body: None,
                    group: None,
                },
            )
            .unwrap()
    }

    pub fn copy(&mut self, home: &TempHome, lock: &HomeLock) -> Placement {
        let path = home.root().join("copy.txt");
        let blob = self
            .objects
            .put(lock, ObjectKind::Blob, b"initial")
            .unwrap();
        let placement = folt_core::placement::Placement {
            path: path.clone(),
            kind: PlacementKind::Copy,
            head: "main".into(),
            commit_id: None,
            base: None,
        };
        let written = PlacementFs::new(&home.home)
            .create(self.folio.id(), &placement, &self.objects, blob)
            .unwrap();
        let row = NewPlacement {
            path: PlacementPath::resolve(path).unwrap(),
            id: self.folio.id(),
            kind: PlacementKind::Copy,
            head: None,
            commit_id: None,
            base: written.base,
            staged: None,
        };
        self.registry
            .write(lock, |tx| tx.add_placement(&row))
            .unwrap();
        self.registry.placement_at(&row.path).unwrap().unwrap()
    }

    pub fn commit_internal(
        &mut self,
        home: &TempHome,
        lock: &mut HomeLock,
        content: &[u8],
    ) -> ObjectId {
        let row = self.internal();
        std::fs::write(row.path.to_path_buf(), content).unwrap();
        let fs = PlacementFs::new(&home.home);
        working_state::stage(
            &row,
            &fs,
            &self.objects,
            &self.refs,
            &mut self.registry,
            lock,
        )
        .unwrap();
        let row = self.internal();
        let stores = HistoryStores {
            objects: &self.objects,
            refs: &self.refs,
            object_limit: 16 * 1024,
        };
        let facts = history::gather_commit(&row, &fs, &stores, &self.registry).unwrap();
        let plan = history::judge_commit(&facts).unwrap();
        history::apply_commit(&plan, metadata(), &fs, &stores, &mut self.registry, lock).unwrap()
    }
}

pub fn metadata() -> CommitMetadata {
    CommitMetadata {
        author: Author::anonymous(),
        time: CommitTime::from_unix_seconds(0).unwrap(),
        message: "test commit".into(),
        body: None,
        group: None,
    }
}
