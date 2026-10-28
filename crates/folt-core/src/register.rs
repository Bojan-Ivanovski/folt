//! Explicit-path registration. Planning reads metadata, never content. Apply
//! captures bytes once and registers the entire batch in one transaction.
//!
//! Ordinary failures remove this call's markers and Folio directories. A crash
//! before the transaction can leave orphan directories; discovery/repair owns
//! those leftovers, and a retry creates new Folios rather than adopting them.
use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use crate::config::{Config, ConfigError};
use crate::folio_dir::{Durability, FolioDirError, FolioSpec, create_folio};
use crate::home::{Home, HomeError};
use crate::layout::{FOLIO_LAYOUT_VERSION, OBJECT_ALGORITHM};
use crate::object::hash_blob;
use crate::placement::{Observation, PlacementBase, PlacementKind, PlacementOps};
use crate::placement_fs::PlacementFs;
use crate::registry::{
    self, FolioFormat, NewFolio, NewPlacement, PlacementPath, Registry, RegistryError,
};

pub const DIRECTORY_MARKER: &str = "__folio__.md";
/// Batches above 256 MiB require confirmation, independently of the file limit.
pub const BATCH_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RefusalReason {
    /// Name the target explicitly instead of its symlink.
    Symlink,
    NotAFile,
    Missing,
    Unreadable,
    InsideHome,
    InsideDirectoryFolio,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegisterStatus {
    New,
    AlreadyRegistered,
    Refused(RefusalReason),
    NeedsConfirmation { size: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterEntry {
    pub path: PathBuf,
    pub status: RegisterStatus,
    pub size: u64,
    /// An empty marker to create in the user's directory, with no clobbering.
    pub creates_marker: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RegisterSummary {
    pub new: usize,
    pub already_registered: usize,
    pub refused: usize,
    pub needs_confirmation: usize,
    pub total_bytes: u64,
    /// Up to ten entries, largest first (path breaks ties).
    pub largest: Vec<(PathBuf, u64)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegisterNote {
    NeedsConfirmation { size: u64 },
}

/// The dry-run result, also consumed by apply. Request paths remain private so
/// changing display data cannot bypass apply's checks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterPlan {
    pub entries: Vec<RegisterEntry>,
    pub notes: Vec<RegisterNote>,
    pub summary: RegisterSummary,
    paths: Vec<PathBuf>,
}

pub fn plan_register(
    home: &Home,
    paths: impl IntoIterator<Item = impl AsRef<Path>>,
) -> Result<RegisterPlan, RegisterError> {
    let config = Config::load(home)?;
    let registry = Registry::open_existing(home)?;
    let home_path = fs::canonicalize(home.path())?;
    let mut entries: Vec<RegisterEntry> = Vec::new();
    let mut requests = Vec::new();
    let mut seen = HashSet::new();
    for input in paths {
        let input = input.as_ref();
        let absolute = if input.is_absolute() {
            input.to_path_buf()
        } else {
            std::env::current_dir()?.join(input)
        };
        // A missing parent is a refusal too, rather than aborting the report.
        let resolved = PlacementPath::resolve(&absolute);
        let path = resolved
            .as_ref()
            .map_or_else(|_| absolute.clone(), PlacementPath::to_path_buf);
        let directory = fs::symlink_metadata(&absolute).is_ok_and(|m| m.is_dir());
        if !seen.insert(path.clone()) {
            // Keep directory intent when its missing marker was named first.
            if directory && let Some(index) = entries.iter().position(|e| e.path == path) {
                requests[index] = absolute.clone();
                entries[index] =
                    inspect_entry(home_path.as_path(), &registry, &config, &absolute, path)?;
            }
            continue;
        }
        entries.push(inspect_entry(
            &home_path, &registry, &config, &absolute, path,
        )?);
        requests.push(absolute);
    }
    Ok(report(entries, requests))
}

fn inspect_entry(
    home: &Path,
    registry: &Registry,
    config: &Config,
    input: &Path,
    path: PathBuf,
) -> Result<RegisterEntry, RegisterError> {
    let mut entry = RegisterEntry {
        path: path.clone(),
        status: RegisterStatus::New,
        size: 0,
        creates_marker: false,
    };
    let refuse = |mut entry: RegisterEntry, reason| {
        entry.status = RegisterStatus::Refused(reason);
        entry
    };
    if path.starts_with(home) {
        return Ok(refuse(entry, RefusalReason::InsideHome));
    }
    if let Ok(key) = PlacementPath::resolve(input)
        && registry.placement_at(&key)?.is_some()
    {
        entry.status = RegisterStatus::AlreadyRegistered;
        return Ok(entry);
    }
    for ancestor in path.ancestors().skip(1) {
        let marker = ancestor.join(DIRECTORY_MARKER);
        if marker != path
            && let Ok(key) = PlacementPath::resolve(&marker)
            && registry.placement_at(&key)?.is_some()
        {
            return Ok(refuse(entry, RefusalReason::InsideDirectoryFolio));
        }
    }
    let named = match fs::symlink_metadata(input) {
        Ok(meta) => meta,
        Err(error) => return Ok(refuse(entry, io_refusal(&error))),
    };
    if named.is_symlink() {
        return Ok(refuse(entry, RefusalReason::Symlink));
    }
    let metadata = if named.is_dir() {
        match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                entry.creates_marker = true;
                return Ok(entry);
            }
            Err(error) => return Ok(refuse(entry, io_refusal(&error))),
        }
    } else {
        named
    };
    if metadata.is_symlink() {
        return Ok(refuse(entry, RefusalReason::Symlink));
    }
    if !metadata.is_file() {
        return Ok(refuse(entry, RefusalReason::NotAFile));
    }
    // Opening a regular file checks access without reading its bytes.
    if File::open(&path).is_err() {
        return Ok(refuse(entry, RefusalReason::Unreadable));
    }
    entry.size = metadata.len();
    if entry.size >= config.large_file_bytes() {
        entry.status = RegisterStatus::NeedsConfirmation { size: entry.size };
    }
    Ok(entry)
}

fn io_refusal(error: &io::Error) -> RefusalReason {
    if error.kind() == io::ErrorKind::NotFound {
        RefusalReason::Missing
    } else {
        RefusalReason::Unreadable
    }
}

fn report(entries: Vec<RegisterEntry>, paths: Vec<PathBuf>) -> RegisterPlan {
    let mut summary = RegisterSummary::default();
    for entry in &entries {
        match entry.status {
            RegisterStatus::New => summary.new += 1,
            RegisterStatus::AlreadyRegistered => summary.already_registered += 1,
            RegisterStatus::Refused(_) => summary.refused += 1,
            RegisterStatus::NeedsConfirmation { .. } => summary.needs_confirmation += 1,
        }
        if matches!(
            entry.status,
            RegisterStatus::New | RegisterStatus::NeedsConfirmation { .. }
        ) {
            summary.total_bytes = summary.total_bytes.saturating_add(entry.size);
            summary.largest.push((entry.path.clone(), entry.size));
        }
    }
    summary
        .largest
        .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    summary.largest.truncate(10);
    let notes = if summary.total_bytes > BATCH_TOTAL_BYTES {
        vec![RegisterNote::NeedsConfirmation {
            size: summary.total_bytes,
        }]
    } else {
        vec![]
    };
    RegisterPlan {
        entries,
        notes,
        summary,
        paths,
    }
}

fn blocked(plan: &RegisterPlan, confirm_large: bool) -> bool {
    plan.summary.refused != 0
        || (!confirm_large && (plan.summary.needs_confirmation != 0 || !plan.notes.is_empty()))
}

/// Replans under the exclusive home lock. Returns the plan actually applied,
/// or a blocked plan without writing. Does not create an initial history commit.
pub fn apply_register(
    home: &Home,
    plan: &RegisterPlan,
    confirm_large: bool,
) -> Result<RegisterPlan, RegisterError> {
    apply_with_hook(home, plan, confirm_large, |_| Ok(()))
}

// Hook payloads are observed by the regression tests.
#[cfg_attr(not(test), allow(dead_code))]
enum Step<'a> {
    Markers,
    Refreshed(&'a mut RegisterPlan),
    MarkerSynced,
    Folio(Durability),
    Transaction,
    Commit,
}

fn apply_with_hook(
    home: &Home,
    plan: &RegisterPlan,
    confirm_large: bool,
    mut hook: impl FnMut(Step<'_>) -> Result<(), RegistryError>,
) -> Result<RegisterPlan, RegisterError> {
    let lock = home.lock()?;
    let mut current = plan_register(home, &plan.paths)?;
    if blocked(&current, confirm_large) {
        return Ok(current);
    }
    let large_file_bytes = Config::load(home)?.large_file_bytes();
    let mut markers = Vec::new();
    let mut directories = Vec::new();
    let result = (|| {
        hook(Step::Markers)?;
        for entry in &current.entries {
            if entry.creates_marker {
                match OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&entry.path)
                {
                    Ok(file) => {
                        let signature = crate::placement_fs::signature(&file.metadata()?);
                        markers.push((entry.path.clone(), signature));
                        file.sync_all()?;
                        drop(file);
                        if let Some(parent) = entry.path.parent() {
                            crate::fs::sync_directory(parent)?;
                        }
                        hook(Step::MarkerSynced)?;
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        // An appearing marker must be checked for type, readability and size.
        let mut refreshed = plan_register(home, &plan.paths)?;
        hook(Step::Refreshed(&mut refreshed))?;
        current = refreshed;
        if blocked(&current, confirm_large) {
            return Ok(false);
        }
        let mut rows = Vec::new();
        // Each index identifies a request in the refreshed plan. Display paths
        // may change spelling during replan, so never use them as identities.
        for (index, entry) in current.entries.clone().into_iter().enumerate() {
            if entry.status == RegisterStatus::AlreadyRegistered {
                continue;
            }
            let content = match fs::read(&entry.path) {
                Ok(content) => content,
                Err(error) => {
                    current.entries[index].status = RegisterStatus::Refused(io_refusal(&error));
                    current = report(current.entries.clone(), current.paths.clone());
                    return Ok(false);
                }
            };
            // A file may grow even after the metadata recheck. Confirmation
            // also covers the bytes actually captured, before publishing them.
            current.entries[index].size = content.len() as u64;
            if content.len() as u64 >= large_file_bytes {
                current.entries[index].status = RegisterStatus::NeedsConfirmation {
                    size: content.len() as u64,
                };
            }
            current = report(current.entries.clone(), current.paths.clone());
            if blocked(&current, confirm_large) {
                return Ok(false);
            }
            let hash = hash_blob(&content);
            let durability = Durability::DataAndDirectories;
            hook(Step::Folio(durability))?;
            let folio = create_folio(home.path(), &lock, FolioSpec { content }, durability)?;
            directories.push(folio.path().to_path_buf());
            let mut internal = registry::internal_placement(home, folio.id())?;
            let mut copy = NewPlacement {
                path: PlacementPath::resolve(&entry.path)?,
                id: folio.id(),
                kind: PlacementKind::Copy,
                head: None,
                commit_id: None,
                base: None,
                staged: None,
            };
            for row in [&mut internal, &mut copy] {
                let placement = crate::placement::Placement {
                    path: row.path.to_path_buf(),
                    kind: row.kind,
                    head: "main".into(),
                    commit_id: None,
                    base: None,
                };
                let Observation::File { signature } = PlacementFs::new(home)
                    .read(folio.id(), &placement)
                    .map_err(|error| RegisterError::Io(io::Error::other(error)))?
                else {
                    return Err(io::Error::other("registration file changed type").into());
                };
                row.base = Some(PlacementBase { hash, signature });
            }
            rows.push((
                NewFolio {
                    id: folio.id(),
                    format: FolioFormat {
                        layout: FOLIO_LAYOUT_VERSION,
                        algorithm: OBJECT_ALGORITHM.into(),
                    },
                },
                internal,
                copy,
            ));
        }
        if rows.is_empty() {
            return Ok(true);
        }
        // Persist the home entry too when create_folio first created folios/.
        crate::fs::sync_directory(home.path())?;
        hook(Step::Transaction)?;
        let mut registry = Registry::open_or_create(home, &lock)?;
        registry.write(&lock, |tx| {
            for (folio, internal, copy) in &rows {
                tx.add_folio(folio)?;
                tx.add_placement(internal)?;
                tx.add_placement(copy)?;
            }
            hook(Step::Commit)
        })?;
        Ok(true)
    })();
    if !matches!(result, Ok(true)) {
        for directory in directories {
            fs::remove_dir_all(directory)?;
        }
        for (marker, signature) in markers {
            if fs::symlink_metadata(&marker).is_ok_and(|meta| {
                meta.is_file() && crate::placement_fs::signature(&meta) == signature
            }) {
                fs::remove_file(marker)?;
            }
        }
    }
    result.map(|_| current)
}

#[derive(Debug)]
pub enum RegisterError {
    Io(io::Error),
    Config(ConfigError),
    Registry(RegistryError),
    Home(HomeError),
    Folio(FolioDirError),
}
impl fmt::Display for RegisterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => e.fmt(f),
            Self::Config(e) => e.fmt(f),
            Self::Registry(e) => e.fmt(f),
            Self::Home(e) => e.fmt(f),
            Self::Folio(e) => e.fmt(f),
        }
    }
}
impl Error for RegisterError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(match self {
            Self::Io(e) => e,
            Self::Config(e) => e,
            Self::Registry(e) => e,
            Self::Home(e) => e,
            Self::Folio(e) => e,
        })
    }
}
macro_rules! from_error {
    ($source:ty, $variant:ident) => {
        impl From<$source> for RegisterError {
            fn from(error: $source) -> Self {
                Self::$variant(error)
            }
        }
    };
}
from_error!(io::Error, Io);
from_error!(ConfigError, Config);
from_error!(RegistryError, Registry);
from_error!(HomeError, Home);
from_error!(FolioDirError, Folio);

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        root: PathBuf,
        home: Home,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "folt-register-{}",
                crate::folio_dir::FolioId::generate()
            ));
            fs::create_dir(&root).unwrap();
            let home = Home::open_or_create(root.join("home")).unwrap();
            Self { root, home }
        }
        fn folio_count(&self) -> usize {
            fs::read_dir(self.home.path().join("folios"))
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name() != ".new")
                .count()
        }
        fn dir(&self, name: &str) -> PathBuf {
            let path = self.root.join(name);
            fs::create_dir(&path).unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    fn injected() -> RegistryError {
        RegistryError::Io(io::Error::other("injected failure"))
    }

    #[test]
    fn published_folio_sync_failure_rolls_back_directory_markers_and_rows() {
        let fixture = Fixture::new();
        let directory = fixture.dir("directory");
        let plan = plan_register(&fixture.home, [&directory]).unwrap();
        let folios = fixture.home.path().join("folios");
        let result =
            crate::fs::with_sync_failure(&folios, || apply_register(&fixture.home, &plan, false));
        match result {
            Err(RegisterError::Folio(FolioDirError::Io { path, source })) => {
                assert_eq!(path, folios);
                assert_eq!(source.kind(), io::ErrorKind::Other);
                assert_eq!(source.to_string(), "injected directory sync failure");
            }
            other => panic!("expected the published Folio sync error, got {other:?}"),
        }
        assert!(!directory.join(DIRECTORY_MARKER).exists());
        assert_eq!(fixture.folio_count(), 0);
        assert_eq!(fs::read_dir(folios.join(".new")).unwrap().count(), 0);
        let registry = Registry::open_existing(&fixture.home).unwrap();
        assert!(registry.folios().unwrap().is_empty());
        assert!(
            registry
                .placement_at(&PlacementPath::resolve(directory.join(DIRECTORY_MARKER)).unwrap())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn ordinary_failures_roll_back_all_rows_and_remove_only_owned_artifacts() {
        for fail_before_transaction in [false, true] {
            let fixture = Fixture::new();
            let new = fixture.dir("new");
            let existing = fixture.dir("existing");
            let marker = existing.join(DIRECTORY_MARKER);
            fs::write(&marker, b"keep").unwrap();
            let plan = plan_register(&fixture.home, [&new, &existing]).unwrap();
            let result = apply_with_hook(&fixture.home, &plan, false, |step| {
                if matches!(step, Step::Transaction) == fail_before_transaction
                    && matches!(step, Step::Transaction | Step::Commit)
                {
                    Err(injected())
                } else {
                    Ok(())
                }
            });
            assert!(result.is_err());
            assert!(!new.join(DIRECTORY_MARKER).exists());
            assert_eq!(fs::read(&marker).unwrap(), b"keep");
            assert_eq!(fixture.folio_count(), 0);
            let registry = Registry::open_existing(&fixture.home).unwrap();
            assert!(registry.folios().unwrap().is_empty());
            for entry in &plan.entries {
                assert!(
                    registry
                        .placement_at(&PlacementPath::resolve(&entry.path).unwrap())
                        .unwrap()
                        .is_none()
                );
            }
        }
    }

    #[test]
    fn marker_that_appears_during_apply_is_used_and_preserved_on_failure() {
        let fixture = Fixture::new();
        let dir = fixture.dir("directory");
        let marker = dir.join(DIRECTORY_MARKER);
        let plan = plan_register(&fixture.home, [&dir]).unwrap();
        assert!(
            apply_with_hook(&fixture.home, &plan, false, |step| {
                match step {
                    Step::Markers => {
                        fs::write(&marker, b"appeared").unwrap();
                        Ok(())
                    }
                    Step::Transaction => Err(injected()),
                    _ => Ok(()),
                }
            })
            .is_err()
        );
        assert_eq!(fs::read(marker).unwrap(), b"appeared");
        assert_eq!(fixture.folio_count(), 0);
    }

    #[test]
    fn replaced_owned_marker_is_never_removed() {
        let fixture = Fixture::new();
        let dir = fixture.dir("directory");
        let marker = dir.join(DIRECTORY_MARKER);
        let plan = plan_register(&fixture.home, [&dir]).unwrap();
        assert!(
            apply_with_hook(&fixture.home, &plan, false, |step| {
                if matches!(step, Step::Transaction) {
                    let replacement = fixture.root.join("replacement");
                    fs::write(&replacement, b"replacement").unwrap();
                    fs::remove_file(&marker).unwrap();
                    fs::rename(replacement, &marker).unwrap();
                    Err(injected())
                } else {
                    Ok(())
                }
            })
            .is_err()
        );
        assert_eq!(fs::read(marker).unwrap(), b"replacement");
    }

    #[test]
    fn crash_before_transaction_leaves_orphan_and_retry_creates_a_new_folio() {
        let fixture = Fixture::new();
        let path = fixture.root.join("file");
        fs::write(&path, b"content").unwrap();
        let plan = plan_register(&fixture.home, [&path]).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = apply_with_hook(&fixture.home, &plan, false, |step| {
                if matches!(step, Step::Transaction) {
                    panic!("simulated crash");
                }
                Ok(())
            });
        }));
        assert!(result.is_err());
        assert_eq!(fixture.folio_count(), 1);
        assert!(
            Registry::open_existing(&fixture.home)
                .unwrap()
                .folios()
                .unwrap()
                .is_empty()
        );
        apply_register(&fixture.home, &plan, false).unwrap();
        assert_eq!(fixture.folio_count(), 2);
        assert_eq!(
            Registry::open_existing(&fixture.home)
                .unwrap()
                .folios()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn oversized_appearing_marker_blocks_and_cleans_only_the_new_marker() {
        let fixture = Fixture::new();
        fs::write(
            fixture.home.path().join("config.toml"),
            "large_file_bytes = 4\n",
        )
        .unwrap();
        let first = fixture.dir("first");
        let second = fixture.dir("second");
        let marker = second.join(DIRECTORY_MARKER);
        let plan = plan_register(&fixture.home, [&first, &second]).unwrap();
        let result = apply_with_hook(&fixture.home, &plan, false, |step| {
            if matches!(step, Step::Markers) {
                fs::write(&marker, b"large").unwrap();
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(result.summary.needs_confirmation, 1);
        assert!(!first.join(DIRECTORY_MARKER).exists());
        assert_eq!(fs::read(marker).unwrap(), b"large");
        assert!(
            Registry::open_existing(&fixture.home)
                .unwrap()
                .folios()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn case_variant_appearing_marker_replans_and_cleans_up_on_failure() {
        for fail_transaction in [false, true] {
            let fixture = Fixture::new();
            let owned = fixture.dir("owned");
            let appeared = fixture.dir("appeared");
            let marker = appeared.join("__FOLIO__.MD");
            let lowercase = appeared.join(DIRECTORY_MARKER);
            let plan = plan_register(&fixture.home, [&owned, &appeared]).unwrap();
            let mut reached_transaction = false;
            let result = apply_with_hook(&fixture.home, &plan, false, |step| {
                match step {
                    Step::Markers => {
                        fs::write(&marker, b"editor content").unwrap();
                        // A hard link supplies the case-insensitive alias on filesystems
                        // where the two spellings name distinct paths. create_new must
                        // see an existing file and must not claim ownership of it.
                        if !lowercase.exists() {
                            fs::hard_link(&marker, &lowercase).unwrap();
                        }
                    }
                    Step::Refreshed(refreshed) => {
                        let index = refreshed.paths.iter().position(|p| p == &appeared).unwrap();
                        refreshed.entries[index].path = marker.clone();
                    }
                    Step::Transaction => {
                        reached_transaction = true;
                        if fail_transaction {
                            return Err(injected());
                        }
                    }
                    _ => {}
                }
                Ok(())
            });
            assert!(reached_transaction);
            assert_eq!(fs::read(&marker).unwrap(), b"editor content");
            assert_eq!(fs::read(&lowercase).unwrap(), b"editor content");
            if fail_transaction {
                assert!(result.is_err());
                assert!(!owned.join(DIRECTORY_MARKER).exists());
                assert_eq!(fixture.folio_count(), 0);
                assert!(
                    Registry::open_existing(&fixture.home)
                        .unwrap()
                        .folios()
                        .unwrap()
                        .is_empty()
                );
            } else {
                let applied = result.unwrap();
                assert_eq!(applied.entries[1].path, marker);
                assert_eq!(applied.entries[1].size, 14);
                assert!(!applied.entries[1].creates_marker);
                let registry = Registry::open_existing(&fixture.home).unwrap();
                let row = registry
                    .placement_at(&PlacementPath::resolve(&marker).unwrap())
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    fs::read(
                        fixture
                            .home
                            .path()
                            .join("folios")
                            .join(row.id.to_string())
                            .join(crate::layout::CONTENT_FILE)
                    )
                    .unwrap(),
                    b"editor content"
                );
            }
        }
    }

    #[test]
    fn registration_syncs_markers_and_uses_durable_folios_before_transaction() {
        let fixture = Fixture::new();
        let directory = fixture.dir("directory");
        let file = fixture.root.join("file");
        fs::write(&file, b"content").unwrap();
        let plan = plan_register(&fixture.home, [&directory, &file]).unwrap();
        let mut marker_synced = false;
        let mut durable_folios = 0;
        let mut committed = false;
        apply_with_hook(&fixture.home, &plan, false, |step| {
            match step {
                Step::MarkerSynced => marker_synced = true,
                Step::Folio(durability) => {
                    assert!(marker_synced);
                    assert_eq!(durability, Durability::DataAndDirectories);
                    durable_folios += 1;
                }
                Step::Transaction => {
                    assert!(marker_synced);
                    assert_eq!(durable_folios, 2);
                    assert!(
                        Registry::open_existing(&fixture.home)
                            .unwrap()
                            .folios()
                            .unwrap()
                            .is_empty()
                    );
                }
                Step::Commit => committed = true,
                _ => {}
            }
            Ok(())
        })
        .unwrap();
        assert!(committed);
    }

    #[test]
    fn permission_error_maps_to_unreadable() {
        assert_eq!(
            io_refusal(&io::Error::from(io::ErrorKind::PermissionDenied)),
            RefusalReason::Unreadable
        );
    }
}
