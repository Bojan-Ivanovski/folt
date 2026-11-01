//! Rename the registry first, then the internal main file, then commit on main.
//! Copies keep their bytes and bases. Hold the home lock from planning through
//! apply. After interruption, repeat the same old/new request: only the exact
//! transformation of the old base is accepted as this operation's pending edit.
//! A completed rename clears its pending record; repeating it may return
//! UnknownName because the old registry name no longer exists.
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::PathBuf;

use crate::folio_dir::FolioId;
use crate::folio_name::FolioName;
use crate::front_matter::{self, FrontMatterError};
use crate::history::{self, CommitMetadata, HistoryError, HistoryStores};
use crate::home::HomeLock;
use crate::object::{Commit, ObjectError, ObjectId, ObjectKind, hash_blob};
use crate::placement::{Observation, PlacementOps, Signature};
use crate::placement_fs::{PlacementFs, atomic_rewrite};
use crate::placement_rules::RulesError;
use crate::registry::{Placement, Registry, RegistryError};
use crate::store::StoreError;
use crate::working_state;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenamePlan {
    row: Placement,
    old: FolioName,
    new: FolioName,
    current: FolioName,
    bytes: Vec<u8>,
    signature: Signature,
    replacement: Option<Vec<u8>>,
}

impl RenamePlan {
    pub fn id(&self) -> FolioId {
        self.row.id
    }
}

#[derive(Debug)]
pub enum RenameError {
    UnknownName(FolioName),
    InternalChanged(PathBuf),
    PendingRename { old: FolioName, new: FolioName },
    PlanChanged,
    Registry(RegistryError),
    Io(std::io::Error),
    FrontMatter(FrontMatterError),
    Store(StoreError),
    Working(RulesError),
    History(HistoryError),
    Object(ObjectError),
}

impl fmt::Display for RenameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PendingRename { old, new } => {
                write!(f, "finish pending rename `{old}` to `{new}` first")
            }
            Self::UnknownName(name) => write!(f, "unknown Folio name: {name}"),
            Self::InternalChanged(path) => write!(
                f,
                "cannot rename: {} has uncommitted edits or a staged change; commit or discard the internal file's changes and unstage it before retrying",
                path.display()
            ),
            Self::PlanChanged => f.write_str("rename state changed; plan the rename again"),
            Self::Registry(e) => e.fmt(f),
            Self::Io(e) => e.fmt(f),
            Self::FrontMatter(e) => e.fmt(f),
            Self::Store(e) => e.fmt(f),
            Self::Working(e) => e.fmt(f),
            Self::History(e) => e.fmt(f),
            Self::Object(e) => e.fmt(f),
        }
    }
}
impl Error for RenameError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Registry(e) => Some(e),
            Self::Io(e) => Some(e),
            Self::FrontMatter(e) => Some(e),
            Self::Store(e) => Some(e),
            Self::Working(e) => Some(e),
            Self::History(e) => Some(e),
            Self::Object(e) => Some(e),
            _ => None,
        }
    }
}
macro_rules! from_error {
    ($ty:ty, $variant:ident) => {
        impl From<$ty> for RenameError {
            fn from(error: $ty) -> Self {
                Self::$variant(error)
            }
        }
    };
}
from_error!(RegistryError, Registry);
from_error!(std::io::Error, Io);
from_error!(FrontMatterError, FrontMatter);
from_error!(StoreError, Store);
from_error!(RulesError, Working);
from_error!(HistoryError, History);
from_error!(ObjectError, Object);

fn renamed(bytes: &[u8], name: &FolioName) -> Result<Vec<u8>, RenameError> {
    let edit = front_matter::set_name(bytes, Some(name.as_str()), false)?;
    Ok(match edit {
        Some(edit) => front_matter::apply_edit(bytes, &edit)?,
        None => bytes.to_vec(),
    })
}

