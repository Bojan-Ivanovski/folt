use std::borrow::Borrow;
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::path::PathBuf;

use crate::folio_dir::FolioId;
use crate::home::HomeLock;
use crate::layout::DEFAULT_BRANCH;
use crate::object::ObjectId;
use crate::placement::{self, Drift, Observation, PlacementError, PlacementKind, PlacementOps};
use crate::placement_fs::resolved_path;
use crate::refs::{Kind, RefError, RefName, RefStore};
use crate::registry::{self, Registry, RegistryError};
use crate::store::ObjectStore;

/// Read-only facts used by status and move decisions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlacementFacts {
    pub placement: registry::Placement,
    pub observation: Observation,
    pub placement_hash: Option<ObjectId>,
    pub tip: Option<ObjectId>,
}

pub(crate) fn needs_hash(row: &registry::Placement, observation: &Observation) -> bool {
    placement::needs_hash(&row.to_placement().1, observation)
        || matches!((observation, row.staged),
            (Observation::File { signature }, Some(staged)) if signature.size == staged.size)
}

/// Collects only the supplied rows, reading each distinct Folio/branch tip once.
/// The caller selects rows with `placement_at` for one path or its own set query.
/// `open_refs` supplies a ref store for the requested Folio without selecting placements.
pub fn gather<R: Borrow<RefStore>>(
    placements: &[registry::Placement],
    fs: &impl PlacementOps,
    mut open_refs: impl FnMut(FolioId) -> Result<R, RefError>,
) -> Result<Vec<PlacementFacts>, RulesError> {
    let mut stores = HashMap::new();
    let mut tips = HashMap::new();
    let mut facts = Vec::with_capacity(placements.len());
    for row in placements {
        let (id, placement) = row.to_placement();
        let observation = fs.read(id, &placement)?;
        let placement_hash = if needs_hash(row, &observation) {
            Some(fs.hash_placement(&placement)?)
        } else {
            None
        };
        let key = (id, row.head.clone());
        let tip = if let Some(tip) = tips.get(&key) {
            *tip
        } else {
            let name = RefName::parse(&row.head)?;
            let refs = match stores.entry(id) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) => entry.insert(open_refs(id)?),
            };
            let refs: &RefStore = (*refs).borrow();
            let tip = refs.read(Kind::Branch, &name)?;
            tips.insert(key, tip);
            tip
        };
        facts.push(PlacementFacts {
            placement: row.clone(),
            observation,
            placement_hash,
            tip,
        });
    }
    Ok(facts)
}

/// Collects one move's facts, also hashing size-changed copies to compare with
/// incoming content. Status gathering never needs that extra comparison.
pub fn gather_move<R: Borrow<RefStore>>(
    placement: &registry::Placement,
    fs: &impl PlacementOps,
    open_refs: impl FnMut(FolioId) -> Result<R, RefError>,
) -> Result<PlacementFacts, RulesError> {
    let mut facts = gather(std::slice::from_ref(placement), fs, open_refs)?;
    let mut facts = facts.pop().expect("one placement was gathered");
    if placement.kind != PlacementKind::Link
        && matches!(facts.observation, Observation::File { .. })
        && facts.placement_hash.is_none()
    {
        facts.placement_hash = Some(fs.hash_placement(&placement.to_placement().1)?);
    }
    Ok(facts)
}

/// Collects conversion facts, hashing regular files even when the row is a link
/// so a conversion interrupted before its row write can be resumed.
pub fn gather_convert<R: Borrow<RefStore>>(
    placement: &registry::Placement,
    fs: &impl PlacementOps,
    open_refs: impl FnMut(FolioId) -> Result<R, RefError>,
) -> Result<PlacementFacts, RulesError> {
    let mut facts = gather_move(placement, fs, open_refs)?;
    if matches!(facts.observation, Observation::File { .. }) && facts.placement_hash.is_none() {
        facts.placement_hash = Some(fs.hash_placement(&placement.to_placement().1)?);
    }
    Ok(facts)
}

/// Tip comparison only; this does not inspect commit ancestry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Behind {
    UpToDate,
    Behind {
        tip: ObjectId,
    },
    NoHistory,
    Unrecorded,
    /// A recorded commit remains, but its branch ref is missing.
    MissingTip,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlacementStatus {
    pub placement: registry::Placement,
    pub drift: Drift,
    /// Links always show `folio.main` and have no recorded-commit comparison.
    pub behind: Option<Behind>,
    pub staged: crate::working_state::Staged,
    pub edit: crate::working_state::Edit,
}

/// Include internal-row facts to report an intact link's shared edit and stage.
pub fn judge_status(facts_set: &[PlacementFacts]) -> Vec<PlacementStatus> {
    facts_set
        .iter()
        .map(|facts| {
            let (_, placement) = facts.placement.to_placement();
            let working_facts = if placement.kind == PlacementKind::Link
                && facts.observation == Observation::IntactLink
            {
                facts_set
                    .iter()
                    .find(|internal| {
                        internal.placement.id == facts.placement.id
                            && internal.placement.kind == PlacementKind::Internal
                    })
                    .unwrap_or(facts)
            } else {
                facts
            };
            let working = crate::working_state::judge_working(working_facts);
            let behind = (placement.kind != PlacementKind::Link).then_some(working.behind);
            PlacementStatus {
                placement: facts.placement.clone(),
                drift: placement::classify(&placement, &facts.observation, facts.placement_hash),
                behind,
                staged: working.staged,
                edit: working.edit,
            }
        })
        .collect()
}

