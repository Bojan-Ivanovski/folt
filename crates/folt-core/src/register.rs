//! Explicit-path registration. Planning reads metadata and a bounded Markdown
//! prefix. Apply atomically writes missing folio-name keys before hashing and
//! ingesting those bytes, and registers the batch in one transaction.
//!
//! Ordinary failures restore rewritten files if still unchanged, and remove this
//! call's markers and Folio directories. Failed restores report that the file keeps
//! its name. Writes sync a temporary file and its directory, change the inode,
//! and refuse hard links; permissions are copied, but ownership/ACLs/xattrs are not.
//! A crash before the transaction can leave names in files and orphan directories.
//! A fresh plan reads those names on retry; discovery/repair owns the orphans,
//! and retry creates new Folios rather than adopting them.
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::config::{Config, ConfigError};
use crate::folio_dir::FolioId;
use crate::folio_dir::{Durability, FolioDirError, FolioSpec, create_folio};
use crate::folio_name::{FolioName, FolioNameError, StemError};
use crate::front_matter::{self, Edit, FrontMatterError};
use crate::home::{Home, HomeError, HomeLock};
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

/// One path and its optional explicit name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterRequest {
    pub path: PathBuf,
    pub name: Option<FolioName>,
}
impl<P: AsRef<Path>> From<P> for RegisterRequest {
    fn from(path: P) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            name: None,
        }
    }
}
impl RegisterRequest {
    pub fn new(path: impl AsRef<Path>, name: FolioName) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            name: Some(name),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NameSource {
    FrontMatter,
    Option,
    FileName { suffix: u32 },
}

/// Exact prefix edit; apply must recompute it against the captured full bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NameWrite {
    None,
    Insert(Edit),
    InsertBlock(Edit),
    Replace { old: String, edit: Edit },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NameHolder {
    Folio(FolioId),
    Path(PathBuf),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RefusalReason {
    /// Name the target explicitly instead of its symlink.
    Symlink,
    NotAFile,
    Missing,
    Unreadable,
    InsideHome,
    InsideDirectoryFolio,
    ConflictingNames,
    FileChanged,
    DestinationChanged {
        path: PathBuf,
    },
    /// Renames belong to issue #155.
    NameOnRegisteredPath,
    NameTaken {
        name: FolioName,
        holder: NameHolder,
    },
    InvalidFrontMatterName(FolioNameError),
    FrontMatter(FrontMatterError),
    NameOptionWithFrontMatter,
    NoUsableStem(StemError),
    NameNotWritable {
        reason: String,
    },
}

impl fmt::Display for RefusalReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NameOnRegisteredPath => {
                f.write_str("--name on a registered path is refused; use rename (#155)")
            }
            Self::NameOptionWithFrontMatter => {
                f.write_str("--name is refused when folio-name already exists; use rename (#155)")
            }
            Self::ConflictingNames => f.write_str("one path requested with different names"),
            Self::DestinationChanged { path } => {
                write!(f, "{}: destination changed since the plan", path.display())
            }
            Self::NoUsableStem(error) => error.fmt(f),
            Self::FrontMatter(error) => error.fmt(f),
            Self::InvalidFrontMatterName(error) => write!(f, "invalid folio-name: {error}"),
            Self::NameNotWritable { reason } => write!(f, "cannot write folio-name: {reason}"),
            Self::NameTaken { name, holder } => write!(f, "name {name} is taken by {holder:?}"),
            other => write!(f, "{other:?}"),
        }
    }
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
    pub name: Option<FolioName>,
    pub name_source: Option<NameSource>,
    pub name_write: NameWrite,
    signature: Option<crate::placement::Signature>,
    prefix: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RegisterSummary {
    pub new: usize,
    pub already_registered: usize,
    pub refused: usize,
    pub needs_confirmation: usize,
    pub total_bytes: u64,
    pub files_to_write: usize,
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
    requests: Vec<RegisterRequest>,
    // Resolved paths aligned with original requests, before entry deduplication.
    destinations: Vec<PathBuf>,
}

pub fn plan_register(
    home: &Home,
    paths: impl IntoIterator<Item = impl Into<RegisterRequest>>,
) -> Result<RegisterPlan, RegisterError> {
    let config = Config::load(home)?;
    let registry = Registry::open_existing(home)?;
    let home_path = fs::canonicalize(home.path())?;
    let mut entries: Vec<RegisterEntry> = Vec::new();
    let mut requests: Vec<RegisterRequest> = Vec::new();
    let mut seen = HashMap::<PathBuf, usize>::new();
    let mut replay = Vec::new();
    let mut destinations = Vec::new();
    for input in paths {
        let mut request = input.into();
        let absolute = if request.path.is_absolute() {
            request.path.clone()
        } else {
            std::env::current_dir()?.join(&request.path)
        };
        request.path = absolute.clone();
        replay.push(request.clone());
        let resolved = PlacementPath::resolve(&absolute);
        let path = resolved
            .as_ref()
            .map_or_else(|_| absolute.clone(), PlacementPath::to_path_buf);
        let directory = fs::symlink_metadata(&absolute).is_ok_and(|m| m.is_dir());
        destinations.push(if directory {
            path.parent()
                .expect("directory marker parent")
                .to_path_buf()
        } else {
            destination_path(&path).to_path_buf()
        });
        if let Some(&index) = seen.get(&path) {
            let previous: &mut RegisterRequest = &mut requests[index];
            if previous.name.is_some() && request.name.is_some() && previous.name != request.name {
                entries[index].status = RegisterStatus::Refused(RefusalReason::ConflictingNames);
                continue;
            }
            if previous.name.is_none() {
                previous.name = request.name;
            }
            // Preserve directory intent in either request order.
            if directory {
                previous.path = absolute;
            }
            if !matches!(
                entries[index].status,
                RegisterStatus::Refused(RefusalReason::ConflictingNames)
            ) {
                entries[index] =
                    inspect_entry(&home_path, &registry, &config, &previous.path, path)?;
            }
            continue;
        }
        seen.insert(path.clone(), entries.len());
        entries.push(inspect_entry(
            &home_path, &registry, &config, &absolute, path,
        )?);
        requests.push(request);
    }
    resolve_names(&mut entries, &requests, &registry)?;
    Ok(report(entries, replay, destinations))
}