/// Reads only the internal row. Referrers are checked in apply's registry
/// transaction, with the caller's cross-Folio branch-ref liveness callback.
pub fn plan_rename(
    old: &FolioName,
    new: &FolioName,
    fs: &PlacementFs<'_>,
    stores: &HistoryStores<'_>,
    registry: &Registry,
) -> Result<RenamePlan, RenameError> {
    let (id, retry) = match registry.find_by_name(old)? {
        Some(id) => (id, false),
        None => (
            registry
                .find_by_name(new)?
                .ok_or_else(|| RenameError::UnknownName(old.clone()))?,
            true,
        ),
    };
    let pending = registry.pending_rename(id)?;
    if retry && pending.as_ref() != Some(&(old.clone(), new.clone())) {
        return Err(RenameError::UnknownName(old.clone()));
    }
    if let Some((pending_old, pending_new)) = pending
        && (pending_old != *old || pending_new != *new)
    {
        return Err(RenameError::PendingRename {
            old: pending_old,
            new: pending_new,
        });
    }
    if registry
        .find_by_name(new)?
        .is_some_and(|holder| holder != id)
    {
        return Err(RegistryError::NameTaken(new.clone()).into());
    }
    let row = registry
        .internal_placement(id)?
        .ok_or(history::Refusal::MissingInternal)
        .map_err(HistoryError::from)?;
    let facts = working_state::gather(&row, fs, stores.refs, registry)?;
    let Observation::File { signature } = facts.observation else {
        return Err(RenameError::InternalChanged(row.path.to_path_buf()));
    };
    let bytes = fs::read(row.path.to_path_buf())?;
    let base = row
        .base
        .as_ref()
        .ok_or_else(|| RenameError::InternalChanged(row.path.to_path_buf()))?;
    let mut base_bytes = Vec::new();
    stores
        .objects
        .get_to(base.hash, ObjectKind::Blob, &mut base_bytes)?;
    let named = match front_matter::read_name(&base_bytes) {
        Ok(Some(_)) => true,
        Ok(None) | Err(FrontMatterError::Binary) => false,
        Err(error) => return Err(RenameError::FrontMatter(error)),
    };
    // The pending record identifies the operation. The base must still hold
    // its old name or the new name from a commit completed before record clear.
    if retry {
        let base_name = front_matter::read_name(&base_bytes)?;
        if !named
            || !matches!(base_name.as_deref(), Some(name) if name == old.as_str() || name == new.as_str())
        {
            return Err(RenameError::UnknownName(old.clone()));
        }
    }
    let replacement = if named && old != new {
        let replacement = renamed(&base_bytes, new)?;
        (replacement != base_bytes).then_some(replacement)
    } else {
        None
    };
    // Preflight both history and the exact replacement prefix before any
    // registry/file writes, even when resuming an own edit or existing stage.
    for content in std::iter::once(base_bytes.as_slice()).chain(replacement.as_deref()) {
        for reference in working_state::stage_references(&row, content)? {
            crate::registry::reference_name(&reference)?;
        }
    }
    let own_edit = retry && replacement.as_ref() == Some(&bytes);
    let clean = hash_blob(&bytes) == base.hash;
    let own_stage = own_edit
        && row.staged.is_some_and(|stage| {
            stage.hash == hash_blob(&bytes) && stage.size == bytes.len() as u64
        });
    if (!clean && !own_edit) || (row.staged.is_some() && !own_stage) {
        return Err(RenameError::InternalChanged(row.path.to_path_buf()));
    }
    // Ref advancement is owned by history. Only a pending rename stage can
    // resume a published commit; other branch movement must be resolved first.
    if facts.tip != row.commit_id {
        if !own_stage {
            return Err(RenameError::PlanChanged);
        }
        history::judge_commit(&history::gather_commit(&row, fs, stores, registry)?)
            .map_err(HistoryError::from)?;
    }
    if fs::metadata(row.path.to_path_buf()).map(|m| crate::placement_fs::signature(&m))?
        != signature
    {
        return Err(RenameError::PlanChanged);
    }
    Ok(RenamePlan {
        row,
        old: old.clone(),
        new: new.clone(),
        current: if retry { new.clone() } else { old.clone() },
        bytes,
        signature,
        replacement,
    })
}

/// The callback must check branch refs for the supplied parent Folio. Registry
/// placements also keep branches live. Metadata identity/time/body/group come
/// from the caller; rename supplies the operation's required commit message.
/// Returns the main commit, or None for a registry-only rename/no-op.
pub fn apply_rename(
    plan: &RenamePlan,
    mut metadata: CommitMetadata,
    fs: &PlacementFs<'_>,
    stores: &HistoryStores<'_>,
    registry: &mut Registry,
    lock: &mut HomeLock,
    is_live: &mut dyn FnMut(FolioId, &str) -> Result<bool, RegistryError>,
) -> Result<Option<ObjectId>, RenameError> {
    if plan_rename(&plan.old, &plan.new, fs, stores, registry)? != *plan {
        return Err(RenameError::PlanChanged);
    }
    metadata.message = format!("Rename `{}` to `{}`", plan.old, plan.new);
    if let Some(bytes) = &plan.replacement {
        Commit {
            content: hash_blob(bytes),
            parents: plan.row.commit_id.into_iter().collect(),
            author: metadata.author.clone(),
            time: metadata.time,
            message: metadata.message.clone(),
            body: metadata.body.clone(),
            group: metadata.group.clone(),
        }
        .encode_body()?;
    }
    registry.write(lock, |tx| {
        if tx.placement_at(&plan.row.path)?.as_ref() != Some(&plan.row)
            || tx.find_by_name(&plan.current)? != Some(plan.id())
        {
            return Err(RegistryError::PlacementChanged(plan.row.path.clone()));
        }
        tx.check_referrers(plan.id(), "rename", is_live)?;
        tx.rename(plan.id(), &plan.new)?;
        if plan.replacement.is_some() {
            tx.set_pending_rename(plan.id(), &plan.old, &plan.new)?;
        } else {
            tx.clear_pending_rename(plan.id())?;
        }
        Ok(())
    })?;
    let Some(bytes) = &plan.replacement else {
        return Ok(None);
    };
    if *bytes != plan.bytes {
        atomic_rewrite(&plan.row.path.to_path_buf(), bytes, &plan.signature, |_| {})?;
    }
    if fs
        .hash_placement(&plan.row.to_placement().1)
        .map_err(RulesError::from)?
        != hash_blob(bytes)
    {
        return Err(RenameError::InternalChanged(plan.row.path.to_path_buf()));
    }
    // Retain a pending stage so history can resume its published-ref window.
    if plan.row.staged.is_none() {
        working_state::stage(&plan.row, fs, stores.objects, stores.refs, registry, lock)?;
    }
    let row = registry
        .internal_placement(plan.id())?
        .ok_or(history::Refusal::MissingInternal)
        .map_err(HistoryError::from)?;
    if !row
        .staged
        .is_some_and(|stage| stage.hash == hash_blob(bytes) && stage.size == bytes.len() as u64)
    {
        return Err(RenameError::InternalChanged(row.path.to_path_buf()));
    }
    let facts = history::gather_commit(&row, fs, stores, registry)?;
    let commit = history::judge_commit(&facts).map_err(HistoryError::from)?;
    let commit = history::apply_commit(&commit, metadata, fs, stores, registry, lock)?;
    registry.write(lock, |tx| tx.clear_pending_rename(plan.id()))?;
    Ok(Some(commit))
}
