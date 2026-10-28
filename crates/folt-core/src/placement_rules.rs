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
        Observation::Directory | Observation::NotAFile => {
            return Err(refuse(RefusalReason::NotAFile));
        }
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
        (Observation::Directory | Observation::NotAFile, _) => {
            return Err(refuse(RefusalReason::NotAFile));
        }
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

/// Destination and registry facts collected while the home lock is held.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelocateTarget {
    pub path: registry::PlacementPath,
    pub observation: Observation,
    pub registered: bool,
    pub pending: bool,
    pub rows: Vec<(registry::Placement, registry::Placement)>,
}

/// Directory callers supply a synthetic row whose path and destination use
/// `PlacementPath::resolve_directory`; its kind is Copy and its base is absent.
pub fn gather_relocate_target(
    row: &registry::Placement,
    to: &registry::PlacementPath,
    fs: &impl PlacementOps,
    registry: &Registry,
) -> Result<RelocateTarget, RulesError> {
    let from = row.path.to_path_buf();
    let destination = to.to_path_buf();
    let contains = |protected: &std::path::Path| {
        protected.starts_with(&from) || protected.starts_with(&destination)
    };
    let refuse = || Refusal {
        path: from.clone(),
        reason: RefusalReason::Internal,
    };
    if contains(&crate::placement_fs::resolved_path(registry.home_path())?) {
        return Err(refuse().into());
    }
    for id in registry.folios()? {
        if let Some(internal) = registry.internal_placement(id)?
            && contains(&internal.path.to_path_buf())
        {
            return Err(refuse().into());
        }
    }
    let (id, mut placement) = row.to_placement();
    placement.path = destination;
    let observation = fs.read(id, &placement)?;
    let mut originals = registry.placements_under(&registry::PlacementPrefix::resolve(&from)?)?;
    if registry.placement_at(&row.path)?.is_some() {
        originals = vec![row.clone()];
    } else if originals.is_empty() && fs.read(id, &row.to_placement().1)? != Observation::Directory
    {
        originals.push(row.clone());
    }
    let mut rows = Vec::with_capacity(originals.len());
    let mut registered = false;
    let mut copies = 0;
    for old in originals {
        let mut new = old.clone();
        new.path = old.path.replace_prefix(&row.path, to);
        if let Some(existing) = registry.placement_at(&new.path)? {
            if existing == new {
                copies += 1;
            } else {
                registered = true;
            }
        }
        rows.push((old, new));
    }
    // A prefix cannot absorb rows outside this move's pending set.
    let destination_rows =
        registry.placements_under(&registry::PlacementPrefix::resolve(to.to_path_buf())?)?;
    registered |= destination_rows
        .iter()
        .any(|row| !rows.iter().any(|(_, new)| new == row));
    if let Some(root) = registry.placement_at(to)? {
        registered |= !rows.iter().any(|(_, new)| new == &root);
    }
    registered |= copies != 0 && copies != rows.len();
    Ok(RelocateTarget {
        path: to.clone(),
        observation,
        registered,
        pending: !rows.is_empty() && copies == rows.len(),
        rows,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelocatePlan {
    facts: PlacementFacts,
    target: RelocateTarget,
}

/// Plans a move or the cleanup of its pending rows before reporting a collision.
pub fn judge_relocate(
    facts: &PlacementFacts,
    target: &RelocateTarget,
) -> Result<RelocatePlan, Refusal> {
    match judge_relocate_checked(facts, target) {
        Err(error)
            if target.pending
                && facts.observation != Observation::Missing
                && matches!(
                    error.reason,
                    RefusalReason::DestinationExists | RefusalReason::DestinationRegistered
                ) =>
        {
            Ok(RelocatePlan {
                facts: facts.clone(),
                target: target.clone(),
            })
        }
        result => result,
    }
}

fn judge_relocate_checked(
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
    let destination = target.path.to_path_buf();
    if destination == path || destination.starts_with(&path) {
        return Err(refuse(RefusalReason::InsideSource));
    }
    if target.registered {
        return Err(refuse(RefusalReason::DestinationRegistered));
    }
    if facts.observation == Observation::Missing {
        if !target.pending || target.observation == Observation::Missing {
            return Err(refuse(RefusalReason::Gone));
        }
    } else {
        if target.observation != Observation::Missing {
            return Err(refuse(RefusalReason::DestinationExists));
        }
        match (&facts.observation, facts.placement.kind) {
            (Observation::IntactLink, PlacementKind::Link)
            | (Observation::File { .. }, PlacementKind::Copy) => {}
            (Observation::Directory, PlacementKind::Copy)
                if !target
                    .rows
                    .iter()
                    .any(|(old, _)| old.path == facts.placement.path) => {}
            (Observation::Directory | Observation::NotAFile, _) => {
                return Err(refuse(RefusalReason::NotAFile));
            }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RelocateStep {
    Inserted,
    Renamed,
    Deleted,
}

/// Inserts destination rows, renames without replacement, then deletes source rows.
pub fn apply_relocate(
    plan: &RelocatePlan,
    fs: &impl PlacementOps,
    registry: &mut Registry,
    lock: &HomeLock,
) -> Result<(), RulesError> {
    apply_relocate_steps(plan, fs, registry, lock, |_, _| Ok(()))
}

fn apply_relocate_steps(
    plan: &RelocatePlan,
    fs: &impl PlacementOps,
    registry: &mut Registry,
    lock: &HomeLock,
    mut after: impl FnMut(RelocateStep, &Registry) -> Result<(), RulesError>,
) -> Result<(), RulesError> {
    let row = &plan.facts.placement;
    let (id, placement) = row.to_placement();
    let source = fs.read(id, &placement)?;
    let destination = gather_relocate_target(row, &plan.target.path, fs, registry)?;
    let rows = &plan.target.rows;
    let originals = rows
        .iter()
        .map(|(old, _)| registry.placement_at(&old.path))
        .collect::<Result<Vec<_>, _>>()?;
    let copies = rows
        .iter()
        .map(|(_, new)| registry.placement_at(&new.path))
        .collect::<Result<Vec<_>, _>>()?;
    let pending = copies
        .iter()
        .zip(rows)
        .all(|(copy, (_, new))| copy.as_ref() == Some(new));
    let finished = originals.iter().all(Option::is_none) && pending;
    if finished && source == Observation::Missing && destination.observation != Observation::Missing
    {
        return Ok(());
    }
    for (actual, (old, _)) in originals.iter().zip(rows) {
        if actual.as_ref() != Some(old) {
            return Err(Refusal {
                path: old.path.to_path_buf(),
                reason: RefusalReason::RegistryChanged,
            }
            .into());
        }
    }
    let cleanup = |registry: &mut Registry| -> Result<(), RulesError> {
        registry.write(lock, |tx| {
            for (_, new) in rows {
                tx.remove_placement(&new.path)?;
            }
            Ok(())
        })?;
        Ok(())
    };
    let mut current = plan.facts.clone();
    current.observation = source.clone();
    if let Err(error) = judge_relocate_checked(&current, &destination) {
        if pending && source != Observation::Missing {
            cleanup(registry)?;
        }
        return Err(error.into());
    }
    if source != Observation::Missing {
        if let Err(error) = check_file(&plan.facts, fs) {
            if pending {
                cleanup(registry)?;
            }
            return Err(error);
        }
        // Recheck the entire planned prefix before either registry transaction.
        if destination.rows != *rows {
            return Err(Refusal {
                path: placement.path,
                reason: RefusalReason::RegistryChanged,
            }
            .into());
        }
        // Normalising preserves meaning at the old path, even if the move fails.
        for (old, _) in rows {
            if old.kind == PlacementKind::Link {
                fs.normalize_link(old.id, &old.to_placement().1)?;
            }
        }
        if !pending {
            registry.write(lock, |tx| {
                for (_, new) in rows {
                    tx.insert_placement(new)?;
                }
                Ok(())
            })?;
        }
        after(RelocateStep::Inserted, registry)?;
        if let Err(error) = fs.relocate(id, &placement, &plan.target.path.to_path_buf(), &source) {
            cleanup(registry)?;
            return Err(error.into());
        }
        after(RelocateStep::Renamed, registry)?;
    } else if !pending {
        return Err(Refusal {
            path: placement.path,
            reason: RefusalReason::Gone,
        }
        .into());
    }
    registry.write(lock, |tx| {
        for (old, _) in rows {
            tx.remove_placement(&old.path)?;
        }
        Ok(())
    })?;
    after(RelocateStep::Deleted, registry)?;
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
            (_, Observation::Directory | Observation::NotAFile) => {
                return Err(refuse(RefusalReason::NotAFile));
            }
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
    apply_remove_after_row(plan, fs, registry, lock, || Ok(()))
}

fn apply_remove_after_row(
    plan: &RemovePlan,
    fs: &impl PlacementOps,
    registry: &mut Registry,
    lock: &HomeLock,
    after_delete: impl FnOnce() -> Result<(), RulesError>,
) -> Result<(), RulesError> {
    let row = &plan.facts.placement;
    check_row(row, registry)?;
    let (id, placement) = row.to_placement();
    let observation = fs.read(id, &placement)?;
    if observation != Observation::Missing {
        check_file(&plan.facts, fs)?;
    }
    registry.write(lock, |tx| tx.remove_placement(&row.path))?;
    after_delete()?;
    if plan.delete_file && observation != Observation::Missing {
        fs.unlink(id, &placement, &plan.facts.observation)?;
    }
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
            registered: false,
            pending: false,
            rows: Vec::new(),
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

    struct MoveFixture {
        root: PathBuf,
        home: crate::home::Home,
        row: registry::Placement,
    }

    impl MoveFixture {
        fn new(directory: bool) -> (Self, HomeLock, Registry) {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "folt-move-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let home = crate::home::Home::open_or_create(root.join("home")).unwrap();
            let root = std::fs::canonicalize(root).unwrap();
            let lock = home.try_lock().unwrap();
            let mut registry = Registry::open_or_create(&home, &lock).unwrap();
            let mut row = facts().placement;
            row.path = registry::PlacementPath::resolve(root.join("source")).unwrap();
            let fs = crate::placement_fs::PlacementFs::new(&home);
            if directory {
                std::fs::create_dir(row.path.to_path_buf()).unwrap();
            }
            registry
                .write(&lock, |tx| {
                    tx.add_folio(&registry::NewFolio {
                        id: row.id,
                        format: registry::FolioFormat {
                            layout: crate::layout::FOLIO_LAYOUT_VERSION,
                            algorithm: crate::layout::OBJECT_ALGORITHM.into(),
                        },
                    })?;
                    for index in 0..if directory { 3 } else { 1 } {
                        let mut child = row.clone();
                        if directory {
                            child.path = registry::PlacementPath::resolve(
                                row.path.to_path_buf().join(format!("file-{index}")),
                            )?;
                        }
                        std::fs::write(child.path.to_path_buf(), b"edited content").unwrap();
                        child.staged = Some(placement::StagedBlob {
                            hash: hash_blob(b"stage"),
                            size: 5,
                        });
                        tx.insert_placement(&child)?;
                        if !directory {
                            row = child;
                        }
                    }
                    Ok(())
                })
                .unwrap();
            assert_eq!(
                fs.read(row.id, &row.to_placement().1).unwrap() == Observation::Directory,
                directory
            );
            (Self { root, home, row }, lock, registry)
        }

        fn facts(&self) -> PlacementFacts {
            let fs = crate::placement_fs::PlacementFs::new(&self.home);
            let observation = fs.read(self.row.id, &self.row.to_placement().1).unwrap();
            let placement_hash = matches!(observation, Observation::File { .. })
                .then(|| fs.hash_placement(&self.row.to_placement().1).unwrap());
            PlacementFacts {
                placement: self.row.clone(),
                observation,
                placement_hash,
                tip: None,
            }
        }

        fn plan(&self, registry: &Registry) -> RelocatePlan {
            let fs = crate::placement_fs::PlacementFs::new(&self.home);
            let to =
                registry::PlacementPath::resolve_directory(self.root.join("destination")).unwrap();
            let target = gather_relocate_target(&self.row, &to, &fs, registry).unwrap();
            judge_relocate(&self.facts(), &target).unwrap()
        }
    }

    impl Drop for MoveFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn assert_move_rows(plan: &RelocatePlan, registry: &Registry, old: bool, new: bool) {
        for (source, destination) in &plan.target.rows {
            assert_eq!(
                registry.placement_at(&source.path).unwrap(),
                old.then(|| source.clone())
            );
            assert_eq!(
                registry.placement_at(&destination.path).unwrap(),
                new.then(|| destination.clone())
            );
        }
    }

    #[test]
    fn move_crash_windows_reopen_and_reuse_pending_rows() {
        for directory in [false, true] {
            for crash in [
                RelocateStep::Inserted,
                RelocateStep::Renamed,
                RelocateStep::Deleted,
            ] {
                let (fixture, lock, mut registry) = MoveFixture::new(directory);
                let fs = crate::placement_fs::PlacementFs::new(&fixture.home);
                let plan = fixture.plan(&registry);
                assert_eq!(plan.target.rows.len(), if directory { 3 } else { 1 });
                let result =
                    apply_relocate_steps(&plan, &fs, &mut registry, &lock, |step, registry| {
                        assert_move_rows(&plan, registry, step != RelocateStep::Deleted, true);
                        if step == crash {
                            Err(PlacementError::PlacementChanged.into())
                        } else {
                            Ok(())
                        }
                    });
                assert!(result.is_err());
                assert_eq!(
                    fixture.row.path.to_path_buf().exists(),
                    crash == RelocateStep::Inserted
                );
                assert_eq!(
                    plan.target.path.to_path_buf().exists(),
                    crash != RelocateStep::Inserted
                );
                drop(registry);
                let mut registry = Registry::open_or_create(&fixture.home, &lock).unwrap();
                // Re-gather after a restart while the old rows remain. Reusing a
                // pending set must succeed even though another INSERT would fail.
                let retry = if crash == RelocateStep::Deleted {
                    plan.clone()
                } else {
                    fixture.plan(&registry)
                };
                apply_relocate(&retry, &fs, &mut registry, &lock).unwrap();
                assert_move_rows(&plan, &registry, false, true);
                apply_relocate(&plan, &fs, &mut registry, &lock).unwrap();
                if directory {
                    for index in 0..3 {
                        assert_eq!(
                            std::fs::read(
                                plan.target.path.to_path_buf().join(format!("file-{index}"))
                            )
                            .unwrap(),
                            b"edited content"
                        );
                    }
                } else {
                    assert_eq!(
                        std::fs::read(plan.target.path.to_path_buf()).unwrap(),
                        b"edited content"
                    );
                }
            }
        }
    }

    #[test]
    fn rename_failure_rolls_back_new_rows_and_preserves_racing_destinations() {
        for directory in [false, true] {
            for target_directory in [false, true] {
                let (fixture, lock, mut registry) = MoveFixture::new(directory);
                let fs = crate::placement_fs::PlacementFs::new(&fixture.home);
                let plan = fixture.plan(&registry);
                let destination = plan.target.path.to_path_buf();
                let result =
                    apply_relocate_steps(&plan, &fs, &mut registry, &lock, |step, registry| {
                        assert_eq!(step, RelocateStep::Inserted);
                        assert_move_rows(&plan, registry, true, true);
                        if target_directory {
                            std::fs::create_dir(&destination).unwrap();
                        } else {
                            std::fs::write(&destination, b"other process").unwrap();
                        }
                        Ok(())
                    });
                assert!(result.is_err());
                assert_move_rows(&plan, &registry, true, false);
                assert!(fixture.row.path.to_path_buf().exists());
                assert_eq!(destination.is_dir(), target_directory);
                if !target_directory {
                    assert_eq!(std::fs::read(destination).unwrap(), b"other process");
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn rename_race_preserves_a_new_link() {
        let (fixture, lock, mut registry) = MoveFixture::new(false);
        let fs = crate::placement_fs::PlacementFs::new(&fixture.home);
        let plan = fixture.plan(&registry);
        let result = apply_relocate_steps(&plan, &fs, &mut registry, &lock, |step, _| {
            assert_eq!(step, RelocateStep::Inserted);
            std::os::unix::fs::symlink("foreign target", plan.target.path.to_path_buf()).unwrap();
            Ok(())
        });
        assert!(result.is_err());
        assert_move_rows(&plan, &registry, true, false);
        assert_eq!(
            std::fs::read_link(plan.target.path.to_path_buf()).unwrap(),
            PathBuf::from("foreign target")
        );
    }

    #[test]
    fn pending_rows_are_cleaned_when_retry_destination_is_occupied() {
        let (fixture, lock, mut registry) = MoveFixture::new(true);
        let fs = crate::placement_fs::PlacementFs::new(&fixture.home);
        let plan = fixture.plan(&registry);
        assert!(
            apply_relocate_steps(&plan, &fs, &mut registry, &lock, |_, _| {
                Err(PlacementError::PlacementChanged.into())
            })
            .is_err()
        );
        std::fs::create_dir(plan.target.path.to_path_buf()).unwrap();
        let retry = fixture.plan(&registry);
        assert!(apply_relocate(&retry, &fs, &mut registry, &lock).is_err());
        assert_move_rows(&plan, &registry, true, false);
        assert!(fixture.row.path.to_path_buf().exists());
    }

    #[test]
    fn directory_registry_transactions_are_all_or_nothing() {
        for insert in [false, true] {
            let (fixture, lock, mut registry) = MoveFixture::new(true);
            let fs = crate::placement_fs::PlacementFs::new(&fixture.home);
            let plan = fixture.plan(&registry);
            let (old, new) = &plan.target.rows[1];
            registry.fail_writes_at(if insert { &new.path } else { &old.path }, insert);
            let result = apply_relocate(&plan, &fs, &mut registry, &lock);
            assert!(result.is_err());
            assert_move_rows(&plan, &registry, true, !insert);
            assert_eq!(fixture.row.path.to_path_buf().exists(), insert);
            assert_eq!(plan.target.path.to_path_buf().exists(), !insert);
        }
    }

    #[test]
    fn remove_crash_leaves_only_an_unregistered_file() {
        for delete_file in [false, true] {
            let (fixture, lock, mut registry) = MoveFixture::new(false);
            let fs = crate::placement_fs::PlacementFs::new(&fixture.home);
            let plan = judge_remove(&fixture.facts(), delete_file, true).unwrap();
            assert!(
                apply_remove_after_row(&plan, &fs, &mut registry, &lock, || {
                    Err(PlacementError::PlacementChanged.into())
                })
                .is_err()
            );
            assert!(registry.placement_at(&fixture.row.path).unwrap().is_none());
            assert_eq!(
                std::fs::read(fixture.row.path.to_path_buf()).unwrap(),
                b"edited content"
            );
        }
    }
    #[cfg(unix)]
    #[test]
    fn link_move_retries_each_crash_window_using_only_pending_rows() {
        for crash in [
            RelocateStep::Inserted,
            RelocateStep::Renamed,
            RelocateStep::Deleted,
        ] {
            let (mut fixture, lock, mut registry) = MoveFixture::new(false);
            let fs = crate::placement_fs::PlacementFs::new(&fixture.home);
            let target = fs.link_target(fixture.row.id);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(&target, b"shared content").unwrap();
            std::fs::remove_file(fixture.row.path.to_path_buf()).unwrap();
            std::os::unix::fs::symlink(&target, fixture.row.path.to_path_buf()).unwrap();
            fixture.row.kind = PlacementKind::Link;
            fixture.row.head = "main".into();
            fixture.row.commit_id = None;
            fixture.row.base = None;
            fixture.row.staged = None;
            registry
                .write(&lock, |tx| tx.update_placement(&fixture.row))
                .unwrap();
            let plan = fixture.plan(&registry);
            assert!(
                apply_relocate_steps(&plan, &fs, &mut registry, &lock, |step, _| {
                    if step == crash {
                        Err(PlacementError::PlacementChanged.into())
                    } else {
                        Ok(())
                    }
                })
                .is_err()
            );
            drop(registry);
            let mut registry = Registry::open_or_create(&fixture.home, &lock).unwrap();
            let retry = if crash == RelocateStep::Deleted {
                plan.clone()
            } else {
                fixture.plan(&registry)
            };
            apply_relocate(&retry, &fs, &mut registry, &lock).unwrap();
            assert_move_rows(&plan, &registry, false, true);
            assert_eq!(
                std::fs::read_link(plan.target.path.to_path_buf()).unwrap(),
                target
            );
            assert_eq!(
                std::fs::read(plan.target.path.to_path_buf()).unwrap(),
                b"shared content"
            );
        }
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn relative_link_normalization_preserves_registry_failure_windows() {
        for insert in [true, false] {
            let (mut fixture, lock, mut registry) = MoveFixture::new(false);
            let fs = crate::placement_fs::PlacementFs::new(&fixture.home);
            if let Err(error) = fs.probe_link(&fixture.row.path.to_path_buf()) {
                eprintln!("skipping relative link failure windows: {error}");
                return;
            }
            let target = fs.link_target(fixture.row.id);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(&target, b"shared content").unwrap();
            let relative = target
                .strip_prefix(fixture.home.path().parent().unwrap())
                .unwrap();
            std::fs::remove_file(fixture.row.path.to_path_buf()).unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(relative, fixture.row.path.to_path_buf()).unwrap();
            #[cfg(windows)]
            std::os::windows::fs::symlink_file(relative, fixture.row.path.to_path_buf()).unwrap();
            fixture.row.kind = PlacementKind::Link;
            fixture.row.head = "main".into();
            fixture.row.commit_id = None;
            fixture.row.base = None;
            fixture.row.staged = None;
            registry
                .write(&lock, |tx| tx.update_placement(&fixture.row))
                .unwrap();
            let plan = fixture.plan(&registry);
            let (old, new) = &plan.target.rows[0];
            registry.fail_writes_at(if insert { &new.path } else { &old.path }, insert);
            assert!(apply_relocate(&plan, &fs, &mut registry, &lock).is_err());
            assert_move_rows(&plan, &registry, true, !insert);
            let mut present = if insert { old.clone() } else { new.clone() };
            assert_eq!(
                fs.read(present.id, &present.to_placement().1).unwrap(),
                Observation::IntactLink
            );
            assert!(
                std::fs::read_link(present.path.to_path_buf())
                    .unwrap()
                    .is_absolute()
            );
            assert_eq!(
                std::fs::read(present.path.to_path_buf()).unwrap(),
                b"shared content"
            );
            assert_eq!(old.path.to_path_buf().exists(), insert);
            assert_eq!(new.path.to_path_buf().exists(), !insert);
            registry.clear_write_failure();
            drop(registry);
            let mut registry = Registry::open_or_create(&fixture.home, &lock).unwrap();
            let retry = fixture.plan(&registry);
            apply_relocate(&retry, &fs, &mut registry, &lock).unwrap();
            assert_move_rows(&plan, &registry, false, true);
            present.path = new.path.clone();
            assert_eq!(
                fs.read(present.id, &present.to_placement().1).unwrap(),
                Observation::IntactLink
            );
            assert!(
                std::fs::read_link(present.path.to_path_buf())
                    .unwrap()
                    .is_absolute()
            );
        }
    }

    #[test]
    fn pending_copy_requires_every_field_to_match() {
        for field in 0..5 {
            let (fixture, lock, mut registry) = MoveFixture::new(false);
            let plan = fixture.plan(&registry);
            let fs = crate::placement_fs::PlacementFs::new(&fixture.home);
            let mut other = plan.target.rows[0].1.clone();
            match field {
                0 => other.head = "other".into(),
                1 => other.commit_id = None,
                2 => other.base = None,
                3 => other.staged = None,
                _ => {
                    other.kind = PlacementKind::Link;
                    other.head = "main".into();
                    other.commit_id = None;
                    other.base = None;
                    other.staged = None;
                }
            }
            registry
                .write(&lock, |tx| tx.insert_placement(&other))
                .unwrap();
            let target =
                gather_relocate_target(&fixture.row, &plan.target.path, &fs, &registry).unwrap();
            assert!(!target.pending);
            assert_eq!(
                judge_relocate(&fixture.facts(), &target)
                    .unwrap_err()
                    .reason,
                RefusalReason::DestinationRegistered
            );
            assert!(apply_relocate(&plan, &fs, &mut registry, &lock).is_err());
            assert_eq!(registry.placement_at(&other.path).unwrap(), Some(other));
            assert_eq!(
                registry.placement_at(&fixture.row.path).unwrap(),
                Some(fixture.row.clone())
            );
        }
    }

    #[test]
    fn directory_plan_refuses_inside_source_and_unrelated_destination_rows() {
        let (fixture, lock, mut registry) = MoveFixture::new(true);
        let fs = crate::placement_fs::PlacementFs::new(&fixture.home);
        let path =
            registry::PlacementPath::resolve(fixture.row.path.to_path_buf().join("child")).unwrap();
        let target = gather_relocate_target(&fixture.row, &path, &fs, &registry).unwrap();
        assert_eq!(
            judge_relocate(&fixture.facts(), &target)
                .unwrap_err()
                .reason,
            RefusalReason::InsideSource
        );
        let plan = fixture.plan(&registry);
        // A registered path under an absent destination is still a collision.
        let mut foreign = plan.target.rows[0].1.clone();
        foreign.head = "foreign".into();
        registry
            .write(&lock, |tx| tx.insert_placement(&foreign))
            .unwrap();
        let target =
            gather_relocate_target(&fixture.row, &plan.target.path, &fs, &registry).unwrap();
        assert_eq!(
            judge_relocate(&fixture.facts(), &target)
                .unwrap_err()
                .reason,
            RefusalReason::DestinationRegistered
        );
        assert!(apply_relocate(&plan, &fs, &mut registry, &lock).is_err());
        assert_eq!(registry.placement_at(&foreign.path).unwrap(), Some(foreign));
    }
    #[test]
    fn a_registered_copy_replaced_by_a_directory_is_refused() {
        let (fixture, _lock, registry) = MoveFixture::new(false);
        std::fs::remove_file(fixture.row.path.to_path_buf()).unwrap();
        std::fs::create_dir(fixture.row.path.to_path_buf()).unwrap();
        let fs = crate::placement_fs::PlacementFs::new(&fixture.home);
        let to = registry::PlacementPath::resolve(fixture.root.join("destination")).unwrap();
        let target = gather_relocate_target(&fixture.row, &to, &fs, &registry).unwrap();
        assert_eq!(
            judge_relocate(&fixture.facts(), &target)
                .unwrap_err()
                .reason,
            RefusalReason::NotAFile
        );
    }
}