fn refuse_name(entry: &mut RegisterEntry, reason: RefusalReason) {
    entry.status = RegisterStatus::Refused(reason);
    entry.name = None;
    entry.name_source = None;
    entry.name_write = NameWrite::None;
}

fn markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
}

fn holder(
    registry: &Registry,
    assigned: &HashMap<FolioName, PathBuf>,
    name: &FolioName,
) -> Result<Option<NameHolder>, RegisterError> {
    // Exactly one indexed lookup for each candidate, including local collisions.
    let registered = registry.find_by_name(name)?;
    Ok(registered
        .map(NameHolder::Folio)
        .or_else(|| assigned.get(name).cloned().map(NameHolder::Path)))
}

fn resolve_names(
    entries: &mut [RegisterEntry],
    requests: &[RegisterRequest],
    registry: &Registry,
) -> Result<(), RegisterError> {
    let mut prefixes = HashMap::new();
    let mut assigned = HashMap::new();
    // Explicit options first, then front matter, preserving request order in each group.
    for (index, (entry, request)) in entries.iter_mut().zip(requests).enumerate() {
        if entry.status == RegisterStatus::AlreadyRegistered && request.name.is_some() {
            refuse_name(entry, RefusalReason::NameOnRegisteredPath);
        }
        if !matches!(
            entry.status,
            RegisterStatus::New | RegisterStatus::NeedsConfirmation { .. }
        ) {
            continue;
        }
        let existing = if markdown(&entry.path) {
            let mut bytes = Vec::new();
            if !entry.creates_marker {
                let read = File::open(&entry.path).and_then(|file| {
                    file.take((front_matter::SCAN_LIMIT + 8) as u64)
                        .read_to_end(&mut bytes)
                });
                if let Err(error) = read {
                    refuse_name(entry, io_refusal(&error));
                    continue;
                }
            }
            let name = match front_matter::read_name(&bytes) {
                Ok(name) => name.map(|name| name.into_owned()),
                Err(error) => {
                    refuse_name(entry, RefusalReason::FrontMatter(error));
                    continue;
                }
            };
            entry.prefix = Some(bytes.clone());
            prefixes.insert(index, bytes);
            name
        } else {
            None
        };
        if let Some(value) = existing {
            let name = match FolioName::parse(&value) {
                Ok(name) => name,
                Err(error) => {
                    refuse_name(entry, RefusalReason::InvalidFrontMatterName(error));
                    continue;
                }
            };
            if request.name.is_some() {
                refuse_name(entry, RefusalReason::NameOptionWithFrontMatter);
                continue;
            }
            entry.name = Some(name);
            entry.name_source = Some(NameSource::FrontMatter);
        } else if let Some(name) = &request.name {
            entry.name = Some(name.clone());
            entry.name_source = Some(NameSource::Option);
        }
    }
    for option_pass in [true, false] {
        for entry in entries.iter_mut() {
            if entry.name.is_none()
                || matches!(entry.name_source, Some(NameSource::Option)) != option_pass
            {
                continue;
            }
            let name = entry.name.clone().expect("named entry");
            if let Some(holder) = holder(registry, &assigned, &name)? {
                refuse_name(entry, RefusalReason::NameTaken { name, holder });
            } else {
                assigned.insert(name, entry.path.clone());
            }
        }
    }
    let mut counters = HashMap::<FolioName, u32>::new();
    for (index, (entry, request)) in entries.iter_mut().zip(requests).enumerate() {
        if !matches!(
            entry.status,
            RegisterStatus::New | RegisterStatus::NeedsConfirmation { .. }
        ) {
            continue;
        }
        if entry.name.is_none() {
            let directory = fs::symlink_metadata(&request.path).is_ok_and(|m| m.is_dir());
            let stem = if directory {
                entry.path.parent().and_then(Path::file_name)
            } else {
                request.path.file_stem()
            };
            let base = match FolioName::from_stem(stem.unwrap_or_default()) {
                Ok(name) => name,
                Err(error) => {
                    refuse_name(entry, RefusalReason::NoUsableStem(error));
                    continue;
                }
            };
            let counter = counters.entry(base.clone()).or_default();
            loop {
                let name = if *counter == 0 {
                    base.clone()
                } else {
                    base.with_suffix(*counter)
                };
                let suffix = *counter;
                *counter = counter
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("name suffix exhausted"))?;
                if holder(registry, &assigned, &name)?.is_none() {
                    assigned.insert(name.clone(), entry.path.clone());
                    entry.name = Some(name);
                    entry.name_source = Some(NameSource::FileName { suffix });
                    break;
                }
            }
        }
        if entry.name_source == Some(NameSource::FrontMatter) {
            continue;
        }
        if let Some(bytes) = prefixes.get(&index) {
            let name = entry.name.as_ref().expect("resolved name");
            let edit = match front_matter::set_name(bytes, Some(name.as_str()), true) {
                Ok(Some(edit)) => edit,
                Ok(None) => continue,
                Err(error) => {
                    refuse_name(entry, RefusalReason::FrontMatter(error));
                    continue;
                }
            };
            entry.name_write = if front_matter::locate(bytes)
                .map_err(|e| io::Error::other(e.to_string()))?
                .is_some()
            {
                NameWrite::Insert(edit)
            } else {
                NameWrite::InsertBlock(edit)
            };
            if !entry.creates_marker {
                let meta = fs::metadata(&entry.path)?;
                let reason = if meta.permissions().readonly() {
                    Some("read-only file".to_owned())
                } else {
                    crate::placement_fs::require_single_link(&entry.path)
                        .err()
                        .map(|e| e.to_string())
                };
                if let Some(reason) = reason {
                    refuse_name(entry, RefusalReason::NameNotWritable { reason });
                }
            }
        }
    }
    Ok(())
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
        name: None,
        name_source: None,
        name_write: NameWrite::None,
        signature: None,
        prefix: None,
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
    entry.signature = Some(crate::placement_fs::signature(&metadata));
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