/// Collects status for the selected paths, including each intact link's shared edits.
pub fn gather_status<R: Borrow<RefStore>>(
    placements: &[registry::Placement],
    fs: &impl PlacementOps,
    mut open_refs: impl FnMut(FolioId) -> Result<R, RefError>,
    registry: &Registry,
) -> Result<Vec<PlacementStatus>, RulesError> {
    let mut facts = gather(placements, fs, &mut open_refs)?;
    for row in placements
        .iter()
        .filter(|row| row.kind == PlacementKind::Link)
    {
        let internal = crate::working_state::resolve_working(row, registry, fs)?;
        let refs = open_refs(row.id)?;
        facts.push(crate::working_state::gather(
            &internal,
            fs,
            refs.borrow(),
            registry,
        )?);
    }
    let mut status = judge_status(&facts);
    status.truncate(placements.len());
    Ok(status)
}

/// The caller resolves a commit and its content blob before judging a move.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MoveTarget {
    pub branch: RefName,
    pub commit: ObjectId,
    pub content: ObjectId,
}

/// An allowed move, constructible only through `judge_move`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MovePlan {
    placement: registry::Placement,
    observation: Observation,
    target: MoveTarget,
}

impl MovePlan {
    pub fn placement(&self) -> &registry::Placement {
        &self.placement
    }

    pub fn target(&self) -> &MoveTarget {
        &self.target
    }
}

pub fn judge_move(
    facts: &PlacementFacts,
    target: &MoveTarget,
    discard: bool,
) -> Result<MovePlan, Refusal> {
    let (_, placement) = facts.placement.to_placement();
    let refuse = |reason| Refusal {
        path: placement.path.clone(),
        reason,
    };
    if placement.kind == PlacementKind::Link {
        return Err(refuse(RefusalReason::Link));
    }
    if placement.kind == PlacementKind::Internal && target.branch.as_str() != DEFAULT_BRANCH {
        return Err(refuse(RefusalReason::InternalBranch));
    }
    match &facts.observation {
        Observation::Missing => return Err(refuse(RefusalReason::Gone)),
        Observation::NotAFile => return Err(refuse(RefusalReason::NotAFile)),
        Observation::IntactLink | Observation::OtherLink { .. } => {
            return Err(refuse(RefusalReason::Unusable));
        }
        Observation::File { .. } => {}
    }
    match placement::classify(&placement, &facts.observation, facts.placement_hash) {
        Drift::InSync => {}
        Drift::PlacementEdited if discard || facts.placement_hash == Some(target.content) => {}
        Drift::PlacementEdited => return Err(refuse(RefusalReason::Edited)),
        _ => return Err(refuse(RefusalReason::Unusable)),
    }
    Ok(MovePlan {
        placement: facts.placement.clone(),
        observation: facts.observation.clone(),
        target: target.clone(),
    })
}

/// Rechecks an allowed move before writing its file, then updates just its row.
/// The home lock must cover gathering, judging and applying the move.
pub fn apply_move(
    plan: &MovePlan,
    fs: &impl PlacementOps,
    objects: &ObjectStore,
    registry: &mut Registry,
    lock: &HomeLock,
) -> Result<(), RulesError> {
    let base = apply_move_file(plan, fs, objects, registry, lock)?;
    let updated = moved_row(plan, base);
    registry.write(lock, |tx| tx.update_placement(&updated))?;
    Ok(())
}

/// Rechecks the row and observation, then refreshes only the file.
/// The caller persists `moved_row` after all related file work succeeds.
pub fn apply_move_file(
    plan: &MovePlan,
    fs: &impl PlacementOps,
    objects: &ObjectStore,
    registry: &Registry,
    _lock: &HomeLock,
) -> Result<Option<crate::placement::PlacementBase>, RulesError> {
    let (id, placement) = plan.placement.to_placement();
    if registry.placement_at(&plan.placement.path)?.as_ref() != Some(&plan.placement) {
        return Err(Refusal {
            path: placement.path,
            reason: RefusalReason::RegistryChanged,
        }
        .into());
    }
    if fs.read(id, &placement)? != plan.observation {
        return Err(Refusal {
            path: placement.path,
            reason: RefusalReason::PlacementChanged,
        }
        .into());
    }
    let written = fs
        .refresh(
            id,
            &placement,
            objects,
            plan.target.content,
            Some(&plan.observation),
        )
        .map_err(|error| match error {
            PlacementError::PlacementChanged => RulesError::Refused(Refusal {
                path: placement.path.clone(),
                reason: RefusalReason::PlacementChanged,
            }),
            error => RulesError::Placement(error),
        })?;
    Ok(written.base)
}

pub fn moved_row(
    plan: &MovePlan,
    base: Option<crate::placement::PlacementBase>,
) -> registry::Placement {
    let mut updated = plan.placement.clone();
    updated.head = plan.target.branch.as_str().into();
    updated.commit_id = Some(plan.target.commit);
    updated.base = base;
    updated.staged = None;
    updated
}

/// An allowed kind change, constructible only through `judge_convert`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConvertPlan {
    placement: registry::Placement,
    observation: Observation,
    placement_hash: Option<ObjectId>,
    to_kind: PlacementKind,
    target: MoveTarget,
    internal: Option<PlacementFacts>,
    resume: bool,
}

impl ConvertPlan {
    pub fn placement(&self) -> &registry::Placement {
        &self.placement
    }

    pub fn to_kind(&self) -> PlacementKind {
        self.to_kind
    }
}

