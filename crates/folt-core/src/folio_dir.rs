use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::layout::{
    CONTENT_FILE, DEFAULT_BRANCH, FOLIO_LAYOUT_VERSION, LayoutSupport, LayoutVersion,
    OBJECT_ALGORITHM,
};
use crate::registry::{FolioFormat, FolioRow};

/// A canonical, randomly generated Folio identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FolioId(Uuid);

impl FolioId {
    /// Generates a version 4 identifier.
    pub fn generate() -> Self {
        Self(Uuid::new_v4())
    }

    /// Parses only the lowercase hyphenated representation.
    pub fn parse(value: &str) -> Result<Self, FolioDirError> {
        let id = Uuid::try_parse(value).map_err(|_| FolioDirError::InvalidId(value.into()))?;
        if id.hyphenated().to_string() != value {
            return Err(FolioDirError::InvalidId(value.into()));
        }
        Ok(Self(id))
    }

    /// Returns the 16 bytes used by the registry.
    pub fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }

    /// Constructs an identifier from registry bytes.
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(Uuid::from_bytes(bytes))
    }
}

impl fmt::Display for FolioId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.hyphenated().fmt(formatter)
    }
}

/// Content used to create a Folio directory.
pub struct FolioSpec {
    /// Content bytes; an empty vector represents an empty body.
    pub content: Vec<u8>,
}

/// Requested durability for a newly created Folio.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Durability {
    /// Rely on normal filesystem write behavior.
    None,
    /// Sync the content and containing directories before returning.
    DataAndDirectories,
}

/// A validated Folio directory.
#[derive(Clone, Debug)]
pub struct FolioDir {
    id: FolioId,
    path: PathBuf,
}

impl FolioDir {
    pub fn id(&self) -> FolioId {
        self.id
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// A repair that can safely recreate a missing empty directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Fix {
    CreateBlobs,
    CreateRefs,
}

/// An entry that prevents validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Problem {
    pub entry: OsString,
    pub message: String,
}

/// Classified result of inspecting an entry in `folios/`.
#[derive(Clone, Debug)]
pub enum Inspection {
    Valid(FolioDir),
    Repairable(FolioDir, Vec<Fix>),
    Newer {
        found: LayoutVersion,
        supported: LayoutVersion,
    },
    Unregistered(FolioDir, Vec<Fix>),
    Damaged(Vec<Problem>),
    NotAFolio,
}

/// Creates a Folio by staging it under `folios/.new/` and renaming it into place.
pub fn create_folio(
    home: &Path,
    lock: &super::home::HomeLock,
    spec: FolioSpec,
    durability: Durability,
) -> Result<FolioDir, FolioDirError> {
    let folios = home.join("folios");
    ensure_directory(&folios)?;
    let staging = folios.join(".new");
    ensure_directory(&staging)?;
    let id = FolioId::generate();
    let temporary = staging.join(id.to_string());
    fs::create_dir(&temporary).map_err(|source| io_error(&temporary, source))?;
    let result = build_and_publish(home, lock, &folios, &temporary, id, spec, durability);
    if result.is_err() {
        let _ = fs::remove_dir_all(&temporary);
    }
    result
}

fn build_and_publish(
    home: &Path,
    _lock: &super::home::HomeLock,
    folios: &Path,
    temporary: &Path,
    id: FolioId,
    spec: FolioSpec,
    durability: Durability,
) -> Result<FolioDir, FolioDirError> {
    let content_path = temporary.join(CONTENT_FILE);
    let mut file = create_file(&content_path)?;
    file.write_all(&spec.content)
        .map_err(|source| io_error(&content_path, source))?;
    if durability == Durability::DataAndDirectories {
        file.sync_all()
            .map_err(|source| io_error(&content_path, source))?;
    }
    ensure_directory(&temporary.join("blobs"))?;
    ensure_directory(&temporary.join("refs"))?;
    let head_path = temporary.join("HEAD");
    let mut head = create_file(&head_path)?;
    writeln!(head, "ref: refs/heads/{DEFAULT_BRANCH}")
        .map_err(|source| io_error(&head_path, source))?;
    if durability == Durability::DataAndDirectories {
        head.sync_all()
            .map_err(|source| io_error(&head_path, source))?;
    }
    drop(head);
    if durability == Durability::DataAndDirectories {
        File::open(temporary)
            .and_then(|file| file.sync_all())
            .map_err(|source| io_error(temporary, source))?;
    }
    let destination = folios.join(id.to_string());
    if fs::symlink_metadata(&destination).is_ok() {
        return Err(FolioDirError::AlreadyExists(destination));
    }
    fs::rename(temporary, &destination).map_err(|source| io_error(&destination, source))?;
    if durability == Durability::DataAndDirectories {
        File::open(folios)
            .and_then(|file| file.sync_all())
            .map_err(|source| io_error(folios, source))?;
    }
    let _ = home;
    Ok(FolioDir {
        id,
        path: destination,
    })
}

