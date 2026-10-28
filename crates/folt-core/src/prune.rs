//! Prunes absent external placement rows, never files or objects.
use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;

use crate::folio_dir::FolioId;
use crate::home::HomeLock;
use crate::object::ObjectId;
use crate::placement::{self, Drift, Observation, PlacementKind, PlacementOps};
use crate::registry::{Placement, PlacementPath, PlacementPrefix, Registry, RegistryError};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PruneOptions {
    pub apply: bool,
    pub confirm_mass_deletion: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PruneFacts {
    pub row: Placement,
    pub observation: Result<Observation, String>,
    pub parent_exists: Result<bool, String>,
    pub hash: Option<ObjectId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DroppedPlacement {
    pub path: PlacementPath,
    pub kind: PlacementKind,
    /// The staged object held by this copy, if any.
    pub stage: Option<ObjectId>,
    /// A copy's stage has no reference in the surviving row set.
    pub stage_unreachable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeptPlacement {
    pub path: PlacementPath,
    pub kind: PlacementKind,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PruneCounts {
    pub in_scope: usize,
    pub dropped: usize,
    pub kept: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PruneReport {
    /// In a dry run these are the rows that would be dropped.
    pub dropped: Vec<DroppedPlacement>,
    pub kept: Vec<KeptPlacement>,
    /// Distinct (Folio, blob) stages held only by dropped rows, including twins.
    pub unreachable_staged_blobs: Vec<(FolioId, ObjectId)>,
    pub warnings: Vec<String>,
    pub counts: PruneCounts,
    pub dry_run: bool,
}

#[derive(Clone, Debug)]
pub struct PrunePlan {
    facts: Vec<PruneFacts>,
    all_rows: Vec<Placement>,
    scope: Vec<PlacementPath>,
}

#[derive(Debug)]
pub enum PruneError {
    Registry(RegistryError),
    ConfirmationRequired { dropped: usize, in_scope: usize },
}

impl fmt::Display for PruneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Registry(error) => error.fmt(f),
            Self::ConfirmationRequired { dropped, in_scope } => write!(
                f,
                "prune would drop {dropped} of {in_scope} rows in scope; explicit mass-deletion confirmation is required"
            ),
        }
    }
}

impl Error for PruneError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Registry(error) => Some(error),
            Self::ConfirmationRequired { .. } => None,
        }
    }
}

impl From<RegistryError> for PruneError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(error)
    }
}

fn in_scope(row: &Placement, scope: &[PlacementPath]) -> bool {
    scope.is_empty()
        || scope
            .iter()
            .any(|path| row.path.to_path_buf().starts_with(path.to_path_buf()))
}

fn observe(row: &Placement, fs: &impl PlacementOps) -> PruneFacts {
    let (id, placement) = row.to_placement();
    let mut observation = fs.read(id, &placement).map_err(|error| error.to_string());
    let hash =
        if matches!(observation, Ok(Observation::File { .. })) && row.kind != PlacementKind::Link {
            match fs.hash_placement(&placement) {
                Ok(hash) => Some(hash),
                Err(error) => {
                    observation = Err(error.to_string());
                    None
                }
            }
        } else {
            None
        };
    let parent_exists =
        match fs::metadata(placement.path.parent().expect("absolute placement path")) {
            Ok(metadata) if metadata.is_dir() => Ok(true),
            Ok(_) => Err("parent is not a directory".into()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.to_string()),
        };
    PruneFacts {
        row: row.clone(),
        observation,
        parent_exists,
        hash,
    }
}