fn report(
    entries: Vec<RegisterEntry>,
    requests: Vec<RegisterRequest>,
    destinations: Vec<PathBuf>,
) -> RegisterPlan {
    let mut summary = RegisterSummary::default();
    for entry in &entries {
        if entry.name_write != NameWrite::None {
            summary.files_to_write += 1;
        }
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
        requests,
        destinations,
    }
}

impl fmt::Display for RegisterPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for entry in &self.entries {
            if let RegisterStatus::Refused(reason) = &entry.status {
                writeln!(f, "{}: {reason}", entry.path.display())?;
            } else {
                writeln!(f, "{}: {:?}", entry.path.display(), entry.status)?;
            }
            if let Some(name) = &entry.name {
                writeln!(f, "  name: {name}")?;
                if entry.name_write != NameWrite::None {
                    writeln!(
                        f,
                        "will write folio-name: \"{name}\" into {}",
                        entry.path.display()
                    )?;
                }
            }
        }
        writeln!(f, "{} files to write", self.summary.files_to_write)
    }
}

fn is_marker(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case(DIRECTORY_MARKER))
}

fn destination_path(path: &Path) -> &Path {
    if is_marker(path) {
        // Marker spelling may change during replan; its directory must not.
        path.parent().expect("directory marker parent")
    } else {
        path
    }
}

fn destination(entry: &RegisterEntry) -> &Path {
    destination_path(&entry.path)
}

