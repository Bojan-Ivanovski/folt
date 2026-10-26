use std::error::Error;
use std::fmt;
use std::path::PathBuf;

use crate::folio_dir::FolioId;
use crate::home::HomeLock;
use crate::layout::DEFAULT_BRANCH;
use crate::object::{
    Author, Commit, CommitTime, ObjectError, ObjectId, ObjectKind, Tag, hash_blob,
};
use crate::placement::{
    Observation, PlacementBase, PlacementError, PlacementKind, PlacementOps, Signature,
};
use crate::placement_rules::{self, MovePlan, MoveTarget, PlacementFacts, RulesError};
use crate::refs::{Expect, Kind, RefError, RefName, RefStore};
use crate::registry::{Placement, Registry, RegistryError};
use crate::store::{ObjectStore, StoreError};
use crate::working_state::{self, Edit};

/// Stores for one Folio and the caller's bound for commit object reads.
#[derive(Clone, Copy)]
pub struct HistoryStores<'a> {
    pub objects: &'a ObjectStore,
    pub refs: &'a RefStore,
    pub object_limit: u64,
}

/// Facts for one commit, including the placements that can identify an advanced tip.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitFacts {
    pub working: PlacementFacts,
    pub internal: Option<PlacementFacts>,
    pub placements: Vec<Placement>,
    pub tip_commit: Option<Commit>,
}

/// The home lock must cover gathering, judging and applying history operations.
/// `object_limit` bounds commit reads independently of blob size.
pub fn gather_commit(
    row: &Placement,
    fs: &impl PlacementOps,
    stores: &HistoryStores<'_>,
    registry: &Registry,
) -> Result<CommitFacts, HistoryError> {
    let HistoryStores {
        objects,
        refs,
        object_limit,
    } = *stores;
    let working = working_state::gather(row, fs, refs)?;
    let placements = registry.placements_of_head(row.id, &row.head)?;
    let internal = if row.kind == PlacementKind::Copy && row.head == DEFAULT_BRANCH {
        let internal = placements
            .iter()
            .find(|row| row.kind == PlacementKind::Internal)
            .ok_or(Refusal::MissingInternal)?;
        Some(gather_file(internal, fs, refs)?)
    } else {
        None
    };
    let tip_commit = working
        .tip
        .map(|id| objects.get_commit(id, object_limit))
        .transpose()?;
    Ok(CommitFacts {
        working,
        internal,
        placements,
        tip_commit,
    })
}

/// An allowed commit; content and parent are fixed by the recorded stage and base.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitPlan {
    facts: CommitFacts,
    resume: Option<ObjectId>,
}

impl CommitPlan {
    pub fn placement(&self) -> &Placement {
        &self.facts.working.placement
    }
    pub fn resumed_commit(&self) -> Option<ObjectId> {
        self.resume
    }
}

/// Recognises a published commit whose remaining file/registry work can be finished.
pub fn resume_commit(facts: &CommitFacts) -> Option<ObjectId> {
    let row = &facts.working.placement;
    let staged = row.staged?;
    let tip = facts.working.tip?;
    let commit = facts.tip_commit.as_ref()?;
    (Some(tip) != row.commit_id
        && commit.content == staged.hash
        && commit.parents == row.commit_id.into_iter().collect::<Vec<_>>())
    .then_some(tip)
}

fn matches_requested_commit(actual: &Commit, requested: &Commit) -> bool {
    actual.author == requested.author
        && actual.message == requested.message
        && actual.body.as_deref() == requested.body.as_deref().filter(|body| !body.is_empty())
        && actual.group == requested.group
        && actual.content == requested.content
        && actual.parents == requested.parents
}

