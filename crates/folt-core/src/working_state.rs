use crate::home::HomeLock;
use crate::object::{ObjectId, ObjectKind};
use crate::placement::{Observation, PlacementError, PlacementKind, PlacementOps, StagedBlob};
use crate::placement_rules::{
    Behind, PlacementFacts, Refusal, RefusalReason, RulesError, needs_hash,
};
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

/// Collects one working file, hashing when it has no base or a recorded size matches.
pub fn gather(
    row: &Placement,
    fs: &impl PlacementOps,
    refs: &RefStore,
    registry: &Registry,
) -> Result<PlacementFacts, RulesError> {
    let row = &resolve_working(row, registry, fs)?;
    let observation = read_working(row, fs)?;
    let placement_hash = if needs_hash(row, &observation) {
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
    let selected = row;
    let row = &resolve_working(selected, registry, fs)?;
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
    resolve_working(selected, registry, fs)?;
    write_stage(row, staged, Some((objects, hash)), registry, lock)
}

/// Clears the recorded stage without reading or changing the working file.
pub fn unstage(
    row: &Placement,
    fs: &impl PlacementOps,
    registry: &mut Registry,
    lock: &HomeLock,
) -> Result<(), RulesError> {
    let row = &resolve_working(row, registry, fs)?;
    write_stage(row, None, None, registry, lock)
}

fn write_stage(
    row: &Placement,
    staged: Option<StagedBlob>,
    blob: Option<(&ObjectStore, ObjectId)>,
    registry: &mut Registry,
    lock: &HomeLock,
) -> Result<(), RulesError> {
    let prefix = blob
        .map(|(objects, hash)| {
            objects.get_prefix(hash, ObjectKind::Blob, crate::front_matter::SCAN_LIMIT + 8)
        })
        .transpose()
        .map_err(PlacementError::Store)?;
    let references = prefix
        .as_ref()
        .map(|bytes| match crate::front_matter::read_references(bytes) {
            Ok(refs) => Ok(refs),
            Err(crate::front_matter::FrontMatterError::Binary) => Ok(Vec::new()),
            Err(error) => Err(RegistryError::References(error.to_string())),
        })
        .transpose()?;
    registry.write(lock, |tx| {
        if tx.placement_at(&row.path)?.as_ref() != Some(row) {
            return Err(RegistryError::PlacementChanged(row.path.clone()));
        }
        let mut updated = row.clone();
        updated.staged = staged;
        tx.update_placement(&updated)?;
        if let Some(references) = &references {
            // Internal rows (including the target of a link) are constrained to
            // main by the placements schema; copies stage on their own head.
            tx.rebuild_references(row.id, &row.head, references)?;
        }
        Ok(())
    })?;
    Ok(())
}

/// Links share the internal working row, provided their entry still points to it.
pub fn resolve_working(
    row: &Placement,
    registry: &Registry,
    fs: &impl PlacementOps,
) -> Result<Placement, RulesError> {
    if row.kind != PlacementKind::Link {
        return Ok(row.clone());
    }
    let observation = fs.read(row.id, &row.to_placement().1)?;
    let internal = registry.internal_placement(row.id)?;
    resolve_observed(row, &observation, internal)
}

fn resolve_observed(
    row: &Placement,
    observation: &Observation,
    internal: Option<Placement>,
) -> Result<Placement, RulesError> {
    if row.kind != PlacementKind::Link {
        return Ok(row.clone());
    }
    let reason = match observation {
        Observation::IntactLink => {
            return internal
                .filter(|internal| {
                    internal.id == row.id
                        && internal.kind == PlacementKind::Internal
                        && internal.head == crate::layout::DEFAULT_BRANCH
                })
                .ok_or_else(|| {
                    Refusal {
                        path: row.path.to_path_buf(),
                        reason: RefusalReason::MissingInternal,
                    }
                    .into()
                });
        }
        Observation::Missing => RefusalReason::Gone,
        Observation::Directory | Observation::NotAFile => RefusalReason::NotAFile,
        Observation::File { .. } | Observation::OtherLink { .. } => RefusalReason::LinkReplaced,
    };
    Err(Refusal {
        path: row.path.to_path_buf(),
        reason,
    }
    .into())
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
        Observation::Directory | Observation::NotAFile => RefusalReason::NotAFile,
        Observation::IntactLink | Observation::OtherLink { .. } => RefusalReason::Unusable,
    };
    Err(Refusal {
        path: row.path.to_path_buf(),
        reason,
    }
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::hash_blob;
    use crate::placement_rules::tests::facts;

    #[test]
    fn working_target_decision_table() {
        let mut row = facts().placement;
        let mut internal = row.clone();
        internal.kind = PlacementKind::Internal;
        internal.head = "main".into();
        for kind in [PlacementKind::Copy, PlacementKind::Internal] {
            row.kind = kind;
            assert_eq!(
                resolve_observed(&row, &Observation::Missing, None).unwrap(),
                row
            );
        }
        row.kind = PlacementKind::Link;
        assert_eq!(
            resolve_observed(&row, &Observation::IntactLink, Some(internal.clone())).unwrap(),
            internal
        );
        for (observation, internal, reason) in [
            (
                facts().observation,
                Some(internal.clone()),
                RefusalReason::LinkReplaced,
            ),
            (
                Observation::OtherLink {
                    target: "other".into(),
                },
                Some(internal.clone()),
                RefusalReason::LinkReplaced,
            ),
            (
                Observation::Missing,
                Some(internal.clone()),
                RefusalReason::Gone,
            ),
            (
                Observation::NotAFile,
                Some(internal.clone()),
                RefusalReason::NotAFile,
            ),
            (
                Observation::IntactLink,
                None,
                RefusalReason::MissingInternal,
            ),
        ] {
            assert!(
                matches!(resolve_observed(&row, &observation, internal), Err(RulesError::Refused(ref refusal)) if refusal.reason == reason)
            );
        }
        internal.id = crate::folio_dir::FolioId::from_bytes([2; 16]);
        assert!(
            matches!(resolve_observed(&row, &Observation::IntactLink, Some(internal)), Err(RulesError::Refused(ref refusal)) if refusal.reason == RefusalReason::MissingInternal)
        );
    }

    #[test]
    fn edit_requires_matching_size_and_hash() {
        let mut facts = facts();
        assert_eq!(judge_working(&facts).edit, Edit::Clean);
        facts.placement_hash = Some(hash_blob(b"edit"));
        assert_eq!(judge_working(&facts).edit, Edit::Edited);
        facts.placement_hash = facts.placement.base.as_ref().map(|base| base.hash);
        if let Observation::File { signature } = &mut facts.observation {
            signature.size += 1;
        }
        assert_eq!(judge_working(&facts).edit, Edit::Edited);
        facts.placement.base = None;
        assert_eq!(judge_working(&facts).edit, Edit::Edited);
    }

    #[test]
    fn stage_distinguishes_later_edits() {
        let mut facts = facts();
        assert_eq!(judge_working(&facts).staged, Staged::None);
        facts.placement.staged = Some(StagedBlob {
            hash: facts.placement_hash.unwrap(),
            size: 4,
        });
        assert_eq!(judge_working(&facts).staged, Staged::Staged);
        facts.placement_hash = Some(hash_blob(b"edit"));
        assert_eq!(judge_working(&facts).staged, Staged::StagedThenModified);
        facts.placement_hash = Some(facts.placement.staged.unwrap().hash);
        facts.observation = Observation::Missing;
        assert_eq!(judge_working(&facts).staged, Staged::StagedThenModified);
    }
}
