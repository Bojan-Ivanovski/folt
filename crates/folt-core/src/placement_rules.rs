use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::path::PathBuf;

use crate::folio_dir::FolioId;
use crate::home::HomeLock;
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

/// Collects only the supplied rows, reading each distinct Folio/branch tip once.
/// The caller selects rows with `placement_at` for one path or its own set query.
/// `open_refs` supplies a ref store for the requested Folio without selecting placements.
pub fn gather(
    placements: &[registry::Placement],
    fs: &impl PlacementOps,
    mut open_refs: impl FnMut(FolioId) -> Result<RefStore, RefError>,
) -> Result<Vec<PlacementFacts>, RulesError> {
    let mut stores = HashMap::new();
    let mut tips = HashMap::new();
    let mut facts = Vec::with_capacity(placements.len());
    for row in placements {
        let (id, placement) = row.to_placement();
        let observation = fs.read(id, &placement)?;
        let placement_hash = if placement::needs_hash(&placement, &observation)
            || matches!((&observation, row.staged),
                (Observation::File { signature }, Some(staged)) if signature.size == staged.size)
        {
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
pub fn gather_move(
    placement: &registry::Placement,
    fs: &impl PlacementOps,
    open_refs: impl FnMut(FolioId) -> Result<RefStore, RefError>,
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
    let mut updated = plan.placement.clone();
    updated.head = plan.target.branch.as_str().into();
    updated.commit_id = Some(plan.target.commit);
    updated.base = written.base;
    updated.staged = None;
    registry.write(lock, |tx| tx.update_placement(&updated))?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefusalReason {
    Link,
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
            RefusalReason::Link => "links always show folio.main",
            RefusalReason::Edited => "copy has edits; discard is required to lose them",
            RefusalReason::Gone => "placement is missing",
            RefusalReason::NotAFile => "placement is not a regular file",
            RefusalReason::Unusable => "placement is unusable",
            RefusalReason::PlacementChanged => "placement changed since the move was judged",
            RefusalReason::RegistryChanged => "registry row changed since the move was judged",
        };
        write!(f, "cannot move {}: {reason}", self.path.display())
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
