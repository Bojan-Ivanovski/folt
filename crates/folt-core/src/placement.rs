use crate::object::ObjectId;
use crate::store::{ObjectStore, StoreError};
use std::error::Error;
use std::fmt;
use std::path::PathBuf;

/// The way a Folio is presented at a placement path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlacementKind {
    Link,
    Copy,
    Internal,
}

impl PlacementKind {
    /// The default for a user placement is a copy; Internal is never chosen by a user.
    pub const DEFAULT: Self = Self::Copy;

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Link => "link",
            Self::Copy => "copy",
            Self::Internal => "internal",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "link" => Some(Self::Link),
            "copy" => Some(Self::Copy),
            "internal" => Some(Self::Internal),
            _ => None,
        }
    }

    /// Parses a user placement kind from an option or setting value.
    pub fn parse_user(value: &str) -> Option<Self> {
        match value {
            "link" => Some(Self::Link),
            "copy" => Some(Self::Copy),
            _ => None,
        }
    }
}

/// File metadata captured with a copy's base and checked before applying a move.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Signature {
    pub size: u64,
    pub mtime: Option<i128>,
    pub ctime: Option<i128>,
    pub inode: Option<u64>,
}

/// The in-memory placement record. Persistence is handled by the caller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Placement {
    pub path: PathBuf,
    pub kind: PlacementKind,
    pub head: String,
    pub commit_id: Option<ObjectId>,
    pub base: Option<PlacementBase>,
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

/// The outcome of comparing a copy with its base or observing a link.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Drift {
    InSync,
    PlacementEdited,
    Gone,
    Detached,
    Unusable,
}

/// Base facts captured while creating or refreshing a placement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Written {
    pub base: Option<PlacementBase>,
}

/// Content and file metadata captured for a copy placement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlacementBase {
    pub hash: ObjectId,
    pub signature: Signature,
}

/// A blob captured from a working file for the next commit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StagedBlob {
    pub hash: ObjectId,
    pub size: u64,
}

/// Whether the caller must hash content before classifying drift.
pub fn needs_hash(placement: &Placement, observation: &Observation) -> bool {
    placement.kind != PlacementKind::Link
        && matches!(observation, Observation::File { signature }
            if placement.base.as_ref().is_none_or(|base| base.signature.size == signature.size))
}

/// Classifies a placement using facts and hashes supplied by the caller.
pub fn classify(
    placement: &Placement,
    observation: &Observation,
    placement_hash: Option<ObjectId>,
) -> Drift {
    match observation {
        Observation::Missing => Drift::Gone,
        Observation::NotAFile => Drift::Unusable,
        Observation::IntactLink if placement.kind == PlacementKind::Link => Drift::InSync,
        Observation::OtherLink { .. } | Observation::File { .. }
            if placement.kind == PlacementKind::Link =>
        {
            Drift::Detached
        }
        Observation::File { signature } => {
            let Some(base) = &placement.base else {
                return if placement_hash.is_some() {
                    Drift::PlacementEdited
                } else {
                    Drift::Unusable
                };
            };
            if signature.size != base.signature.size {
                return Drift::PlacementEdited;
            }
            match placement_hash {
                Some(hash) if hash == base.hash => Drift::InSync,
                Some(_) => Drift::PlacementEdited,
                None => Drift::Unusable,
            }
        }
        Observation::IntactLink | Observation::OtherLink { .. } => Drift::Unusable,
    }
}

/// Kind-specific placement operations. Implementations perform filesystem work.
pub trait PlacementOps {
    /// Creates a placement; copies read the supplied commit content blob.
    fn create(
        &self,
        id: crate::folio_dir::FolioId,
        placement: &Placement,
        objects: &ObjectStore,
        content: ObjectId,
    ) -> Result<Written, PlacementError>;

    /// Reads facts about a placement path.
    fn read(
        &self,
        id: crate::folio_dir::FolioId,
        placement: &Placement,
    ) -> Result<Observation, PlacementError>;

    /// Refreshes a placement; copies read the supplied commit content blob.
    /// When supplied, copies recheck the expected observation before replacement.
    fn refresh(
        &self,
        id: crate::folio_dir::FolioId,
        placement: &Placement,
        objects: &ObjectStore,
        content: ObjectId,
        expected: Option<&Observation>,
    ) -> Result<Written, PlacementError>;

    /// Hashes a placement file without retaining its contents.
    fn hash_placement(&self, placement: &Placement) -> Result<ObjectId, PlacementError>;

    /// Uses supplied facts to detect drift, requesting hashes when needed.
    fn detect_drift(
        &self,
        placement: &Placement,
        observation: &Observation,
        placement_hash: Option<ObjectId>,
    ) -> Result<Drift, PlacementError> {
        if needs_hash(placement, observation) && placement_hash.is_none() {
            return Err(PlacementError::HashRequired);
        }
        Ok(classify(placement, observation, placement_hash))
    }
}

/// Errors returned by placement operations.
#[derive(Debug)]
pub enum PlacementError {
    HashRequired,
    PlacementChanged,
    Store(StoreError),
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
            Self::Store(error) => error.fmt(formatter),
            Self::PlacementChanged => write!(formatter, "placement changed before refresh"),
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
            Self::HashRequired | Self::PlacementChanged => None,
            Self::Store(error) => Some(error),
            Self::OperationFailed { source, .. } => Some(source),
            Self::LinkUnavailable { source, .. } => Some(source),
        }
    }
}