fn block_changed(previous: &RegisterPlan, mut current: RegisterPlan) -> RegisterPlan {
    let mut previous_by_destination = HashMap::<_, Vec<_>>::new();
    for entry in &previous.entries {
        previous_by_destination
            .entry(destination(entry))
            .or_default()
            .push(entry);
    }
    for entry in &mut current.entries {
        let request_moved = previous
            .destinations
            .iter()
            .zip(&current.destinations)
            .any(|(old, new)| new == destination(entry) && old != new);
        if request_moved {
            let path = entry.path.clone();
            refuse_name(entry, RefusalReason::DestinationChanged { path });
            continue;
        }
        let old = previous_by_destination
            .get(destination(entry))
            .and_then(|candidates| {
                // Distinct case-variant marker files retain their full path identities.
                // Accept a changed marker spelling only for an unambiguous destination.
                candidates
                    .iter()
                    .copied()
                    .find(|old| old.path == entry.path)
                    .or(match candidates.as_slice() {
                        [old] if is_marker(&old.path) && is_marker(&entry.path) => Some(*old),
                        _ => None,
                    })
            });
        let Some(old) = old else {
            let path = entry.path.clone();
            refuse_name(entry, RefusalReason::DestinationChanged { path });
            continue;
        };
        if matches!(
            entry.status,
            RegisterStatus::New | RegisterStatus::NeedsConfirmation { .. }
        ) && old.prefix.is_some()
            && old.signature.is_some()
            && (old.signature != entry.signature || old.prefix != entry.prefix)
        {
            refuse_name(entry, RefusalReason::FileChanged);
        }
    }
    report(current.entries, current.requests, current.destinations)
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegisterLinkResult {
    Linked,
    LeftAsCopy { reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterLinkOutcome {
    pub registration: RegisterPlan,
    pub paths: Vec<(PathBuf, RegisterLinkResult)>,
}

/// Registers the batch before converting each placement independently. The home
/// lock covers probing, registration and conversion; conversion errors keep rows.
pub fn apply_register_links(
    home: &Home,
    plan: &RegisterPlan,
    confirm_large: bool,
) -> Result<RegisterLinkOutcome, RegisterError> {
    apply_links_with_hook(home, plan, confirm_large, |step| {
        if let LinkStep::Probe(path) = step {
            PlacementFs::new(home).probe_link(path)?;
        }
        Ok(())
    })
}

#[cfg_attr(not(test), allow(dead_code))]
enum LinkStep<'a> {
    Probe(&'a Path),
    Registered,
    Convert(&'a Path),
}

fn apply_links_with_hook(
    home: &Home,
    plan: &RegisterPlan,
    confirm_large: bool,
    mut hook: impl FnMut(LinkStep<'_>) -> Result<(), RegisterError>,
) -> Result<RegisterLinkOutcome, RegisterError> {
    let lock = home.lock()?;
    let current = block_changed(plan, plan_register(home, plan.requests.clone())?);
    if blocked(&current, confirm_large) {
        return Ok(RegisterLinkOutcome {
            registration: current,
            paths: Vec::new(),
        });
    }
    for entry in &current.entries {
        hook(LinkStep::Probe(&entry.path))?;
    }
    let registration = apply_locked(home, &current, confirm_large, &lock, |_| Ok(()))?;
    if blocked(&registration, confirm_large) {
        return Ok(RegisterLinkOutcome {
            registration,
            paths: Vec::new(),
        });
    }
    hook(LinkStep::Registered)?;
    let paths = registration
        .entries
        .iter()
        .map(|entry| {
            let result = hook(LinkStep::Convert(&entry.path))
                .map_err(|error| error.to_string())
                .and_then(|()| {
                    register_link(home, &entry.path, &lock).map_err(|error| error.to_string())
                });
            let result = match result {
                Ok(()) => RegisterLinkResult::Linked,
                Err(reason) => RegisterLinkResult::LeftAsCopy { reason },
            };
            (entry.path.clone(), result)
        })
        .collect();
    Ok(RegisterLinkOutcome {
        registration,
        paths,
    })
}

fn register_link(home: &Home, path: &Path, lock: &HomeLock) -> Result<(), Box<dyn Error>> {
    use crate::placement_rules::{apply_convert, gather_convert, judge_convert};
    let mut registry = Registry::open_or_create(home, lock)?;
    let row = registry
        .placement_at(&PlacementPath::resolve(path)?)?
        .ok_or_else(|| io::Error::other("placement is missing; run repair (#38)"))?;
    let folio = registry
        .folio_row(row.id)?
        .ok_or_else(|| io::Error::other("Folio is missing"))?;
    let directory = crate::folio_dir::validate(home.path(), &folio)?;
    let objects = crate::store::ObjectStore::open(&directory, &folio.format)?;
    let refs = crate::refs::RefStore::open(&directory, &folio.format)?;
    let fs = PlacementFs::new(home);
    let facts = gather_convert(&row, &fs, |_| Ok(&refs))?;
    if row.kind == PlacementKind::Link && facts.observation == Observation::IntactLink {
        return Ok(());
    }
    let plan = judge_convert(&facts, PlacementKind::Link, None, None)?;
    apply_convert(&plan, &fs, &objects, &mut registry, lock)?;
    Ok(())
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
    hook: impl FnMut(Step<'_>) -> Result<(), RegistryError>,
) -> Result<RegisterPlan, RegisterError> {
    let lock = home.lock()?;
    apply_locked(home, plan, confirm_large, &lock, hook)
}

fn apply_locked(
    home: &Home,
    plan: &RegisterPlan,
    confirm_large: bool,
    lock: &HomeLock,
    mut hook: impl FnMut(Step<'_>) -> Result<(), RegistryError>,
) -> Result<RegisterPlan, RegisterError> {
    let mut current = block_changed(plan, plan_register(home, plan.requests.clone())?);
    if blocked(&current, confirm_large) {
        return Ok(current);
    }
    let large_file_bytes = Config::load(home)?.large_file_bytes();
    let mut markers = Vec::new();
    let mut directories = Vec::new();
    let mut rewritten = Vec::new();
    let result: Result<bool, RegisterError> = (|| {
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
        let mut refreshed = plan_register(home, plan.requests.clone())?;
        hook(Step::Refreshed(&mut refreshed))?;
        current = block_changed(&current, refreshed);
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
            let mut content = match fs::read(&entry.path) {
                Ok(content) => content,
                Err(error) => {
                    refuse_name(&mut current.entries[index], io_refusal(&error));
                    current = report(
                        current.entries.clone(),
                        current.requests.clone(),
                        current.destinations.clone(),
                    );
                    return Ok(false);
                }
            };
            if let Some(expected) = &entry.signature {
                let metadata = fs::metadata(&entry.path)?;
                if &crate::placement_fs::signature(&metadata) != expected
                    || entry.prefix.as_ref().is_some_and(|prefix| {
                        content.get(..content.len().min(front_matter::SCAN_LIMIT + 8))
                            != Some(prefix.as_slice())
                    })
                {
                    refuse_name(&mut current.entries[index], RefusalReason::FileChanged);
                    current = report(
                        current.entries.clone(),
                        current.requests.clone(),
                        current.destinations.clone(),
                    );
                    return Ok(false);
                }
            }
            if entry.name_write != NameWrite::None {
                let edit = front_matter::set_name(
                    &content,
                    entry.name.as_ref().map(FolioName::as_str),
                    true,
                )
                .map_err(|error| io::Error::other(format!("{}: {error}", entry.path.display())))?;
                let planned_edit = match &entry.name_write {
                    NameWrite::Insert(edit)
                    | NameWrite::InsertBlock(edit)
                    | NameWrite::Replace { edit, .. } => edit,
                    NameWrite::None => unreachable!(),
                };
                if edit.as_ref() != Some(planned_edit) {
                    refuse_name(&mut current.entries[index], RefusalReason::FileChanged);
                    current = report(
                        current.entries.clone(),
                        current.requests.clone(),
                        current.destinations.clone(),
                    );
                    return Ok(false);
                }
                let updated = front_matter::apply_edit(&content, planned_edit)
                    .map_err(|error| io::Error::other(error.to_string()))?;
                let Some(expected) = entry.signature.as_ref() else {
                    refuse_name(&mut current.entries[index], RefusalReason::FileChanged);
                    current = report(
                        current.entries.clone(),
                        current.requests.clone(),
                        current.destinations.clone(),
                    );
                    return Ok(false);
                };
                let write = crate::placement_fs::atomic_rewrite(
                    &entry.path,
                    &updated,
                    expected,
                    |signature| {
                        rewritten.push((entry.path.clone(), content.clone(), signature.clone()));
                        if let Some((_, marker_signature)) =
                            markers.iter_mut().find(|(path, _)| path == &entry.path)
                        {
                            *marker_signature = signature;
                        }
                    },
                );
                if let Err(error) = write {
                    if error.kind() == io::ErrorKind::InvalidData {
                        refuse_name(&mut current.entries[index], RefusalReason::FileChanged);
                        current = report(
                            current.entries.clone(),
                            current.requests.clone(),
                            current.destinations.clone(),
                        );
                        return Ok(false);
                    }
                    return Err(error.into());
                }
                content = updated;
            }
            // A file may grow even after the metadata recheck. Confirmation
            // also covers the bytes actually captured, before publishing them.
            current.entries[index].size = content.len() as u64;
            if content.len() as u64 >= large_file_bytes {
                current.entries[index].status = RegisterStatus::NeedsConfirmation {
                    size: content.len() as u64,
                };
            }
            current = report(
                current.entries.clone(),
                current.requests.clone(),
                current.destinations.clone(),
            );
            if blocked(&current, confirm_large) {
                return Ok(false);
            }
            let hash = hash_blob(&content);
            let durability = Durability::DataAndDirectories;
            hook(Step::Folio(durability))?;
            let folio = create_folio(home.path(), lock, FolioSpec { content }, durability)?;
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
                entry.name.clone().expect("new entry has a name"),
            ));
        }
        if rows.is_empty() {
            return Ok(true);
        }
        // Persist the home entry too when create_folio first created folios/.
        crate::fs::sync_directory(home.path())?;
        hook(Step::Transaction)?;
        let mut registry = Registry::open_or_create(home, lock)?;
        registry.write(lock, |tx| {
            for (folio, internal, copy, name) in &rows {
                tx.add_folio(folio)?;
                tx.set_name(folio.id, name)?;
                tx.add_placement(internal)?;
                tx.add_placement(copy)?;
            }
            hook(Step::Commit)
        })?;
        Ok(true)
    })();
    if matches!(result, Ok(true)) {
        // The empty file used to create an owned marker was only an
        // intermediate state, not an input snapshot for the returned plan.
        for entry in &mut current.entries {
            if markers.iter().any(|(path, _)| path == &entry.path) {
                entry.signature = None;
            }
        }
    }
    if !matches!(result, Ok(true)) {
        let mut failures = Vec::new();
        for (path, original, signature) in rewritten.into_iter().rev() {
            if !fs::symlink_metadata(&path).is_ok_and(|meta| {
                meta.is_file() && crate::placement_fs::signature(&meta) == signature
            }) {
                failures.push(format!(
                    "{} keeps the name: file changed after our write",
                    path.display()
                ));
                continue;
            }
            if let Err(error) =
                crate::placement_fs::atomic_rewrite(&path, &original, &signature, |restored| {
                    if let Some((_, marker_signature)) =
                        markers.iter_mut().find(|(marker, _)| marker == &path)
                    {
                        *marker_signature = restored;
                    }
                })
            {
                failures.push(format!(
                    "{} keeps the name: restore failed: {error}",
                    path.display()
                ));
            }
        }
        for directory in directories {
            if let Err(error) = fs::remove_dir_all(&directory) {
                failures.push(format!("{}: {error}", directory.display()));
            }
        }
        for (marker, signature) in markers {
            if fs::symlink_metadata(&marker).is_ok_and(|meta| {
                meta.is_file() && crate::placement_fs::signature(&meta) == signature
            }) && let Err(error) = fs::remove_file(&marker)
            {
                failures.push(format!("{}: {error}", marker.display()));
            }
        }
        if !failures.is_empty() {
            let cause = match &result {
                Err(error) => error.to_string(),
                _ => "registration blocked".into(),
            };
            return Err(io::Error::other(format!("{cause}; {}", failures.join("; "))).into());
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
    Placement(crate::placement::PlacementError),
}
impl fmt::Display for RegisterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => e.fmt(f),
            Self::Config(e) => e.fmt(f),
            Self::Registry(e) => e.fmt(f),
            Self::Home(e) => e.fmt(f),
            Self::Folio(e) => e.fmt(f),
            Self::Placement(e) => e.fmt(f),
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
            Self::Placement(e) => e,
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
from_error!(crate::placement::PlacementError, Placement);

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved_path(path: impl AsRef<Path>) -> PathBuf {
        fs::canonicalize(path).unwrap()
    }

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
            let root = resolved_path(root);
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
    fn name_writes_restore_the_whole_batch_at_transaction_and_commit_failures() {
        for fail_commit in [false, true] {
            let fixture = Fixture::new();
            let paths = [fixture.root.join("one.md"), fixture.root.join("two.md")];
            let originals: [&[u8]; 2] = [b"first body", b"---\ntitle: keep\n---\nsecond body"];
            for (path, bytes) in paths.iter().zip(originals) {
                fs::write(path, bytes).unwrap();
            }
            let directory = fixture.dir("directory");
            let marker = directory.join(DIRECTORY_MARKER);
            let plan = plan_register(&fixture.home, [&paths[0], &paths[1], &directory]).unwrap();
            let mut reached = false;
            let error = apply_with_hook(&fixture.home, &plan, false, |step| {
                if matches!(step, Step::Transaction) && !fail_commit
                    || matches!(step, Step::Commit) && fail_commit
                {
                    reached = true;
                    for (path, expected) in paths.iter().zip(["one", "two"]) {
                        assert_eq!(
                            front_matter::read_name(&fs::read(path).unwrap())
                                .unwrap()
                                .as_deref(),
                            Some(expected)
                        );
                    }
                    assert_eq!(
                        fs::read(&marker).unwrap(),
                        b"---\nfolio-name: \"directory\"\n---\n"
                    );
                    return Err(injected());
                }
                Ok(())
            })
            .unwrap_err();
            assert!(reached);
            assert!(error.to_string().contains("injected failure"));
            for (path, bytes) in paths.iter().zip(originals) {
                assert_eq!(fs::read(path).unwrap(), bytes);
            }
            assert!(!marker.exists());
            assert_eq!(fixture.folio_count(), 0);
            assert!(
                Registry::open_existing(&fixture.home)
                    .unwrap()
                    .folios()
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn restore_preserves_external_edits_and_reports_the_retained_name() {
        let fixture = Fixture::new();
        let path = fixture.root.join("file.md");
        fs::write(&path, b"body").unwrap();
        let plan = plan_register(&fixture.home, [&path]).unwrap();
        let error = apply_with_hook(&fixture.home, &plan, false, |step| {
            if matches!(step, Step::Transaction) {
                assert_eq!(
                    front_matter::read_name(&fs::read(&path).unwrap())
                        .unwrap()
                        .as_deref(),
                    Some("file")
                );
                fs::write(&path, b"---\nfolio-name: file\n---\nuser edit").unwrap();
                return Err(injected());
            }
            Ok(())
        })
        .unwrap_err();
        assert!(error.to_string().contains("keeps the name"));
        assert!(
            error
                .to_string()
                .contains(&plan.entries[0].path.display().to_string())
        );
        assert_eq!(
            fs::read(path).unwrap(),
            b"---\nfolio-name: file\n---\nuser edit"
        );
        assert_eq!(fixture.folio_count(), 0);
        assert!(
            Registry::open_existing(&fixture.home)
                .unwrap()
                .folios()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn changed_after_refresh_blocks_without_writing_and_restores_earlier_files() {
        let fixture = Fixture::new();
        let first = fixture.root.join("first.md");
        let second = fixture.root.join("second.md");
        fs::write(&first, b"first body").unwrap();
        fs::write(&second, b"second body").unwrap();
        let plan = plan_register(&fixture.home, [&first, &second]).unwrap();
        let result = apply_with_hook(&fixture.home, &plan, false, |step| {
            if matches!(step, Step::Refreshed(_)) {
                fs::write(&second, b"user edit").unwrap();
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(
            result.entries[1].status,
            RegisterStatus::Refused(RefusalReason::FileChanged)
        );
        assert_eq!(fs::read(first).unwrap(), b"first body");
        assert_eq!(fs::read(second).unwrap(), b"user edit");
        assert_eq!(fixture.folio_count(), 0);
    }

    #[test]
    fn missing_refreshed_marker_signature_blocks_and_rolls_back_earlier_files() {
        let fixture = Fixture::new();
        let first = fixture.root.join("first.md");
        let notes = fixture.dir("notes");
        let original = b"body";
        fs::write(&first, original).unwrap();
        let plan = plan_register(&fixture.home, [&first, &notes]).unwrap();
        let first = &plan.entries[0].path;
        let marker = &plan.entries[1].path;
        let mut folios_started = 0;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            apply_with_hook(&fixture.home, &plan, false, |step| {
                match step {
                    Step::MarkerSynced => fs::remove_file(marker).unwrap(),
                    Step::Refreshed(refreshed) => {
                        assert!(refreshed.entries[1].signature.is_none());
                        fs::write(marker, b"").unwrap();
                    }
                    Step::Folio(_) => {
                        folios_started += 1;
                        assert_ne!(fs::read(first).unwrap(), original);
                    }
                    _ => {}
                }
                Ok(())
            })
        }));
        let result = result.expect("a missing refreshed signature must not panic");
        if let Ok(blocked) = result {
            assert_eq!(
                blocked.entries[1].status,
                RegisterStatus::Refused(RefusalReason::FileChanged)
            );
        }
        assert_eq!(folios_started, 1);
        assert_eq!(fs::read(first).unwrap(), original);
        assert_eq!(fixture.folio_count(), 0);
        assert_eq!(
            fs::read_dir(fixture.home.path().join("folios/.new"))
                .unwrap()
                .count(),
            0
        );
        assert!(
            Registry::open_existing(&fixture.home)
                .unwrap()
                .folios()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn captured_size_confirmation_restores_names_and_removes_new_markers() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.home.path().join("folios")).unwrap();
        fs::write(
            fixture.home.path().join("config.toml"),
            "large_file_bytes = 20\n",
        )
        .unwrap();
        let directory = fixture.dir("directory");
        let path = fixture.root.join("file.md");
        fs::write(&path, b"body").unwrap();
        let plan = plan_register(&fixture.home, [&directory, &path]).unwrap();
        assert_eq!(plan.summary.needs_confirmation, 0);
        let blocked = apply_register(&fixture.home, &plan, false).unwrap();
        assert_eq!(blocked.summary.needs_confirmation, 1);
        assert!(!directory.join(DIRECTORY_MARKER).exists());
        assert_eq!(fs::read(&path).unwrap(), b"body");
        assert_eq!(fixture.folio_count(), 0);
        // Replan because restoring the original bytes changes the signature.
        let retry = plan_register(&fixture.home, [&directory, &path]).unwrap();
        let confirmed = apply_register(&fixture.home, &retry, true).unwrap();
        assert_eq!(confirmed.summary.refused, 0);
        assert!(directory.join(DIRECTORY_MARKER).exists());
        assert_eq!(fixture.folio_count(), 2);
    }

    #[test]
    fn metadata_failure_after_name_publish_restores_or_reports_retained_name() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.home.path().join("folios")).unwrap();
        let path = fixture.root.join("file.md");
        let original = b"body";
        fs::write(&path, original).unwrap();
        let plan = plan_register(&fixture.home, [&path]).unwrap();
        let path = &plan.entries[0].path;
        let error = crate::placement_fs::with_rewrite_metadata_failure(path, || {
            apply_register(&fixture.home, &plan, false)
        })
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("injected rewrite metadata failure"));
        if fs::read(path).unwrap() != original {
            assert!(
                message.contains(&format!("{} keeps the name:", path.display())),
                "a retained name must be reported with its path: {message}"
            );
        }
        assert_eq!(fixture.folio_count(), 0);
        assert!(
            Registry::open_existing(&fixture.home)
                .unwrap()
                .folios()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn directory_sync_failure_after_name_publish_still_attempts_restore() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.home.path().join("folios")).unwrap();
        let path = fixture.root.join("file.md");
        fs::write(&path, b"body").unwrap();
        let plan = plan_register(&fixture.home, [&path]).unwrap();
        let error = crate::fs::with_sync_failure(&fixture.root, || {
            apply_register(&fixture.home, &plan, false)
        })
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("injected directory sync failure")
        );
        assert!(error.to_string().contains("restore failed"));
        // Both renames happened even though their directory syncs failed.
        assert_eq!(fs::read(path).unwrap(), b"body");
        assert_eq!(fixture.folio_count(), 0);
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
                assert_eq!(path, resolved_path(&folios));
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
                        fs::write(&marker, b"---\nfolio-name: appeared\n---\neditor content")
                            .unwrap();
                        // A hard link supplies the case-insensitive alias on filesystems
                        // where the two spellings name distinct paths. create_new must
                        // see an existing file and must not claim ownership of it.
                        if !lowercase.exists() {
                            fs::hard_link(&marker, &lowercase).unwrap();
                        }
                    }
                    Step::Refreshed(refreshed) => {
                        let index = refreshed
                            .requests
                            .iter()
                            .position(|p| p.path == appeared)
                            .unwrap();
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
            assert_eq!(
                fs::read(&marker).unwrap(),
                b"---\nfolio-name: appeared\n---\neditor content"
            );
            assert_eq!(
                fs::read(&lowercase).unwrap(),
                b"---\nfolio-name: appeared\n---\neditor content"
            );
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
                assert_eq!(applied.entries[1].path, resolved_path(&marker));
                assert_eq!(
                    applied.entries[1].size,
                    b"---\nfolio-name: appeared\n---\neditor content".len() as u64
                );
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
                    b"---\nfolio-name: appeared\n---\neditor content"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn collapsed_directory_request_is_blocked_before_creating_markers() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        let a = fixture.dir("a");
        let b = fixture.dir("b");
        fs::create_dir(a.join("notes")).unwrap();
        fs::create_dir(b.join("notes")).unwrap();
        let current = fixture.root.join("current");
        symlink(&a, &current).unwrap();
        let plan = plan_register(
            &fixture.home,
            [
                RegisterRequest::new(current.join("notes"), FolioName::parse("Chosen").unwrap()),
                RegisterRequest::from(b.join("notes")),
            ],
        )
        .unwrap();
        assert_eq!(plan.summary.new, 2);
        fs::remove_file(&current).unwrap();
        symlink(&b, &current).unwrap();

        let mut marker_created = false;
        let blocked = apply_with_hook(&fixture.home, &plan, false, |step| {
            if matches!(step, Step::MarkerSynced) {
                marker_created = true;
            }
            Ok(())
        })
        .unwrap();
        assert!(
            !marker_created,
            "moved request must be blocked before marker creation"
        );
        assert_eq!(blocked.entries.len(), 1);
        assert_eq!(blocked.summary.refused, 1);
        assert_eq!(blocked.entries[0].path, plan.entries[1].path);
        assert_eq!(
            blocked.entries[0].status,
            RegisterStatus::Refused(RefusalReason::DestinationChanged {
                path: plan.entries[1].path.clone(),
            })
        );
        for entry in &plan.entries {
            assert!(!entry.path.exists());
        }
        assert!(
            Registry::open_existing(&fixture.home)
                .unwrap()
                .folios()
                .unwrap()
                .is_empty()
        );
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
    fn probes_the_entire_batch_before_creating_anything() {
        let fixture = Fixture::new();
        let first = fixture.dir("first");
        let second = fixture.dir("second");
        let plan = plan_register(&fixture.home, [&first, &second]).unwrap();
        let mut probed = Vec::new();
        let result = apply_links_with_hook(&fixture.home, &plan, false, |step| {
            if let LinkStep::Probe(path) = step {
                assert!(!first.join(DIRECTORY_MARKER).exists());
                assert!(!second.join(DIRECTORY_MARKER).exists());
                assert!(!fixture.home.path().join("folios").exists());
                assert!(!fixture.home.path().join("registry.db").exists());
                assert!(matches!(
                    fixture.home.try_lock(),
                    Err(HomeError::Locked { .. })
                ));
                probed.push(path.to_path_buf());
                if probed.len() == 2 {
                    return Err(io::Error::other("probe unavailable").into());
                }
            } else {
                panic!("registration must not start after a failed probe");
            }
            Ok(())
        });
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("probe unavailable")
        );
        assert_eq!(probed.len(), 2);
        assert!(!first.join(DIRECTORY_MARKER).exists());
        assert!(!fixture.home.path().join("registry.db").exists());
    }

    #[test]
    fn blocked_link_registration_does_not_probe_or_create_markers() {
        let fixture = Fixture::new();
        let directory = fixture.dir("directory");
        let missing = fixture.root.join("missing");
        let plan = plan_register(&fixture.home, [&directory, &missing]).unwrap();
        let result = apply_links_with_hook(&fixture.home, &plan, false, |_| {
            panic!("blocked apply must not probe")
        })
        .unwrap();
        assert_eq!(result.registration.summary.refused, 1);
        assert!(result.paths.is_empty());
        assert!(!directory.join(DIRECTORY_MARKER).exists());
        assert!(!fixture.home.path().join("registry.db").exists());
    }

    #[test]
    fn link_registration_failure_rolls_back_before_commit() {
        let fixture = Fixture::new();
        let directory = fixture.dir("directory");
        let plan = plan_register(&fixture.home, [&directory]).unwrap();
        let folios = fixture.home.path().join("folios");
        let result = crate::fs::with_sync_failure(&folios, || {
            apply_links_with_hook(&fixture.home, &plan, false, |_| Ok(()))
        });
        assert!(result.is_err());
        assert!(!directory.join(DIRECTORY_MARKER).exists());
        assert_eq!(fixture.folio_count(), 0);
        assert!(
            Registry::open_existing(&fixture.home)
                .unwrap()
                .folios()
                .unwrap()
                .is_empty()
        );
    }

    #[cfg_attr(windows, ignore = "real symlinks require privileges")]
    #[test]
    fn crash_after_registration_keeps_copies_and_retry_converts_without_new_folios() {
        let fixture = Fixture::new();
        let path = fixture.root.join("file");
        fs::write(&path, b"content").unwrap();
        let plan = plan_register(&fixture.home, [&path]).unwrap();
        let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = apply_links_with_hook(&fixture.home, &plan, false, |step| {
                if matches!(step, LinkStep::Registered) {
                    panic!("simulated crash");
                }
                Ok(())
            });
        }));
        assert!(crash.is_err());
        assert_eq!(fixture.folio_count(), 1);
        assert!(!fs::symlink_metadata(&path).unwrap().is_symlink());
        assert_eq!(fs::read(&path).unwrap(), b"content");
        let result = apply_register_links(&fixture.home, &plan, false).unwrap();
        assert_eq!(result.registration.summary.already_registered, 1);
        assert_eq!(result.paths[0].1, RegisterLinkResult::Linked);
        assert_eq!(fixture.folio_count(), 1);
    }

    #[cfg_attr(windows, ignore = "real symlinks require privileges")]
    #[test]
    fn edited_path_is_reported_and_other_conversions_continue() {
        let fixture = Fixture::new();
        let paths = ["first", "edited", "last"].map(|name| fixture.root.join(name));
        for path in &paths {
            fs::write(path, b"original").unwrap();
        }
        let edited = resolved_path(&paths[1]);
        let plan = plan_register(&fixture.home, &paths).unwrap();
        let result = apply_links_with_hook(&fixture.home, &plan, false, |step| {
            match step {
                LinkStep::Probe(path) => PlacementFs::new(&fixture.home).probe_link(path)?,
                LinkStep::Convert(path) if resolved_path(path) == edited => {
                    fs::write(path, b"edit")?
                }
                _ => {}
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(result.paths[0].1, RegisterLinkResult::Linked);
        assert!(
            matches!(&result.paths[1].1, RegisterLinkResult::LeftAsCopy { reason }
            if reason.contains("edited"))
        );
        assert_eq!(result.paths[2].1, RegisterLinkResult::Linked);
        assert_eq!(fs::read(&paths[1]).unwrap(), b"edit");
        let registry = Registry::open_existing(&fixture.home).unwrap();
        assert_eq!(registry.folios().unwrap().len(), 3);
        assert_eq!(
            registry
                .placement_at(&PlacementPath::resolve(&paths[1]).unwrap())
                .unwrap()
                .unwrap()
                .kind,
            PlacementKind::Copy
        );
    }

    #[cfg_attr(windows, ignore = "real symlinks require privileges")]
    #[test]
    fn conversion_syncs_link_creation_and_rename_and_retry_finishes_copy_row() {
        for successful_syncs in [0, 1] {
            let fixture = Fixture::new();
            let directory = fixture.dir("source").canonicalize().unwrap();
            let path = directory.join("file");
            fs::write(&path, b"content").unwrap();
            let plan = plan_register(&fixture.home, [&path]).unwrap();
            apply_register(&fixture.home, &plan, false).unwrap();
            let (result, retry) =
                crate::fs::with_sync_failure_after(&directory, successful_syncs, || {
                    let result = apply_register_links(&fixture.home, &plan, false).unwrap();
                    let retry = apply_register_links(&fixture.home, &plan, false).unwrap();
                    (result, retry)
                });
            assert!(
                matches!(&result.paths[0].1, RegisterLinkResult::LeftAsCopy { reason }
                if reason.contains("injected directory sync failure"))
            );
            assert_eq!(
                fs::symlink_metadata(&path).unwrap().is_symlink(),
                successful_syncs == 1
            );
            assert_eq!(fs::read(&path).unwrap(), b"content");
            assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
            let registry = Registry::open_existing(&fixture.home).unwrap();
            let row = registry
                .placement_at(&PlacementPath::resolve(&path).unwrap())
                .unwrap()
                .unwrap();
            assert_eq!(result.paths[0].0, path);
            assert_eq!(retry.paths[0].0, path);
            assert!(
                matches!(&retry.paths[0].1, RegisterLinkResult::LeftAsCopy { reason }
                if reason.contains("injected directory sync failure"))
            );
            assert_eq!(row.kind, PlacementKind::Copy);
            if successful_syncs == 1 {
                let (_, placement) = row.to_placement();
                assert_eq!(
                    crate::placement::classify(&placement, &Observation::IntactLink, None),
                    crate::placement::Drift::Unusable
                );
            }
            let result = apply_register_links(&fixture.home, &plan, false).unwrap();
            assert_eq!(result.paths[0].1, RegisterLinkResult::Linked);
            assert_eq!(fixture.folio_count(), 1);
            assert_eq!(
                registry.placement_at(&row.path).unwrap().unwrap().kind,
                PlacementKind::Link
            );
        }
    }

    #[test]
    fn unique_name_index_failure_rolls_back_the_entire_registration() {
        let fixture = Fixture::new();
        let paths = ["first.txt", "second.txt"].map(|p| fixture.root.join(p));
        for path in &paths {
            fs::write(path, b"body").unwrap();
        }
        let plan = plan_register(&fixture.home, &paths).unwrap();
        let lock = fixture.home.lock().unwrap();
        let holder = FolioId::generate();
        let result = apply_locked(&fixture.home, &plan, false, &lock, |step| {
            if matches!(step, Step::Transaction) {
                let mut registry = Registry::open_or_create(&fixture.home, &lock)?;
                registry.write(&lock, |tx| {
                    tx.add_folio(&NewFolio {
                        id: holder,
                        format: FolioFormat {
                            layout: FOLIO_LAYOUT_VERSION,
                            algorithm: OBJECT_ALGORITHM.into(),
                        },
                    })?;
                    tx.set_name(holder, &FolioName::parse("second").unwrap())
                })?;
            }
            Ok(())
        });
        assert!(
            matches!(result, Err(RegisterError::Registry(RegistryError::NameTaken(ref name))) if name.as_str() == "second")
        );
        let registry = Registry::open_existing(&fixture.home).unwrap();
        assert_eq!(registry.folios().unwrap(), vec![holder]);
        assert_eq!(
            registry
                .find_by_name(&FolioName::parse("first").unwrap())
                .unwrap(),
            None
        );
        assert_eq!(fixture.folio_count(), 0);
        for path in &paths {
            assert!(
                registry
                    .placement_at(&PlacementPath::resolve(path).unwrap())
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn permission_error_maps_to_unreadable() {
        assert_eq!(
            io_refusal(&io::Error::from(io::ErrorKind::PermissionDenied)),
            RefusalReason::Unreadable
        );
    }
}
