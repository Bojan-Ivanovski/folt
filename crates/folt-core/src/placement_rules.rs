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
}

pub fn judge_status(facts: &[PlacementFacts]) -> Vec<PlacementStatus> {
    facts
        .iter()
        .map(|facts| {
            let (_, placement) = facts.placement.to_placement();
            let working = crate::working_state::judge_working(facts);
            let behind = (placement.kind != PlacementKind::Link).then_some(working.behind);
            PlacementStatus {
                placement: facts.placement.clone(),
                drift: placement::classify(&placement, &facts.observation, facts.placement_hash),
                behind,
                staged: working.staged,
            }
        })
        .collect()
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefusalReason {
    Internal,
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
            RefusalReason::Internal => "internal placements cannot be converted",
            RefusalReason::SameKind => "placement already has the requested kind",
            RefusalReason::CopyBranch => {
                "conversion requires main; copies on other branches are tracked in #73"
            }
            RefusalReason::ConvertGone => {
                "placement is missing; move/remove/repair is tracked in #38"
            }
            RefusalReason::OtherLink => "placement points to another link target",
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
}
