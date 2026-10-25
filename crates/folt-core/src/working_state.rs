use crate::home::HomeLock;
use crate::object::{ObjectId, ObjectKind};
use crate::placement::{Observation, PlacementError, PlacementKind, PlacementOps, StagedBlob};
use crate::placement_rules::{Behind, PlacementFacts, Refusal, RefusalReason, RulesError};
use crate::refs::{Kind, RefName, RefStore};
use crate::registry::{Placement, Registry, RegistryError};
use crate::store::ObjectStore;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Edit {
    Clean,
    Edited,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Staged {
    None,
    Staged,
    StagedThenModified,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkingStatus {
    pub edit: Edit,
    pub staged: Staged,
    pub behind: Behind,
}

/// Collects one working file, hashing only when a recorded size matches.
pub fn gather(
    row: &Placement,
    fs: &impl PlacementOps,
    refs: &RefStore,
) -> Result<PlacementFacts, RulesError> {
    let observation = read_working(row, fs)?;
    let Observation::File { signature } = &observation else {
        unreachable!("read_working requires a regular file");
    };
    let needs_hash = row
        .base
        .as_ref()
        .is_some_and(|base| base.signature.size == signature.size)
        || row
            .staged
            .is_some_and(|staged| staged.size == signature.size);
    let placement_hash = if needs_hash {
        Some(fs.hash_placement(&row.to_placement().1)?)
    } else {
        None
    };
    let tip = refs.read(Kind::Branch, &RefName::parse(&row.head)?)?;
    Ok(PlacementFacts {
        placement: row.clone(),
        observation,
        placement_hash,
        tip,
    })
}

/// Compares only the supplied observations; no commits or files are read.
pub fn judge_working(facts: &PlacementFacts) -> WorkingStatus {
    let row = &facts.placement;
    let matches = |hash: ObjectId, size: u64| {
        matches!(&facts.observation, Observation::File { signature } if signature.size == size)
            && facts.placement_hash == Some(hash)
    };
    let edit = if row
        .base
        .as_ref()
        .is_some_and(|base| matches(base.hash, base.signature.size))
    {
        Edit::Clean
    } else {
        Edit::Edited
    };
    let staged = match row.staged {
        None => Staged::None,
        Some(staged) if matches(staged.hash, staged.size) => Staged::Staged,
        Some(_) => Staged::StagedThenModified,
    };
    let behind = match (row.commit_id, facts.tip) {
        (None, None) => Behind::NoHistory,
        (None, Some(_)) => Behind::Unrecorded,
        (Some(_), None) => Behind::MissingTip,
        (Some(commit), Some(tip)) if commit == tip => Behind::UpToDate,
        (Some(_), Some(tip)) => Behind::Behind { tip },
    };
    WorkingStatus {
        edit,
        staged,
        behind,
    }
}

/// Captures a blob, then records its stage without changing the working base.
/// The home lock must cover selecting the row and staging it.
pub fn stage(
    row: &Placement,
    fs: &impl PlacementOps,
    objects: &ObjectStore,
    registry: &mut Registry,
    lock: &HomeLock,
) -> Result<(), RulesError> {
    let before = read_working(row, fs)?;
    let hash = objects
        .put_file(lock, ObjectKind::Blob, &row.path.to_path_buf())
        .map_err(PlacementError::Store)?;
    if read_working(row, fs)? != before {
        return Err(Refusal {
            path: row.path.to_path_buf(),
            reason: RefusalReason::PlacementChanged,
        }
        .into());
    }
    let Observation::File { signature } = before else {
        unreachable!("read_working requires a regular file");
    };
    let staged = if row.base.as_ref().is_some_and(|base| base.hash == hash) {
        None
    } else {
        Some(StagedBlob {
            hash,
            size: signature.size,
        })
    };
    write_stage(row, staged, registry, lock)
}

/// Clears the recorded stage without reading or changing the working file.
pub fn unstage(
    row: &Placement,
    registry: &mut Registry,
    lock: &HomeLock,
) -> Result<(), RulesError> {
    require_working(row)?;
    write_stage(row, None, registry, lock)
}

fn write_stage(
    row: &Placement,
    staged: Option<StagedBlob>,
    registry: &mut Registry,
    lock: &HomeLock,
) -> Result<(), RulesError> {
    registry.write(lock, |tx| {
        if tx.placement_at(&row.path)?.as_ref() != Some(row) {
            return Err(RegistryError::PlacementChanged(row.path.clone()));
        }
        let mut updated = row.clone();
        updated.staged = staged;
        tx.update_placement(&updated)
    })?;
    Ok(())
}

fn require_working(row: &Placement) -> Result<(), RulesError> {
    if row.kind == PlacementKind::Link {
        return Err(Refusal {
            path: row.path.to_path_buf(),
            reason: RefusalReason::Link,
        }
        .into());
    }
    Ok(())
}

fn read_working(row: &Placement, fs: &impl PlacementOps) -> Result<Observation, RulesError> {
    require_working(row)?;
    let observation = fs.read(row.id, &row.to_placement().1)?;
    let reason = match observation {
        Observation::File { .. } => return Ok(observation),
        Observation::Missing => RefusalReason::Gone,
        Observation::NotAFile => RefusalReason::NotAFile,
        Observation::IntactLink | Observation::OtherLink { .. } => RefusalReason::Unusable,
    };
    Err(Refusal {
        path: row.path.to_path_buf(),
        reason,
    }
    .into())
}