/// Reads the whole registry for stage reachability, observing only selected rows.
/// Scope keys name a path or directory itself (use resolve_directory for directories).
pub fn gather(
    registry: &Registry,
    fs: &impl PlacementOps,
    scope: &[PlacementPath],
) -> Result<PrunePlan, RegistryError> {
    let all_rows = registry.all_placements()?;
    let mut selected = HashSet::new();
    for path in scope {
        selected.insert(path.clone());
        selected.extend(
            registry
                .placements_under(&PlacementPrefix::from(path))?
                .into_iter()
                .map(|row| row.path),
        );
    }
    let facts = all_rows
        .iter()
        .filter(|row| {
            scope.is_empty()
                || selected.contains(&row.path)
                || (row.kind == PlacementKind::Internal && in_scope(row, scope))
        })
        .map(|row| observe(row, fs))
        .collect();
    Ok(PrunePlan {
        facts,
        all_rows,
        scope: scope.to_vec(),
    })
}

/// Returns None only for a missing external row with an existing parent.
/// All decisions depend solely on supplied facts.
pub fn judge(facts: &PruneFacts) -> Option<String> {
    if facts.row.kind == PlacementKind::Internal {
        return Some(match &facts.observation {
            Err(error) => {
                format!("internal placement; read error: {error}; rebuild belongs to #51")
            }
            Ok(Observation::Missing) => "internal placement missing; damaged home (#51)".into(),
            Ok(_) => "internal placement".into(),
        });
    }
    let observation = match &facts.observation {
        Ok(observation) => observation,
        Err(error) => return Some(format!("read error: {error}")),
    };
    match &facts.parent_exists {
        Ok(true) => {}
        Ok(false) => return Some("parent directory missing".into()),
        Err(error) => return Some(format!("parent read error: {error}")),
    }
    if observation == &Observation::Missing {
        return None;
    }
    let (_, placement) = facts.row.to_placement();
    if let (Observation::File { signature }, Some(stage)) = (observation, facts.row.staged)
        && facts.hash == Some(stage.hash)
        && signature.size == stage.size
    {
        return Some("matches stage".into());
    }
    Some(
        match placement::classify(&placement, observation, facts.hash) {
            Drift::InSync => "in sync",
            Drift::Detached => "a different file than expected",
            Drift::PlacementEdited => "edited or different",
            Drift::Unusable => "unusable entry",
            Drift::Gone => unreachable!("missing handled above"),
        }
        .into(),
    )
}

/// Requires at least five drops AND strictly more than half the scoped rows.
/// Division avoids overflow when comparing counts.
pub fn needs_confirmation(dropped: usize, in_scope: usize) -> bool {
    dropped >= 5 && dropped > in_scope / 2
}

impl PrunePlan {
    pub fn report(&self) -> PruneReport {
        report(&self.facts, &self.all_rows, true)
    }
}

fn report(facts: &[PruneFacts], all_rows: &[Placement], dry_run: bool) -> PruneReport {
    let mut kept = Vec::new();
    let mut dropped_rows = Vec::new();
    let mut warnings = Vec::new();
    for facts in facts {
        if let Some(reason) = judge(facts) {
            if reason != "in sync"
                && reason != "matches stage"
                && facts.row.kind != PlacementKind::Internal
            {
                warnings.push(format!(
                    "{}: {reason}",
                    facts.row.path.to_path_buf().display()
                ));
            }
            kept.push(KeptPlacement {
                path: facts.row.path.clone(),
                kind: facts.row.kind,
                reason,
            });
        } else {
            dropped_rows.push(&facts.row);
        }
    }
    let paths: HashSet<_> = dropped_rows.iter().map(|row| &row.path).collect();
    let retained: HashSet<_> = all_rows
        .iter()
        .filter(|row| !paths.contains(&row.path))
        .filter_map(|row| row.staged.map(|stage| (row.id, stage.hash)))
        .collect();
    let mut seen = HashSet::new();
    let unreachable: Vec<_> = dropped_rows
        .iter()
        .filter_map(|row| row.staged.map(|stage| (row.id, stage.hash)))
        .filter(|stage| !retained.contains(stage))
        .filter(|stage| seen.insert(*stage))
        .collect();
    let dropped: Vec<_> = dropped_rows
        .iter()
        .map(|row| DroppedPlacement {
            path: row.path.clone(),
            kind: row.kind,
            stage: row.staged.map(|stage| stage.hash),
            stage_unreachable: row.kind == PlacementKind::Copy
                && row
                    .staged
                    .is_some_and(|stage| seen.contains(&(row.id, stage.hash))),
        })
        .collect();
    if !dropped.is_empty() {
        warnings
            .push("a dropped row's file may still exist elsewhere; rebind is out of scope".into());
    }
    PruneReport {
        counts: PruneCounts {
            in_scope: facts.len(),
            dropped: dropped.len(),
            kept: kept.len(),
        },
        dropped,
        kept,
        unreachable_staged_blobs: unreachable,
        warnings,
        dry_run,
    }
}