/// Removes all crash leftovers beneath the staging directory while holding the home lock.
pub fn sweep_new(home: &Path, _lock: &super::home::HomeLock) -> Result<(), FolioDirError> {
    let staging = home.join("folios").join(".new");
    match fs::read_dir(&staging) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|source| io_error(&staging, source))?;
                let path = entry.path();
                let ty = entry
                    .file_type()
                    .map_err(|source| io_error(&path, source))?;
                if ty.is_dir() {
                    fs::remove_dir_all(&path)
                } else {
                    fs::remove_file(&path)
                }
                .map_err(|source| io_error(&path, source))?;
            }
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io_error(&staging, source)),
    }
    Ok(())
}

/// Inspects a directory entry named by `name` under `folios/`.
pub fn inspect(home: &Path, name: &OsStr, row: Option<&FolioRow>) -> Inspection {
    let Some(text) = name.to_str() else {
        return Inspection::NotAFolio;
    };
    let Ok(id) = FolioId::parse(text) else {
        return Inspection::NotAFolio;
    };
    if let Some(row) = row
        && row.id != id
    {
        return Inspection::Damaged(vec![Problem {
            entry: name.to_os_string(),
            message: "registry row id does not match Folio directory name".into(),
        }]);
    }
    let path = home.join("folios").join(name);
    let mut problems = Vec::new();
    let mut fixes = Vec::new();
    let metadata = match fs::symlink_metadata(&path) {
        Ok(value) => value,
        Err(e) => return Inspection::Damaged(vec![problem(name, e)]),
    };
    if !metadata.file_type().is_dir() {
        return Inspection::NotAFolio;
    }
    let required = [
        (CONTENT_FILE, false),
        ("blobs", true),
        ("refs", true),
        ("HEAD", false),
    ];
    for (entry, directory) in required {
        let child = path.join(entry);
        match fs::symlink_metadata(&child) {
            Ok(meta)
                if meta.file_type().is_symlink()
                    || (directory && !meta.is_dir())
                    || (!directory && !meta.is_file()) =>
            {
                problems.push(Problem {
                    entry: entry.into(),
                    message: "wrong entry type".into(),
                })
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound && entry == "blobs" => {
                fixes.push(Fix::CreateBlobs)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound && entry == "refs" => {
                fixes.push(Fix::CreateRefs)
            }
            Err(e) => problems.push(problem(OsStr::new(entry), e)),
        }
    }
    if let Err(error) = crate::refs::read_head(&path) {
        problems.push(Problem {
            entry: "HEAD".into(),
            message: error.to_string(),
        });
    }
    if let Err(error) = crate::refs::inspect_refs(&path) {
        problems.push(Problem {
            entry: "refs".into(),
            message: error.to_string(),
        });
    }
    match fs::read_dir(&path) {
        Ok(entries) => {
            for entry in entries {
                match entry {
                    Ok(entry)
                        if !required.iter().any(|(name, _)| entry.file_name() == *name)
                            && !entry
                                .file_name()
                                .to_str()
                                .is_some_and(crate::refs::temporary_name) =>
                    {
                        problems.push(Problem {
                            entry: entry.file_name(),
                            message: "unexpected entry".into(),
                        });
                    }
                    Ok(_) => {}
                    Err(error) => problems.push(problem(name, error)),
                }
            }
        }
        Err(error) => problems.push(problem(name, error)),
    }
    if let Some(row) = row {
        match check_format(&row.format) {
            Ok(()) => {}
            Err(FormatError::Newer { found, supported }) => {
                return Inspection::Newer { found, supported };
            }
            Err(error) => {
                return Inspection::Damaged(vec![Problem {
                    entry: "format".into(),
                    message: error.to_string(),
                }]);
            }
        }
    }
    if !problems.is_empty() {
        Inspection::Damaged(problems)
    } else if row.is_some() {
        if !fixes.is_empty() {
            Inspection::Repairable(FolioDir { id, path }, fixes)
        } else {
            Inspection::Valid(FolioDir { id, path })
        }
    } else {
        Inspection::Unregistered(FolioDir { id, path }, fixes)
    }
}

/// Validates a Folio, accepting only a complete directory.
pub fn validate(home: &Path, row: &FolioRow) -> Result<FolioDir, FolioDirError> {
    match inspect(home, OsStr::new(&row.id.to_string()), Some(row)) {
        Inspection::Valid(directory) => Ok(directory),
        Inspection::Newer { found, supported } => Err(FolioDirError::Newer { found, supported }),
        _ => Err(FolioDirError::InvalidDirectory(
            home.join("folios").join(row.id.to_string()),
        )),
    }
}

fn ensure_directory(path: &Path) -> Result<(), FolioDirError> {
    fs::create_dir_all(path).map_err(|source| io_error(path, source))
}
fn create_file(path: &Path) -> Result<File, FolioDirError> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| io_error(path, source))
}
fn io_error(path: &Path, source: io::Error) -> FolioDirError {
    FolioDirError::Io {
        path: path.to_path_buf(),
        source,
    }
}
fn problem(entry: &OsStr, error: io::Error) -> Problem {
    Problem {
        entry: entry.to_os_string(),
        message: error.to_string(),
    }
}
/// Checks whether a registry format can be read by this build.
pub fn check_format(format: &FolioFormat) -> Result<(), FormatError> {
    match format.layout.support_as_folio_layout() {
        LayoutSupport::Supported => {}
        LayoutSupport::Newer => {
            return Err(FormatError::Newer {
                found: format.layout,
                supported: FOLIO_LAYOUT_VERSION,
            });
        }
        LayoutSupport::Older => {
            return Err(FormatError::Older {
                found: format.layout,
                supported: FOLIO_LAYOUT_VERSION,
            });
        }
    }
    if format.algorithm != OBJECT_ALGORITHM {
        return Err(FormatError::UnsupportedAlgorithm(format.algorithm.clone()));
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FormatError {
    Newer {
        found: LayoutVersion,
        supported: LayoutVersion,
    },
    Older {
        found: LayoutVersion,
        supported: LayoutVersion,
    },
    UnsupportedAlgorithm(String),
}
impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Newer { found, supported } => write!(
                f,
                "Folio layout {found} is newer than supported layout {supported}"
            ),
            Self::Older { found, supported } => write!(
                f,
                "Folio layout {found} is older than supported layout {supported}"
            ),
            Self::UnsupportedAlgorithm(value) => write!(f, "unsupported object algorithm: {value}"),
        }
    }
}
impl Error for FormatError {}

/// Errors returned by Folio directory operations.
#[derive(Debug)]
pub enum FolioDirError {
    InvalidId(String),
    AlreadyExists(PathBuf),
    InvalidDirectory(PathBuf),
    Newer {
        found: LayoutVersion,
        supported: LayoutVersion,
    },
    Io {
        path: PathBuf,
        source: io::Error,
    },
}
impl fmt::Display for FolioDirError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId(value) => write!(f, "invalid Folio id: {value}"),
            Self::AlreadyExists(path) => write!(f, "Folio already exists: {}", path.display()),
            Self::InvalidDirectory(path) => {
                write!(f, "invalid Folio directory: {}", path.display())
            }
            Self::Newer { found, supported } => write!(
                f,
                "Folio format {found} is newer than supported format {supported}"
            ),
            Self::Io { path, source } => write!(
                f,
                "Folio directory operation failed at {}: {source}",
                path.display()
            ),
        }
    }
}
impl Error for FolioDirError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