pub fn judge_commit(facts: &CommitFacts) -> Result<CommitPlan, Refusal> {
    let row = &facts.working.placement;
    if row.kind == PlacementKind::Link {
        return Err(Refusal::Link(row.path.to_path_buf()));
    }
    let staged = row.staged.ok_or(Refusal::NothingStaged)?;
    if !matches!(facts.working.observation, Observation::File { .. }) {
        return Err(Refusal::Unusable(row.path.to_path_buf()));
    }
    if row.kind == PlacementKind::Internal && row.head != DEFAULT_BRANCH {
        return Err(Refusal::InvalidFacts);
    }
    let resume = resume_commit(facts);
    if facts.working.tip != row.commit_id && resume.is_none() {
        return Err(branch_moved(facts));
    }
    if row.kind == PlacementKind::Copy && row.head == DEFAULT_BRANCH {
        let internal = facts.internal.as_ref().ok_or(Refusal::MissingInternal)?;
        if internal.placement.id != row.id
            || internal.placement.kind != PlacementKind::Internal
            || internal.placement.head != DEFAULT_BRANCH
        {
            return Err(Refusal::InvalidFacts);
        }
        // A completed file replacement may precede the atomic registry update.
        let already_written = resume.is_some() && internal.placement_hash == Some(staged.hash);
        if working_state::judge_working(internal).edit != Edit::Clean && !already_written {
            return Err(Refusal::InternalEdited(
                internal.placement.path.to_path_buf(),
            ));
        }
        // Only the branch and content affect move permission; apply supplies
        // the new commit id once its caller-provided metadata has been encoded.
        let target = MoveTarget {
            branch: RefName::parse(DEFAULT_BRANCH).expect("default branch is valid"),
            commit: resume.or(row.commit_id).unwrap_or(staged.hash),
            content: staged.hash,
        };
        placement_rules::judge_move(internal, &target, false)?;
    }
    Ok(CommitPlan {
        facts: facts.clone(),
        resume,
    })
}

fn branch_moved(facts: &CommitFacts) -> Refusal {
    Refusal::BranchMoved {
        tip: facts.working.tip,
        ahead: facts
            .placements
            .iter()
            .filter(|row| {
                facts.working.tip.is_some()
                    && row.commit_id == facts.working.tip
                    && row.path != facts.working.placement.path
                    && row.kind != PlacementKind::Link
            })
            .map(|row| row.path.to_path_buf())
            .collect(),
    }
}

/// Metadata comes from the caller; core history never reads identity or a clock.
pub struct CommitMetadata {
    pub author: Author,
    pub time: CommitTime,
    pub message: String,
    pub body: Option<String>,
    pub group: Option<String>,
}