/// Rechecks selected rows and observations inside one transaction under HomeLock.
/// Changed rows are reported and kept. A registry failure rolls back every delete.
pub fn apply(
    plan: &PrunePlan,
    fs: &impl PlacementOps,
    registry: &mut Registry,
    lock: &HomeLock,
    confirm_mass_deletion: bool,
) -> Result<PruneReport, PruneError> {
    registry.write(lock, |tx| {
        let all_rows = tx.all_placements()?;
        let in_scope = all_rows
            .iter()
            .filter(|row| in_scope(row, &plan.scope))
            .count();
        let mut checked = Vec::new();
        for original in &plan.facts {
            let mut facts = observe(&original.row, fs);
            if tx.placement_at(&original.row.path)?.as_ref() != Some(&original.row) {
                facts.observation = Err("registry changed before prune".into());
            } else if judge(original).is_some() && judge(&facts).is_none() {
                facts.observation = Err("observation changed before prune; run prune again".into());
            }
            checked.push(facts);
        }
        let candidates = checked
            .iter()
            .filter(|facts| judge(facts).is_none())
            .count();
        if needs_confirmation(candidates, in_scope) && !confirm_mass_deletion {
            return Ok(Err(PruneError::ConfirmationRequired {
                dropped: candidates,
                in_scope,
            }));
        }
        for facts in &mut checked {
            if judge(facts).is_none() {
                // External editors do not take HomeLock. Recheck immediately
                // before each delete, even after the batch confirmation check.
                let row = &facts.row;
                let mut current = observe(row, fs);
                if tx.placement_at(&row.path)?.as_ref() != Some(row) {
                    current.observation = Err("registry changed before prune".into());
                }
                if judge(&current).is_none() {
                    tx.remove_placement(&row.path)?;
                }
                *facts = current;
            }
        }
        Ok(Ok(report(&checked, &all_rows, false)))
    })?
}