/// The target is the resolved main tip. Link-to-copy also requires the Internal
/// row's facts. The home lock must cover collecting these facts through apply.
pub fn judge_convert(
    facts: &PlacementFacts,
    to_kind: PlacementKind,
    target: &MoveTarget,
    internal: Option<&PlacementFacts>,
) -> Result<ConvertPlan, Refusal> {
    let (_, placement) = facts.placement.to_placement();
    let refuse = |reason| Refusal {
        path: placement.path.clone(),
        reason,
    };
    if placement.kind == PlacementKind::Internal || to_kind == PlacementKind::Internal {
        return Err(refuse(RefusalReason::Internal));
    }
    if placement.kind == to_kind {
        return Err(refuse(RefusalReason::SameKind));
    }
    if target.branch.as_str() != DEFAULT_BRANCH
        || (placement.kind == PlacementKind::Copy && placement.head != DEFAULT_BRANCH)
    {
        return Err(refuse(RefusalReason::CopyBranch));
    }
    if facts.placement.staged.is_some() {
        return Err(refuse(RefusalReason::Staged));
    }
    let resume = match (&facts.observation, to_kind) {
        (Observation::Missing, _) => return Err(refuse(RefusalReason::ConvertGone)),
        (Observation::NotAFile, _) => return Err(refuse(RefusalReason::NotAFile)),
        (Observation::OtherLink { .. }, _) => return Err(refuse(RefusalReason::OtherLink)),
        (Observation::IntactLink, PlacementKind::Link) => true,
        (Observation::File { .. }, PlacementKind::Copy) => {
            if facts.placement_hash != Some(target.content) {
                return Err(refuse(RefusalReason::Edited));
            }
            true
        }
        (Observation::File { .. }, PlacementKind::Link) => {
            match placement::classify(&placement, &facts.observation, facts.placement_hash) {
                Drift::InSync => {}
                Drift::PlacementEdited => return Err(refuse(RefusalReason::Edited)),
                _ => return Err(refuse(RefusalReason::Unusable)),
            }
            false
        }
        (Observation::IntactLink, PlacementKind::Copy) => false,
        _ => return Err(refuse(RefusalReason::Unusable)),
    };
    let internal = if to_kind == PlacementKind::Copy {
        let internal = internal.ok_or_else(|| refuse(RefusalReason::InternalEdited))?;
        let (_, working) = internal.placement.to_placement();
        if working.kind != PlacementKind::Internal
            || internal.placement.id != facts.placement.id
            || working.head != DEFAULT_BRANCH
            || internal.placement.staged.is_some()
            || internal.placement_hash != Some(target.content)
            || placement::classify(&working, &internal.observation, internal.placement_hash)
                != Drift::InSync
        {
            return Err(refuse(RefusalReason::InternalEdited));
        }
        Some(internal.clone())
    } else {
        None
    };
    Ok(ConvertPlan {
        placement: facts.placement.clone(),
        observation: facts.observation.clone(),
        placement_hash: facts.placement_hash,
        to_kind,
        target: target.clone(),
        internal,
        resume,
    })
}

/// Rechecks the selected rows and files, publishes the entry, then writes its row.
/// A retry with the requested shape completes only the registry update.
pub fn apply_convert(
    plan: &ConvertPlan,
    fs: &impl PlacementOps,
    objects: &ObjectStore,
    registry: &mut Registry,
    lock: &HomeLock,
) -> Result<(), RulesError> {
    let refuse = |reason| {
        RulesError::Refused(Refusal {
            path: plan.placement.path.to_path_buf(),
            reason,
        })
    };
    let check = |row: &registry::Placement, observation: &Observation, hash: Option<ObjectId>| {
        if registry.placement_at(&row.path)?.as_ref() != Some(row) {
            return Err(refuse(RefusalReason::RegistryChanged));
        }
        let (id, placement) = row.to_placement();
        if fs.read(id, &placement)? != *observation
            || (matches!(observation, Observation::File { .. })
                && hash.is_some()
                && Some(fs.hash_placement(&placement)?) != hash)
        {
            return Err(refuse(RefusalReason::PlacementChanged));
        }
        Ok(())
    };
    check(&plan.placement, &plan.observation, plan.placement_hash)?;
    if let Some(internal) = &plan.internal {
        check(
            &internal.placement,
            &internal.observation,
            internal.placement_hash,
        )?;
    }
    let base = if plan.resume {
        match &plan.observation {
            Observation::File { signature } => Some(placement::PlacementBase {
                hash: plan.target.content,
                signature: signature.clone(),
            }),
            _ => None,
        }
    } else {
        let (id, placement) = plan.placement.to_placement();
        fs.convert(
            id,
            &placement,
            plan.to_kind,
            objects,
            plan.target.content,
            &plan.observation,
        )
        .map_err(|error| match error {
            PlacementError::PlacementChanged => refuse(RefusalReason::PlacementChanged),
            error => RulesError::Placement(error),
        })?
        .base
    };
    let updated = converted_row(plan, base);
    registry.write(lock, |tx| tx.update_placement(&updated))?;
    Ok(())
}

pub fn converted_row(
    plan: &ConvertPlan,
    base: Option<placement::PlacementBase>,
) -> registry::Placement {
    let mut updated = plan.placement.clone();
    updated.kind = plan.to_kind;
    updated.head = DEFAULT_BRANCH.into();
    updated.commit_id = (plan.to_kind == PlacementKind::Copy).then_some(plan.target.commit);
    updated.base = if plan.to_kind == PlacementKind::Copy {
        base
    } else {
        None
    };
    updated.staged = None;
    updated
}

/// Destination facts collected while the home lock is held.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelocateTarget {
    pub path: registry::PlacementPath,
    pub observation: Observation,
    pub hash: Option<ObjectId>,
    pub registered: bool,
    pub completed: bool,
}

