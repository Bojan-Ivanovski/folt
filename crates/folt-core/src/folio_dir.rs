use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const RECORD_VERSION: u64 = 1;

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

/// Content and metadata used to create a Folio directory.
pub struct FolioSpec {
    /// Original filename, retained in `folio.toml`.
    pub original_name: OsString,
    /// Content bytes; `None` is a directory Folio without `__folio__.md`.
    pub content: Option<Vec<u8>>,
    /// Remaining metadata fields in TOML form. The original name is added by this module.
    pub metadata_toml: String,
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
    Newer { found: u64, supported: u64 },
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
    let toml = format!(
        "original_name = {}\n{}",
        toml_string(&spec.original_name.to_string_lossy()),
        spec.metadata_toml
    );
    write_new(&temporary.join("folio.toml"), toml.as_bytes())?;
    if let Some(content) = spec.content {
        let filename = content_filename(&spec.original_name);
        let content_path = temporary.join(filename);
        let mut file = create_file(&content_path)?;
        file.write_all(&content)
            .map_err(|source| io_error(&content_path, source))?;
        if durability == Durability::DataAndDirectories {
            file.sync_all()
                .map_err(|source| io_error(&content_path, source))?;
        }
    }
    write_new(
        &temporary.join("record"),
        format!("{RECORD_VERSION}\nsha256\n").as_bytes(),
    )?;
    ensure_directory(&temporary.join("blobs"))?;
    ensure_directory(&temporary.join("refs"))?;
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
pub fn inspect(home: &Path, name: &OsStr) -> Inspection {
    let Some(text) = name.to_str() else {
        return Inspection::NotAFolio;
    };
    let Ok(id) = FolioId::parse(text) else {
        return Inspection::NotAFolio;
    };
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
    for (entry, directory) in [
        ("folio.toml", false),
        ("record", false),
        ("blobs", true),
        ("refs", true),
    ] {
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
    if let Ok(entries) = fs::read_dir(&path) {
        let content = entries
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_name().to_string_lossy().starts_with("file.")
                    || entry.file_name() == "file"
            })
            .count();
        if content > 1 {
            problems.push(Problem {
                entry: "file".into(),
                message: "multiple content files".into(),
            });
        }
    }
    match super::store::parse_format(&path.join("record")) {
        Ok(format) if format.layout > RECORD_VERSION => {
            return Inspection::Newer {
                found: format.layout,
                supported: RECORD_VERSION,
            };
        }
        Ok(_) => {}
        Err(error) => problems.push(Problem {
            entry: "record".into(),
            message: error.to_string(),
        }),
    }
    if !problems.is_empty() {
        Inspection::Damaged(problems)
    } else if !fixes.is_empty() {
        Inspection::Repairable(FolioDir { id, path }, fixes)
    } else {
        Inspection::Valid(FolioDir { id, path })
    }
}

/// Validates a Folio, accepting only a complete directory.
pub fn validate(home: &Path, id: FolioId) -> Result<FolioDir, FolioDirError> {
    match inspect(home, OsStr::new(&id.to_string())) {
        Inspection::Valid(directory) => Ok(directory),
        Inspection::Newer { found, supported } => Err(FolioDirError::Newer { found, supported }),
        _ => Err(FolioDirError::InvalidDirectory(
            home.join("folios").join(id.to_string()),
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
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), FolioDirError> {
    create_file(path)?
        .write_all(bytes)
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
fn content_filename(name: &OsStr) -> OsString {
    let text = name.to_string_lossy();
    let ext = Path::new(text.as_ref()).extension().and_then(OsStr::to_str);
    match ext.filter(|ext| !ext.is_empty()) {
        Some(ext) => format!("file.{ext}").into(),
        None => "file".into(),
    }
}
fn toml_string(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    )
}

/// Errors returned by Folio directory operations.
#[derive(Debug)]
pub enum FolioDirError {
    InvalidId(String),
    AlreadyExists(PathBuf),
    InvalidDirectory(PathBuf),
    Newer { found: u64, supported: u64 },
    Io { path: PathBuf, source: io::Error },
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