/// Publishes objects, then the ref, then files, then both registry rows together.
pub fn apply_commit(
    plan: &CommitPlan,
    metadata: CommitMetadata,
    fs: &impl PlacementOps,
    stores: &HistoryStores<'_>,
    registry: &mut Registry,
    lock: &mut HomeLock,
) -> Result<ObjectId, HistoryError> {
    let HistoryStores {
        objects,
        refs,
        object_limit,
    } = *stores;
    let row = plan.placement();
    require_row(registry, row)?;
    require_observation(&plan.facts.working, fs)?;
    let mut facts = gather_commit(row, fs, stores, registry)?;
    let checked = judge_commit(&facts)?;
    if let Some(internal) = &plan.facts.internal {
        require_row(registry, &internal.placement)?;
        if checked.resume.is_none() {
            require_observation(internal, fs)?;
        }
    }
    let staged = row.staged.expect("judged commit has a stage");
    let commit = Commit {
        content: staged.hash,
        parents: row.commit_id.into_iter().collect(),
        author: metadata.author,
        time: metadata.time,
        message: metadata.message,
        body: metadata.body,
        group: metadata.group,
    };
    // Validate all metadata before any writes, even when finishing a prior commit.
    commit.encode_body()?;
    if checked.resume.is_some()
        && !facts
            .tip_commit
            .as_ref()
            .is_some_and(|tip| matches_requested_commit(tip, &commit))
    {
        return Err(branch_moved(&facts).into());
    }
    if staged.hash == hash_blob(b"") {
        if staged.size != 0 {
            return Err(Refusal::InvalidFacts.into());
        }
        objects.put(lock, ObjectKind::Blob, b"")?;
    } else {
        require_blob(objects, staged.hash)?;
    }
    let mut id = if let Some(id) = checked.resume {
        id
    } else {
        objects.put_commit(lock, &commit)?
    };
    objects.sync_blobs()?;
    let name = RefName::parse(&row.head)?;
    let expect = row.commit_id.map_or(Expect::Absent, Expect::Is);
    match refs.update(lock, Kind::Branch, &name, id, expect) {
        Ok(()) => {}
        Err(RefError::Stale { actual }) => {
            facts.working.tip = actual;
            facts.tip_commit = actual
                .map(|id| objects.get_commit(id, object_limit))
                .transpose()?;
            if !facts
                .tip_commit
                .as_ref()
                .is_some_and(|tip| matches_requested_commit(tip, &commit))
            {
                return Err(branch_moved(&facts).into());
            }
            id = judge_commit(&facts)?
                .resume
                .ok_or_else(|| branch_moved(&facts))?;
        }
        Err(error) => return Err(error.into()),
    }
    refs.sync_refs()?;
    let internal = if let Some(internal) = &facts.internal {
        let target = MoveTarget {
            branch: name,
            commit: id,
            content: staged.hash,
        };
        let move_plan = placement_rules::judge_move(internal, &target, false)?;
        let base = placement_rules::apply_move_file(&move_plan, fs, objects, registry, lock)?;
        Some((
            internal.placement.clone(),
            placement_rules::moved_row(&move_plan, base),
        ))
    } else {
        None
    };
    let mut updated = row.clone();
    updated.commit_id = Some(id);
    updated.staged = None;
    let Observation::File { signature } = &facts.working.observation else {
        unreachable!("judged file")
    };
    updated.base = Some(PlacementBase {
        hash: staged.hash,
        signature: if facts.working.placement_hash == Some(staged.hash) {
            signature.clone()
        } else {
            // Post-stage edits stay in the file; their metadata is not the staged base.
            Signature {
                size: staged.size,
                mtime: None,
                ctime: None,
                inode: None,
            }
        },
    });
    registry.write(lock, |tx| {
        if tx.placement_at(&row.path)?.as_ref() != Some(row) {
            return Err(RegistryError::PlacementChanged(row.path.clone()));
        }
        if let Some((before, after)) = &internal {
            if tx.placement_at(&before.path)?.as_ref() != Some(before) {
                return Err(RegistryError::PlacementChanged(before.path.clone()));
            }
            tx.update_placement(after)?;
        }
        tx.update_placement(&updated)
    })?;
    Ok(id)
}

fn require_row(registry: &Registry, row: &Placement) -> Result<(), HistoryError> {
    if registry.placement_at(&row.path)?.as_ref() != Some(row) {
        return Err(Refusal::RegistryChanged(row.path.to_path_buf()).into());
    }
    Ok(())
}

fn require_observation(facts: &PlacementFacts, fs: &impl PlacementOps) -> Result<(), HistoryError> {
    if fs.read(facts.placement.id, &facts.placement.to_placement().1)? != facts.observation {
        return Err(Refusal::PlacementChanged(facts.placement.path.to_path_buf()).into());
    }
    Ok(())
}