pub fn gather_relocate_target(
    row: &registry::Placement,
    to: &registry::PlacementPath,
    fs: &impl PlacementOps,
    registry: &Registry,
) -> Result<RelocateTarget, RulesError> {
    let (id, mut placement) = row.to_placement();
    placement.path = to.to_path_buf();
    let observation = fs.read(id, &placement)?;
    let hash = if matches!(observation, Observation::File { .. }) {
        Some(fs.hash_placement(&placement)?)
    } else {
        None
    };
    Ok(RelocateTarget {
        path: to.clone(),
        observation,
        hash,
        registered: registry.placement_at(to)?.is_some(),
        completed: fs.relocation_completed(id, &row.to_placement().1, &to.to_path_buf())?,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelocatePlan {
    facts: PlacementFacts,
    target: RelocateTarget,
}

fn relocated_matches(facts: &PlacementFacts, target: &RelocateTarget) -> bool {
    if target.completed {
        return true;
    }
    if facts.placement.kind == PlacementKind::Link {
        return false;
    }
    let Observation::File { signature } = &target.observation else {
        return false;
    };
    let original = match &facts.observation {
        Observation::File { signature } => Some(signature),
        _ => facts.placement.base.as_ref().map(|base| &base.signature),
    };
    #[cfg(unix)]
    if original.is_some_and(|old| old.inode.is_some() && old.inode == signature.inode)
        && target.hash.is_some()
        && (target.hash == facts.placement_hash
            || target.hash == facts.placement.base.as_ref().map(|base| base.hash)
            || target.hash == facts.placement.staged.map(|stage| stage.hash))
    {
        return true;
    }
    #[cfg(windows)]
    let _ = (original, signature);
    // The original plan also proves edited content during a cross-device retry.
    if facts.observation != Observation::Missing {
        return facts.placement_hash.is_some() && target.hash == facts.placement_hash;
    }
    #[cfg(windows)]
    {
        target.hash.is_some()
            && (target.hash == facts.placement.base.as_ref().map(|base| base.hash)
                || target.hash == facts.placement.staged.map(|stage| stage.hash))
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub fn judge_relocate(
    facts: &PlacementFacts,
    target: &RelocateTarget,
) -> Result<RelocatePlan, Refusal> {
    let path = facts.placement.path.to_path_buf();
    let refuse = |reason| Refusal {
        path: path.clone(),
        reason,
    };
    if facts.placement.kind == PlacementKind::Internal {
        return Err(refuse(RefusalReason::Internal));
    }
    if target.registered {
        return Err(refuse(RefusalReason::DestinationRegistered));
    }
    let destination = target.path.to_path_buf();
    if destination == path || destination.starts_with(&path) {
        return Err(refuse(RefusalReason::InsideSource));
    }
    if facts.observation == Observation::Missing {
        if !relocated_matches(facts, target) {
            return Err(refuse(RefusalReason::Gone));
        }
    } else {
        if target.observation != Observation::Missing && !target.completed {
            return Err(refuse(RefusalReason::DestinationExists));
        }
        match (&facts.observation, facts.placement.kind) {
            (Observation::IntactLink, PlacementKind::Link)
            | (Observation::File { .. }, PlacementKind::Copy) => {}
            (Observation::NotAFile, _) => return Err(refuse(RefusalReason::NotAFile)),
            _ => return Err(refuse(RefusalReason::LinkReplaced)),
        }
    }
    Ok(RelocatePlan {
        facts: facts.clone(),
        target: target.clone(),
    })
}

fn check_row(row: &registry::Placement, registry: &Registry) -> Result<(), RulesError> {
    if registry.placement_at(&row.path)?.as_ref() != Some(row) {
        return Err(Refusal {
            path: row.path.to_path_buf(),
            reason: RefusalReason::RegistryChanged,
        }
        .into());
    }
    Ok(())
}

fn check_file(facts: &PlacementFacts, fs: &impl PlacementOps) -> Result<(), RulesError> {
    let (id, placement) = facts.placement.to_placement();
    if fs.read(id, &placement)? != facts.observation
        || (facts.placement_hash.is_some()
            && matches!(facts.observation, Observation::File { .. })
            && Some(fs.hash_placement(&placement)?) != facts.placement_hash)
    {
        return Err(Refusal {
            path: placement.path,
            reason: RefusalReason::PlacementChanged,
        }
        .into());
    }
    Ok(())
}

// Linking changes ctime on Unix. Require the original file identity and bytes,
// and the same identity at the destination, before finishing an interrupted link.
fn pending_hard_link(
    plan: &RelocatePlan,
    source: &Observation,
    destination: &RelocateTarget,
) -> bool {
    let (
        Observation::File {
            signature: original,
        },
        Observation::File { signature: current },
        Observation::File { signature: moved },
    ) = (&plan.facts.observation, source, &destination.observation)
    else {
        return false;
    };
    plan.target.observation == Observation::Missing
        && original.inode.is_some()
        && current.inode == original.inode
        && moved.inode == original.inode
        && current.size == original.size
        && current.mtime == original.mtime
        && plan.facts.placement_hash.is_some()
        && destination.hash == plan.facts.placement_hash
}

/// Filesystem first; the same plan can finish an interrupted registry write.
pub fn apply_relocate(
    plan: &RelocatePlan,
    fs: &impl PlacementOps,
    registry: &mut Registry,
    lock: &HomeLock,
) -> Result<(), RulesError> {
    let row = &plan.facts.placement;
    check_row(row, registry)?;
    let (id, placement) = row.to_placement();
    let destination = gather_relocate_target(row, &plan.target.path, fs, registry)?;
    let source = fs.read(id, &placement)?;
    let pending_link = !destination.registered && pending_hard_link(plan, &source, &destination);
    let pending_copy = !destination.registered && destination.completed;
    let base = if !destination.registered
        && ((source == Observation::Missing && relocated_matches(&plan.facts, &destination))
            || pending_link
            || pending_copy)
    {
        if source != Observation::Missing {
            if pending_link {
                if Some(fs.hash_placement(&placement)?) != plan.facts.placement_hash {
                    return Err(Refusal {
                        path: placement.path.clone(),
                        reason: RefusalReason::PlacementChanged,
                    }
                    .into());
                }
            } else {
                check_file(&plan.facts, fs)?;
                // A receipt proves the destination, not newly gathered source facts.
                // Preserve source edits made after the receipt was written.
                if matches!(source, Observation::File { .. })
                    && Some(fs.hash_placement(&placement)?) != destination.hash
                {
                    return Err(Refusal {
                        path: placement.path.clone(),
                        reason: RefusalReason::PlacementChanged,
                    }
                    .into());
                }
            }
            fs.unlink(id, &placement, &source)?;
        }
        let mut base = row.base.clone();
        if let (Some(base), Observation::File { signature }) = (&mut base, &destination.observation)
        {
            let size = base.signature.size;
            base.signature = signature.clone();
            base.signature.size = size;
        }
        base
    } else {
        check_file(&plan.facts, fs)?;
        judge_relocate(&plan.facts, &destination)?;
        fs.relocate(
            id,
            &placement,
            &plan.target.path.to_path_buf(),
            &plan.facts.observation,
        )?
        .base
    };
    let mut updated = row.clone();
    updated.path = plan.target.path.clone();
    updated.base = base;
    registry.write(lock, |tx| {
        if tx.move_placement(&row.path, &updated.path)? != registry::MoveResult::Moved {
            return Err(RegistryError::PlacementChanged(row.path.clone()));
        }
        tx.update_placement(&updated)
    })?;
    fs.forget_relocation(&placement, &plan.target.path.to_path_buf());
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemovePlan {
    facts: PlacementFacts,
    delete_file: bool,
}

pub fn judge_remove(
    facts: &PlacementFacts,
    delete_file: bool,
    discard: bool,
) -> Result<RemovePlan, Refusal> {
    let (_, placement) = facts.placement.to_placement();
    let refuse = |reason| Refusal {
        path: placement.path.clone(),
        reason,
    };
    if placement.kind == PlacementKind::Internal {
        return Err(refuse(RefusalReason::Internal));
    }
    if delete_file && facts.observation != Observation::Missing {
        match (placement.kind, &facts.observation) {
            (PlacementKind::Link, Observation::IntactLink) => {}
            (PlacementKind::Copy, Observation::File { .. }) => {
                if !discard {
                    if facts.placement.staged.is_some() {
                        return Err(refuse(RefusalReason::Staged));
                    }
                    match placement::classify(&placement, &facts.observation, facts.placement_hash)
                    {
                        Drift::InSync => {}
                        Drift::PlacementEdited => return Err(refuse(RefusalReason::Edited)),
                        _ => return Err(refuse(RefusalReason::Unusable)),
                    }
                }
            }
            (_, Observation::NotAFile) => return Err(refuse(RefusalReason::NotAFile)),
            _ => return Err(refuse(RefusalReason::LinkReplaced)),
        }
    }
    Ok(RemovePlan {
        facts: facts.clone(),
        delete_file,
    })
}

pub fn apply_remove(
    plan: &RemovePlan,
    fs: &impl PlacementOps,
    registry: &mut Registry,
    lock: &HomeLock,
) -> Result<(), RulesError> {
    let row = &plan.facts.placement;
    check_row(row, registry)?;
    let (id, placement) = row.to_placement();
    let observation = fs.read(id, &placement)?;
    // Missing also covers a retry after unlink succeeded.
    if observation != Observation::Missing {
        check_file(&plan.facts, fs)?;
        if plan.delete_file {
            fs.unlink(id, &placement, &plan.facts.observation)?;
        }
    }
    registry.write(lock, |tx| tx.remove_placement(&row.path))?;
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepairLinkPlan {
    facts: PlacementFacts,
}

fn former_home(target: &std::path::Path, id: FolioId) -> Option<PathBuf> {
    let suffix = PathBuf::from("folios")
        .join(id.to_string())
        .join(crate::layout::CONTENT_FILE);
    let home = target.parent()?.parent()?.parent()?;
    (target.is_absolute() && home.join(suffix) == target).then(|| home.to_path_buf())
}

fn former_link_home(facts: &PlacementFacts) -> Option<PathBuf> {
    let Observation::OtherLink { target } = &facts.observation else {
        return None;
    };
    let path = facts.placement.path.to_path_buf();
    let target = path.parent()?.join(target);
    former_home(&target, facts.placement.id)
}

pub fn judge_repair_link(facts: &PlacementFacts) -> Result<RepairLinkPlan, Refusal> {
    let refuse = |reason| Refusal {
        path: facts.placement.path.to_path_buf(),
        reason,
    };
    if facts.placement.kind == PlacementKind::Internal {
        return Err(refuse(RefusalReason::Internal));
    }
    if facts.placement.kind != PlacementKind::Link {
        return Err(refuse(RefusalReason::OtherLink));
    }
    match &facts.observation {
        Observation::OtherLink { .. } if former_link_home(facts).is_some() => {}
        _ => return Err(refuse(RefusalReason::OtherLink)),
    }
    Ok(RepairLinkPlan {
        facts: facts.clone(),
    })
}

pub fn apply_repair_link(
    plan: &RepairLinkPlan,
    fs: &impl PlacementOps,
    objects: &ObjectStore,
    registry: &Registry,
    lock: &HomeLock,
) -> Result<(), RulesError> {
    let _ = lock;
    check_row(&plan.facts.placement, registry)?;
    let (id, placement) = plan.facts.placement.to_placement();
    if fs.read(id, &placement)? == Observation::IntactLink {
        return Ok(());
    }
    check_file(&plan.facts, fs)?;
    fs.refresh(
        id,
        &placement,
        objects,
        ObjectId::from_bytes([0; 32]),
        Some(&plan.facts.observation),
    )?;
    Ok(())
}

/// Rewrites every internal path in one transaction before repairing former-home links.
/// Repeating with the same old home resumes the link pass after interruption.
pub fn rehome(
    old_home: &std::path::Path,
    fs: &crate::placement_fs::PlacementFs<'_>,
    objects: &ObjectStore,
    registry: &mut Registry,
    lock: &HomeLock,
) -> Result<(), RulesError> {
    let old_home = resolved_path(old_home)?;
    let mut updates = Vec::new();
    let mut repairs = Vec::new();
    let mut prefix = None;
    for id in registry.folios()? {
        let row = registry.internal_placement(id)?.ok_or_else(|| Refusal {
            path: old_home.to_path_buf(),
            reason: RefusalReason::MissingInternal,
        })?;
        let path = row.path.to_path_buf();
        let home = former_home(&path, id).ok_or_else(|| Refusal {
            path: path.clone(),
            reason: RefusalReason::MixedHomes,
        })?;
        let home = resolved_path(&home)?;
        let current = fs.link_target(id);
        let current_home =
            resolved_path(&former_home(&current, id).expect("derived internal path"))?;
        if (home != old_home && home != current_home)
            || prefix.as_ref().is_some_and(|prefix| prefix != &home)
        {
            return Err(Refusal {
                path,
                reason: RefusalReason::MixedHomes,
            }
            .into());
        }
        prefix = Some(home);
        let new_path = registry::PlacementPath::resolve(current)?;
        if row.path != new_path {
            if registry.placement_at(&new_path)?.is_some() {
                return Err(Refusal {
                    path: new_path.to_path_buf(),
                    reason: RefusalReason::DestinationRegistered,
                }
                .into());
            }
            updates.push((row.path, new_path));
        }
        for link in registry
            .placements_of(id)?
            .into_iter()
            .filter(|row| row.kind == PlacementKind::Link)
        {
            let observation = fs.read(id, &link.to_placement().1)?;
            if observation == Observation::IntactLink {
                continue;
            }
            let facts = PlacementFacts {
                placement: link,
                observation,
                placement_hash: None,
                tip: None,
            };
            let plan = judge_repair_link(&facts)?;
            let target_home = former_link_home(&facts)
                .map(|home| resolved_path(&home))
                .transpose()?;
            if target_home.as_ref() != Some(&old_home) {
                return Err(Refusal {
                    path: facts.placement.path.to_path_buf(),
                    reason: RefusalReason::OtherLink,
                }
                .into());
            }
            repairs.push(plan);
        }
    }
    registry.write(lock, |tx| {
        for (old, new) in &updates {
            if tx.move_placement(old, new)? != registry::MoveResult::Moved {
                return Err(RegistryError::PlacementChanged(old.clone()));
            }
        }
        Ok(())
    })?;
    for plan in repairs {
        apply_repair_link(&plan, fs, objects, registry, lock)?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefusalReason {
    DestinationExists,
    DestinationRegistered,
    InsideSource,
    MixedHomes,
    Internal,
    MissingInternal,
    LinkReplaced,
    SameKind,
    CopyBranch,
    ConvertGone,
    OtherLink,
    Staged,
    InternalEdited,
    Link,
    InternalBranch,
    Edited,
    Gone,
    NotAFile,
    Unusable,
    PlacementChanged,
    RegistryChanged,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Refusal {
    pub path: PathBuf,
    pub reason: RefusalReason,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self.reason {
            RefusalReason::MissingInternal => "missing internal working-file row",
            RefusalReason::LinkReplaced => {
                "link was replaced; use the kind-change command (#33) to switch kind on purpose"
            }
            RefusalReason::Internal => "internal placements cannot be changed by this operation",
            RefusalReason::DestinationExists => "destination already exists",
            RefusalReason::DestinationRegistered => "destination is registered",
            RefusalReason::InsideSource => "destination is inside the source",
            RefusalReason::MixedHomes => "internal paths do not share the expected former home",
            RefusalReason::SameKind => "placement already has the requested kind",
            RefusalReason::CopyBranch => {
                "conversion requires main; copies on other branches are tracked in #73"
            }
            RefusalReason::ConvertGone => {
                "placement is missing; move/remove/repair is tracked in #38"
            }
            RefusalReason::OtherLink => {
                "placement points elsewhere; use the kind-change command (#33) to retarget on purpose"
            }
            RefusalReason::Staged => {
                "copy has a recorded stage; commit or checkout main with discard first"
            }
            RefusalReason::InternalEdited => {
                "internal working file holds edits; commit or checkout main with discard first (#119)"
            }
            RefusalReason::Link => "links always show folio.main",
            RefusalReason::InternalBranch => {
                "internal working files must stay on the default branch"
            }
            RefusalReason::Edited => "copy has edits; discard is required to lose them",
            RefusalReason::Gone => "placement is missing",
            RefusalReason::NotAFile => "placement is not a regular file",
            RefusalReason::Unusable => "placement is unusable",
            RefusalReason::PlacementChanged => "placement changed since the move was judged",
            RefusalReason::RegistryChanged => "registry row changed since the move was judged",
        };
        write!(
            f,
            "cannot change placement {}: {reason}",
            self.path.display()
        )
    }
}

impl Error for Refusal {}

#[derive(Debug)]
pub enum RulesError {
    Placement(PlacementError),
    Ref(RefError),
    Registry(RegistryError),
    Refused(Refusal),
}

impl From<PlacementError> for RulesError {
    fn from(error: PlacementError) -> Self {
        Self::Placement(error)
    }
}

impl From<RefError> for RulesError {
    fn from(error: RefError) -> Self {
        Self::Ref(error)
    }
}

impl From<RegistryError> for RulesError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(error)
    }
}

impl From<Refusal> for RulesError {
    fn from(error: Refusal) -> Self {
        Self::Refused(error)
    }
}

impl fmt::Display for RulesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Placement(error) => error.fmt(f),
            Self::Ref(error) => error.fmt(f),
            Self::Registry(error) => error.fmt(f),
            Self::Refused(error) => error.fmt(f),
        }
    }
}

impl Error for RulesError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Placement(error) => Some(error),
            Self::Ref(error) => Some(error),
            Self::Registry(error) => Some(error),
            Self::Refused(error) => Some(error),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::object::hash_blob;
    use crate::placement::{PlacementBase, Signature};
    use crate::registry::PlacementPath;

    pub(crate) fn facts() -> PlacementFacts {
        let hash = hash_blob(b"base");
        let signature = Signature {
            size: 4,
            mtime: None,
            ctime: None,
            inode: None,
        };
        PlacementFacts {
            placement: registry::Placement {
                path: PlacementPath::resolve(std::env::temp_dir().join("folt-unit-facts")).unwrap(),
                id: FolioId::from_bytes([1; 16]),
                kind: PlacementKind::Copy,
                head: "topic".into(),
                commit_id: Some(hash),
                base: Some(PlacementBase {
                    hash,
                    signature: signature.clone(),
                }),
                staged: None,
            },
            observation: Observation::File { signature },
            placement_hash: Some(hash),
            tip: Some(hash),
        }
    }

    pub(crate) fn target() -> MoveTarget {
        MoveTarget {
            branch: RefName::parse("topic").unwrap(),
            commit: hash_blob(b"commit"),
            content: hash_blob(b"incoming"),
        }
    }

    #[test]
    fn status_compares_all_history_states() {
        let old = hash_blob(b"old");
        let tip = hash_blob(b"tip");
        for (recorded, current, expected) in [
            (None, None, Behind::NoHistory),
            (None, Some(tip), Behind::Unrecorded),
            (Some(old), None, Behind::MissingTip),
            (Some(tip), Some(tip), Behind::UpToDate),
            (Some(old), Some(tip), Behind::Behind { tip }),
        ] {
            let mut facts = facts();
            facts.placement.commit_id = recorded;
            facts.tip = current;
            assert_eq!(judge_status(&[facts])[0].behind, Some(expected));
        }
    }

    #[test]
    fn links_have_no_behind_status() {
        let mut facts = facts();
        facts.placement.kind = PlacementKind::Link;
        facts.observation = Observation::IntactLink;
        let status = judge_status(&[facts]);
        assert_eq!(status[0].behind, None);
        assert_eq!(status[0].drift, Drift::InSync);
    }

    #[test]
    fn link_status_uses_shared_internal_working_state() {
        let mut link = facts();
        link.placement.kind = PlacementKind::Link;
        link.placement.base = None;
        link.observation = Observation::IntactLink;
        let mut internal = facts();
        internal.placement.kind = PlacementKind::Internal;
        internal.placement.head = "main".into();
        internal.placement_hash = Some(hash_blob(b"edit"));
        internal.placement.staged = Some(placement::StagedBlob {
            hash: hash_blob(b"edit"),
            size: 4,
        });
        let status = judge_status(&[link, internal]);
        assert_eq!(status[0].drift, Drift::InSync);
        assert_eq!(status[0].behind, None);
        assert_eq!(status[0].edit, crate::working_state::Edit::Edited);
        assert_eq!(status[0].staged, crate::working_state::Staged::Staged);
    }

    #[test]
    fn move_refuses_links_and_internal_branch_changes() {
        for (kind, reason) in [
            (PlacementKind::Link, RefusalReason::Link),
            (PlacementKind::Internal, RefusalReason::InternalBranch),
        ] {
            let mut facts = facts();
            facts.placement.kind = kind;
            assert_eq!(
                judge_move(&facts, &target(), true).unwrap_err().reason,
                reason
            );
        }
    }

    #[test]
    fn move_refuses_unusable_observations_even_with_discard() {
        for (observation, reason) in [
            (Observation::Missing, RefusalReason::Gone),
            (Observation::NotAFile, RefusalReason::NotAFile),
            (Observation::IntactLink, RefusalReason::Unusable),
            (
                Observation::OtherLink {
                    target: "elsewhere".into(),
                },
                RefusalReason::Unusable,
            ),
        ] {
            let mut facts = facts();
            facts.observation = observation;
            assert_eq!(
                judge_move(&facts, &target(), true).unwrap_err().reason,
                reason
            );
        }
    }

    #[test]
    fn edited_move_requires_discard_or_matching_incoming_content() {
        let mut facts = facts();
        facts.placement_hash = Some(hash_blob(b"edits"));
        assert_eq!(
            judge_move(&facts, &target(), false).unwrap_err().reason,
            RefusalReason::Edited
        );
        assert!(judge_move(&facts, &target(), true).is_ok());
        let mut target = target();
        target.content = facts.placement_hash.unwrap();
        assert!(judge_move(&facts, &target, false).is_ok());
    }

    #[test]
    fn clean_move_preserves_target_and_row() {
        let facts = facts();
        let target = target();
        let plan = judge_move(&facts, &target, false).unwrap();
        assert_eq!(plan.placement(), &facts.placement);
        assert_eq!(plan.target(), &target);
    }
    #[test]
    fn convert_decision_table() {
        let mut copy = facts();
        copy.placement.head = "main".into();
        let mut target = target();
        target.branch = RefName::parse("main").unwrap();
        target.content = copy.placement_hash.unwrap();
        let mut internal = copy.clone();
        internal.placement.kind = PlacementKind::Internal;
        let mut link = copy.clone();
        link.placement.kind = PlacementKind::Link;
        link.placement.base = None;
        link.observation = Observation::IntactLink;
        link.placement_hash = None;

        let mut cases = Vec::new();
        cases.push((copy.clone(), PlacementKind::Link, None));
        let mut behind = copy.clone();
        behind.tip = Some(hash_blob(b"new tip"));
        cases.push((behind, PlacementKind::Link, None));
        cases.push((link.clone(), PlacementKind::Copy, None));
        let mut resumed_copy = link.clone();
        resumed_copy.observation = copy.observation.clone();
        resumed_copy.placement_hash = Some(target.content);
        cases.push((resumed_copy, PlacementKind::Copy, None));
        let mut resumed_link = copy.clone();
        resumed_link.observation = Observation::IntactLink;
        cases.push((resumed_link, PlacementKind::Link, None));
        cases.push((
            internal.clone(),
            PlacementKind::Copy,
            Some(RefusalReason::Internal),
        ));
        cases.push((
            copy.clone(),
            PlacementKind::Internal,
            Some(RefusalReason::Internal),
        ));
        cases.push((
            copy.clone(),
            PlacementKind::Copy,
            Some(RefusalReason::SameKind),
        ));
        let mut topic = copy.clone();
        topic.placement.head = "topic".into();
        cases.push((topic, PlacementKind::Link, Some(RefusalReason::CopyBranch)));
        let mut staged = copy.clone();
        staged.placement.staged = Some(placement::StagedBlob {
            hash: target.content,
            size: 4,
        });
        cases.push((staged, PlacementKind::Link, Some(RefusalReason::Staged)));
        let mut edited = copy.clone();
        edited.placement_hash = Some(hash_blob(b"edit"));
        cases.push((edited, PlacementKind::Link, Some(RefusalReason::Edited)));
        let mut unhashable = copy.clone();
        unhashable.placement_hash = None;
        cases.push((
            unhashable,
            PlacementKind::Link,
            Some(RefusalReason::Unusable),
        ));
        let mut detached = link.clone();
        detached.observation = copy.observation.clone();
        detached.placement_hash = Some(hash_blob(b"edit"));
        cases.push((detached, PlacementKind::Copy, Some(RefusalReason::Edited)));
        for (observation, reason) in [
            (Observation::Missing, RefusalReason::ConvertGone),
            (Observation::NotAFile, RefusalReason::NotAFile),
            (
                Observation::OtherLink {
                    target: "other".into(),
                },
                RefusalReason::OtherLink,
            ),
        ] {
            for (mut facts, to_kind) in [
                (copy.clone(), PlacementKind::Link),
                (link.clone(), PlacementKind::Copy),
            ] {
                facts.observation = observation.clone();
                cases.push((facts, to_kind, Some(reason)));
            }
        }
        for (facts, to_kind, reason) in cases {
            assert_eq!(
                judge_convert(&facts, to_kind, &target, Some(&internal))
                    .err()
                    .map(|r| r.reason),
                reason
            );
        }
        for altered in 0..6 {
            let mut dirty = internal.clone();
            match altered {
                0 => dirty.placement_hash = Some(hash_blob(b"edit")),
                1 => dirty.observation = Observation::Missing,
                2 => {
                    dirty.placement.staged = Some(placement::StagedBlob {
                        hash: target.content,
                        size: 4,
                    })
                }
                3 => dirty.placement.id = FolioId::from_bytes([2; 16]),
                4 => dirty.placement.base = None,
                _ => dirty.placement.kind = PlacementKind::Copy,
            }
            assert_eq!(
                judge_convert(&link, PlacementKind::Copy, &target, Some(&dirty))
                    .unwrap_err()
                    .reason,
                RefusalReason::InternalEdited
            );
        }
        assert_eq!(
            judge_convert(&link, PlacementKind::Copy, &target, None)
                .unwrap_err()
                .reason,
            RefusalReason::InternalEdited
        );
        target.branch = RefName::parse("topic").unwrap();
        assert_eq!(
            judge_convert(&copy, PlacementKind::Link, &target, None)
                .unwrap_err()
                .reason,
            RefusalReason::CopyBranch
        );
    }
    #[test]
    fn relocation_and_removal_refusal_table() {
        let original = facts();
        let target = RelocateTarget {
            path: registry::PlacementPath::resolve(
                std::env::temp_dir().join("folt-unit-destination"),
            )
            .unwrap(),
            observation: Observation::Missing,
            hash: None,
            registered: false,
            completed: false,
        };
        for (kind, observation, reason) in [
            (
                PlacementKind::Internal,
                original.observation.clone(),
                RefusalReason::Internal,
            ),
            (
                PlacementKind::Copy,
                Observation::Missing,
                RefusalReason::Gone,
            ),
            (
                PlacementKind::Copy,
                Observation::NotAFile,
                RefusalReason::NotAFile,
            ),
            (
                PlacementKind::Link,
                original.observation.clone(),
                RefusalReason::LinkReplaced,
            ),
            (
                PlacementKind::Link,
                Observation::OtherLink {
                    target: "other".into(),
                },
                RefusalReason::LinkReplaced,
            ),
        ] {
            let mut source = original.clone();
            source.placement.kind = kind;
            source.observation = observation;
            assert_eq!(judge_relocate(&source, &target).unwrap_err().reason, reason);
            if kind == PlacementKind::Internal {
                for delete_file in [false, true] {
                    assert_eq!(
                        judge_remove(&source, delete_file, true).unwrap_err().reason,
                        reason
                    );
                }
            } else if source.observation == Observation::Missing {
                assert!(judge_remove(&source, true, false).is_ok());
            } else {
                assert_eq!(
                    judge_remove(&source, true, true).unwrap_err().reason,
                    reason
                );
                assert!(judge_remove(&source, false, false).is_ok());
            }
        }
        let mut occupied = target.clone();
        occupied.registered = true;
        assert_eq!(
            judge_relocate(&original, &occupied).unwrap_err().reason,
            RefusalReason::DestinationRegistered
        );
        occupied.registered = false;
        occupied.observation = Observation::NotAFile;
        assert_eq!(
            judge_relocate(&original, &occupied).unwrap_err().reason,
            RefusalReason::DestinationExists
        );
        occupied = target.clone();
        occupied.path = original.placement.path.clone();
        assert_eq!(
            judge_relocate(&original, &occupied).unwrap_err().reason,
            RefusalReason::InsideSource
        );
    }

    #[test]
    fn resume_requires_identity_and_never_just_a_hash_on_unix() {
        let mut source = facts();
        source.observation = Observation::Missing;
        let mut target = RelocateTarget {
            path: registry::PlacementPath::resolve(
                std::env::temp_dir().join("folt-unit-destination"),
            )
            .unwrap(),
            observation: Observation::File {
                signature: source.placement.base.as_ref().unwrap().signature.clone(),
            },
            hash: source.placement_hash,
            registered: false,
            completed: false,
        };
        #[cfg(unix)]
        {
            assert!(judge_relocate(&source, &target).is_err());
            source.placement.base.as_mut().unwrap().signature.inode = Some(42);
            if let Observation::File { signature } = &mut target.observation {
                signature.inode = Some(42);
            }
        }
        assert!(judge_relocate(&source, &target).is_ok());
        // Even a matching inode must refuse unrelated content.
        target.hash = Some(hash_blob(b"unrelated"));
        assert!(judge_relocate(&source, &target).is_err());
    }
}
