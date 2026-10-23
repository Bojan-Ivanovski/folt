use crate::object::ObjectId;
use std::error::Error;
use std::fmt;
use std::path::PathBuf;

/// The way a Folio is presented at a placement path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlacementKind {
    Link,
    Copy,
}

/// File metadata used to avoid hashing an unchanged placement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Signature {
    pub size: u64,
    pub mtime: Option<i128>,
    pub ctime: Option<i128>,
    pub inode: Option<u64>,
}

impl Signature {
    /// Returns true only when every signature field is available and matches.
    pub fn matches(&self, other: &Self) -> bool {
        self.size == other.size
            && self.mtime.is_some()
            && self.mtime == other.mtime
            && self.ctime.is_some()
            && self.ctime == other.ctime
            && self.inode.is_some()
            && self.inode == other.inode
    }
}

/// The in-memory placement record. Persistence is handled by the caller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Placement {
    pub path: PathBuf,
    pub kind: PlacementKind,
    pub base: ObjectId,
    pub signature: Signature,
}

/// Caller supplied facts about the placement path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Observation {
    Missing,
    IntactLink,
    OtherLink { target: PathBuf },
    File { signature: Signature },
    NotAFile,
}

/// The outcome of comparing a placement with its base and working file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Drift {
    InSync,
    PlacementStale,
    PlacementEdited,
    Converged,
    Conflict,
    Gone,
    Detached,
    Unusable,
}

/// Base facts captured while creating or refreshing a placement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Written {
    pub hash: ObjectId,
    pub signature: Signature,
}

/// Whether the caller must hash content before classifying drift.
pub fn needs_hash(placement: &Placement, observation: &Observation) -> bool {
    match observation {
        Observation::IntactLink => false,
        Observation::File { signature } => !placement.signature.matches(signature),
        Observation::Missing | Observation::OtherLink { .. } | Observation::NotAFile => true,
    }
}

/// Classifies a placement using facts and hashes supplied by the caller.
pub fn classify(
    placement: &Placement,
    observation: &Observation,
    working_hash: ObjectId,
    placement_hash: Option<ObjectId>,
) -> Drift {
    match observation {
        Observation::Missing => Drift::Gone,
        Observation::NotAFile => Drift::Unusable,
        Observation::OtherLink { .. } if placement.kind == PlacementKind::Link => Drift::Detached,
        Observation::IntactLink if placement.kind == PlacementKind::Link => {
            compare_hashes(placement.base, placement.base, working_hash)
        }
        Observation::OtherLink { .. } | Observation::File { .. } => {
            let Some(placement_hash) = placement_hash else {
                return Drift::Unusable;
            };
            compare_hashes(placement.base, placement_hash, working_hash)
        }
        Observation::IntactLink => Drift::Unusable,
    }
}

fn compare_hashes(base: ObjectId, placement: ObjectId, working: ObjectId) -> Drift {
    match (placement == base, working == base, placement == working) {
        (true, true, _) => Drift::InSync,
        (true, false, _) => Drift::PlacementStale,
        (false, true, _) => Drift::PlacementEdited,
        (false, false, true) => Drift::Converged,
        (false, false, false) => Drift::Conflict,
    }
}

/// Kind-specific placement operations. Implementations perform filesystem work.
pub trait PlacementOps {
    /// Creates a placement from the supplied record and content hash.
    fn create(
        &self,
        id: crate::folio_dir::FolioId,
        placement: &Placement,
    ) -> Result<Written, PlacementError>;

    /// Reads facts about a placement path.
    fn read(
        &self,
        id: crate::folio_dir::FolioId,
        placement: &Placement,
    ) -> Result<Observation, PlacementError>;

    /// Refreshes a placement to the supplied content and updates its record.
    fn refresh(
        &self,
        id: crate::folio_dir::FolioId,
        placement: &Placement,
    ) -> Result<Written, PlacementError>;

    /// Hashes a placement file without retaining its contents.
    fn hash_placement(&self, placement: &Placement) -> Result<ObjectId, PlacementError>;

    /// Uses supplied facts to detect drift, requesting hashes when needed.
    fn detect_drift(
        &self,
        placement: &Placement,
        observation: &Observation,
        working_hash: ObjectId,
        placement_hash: Option<ObjectId>,
    ) -> Result<Drift, PlacementError> {
        if needs_hash(placement, observation)
            && matches!(
                observation,
                Observation::File { .. } | Observation::OtherLink { .. }
            )
            && placement_hash.is_none()
        {
            return Err(PlacementError::HashRequired);
        }
        Ok(classify(
            placement,
            observation,
            working_hash,
            placement_hash,
        ))
    }
}

/// Errors returned by placement operations.
#[derive(Debug)]
pub enum PlacementError {
    HashRequired,
    OperationFailed {
        path: PathBuf,
        source: std::io::Error,
    },
    LinkUnavailable {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for PlacementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HashRequired => write!(formatter, "placement content hash is required"),
            Self::OperationFailed { path, source } => {
                write!(
                    formatter,
                    "placement operation failed for {}: {source}",
                    path.display()
                )
            }
            Self::LinkUnavailable { path, source } => write!(
                formatter,
                "cannot create a symbolic link at {}: {source}. On Windows, enable Developer Mode or grant the process the Create symbolic links right; otherwise use a filesystem that supports symbolic links or choose a copy placement",
                path.display()
            ),
        }
    }
}

impl Error for PlacementError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::HashRequired => None,
            Self::OperationFailed { source, .. } => Some(source),
            Self::LinkUnavailable { source, .. } => Some(source),
        }
    }
}
