use std::error::Error;
use std::fmt;

use crate::home::HomeLock;
use crate::placement::PlacementOps;
use crate::placement_rules::{self, MoveTarget, RulesError};
use crate::refs::{Kind, RefError, RefName, RefStore};
use crate::registry::{Placement, Registry};
use crate::store::{ObjectStore, StoreError};

/// Stores for one Folio and the caller's bound for commit object reads.
#[derive(Clone, Copy)]
pub struct CheckoutStores<'a> {
    pub objects: &'a ObjectStore,
    pub refs: &'a RefStore,
    pub object_limit: u64,
}

/// Resolves only branch refs; tags and detached commit ids are not checkout targets.
pub fn resolve(
    refs: &RefStore,
    objects: &ObjectStore,
    branch: &RefName,
    object_limit: u64,
) -> Result<MoveTarget, CheckoutError> {
    let commit = refs
        .read(Kind::Branch, branch)?
        .ok_or_else(|| CheckoutError::MissingBranch(branch.clone()))?;
    let content = objects.get_commit(commit, object_limit)?.content;
    Ok(MoveTarget {
        branch: branch.clone(),
        commit,
        content,
    })
}

/// Moves one copy or internal row to a branch tip using the placement move rules.
/// The stores must belong to the row's Folio. The home lock must cover selecting
/// the row and the entire checkout. Internal rows can only check out `main`.
pub fn checkout_placement(
    row: &Placement,
    branch: &RefName,
    discard: bool,
    fs: &impl PlacementOps,
    stores: &CheckoutStores<'_>,
    registry: &mut Registry,
    lock: &HomeLock,
) -> Result<(), CheckoutError> {
    let target = resolve(stores.refs, stores.objects, branch, stores.object_limit)?;
    let facts = placement_rules::gather_move(row, fs, |_| Ok(stores.refs))?;
    let plan = placement_rules::judge_move(&facts, &target, discard).map_err(RulesError::from)?;
    placement_rules::apply_move(&plan, fs, stores.objects, registry, lock)?;
    Ok(())
}

#[derive(Debug)]
pub enum CheckoutError {
    MissingBranch(RefName),
    Ref(RefError),
    Store(StoreError),
    Rules(RulesError),
}

impl From<RefError> for CheckoutError {
    fn from(error: RefError) -> Self {
        Self::Ref(error)
    }
}

impl From<StoreError> for CheckoutError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<RulesError> for CheckoutError {
    fn from(error: RulesError) -> Self {
        Self::Rules(error)
    }
}

impl fmt::Display for CheckoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingBranch(branch) => write!(f, "branch has no tip: {branch}"),
            Self::Ref(error) => error.fmt(f),
            Self::Store(error) => error.fmt(f),
            Self::Rules(error) => error.fmt(f),
        }
    }
}

impl Error for CheckoutError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::MissingBranch(_) => None,
            Self::Ref(error) => Some(error),
            Self::Store(error) => Some(error),
            Self::Rules(error) => Some(error),
        }
    }
}