/// Dry run by default; applying requires options.apply and the caller's home lock.
pub fn prune_placements(
    registry: &mut Registry,
    fs: &impl PlacementOps,
    lock: &HomeLock,
    scope: &[PlacementPath],
    options: PruneOptions,
) -> Result<PruneReport, PruneError> {
    let plan = gather(registry, fs, scope)?;
    if options.apply {
        apply(&plan, fs, registry, lock, options.confirm_mass_deletion)
    } else {
        Ok(plan.report())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::hash_blob;
    use crate::placement::StagedBlob;
    use crate::placement_rules::tests::facts;

    fn missing() -> PruneFacts {
        PruneFacts {
            row: facts().placement,
            observation: Ok(Observation::Missing),
            parent_exists: Ok(true),
            hash: None,
        }
    }

    #[test]
    fn every_observation_by_kind() {
        for kind in [
            PlacementKind::Copy,
            PlacementKind::Link,
            PlacementKind::Internal,
        ] {
            for observation in [
                Observation::Missing,
                Observation::IntactLink,
                Observation::OtherLink {
                    target: "other".into(),
                },
                facts().observation,
                Observation::Directory,
                Observation::NotAFile,
            ] {
                let mut input = missing();
                input.row.kind = kind;
                input.hash = input.row.base.as_ref().map(|base| base.hash);
                input.observation = Ok(observation.clone());
                let expected = match (kind, &observation) {
                    (PlacementKind::Internal, Observation::Missing) => {
                        Some("internal placement missing; damaged home (#51)")
                    }
                    (PlacementKind::Internal, _) => Some("internal placement"),
                    (_, Observation::Missing) => None,
                    (PlacementKind::Link, Observation::IntactLink) => Some("in sync"),
                    (
                        PlacementKind::Link,
                        Observation::File { .. } | Observation::OtherLink { .. },
                    ) => Some("a different file than expected"),
                    (PlacementKind::Copy, Observation::File { .. }) => Some("in sync"),
                    _ => Some("unusable entry"),
                };
                assert_eq!(
                    judge(&input).as_deref(),
                    expected,
                    "{kind:?} {observation:?}"
                );
            }
        }
    }

    #[test]
    fn parent_and_read_errors_keep_rows_and_copy_edits_are_notes() {
        let mut input = missing();
        input.parent_exists = Ok(false);
        assert_eq!(judge(&input).as_deref(), Some("parent directory missing"));
        input.observation = Err("permission denied".into());
        assert_eq!(
            judge(&input).as_deref(),
            Some("read error: permission denied")
        );
        input.parent_exists = Err("unreachable share".into());
        input.observation = Ok(Observation::Missing);
        assert!(judge(&input).unwrap().contains("unreachable share"));
        input.parent_exists = Ok(true);
        input.observation = Ok(facts().observation);
        input.hash = Some(hash_blob(b"other"));
        assert_eq!(judge(&input).as_deref(), Some("edited or different"));
        input.row.staged = Some(StagedBlob {
            hash: input.hash.unwrap(),
            size: 4,
        });
        assert_eq!(judge(&input).as_deref(), Some("matches stage"));
    }

    #[test]
    fn stage_reachability_accounts_for_twins_and_other_folios() {
        let mut input = missing();
        input.row.staged = Some(StagedBlob {
            hash: hash_blob(b"stage"),
            size: 5,
        });
        let mut twin = input.row.clone();
        // Distinct path keys can be resolved without touching the filesystem.
        twin.path = PlacementPath::resolve(std::env::temp_dir().join("folt-prune-twin")).unwrap();
        for (keep_twin, other_folio, expected) in
            [(false, false, 1), (true, false, 0), (true, true, 1)]
        {
            if other_folio {
                twin.id = crate::folio_dir::FolioId::from_bytes([9; 16]);
            }
            let mut all = vec![input.row.clone()];
            if keep_twin {
                all.push(twin.clone());
            }
            let result = report(&[input.clone()], &all, true);
            let expected_blobs = if expected == 1 {
                vec![(input.row.id, input.row.staged.unwrap().hash)]
            } else {
                Vec::new()
            };
            assert_eq!(result.unreachable_staged_blobs, expected_blobs);
            assert_eq!(result.dropped[0].stage, Some(hash_blob(b"stage")));
            assert_eq!(result.dropped[0].stage_unreachable, expected == 1);
        }
        let mut gone_twin = input.clone();
        gone_twin.row.path = twin.path;
        let stage = (input.row.id, input.row.staged.unwrap().hash);
        let both = [input, gone_twin];
        let all: Vec<_> = both.iter().map(|facts| facts.row.clone()).collect();
        let result = report(&both, &all, true);
        assert_eq!(result.unreachable_staged_blobs, [stage]);
        assert_eq!(result.dropped.len(), 2);
        for dropped in result.dropped {
            assert_eq!(dropped.stage, Some(stage.1));
            assert!(dropped.stage_unreachable);
        }
    }

    #[test]
    fn dropped_copy_reports_reachable_stage_distinct_from_unstaged_copy() {
        let mut staged = missing();
        let stage = hash_blob(b"stage");
        staged.row.staged = Some(StagedBlob {
            hash: stage,
            size: 5,
        });
        let mut live_twin = staged.clone();
        live_twin.row.path =
            PlacementPath::resolve(std::env::temp_dir().join("folt-prune-live-twin")).unwrap();
        live_twin.observation = Ok(facts().observation);
        live_twin.hash = live_twin.row.base.as_ref().map(|base| base.hash);
        let mut unstaged = missing();
        unstaged.row.staged = None;
        unstaged.row.path =
            PlacementPath::resolve(std::env::temp_dir().join("folt-prune-unstaged")).unwrap();
        let inputs = [staged.clone(), live_twin.clone(), unstaged.clone()];
        let all: Vec<_> = inputs.iter().map(|input| input.row.clone()).collect();

        let result = report(&inputs, &all, true);

        assert_eq!(
            result.dropped,
            [
                DroppedPlacement {
                    path: staged.row.path,
                    kind: PlacementKind::Copy,
                    stage: Some(stage),
                    stage_unreachable: false,
                },
                DroppedPlacement {
                    path: unstaged.row.path,
                    kind: PlacementKind::Copy,
                    stage: None,
                    stage_unreachable: false,
                },
            ]
        );
        assert_eq!(result.kept.len(), 1);
        assert_eq!(result.kept[0].path, live_twin.row.path);
        assert!(result.unreachable_staged_blobs.is_empty());
    }

    #[test]
    fn dropped_staged_copy_without_twin_reports_unreachable_folio_blob_pair() {
        let mut input = missing();
        let stage = hash_blob(b"stage");
        input.row.staged = Some(StagedBlob {
            hash: stage,
            size: 5,
        });

        let result = report(
            std::slice::from_ref(&input),
            std::slice::from_ref(&input.row),
            true,
        );

        assert_eq!(
            result.dropped,
            [DroppedPlacement {
                path: input.row.path,
                kind: PlacementKind::Copy,
                stage: Some(stage),
                stage_unreachable: true,
            }]
        );
        assert_eq!(result.unreachable_staged_blobs, [(input.row.id, stage)]);
    }

    #[test]
    fn two_dropped_copies_report_shared_unreachable_stage_pair_once() {
        let mut first = missing();
        let stage = hash_blob(b"stage");
        first.row.staged = Some(StagedBlob {
            hash: stage,
            size: 5,
        });
        let mut second = first.clone();
        second.row.path =
            PlacementPath::resolve(std::env::temp_dir().join("folt-prune-dropped-twin")).unwrap();
        let inputs = [first, second];
        let all: Vec<_> = inputs.iter().map(|input| input.row.clone()).collect();

        let result = report(&inputs, &all, true);

        assert_eq!(result.unreachable_staged_blobs, [(all[0].id, stage)]);
        assert_eq!(result.dropped.len(), 2);
        for (dropped, row) in result.dropped.iter().zip(&all) {
            assert_eq!(dropped.path, row.path);
            assert_eq!(dropped.kind, PlacementKind::Copy);
            assert_eq!(dropped.stage, Some(stage));
            assert!(dropped.stage_unreachable);
        }
    }

    #[test]
    fn mass_deletion_boundaries() {
        for (dropped, total, expected) in [
            (0, 0, false),
            (4, 4, false),
            (5, 10, false),
            (5, 9, true),
            (6, 11, true),
            (6, 12, false),
            (usize::MAX, usize::MAX, true),
        ] {
            assert_eq!(needs_confirmation(dropped, total), expected);
        }
    }

    #[test]
    fn scope_matches_whole_path_segments() {
        let mut row = missing().row;
        let root = std::env::temp_dir();
        let scope = [PlacementPath::resolve_directory(root.join("old")).unwrap()];
        for (path, expected) in [("old", true), ("old-notes", false)] {
            row.path = PlacementPath::resolve_directory(root.join(path)).unwrap();
            assert_eq!(in_scope(&row, &scope), expected);
        }
    }
}