pub fn create_branch(
    name: &RefName,
    commit: ObjectId,
    refs: &RefStore,
    lock: &mut HomeLock,
) -> Result<(), HistoryError> {
    refs.update(lock, Kind::Branch, name, commit, Expect::Absent)?;
    refs.sync_refs()?;
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResetFacts {
    pub id: FolioId,
    pub target: MoveTarget,
    pub old: ObjectId,
    pub placements: Vec<PlacementFacts>,
}

pub fn gather_reset(
    id: FolioId,
    branch: &RefName,
    commit: ObjectId,
    fs: &impl PlacementOps,
    stores: &HistoryStores<'_>,
    registry: &Registry,
) -> Result<ResetFacts, HistoryError> {
    let HistoryStores {
        objects,
        refs,
        object_limit,
    } = *stores;
    let old = refs
        .read(Kind::Branch, branch)?
        .ok_or(Refusal::MissingBranch)?;
    let target = objects.get_commit(commit, object_limit)?;
    require_blob(objects, target.content)?;
    let rows = registry.placements_of_head(id, branch.as_str())?;
    let mut placements = Vec::new();
    for row in rows {
        // Links follow the internal file and have no recorded commit or base to move.
        if row.kind == PlacementKind::Link {
            continue;
        }
        placements.push(gather_file(&row, fs, refs)?);
    }
    Ok(ResetFacts {
        id,
        target: MoveTarget {
            branch: branch.clone(),
            commit,
            content: target.content,
        },
        old,
        placements,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResetPlan {
    facts: ResetFacts,
    moves: Vec<MovePlan>,
}

pub fn judge_reset(facts: &ResetFacts, discard: bool) -> Result<ResetPlan, Refusal> {
    let mut moves = Vec::new();
    for placement in &facts.placements {
        if placement.placement.id != facts.id
            || placement.placement.head != facts.target.branch.as_str()
            || placement.tip != Some(facts.old)
        {
            return Err(Refusal::InvalidFacts);
        }
        moves.push(placement_rules::judge_move(
            placement,
            &facts.target,
            discard,
        )?);
    }
    Ok(ResetPlan {
        facts: facts.clone(),
        moves,
    })
}

pub fn apply_reset(
    plan: &ResetPlan,
    fs: &impl PlacementOps,
    stores: &HistoryStores<'_>,
    registry: &mut Registry,
    lock: &mut HomeLock,
) -> Result<(), HistoryError> {
    let HistoryStores { objects, refs, .. } = *stores;
    let facts = &plan.facts;
    let actual = registry.placements_of_head(facts.id, facts.target.branch.as_str())?;
    let expected: Vec<_> = facts
        .placements
        .iter()
        .map(|facts| facts.placement.clone())
        .collect();
    let actual: Vec<_> = actual
        .into_iter()
        .filter(|row| row.kind != PlacementKind::Link)
        .collect();
    if actual != expected {
        return Err(Refusal::PlacementsChanged.into());
    }
    for placement in &facts.placements {
        require_observation(placement, fs)?;
    }
    require_blob(objects, facts.target.content)?;
    objects.sync_blobs()?;
    refs.update(
        lock,
        Kind::Branch,
        &facts.target.branch,
        facts.target.commit,
        Expect::Is(facts.old),
    )?;
    refs.sync_refs()?;
    for movement in &plan.moves {
        placement_rules::apply_move(movement, fs, objects, registry, lock)?;
    }
    Ok(())
}

/// Pass `placements_of_head`, including the internal row, to this pure judge.
pub fn judge_delete_branch(placements: &[Placement]) -> Result<(), Refusal> {
    if placements.is_empty() {
        Ok(())
    } else {
        Err(Refusal::BranchInUse(
            placements
                .iter()
                .map(|row| row.path.to_path_buf())
                .collect(),
        ))
    }
}

pub fn delete_branch(
    id: FolioId,
    name: &RefName,
    old: ObjectId,
    refs: &RefStore,
    registry: &Registry,
    lock: &mut HomeLock,
) -> Result<(), HistoryError> {
    judge_delete_branch(&registry.placements_of_head(id, name.as_str())?)?;
    refs.delete(lock, Kind::Branch, name, Expect::Is(old))?;
    refs.sync_refs()?;
    Ok(())
}

/// `None` makes a lightweight tag; an annotation must name this ref and target.
pub fn create_tag(
    name: &RefName,
    commit: ObjectId,
    annotation: Option<&Tag>,
    stores: &HistoryStores<'_>,
    lock: &mut HomeLock,
) -> Result<ObjectId, HistoryError> {
    let HistoryStores { objects, refs, .. } = *stores;
    if let Some(tag) = annotation {
        if tag.name != name.as_str() || tag.target != commit {
            return Err(Refusal::TagMismatch.into());
        }
        tag.encode_body()?;
    }
    if refs.read(Kind::Tag, name)?.is_some() {
        return Err(Refusal::TagExists.into());
    }
    let actual = objects.kind_of(commit)?;
    if actual != ObjectKind::Commit {
        return Err(RefError::WrongKind { id: commit, actual }.into());
    }
    let target = match annotation {
        Some(tag) => objects.put_tag(lock, tag)?,
        None => commit,
    };
    objects.sync_blobs()?;
    refs.update(lock, Kind::Tag, name, target, Expect::Absent)?;
    refs.sync_refs()?;
    Ok(target)
}

pub fn delete_tag(
    name: &RefName,
    old: ObjectId,
    refs: &RefStore,
    lock: &mut HomeLock,
) -> Result<(), HistoryError> {
    refs.delete(lock, Kind::Tag, name, Expect::Is(old))?;
    refs.sync_refs()?;
    Ok(())
}

fn gather_file(
    row: &Placement,
    fs: &impl PlacementOps,
    refs: &RefStore,
) -> Result<PlacementFacts, HistoryError> {
    let (_, placement) = row.to_placement();
    let observation = fs.read(row.id, &placement)?;
    let placement_hash = if matches!(observation, Observation::File { .. }) {
        Some(fs.hash_placement(&placement)?)
    } else {
        None
    };
    Ok(PlacementFacts {
        placement: row.clone(),
        observation,
        placement_hash,
        tip: refs.read(Kind::Branch, &RefName::parse(&row.head)?)?,
    })
}

fn require_blob(objects: &ObjectStore, id: ObjectId) -> Result<(), HistoryError> {
    let actual = objects.kind_of(id)?;
    if actual != ObjectKind::Blob {
        return Err(RefError::WrongKind { id, actual }.into());
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    NothingStaged,
    BranchMoved {
        tip: Option<ObjectId>,
        ahead: Vec<PathBuf>,
    },
    InternalEdited(PathBuf),
    MissingInternal,
    MissingBranch,
    Link(PathBuf),
    Unusable(PathBuf),
    RegistryChanged(PathBuf),
    PlacementChanged(PathBuf),
    PlacementsChanged,
    BranchInUse(Vec<PathBuf>),
    InvalidFacts,
    TagMismatch,
    TagExists,
    Move(placement_rules::Refusal),
}

impl From<placement_rules::Refusal> for Refusal {
    fn from(error: placement_rules::Refusal) -> Self {
        Self::Move(error)
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NothingStaged => write!(f, "nothing staged"),
            Self::BranchMoved { tip, ahead } => {
                write!(f, "branch moved to {tip:?}")?;
                for path in ahead {
                    write!(f, "; ahead at {}", path.display())?;
                }
                Ok(())
            }
            Self::InternalEdited(path) => write!(
                f,
                "folio.main has uncommitted link edits: {}",
                path.display()
            ),
            Self::MissingInternal => write!(f, "missing internal working-file row"),
            Self::MissingBranch => write!(f, "branch has no tip"),
            Self::Link(path) => write!(f, "cannot commit a link: {}", path.display()),
            Self::Unusable(path) => write!(f, "working file is unusable: {}", path.display()),
            Self::RegistryChanged(path) => write!(f, "registry row changed: {}", path.display()),
            Self::PlacementChanged(path) => write!(f, "working file changed: {}", path.display()),
            Self::PlacementsChanged => {
                write!(f, "branch placements changed since reset was judged")
            }
            Self::BranchInUse(paths) => write!(f, "branch is used by {} placement(s)", paths.len()),
            Self::InvalidFacts => write!(f, "inconsistent history facts"),
            Self::TagMismatch => write!(f, "annotation name or target differs from the tag ref"),
            Self::TagExists => write!(f, "tag already exists"),
            Self::Move(error) => error.fmt(f),
        }
    }
}
impl Error for Refusal {}

#[derive(Debug)]
pub enum HistoryError {
    Refused(Refusal),
    Object(ObjectError),
    Store(StoreError),
    Ref(RefError),
    Registry(RegistryError),
    Placement(PlacementError),
    Rules(RulesError),
}

impl From<Refusal> for HistoryError {
    fn from(error: Refusal) -> Self {
        Self::Refused(error)
    }
}
impl From<ObjectError> for HistoryError {
    fn from(error: ObjectError) -> Self {
        Self::Object(error)
    }
}
impl From<StoreError> for HistoryError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}
impl From<RefError> for HistoryError {
    fn from(error: RefError) -> Self {
        Self::Ref(error)
    }
}
impl From<RegistryError> for HistoryError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(error)
    }
}
impl From<PlacementError> for HistoryError {
    fn from(error: PlacementError) -> Self {
        Self::Placement(error)
    }
}
impl From<RulesError> for HistoryError {
    fn from(error: RulesError) -> Self {
        Self::Rules(error)
    }
}
impl From<placement_rules::Refusal> for HistoryError {
    fn from(error: placement_rules::Refusal) -> Self {
        Self::Refused(error.into())
    }
}

impl fmt::Display for HistoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(error) => error.fmt(f),
            Self::Object(error) => error.fmt(f),
            Self::Store(error) => error.fmt(f),
            Self::Ref(error) => error.fmt(f),
            Self::Registry(error) => error.fmt(f),
            Self::Placement(error) => error.fmt(f),
            Self::Rules(error) => error.fmt(f),
        }
    }
}
impl Error for HistoryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Refused(error) => Some(error),
            Self::Object(error) => Some(error),
            Self::Store(error) => Some(error),
            Self::Ref(error) => Some(error),
            Self::Registry(error) => Some(error),
            Self::Placement(error) => Some(error),
            Self::Rules(error) => Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::placement::StagedBlob;
    use crate::placement_rules::tests::{facts, target};

    fn commit_facts() -> CommitFacts {
        let mut working = facts();
        working.placement.staged = Some(StagedBlob {
            hash: hash_blob(b"staged"),
            size: 6,
        });
        CommitFacts {
            working,
            internal: None,
            placements: vec![],
            tip_commit: None,
        }
    }

    #[test]
    fn commit_refuses_links_missing_stage_and_unusable_files() {
        let mut facts = commit_facts();
        facts.working.placement.kind = PlacementKind::Link;
        assert!(matches!(judge_commit(&facts), Err(Refusal::Link(_))));
        facts.working.placement.kind = PlacementKind::Copy;
        facts.working.placement.staged = None;
        assert_eq!(judge_commit(&facts).unwrap_err(), Refusal::NothingStaged);
        let mut facts = commit_facts();
        facts.working.observation = Observation::Missing;
        assert!(matches!(judge_commit(&facts), Err(Refusal::Unusable(_))));
    }

    #[test]
    fn commit_refuses_moved_branch_and_invalid_internal_head() {
        let mut facts = commit_facts();
        facts.working.tip = Some(hash_blob(b"advanced"));
        assert!(matches!(
            judge_commit(&facts),
            Err(Refusal::BranchMoved { .. })
        ));
        facts.working.placement.kind = PlacementKind::Internal;
        assert_eq!(judge_commit(&facts).unwrap_err(), Refusal::InvalidFacts);
    }

    #[test]
    fn main_copy_requires_clean_internal_file() {
        let mut facts = commit_facts();
        facts.working.placement.head = "main".into();
        assert_eq!(judge_commit(&facts).unwrap_err(), Refusal::MissingInternal);
        let mut internal = facts.working.clone();
        internal.placement.kind = PlacementKind::Internal;
        internal.placement_hash = Some(hash_blob(b"edited"));
        facts.internal = Some(internal);
        assert!(matches!(
            judge_commit(&facts),
            Err(Refusal::InternalEdited(_))
        ));
        facts.internal.as_mut().unwrap().placement.id = FolioId::from_bytes([2; 16]);
        assert_eq!(judge_commit(&facts).unwrap_err(), Refusal::InvalidFacts);
    }

    #[test]
    fn reset_requires_discard_for_edits() {
        let mut placement = facts();
        placement.placement_hash = Some(hash_blob(b"edits"));
        let reset = ResetFacts {
            id: placement.placement.id,
            target: target(),
            old: placement.tip.unwrap(),
            placements: vec![placement],
        };
        assert!(matches!(judge_reset(&reset, false), Err(Refusal::Move(_))));
        assert!(judge_reset(&reset, true).is_ok());
    }

    #[test]
    fn branch_with_any_placement_cannot_be_deleted() {
        assert!(judge_delete_branch(&[]).is_ok());
        let mut row = facts().placement;
        assert_eq!(
            judge_delete_branch(&[row.clone()]).unwrap_err(),
            Refusal::BranchInUse(vec![row.path.to_path_buf()])
        );
        row.kind = PlacementKind::Internal;
        row.head = "main".into();
        assert!(matches!(
            judge_delete_branch(&[row]),
            Err(Refusal::BranchInUse(_))
        ));
    }
}
