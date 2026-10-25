use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::folio_dir::{self, FolioDir, FolioId, FormatError};
use crate::home::HomeLock;
use crate::object::{ObjectId, ObjectKind};
use crate::registry::FolioFormat;
use crate::store::{ObjectStore, StoreError};

/// A portable ref name. Retention names are constructed with `pin_name`.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RefName(String);

impl RefName {
    pub fn parse(value: &str) -> Result<Self, RefError> {
        if value.is_empty()
            || value.len() > 100
            || value.contains("..")
            || (value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()))
            || !value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-/".contains(&b))
            || value.split('/').any(|part| {
                part.is_empty()
                    || part.starts_with(['.', '-'])
                    || part.ends_with('.')
                    || part.ends_with(".lock")
                    || device_name(part)
            })
        {
            return Err(RefError::InvalidName(value.into()));
        }
        Ok(Self(value.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn pin_parts(&self) -> Result<(FolioId, ObjectId), RefError> {
        parse_pin(&self.0)
    }
}
impl FromStr for RefName {
    type Err = RefError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}
impl fmt::Display for RefName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
fn device_name(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or(part);
    matches!(stem, "con" | "prn" | "aux" | "nul")
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

/// The fixed retention filename is 101 bytes, independently of the user name limit.
pub fn pin_name(parent: FolioId, commit: ObjectId) -> RefName {
    RefName(format!("{parent}-{commit}"))
}
/// Splits a canonical retention filename into its parent Folio and commit.
pub fn parse_pin(value: &str) -> Result<(FolioId, ObjectId), RefError> {
    if value.len() != 101 || value.as_bytes()[36] != b'-' || !value.is_ascii() {
        return Err(RefError::InvalidName(value.into()));
    }
    let parent = FolioId::parse(&value[..36]).map_err(|_| RefError::InvalidName(value.into()))?;
    let commit = value[37..]
        .parse()
        .map_err(|_| RefError::InvalidName(value.into()))?;
    Ok((parent, commit))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Branch,
    Tag,
    Pin,
}
impl Kind {
    fn directory(self) -> &'static str {
        match self {
            Self::Branch => "heads",
            Self::Tag => "tags",
            Self::Pin => "pins",
        }
    }
    fn name(self, value: &str) -> Result<RefName, RefError> {
        if self == Self::Pin {
            parse_pin(value)?;
            Ok(RefName(value.into()))
        } else {
            RefName::parse(value)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Expect {
    Absent,
    Is(ObjectId),
    Any,
}
impl Expect {
    fn check(self, actual: Option<ObjectId>) -> Result<(), RefError> {
        if match self {
            Self::Absent => actual.is_none(),
            Self::Is(old) => actual == Some(old),
            Self::Any => true,
        } {
            Ok(())
        } else {
            Err(RefError::Stale { actual })
        }
    }
}

/// Loose refs belonging to one Folio.
pub struct RefStore {
    root: PathBuf,
    objects: ObjectStore,
}
impl RefStore {
    pub fn open(folio: &FolioDir, format: &FolioFormat) -> Result<Self, RefError> {
        folio_dir::check_format(format).map_err(RefError::Format)?;
        Ok(Self {
            root: folio.path().to_path_buf(),
            objects: ObjectStore::open(folio, format).map_err(RefError::Store)?,
        })
    }
    fn path(&self, kind: Kind, name: &RefName) -> Result<PathBuf, RefError> {
        kind.name(name.as_str())?;
        let mut path = self.root.clone();
        check_directory(&path)?;
        for component in ["refs", kind.directory()] {
            path.push(component);
            check_directory(&path)?;
        }
        let parts: Vec<_> = name.as_str().split('/').collect();
        for (index, part) in parts.iter().enumerate() {
            path.push(part);
            match metadata(&path)? {
                Some(meta) if meta.is_dir() && index + 1 == parts.len() => {
                    return Err(RefError::Conflict { existing: path });
                }
                Some(meta) if !meta.file_type().is_file() && !meta.file_type().is_dir() => {
                    return Err(RefError::InvalidRef(path));
                }
                Some(meta) if meta.is_file() && index + 1 != parts.len() => {
                    return Err(RefError::Conflict { existing: path });
                }
                _ => {}
            }
        }
        Ok(path)
    }
    pub fn read(&self, kind: Kind, name: &RefName) -> Result<Option<ObjectId>, RefError> {
        let path = self.path(kind, name)?;
        read_value(&path, kind, name)
    }
    pub fn update(
        &self,
        _lock: &HomeLock,
        kind: Kind,
        name: &RefName,
        new: ObjectId,
        expect: Expect,
    ) -> Result<(), RefError> {
        let path = self.path(kind, name)?;
        expect.check(read_value(&path, kind, name)?)?;
        if kind == Kind::Pin && name.pin_parts()?.1 != new {
            return Err(RefError::InvalidRef(path));
        }
        let actual = self.objects.kind_of(new).map_err(|error| match error {
            StoreError::Io { source, .. } if source.kind() == io::ErrorKind::NotFound => {
                RefError::MissingTarget(new)
            }
            error => RefError::Store(error),
        })?;
        if actual != ObjectKind::Commit && !(kind == Kind::Tag && actual == ObjectKind::Tag) {
            return Err(RefError::WrongKind { id: new, actual });
        }
        atomic_write(&path, format!("{new}\n").as_bytes())
    }
    pub fn delete(
        &self,
        _lock: &HomeLock,
        kind: Kind,
        name: &RefName,
        expect: Expect,
    ) -> Result<(), RefError> {
        let path = self.path(kind, name)?;
        let actual = read_value(&path, kind, name)?;
        expect.check(actual)?;
        if actual.is_none() {
            return Ok(());
        }
        fs::remove_file(&path).map_err(|source| io_error(&path, source))?;
        let base = self.root.join("refs").join(kind.directory());
        let mut parent = path.parent();
        while let Some(path) = parent {
            if path == base {
                break;
            }
            match fs::remove_dir(path) {
                Ok(()) => parent = path.parent(),
                Err(e) if e.kind() == io::ErrorKind::DirectoryNotEmpty => break,
                Err(source) => return Err(io_error(path, source)),
            }
        }
        Ok(())
    }
    pub fn list(&self, kind: Kind) -> Result<Vec<(RefName, ObjectId)>, RefError> {
        check_directory(&self.root)?;
        check_directory(&self.root.join("refs"))?;
        let base = self.root.join("refs").join(kind.directory());
        let mut result = Vec::new();
        walk(&base, &mut |path| {
            let relative = path.strip_prefix(&base).expect("walk stays beneath base");
            let value = relative
                .components()
                .map(|part| part.as_os_str().to_str())
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| RefError::InvalidRef(path.into()))?
                .join("/");
            let name = kind.name(&value)?;
            if let Some(id) = read_value(path, kind, &name)? {
                result.push((name, id));
            }
            Ok(())
        })?;
        result.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(result)
    }
    /// Sync containing directories once after a batch.
    pub fn sync_refs(&self) -> Result<(), RefError> {
        sync_directories(&self.root.join("refs"))
    }
    /// Removes abandoned ref staging files while holding the home lock.
    pub fn clean_temporary_files(&self, _lock: &HomeLock) -> Result<(), RefError> {
        clean_directory(&self.root.join("refs"), true)
    }
}

fn metadata(path: &Path) -> Result<Option<fs::Metadata>, RefError> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(io_error(path, source)),
    }
}
fn check_directory(path: &Path) -> Result<(), RefError> {
    if let Some(meta) = metadata(path)?
        && !meta.file_type().is_dir()
    {
        return Err(RefError::InvalidRef(path.into()));
    }
    Ok(())
}
fn read_bytes(path: &Path, limit: u64) -> Result<Option<Vec<u8>>, RefError> {
    let Some(meta) = metadata(path)? else {
        return Ok(None);
    };
    if !meta.file_type().is_file() {
        return Err(RefError::InvalidRef(path.into()));
    }
    let file = File::open(path).map_err(|source| io_error(path, source))?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    if bytes.len() as u64 > limit {
        return Err(RefError::InvalidRef(path.into()));
    }
    Ok(Some(bytes))
}
fn read_value(path: &Path, kind: Kind, name: &RefName) -> Result<Option<ObjectId>, RefError> {
    let Some(bytes) = read_bytes(path, 65)? else {
        return Ok(None);
    };
    let id = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|text| text.strip_suffix('\n'))
        .and_then(|text| text.parse::<ObjectId>().ok())
        .ok_or_else(|| RefError::InvalidRef(path.into()))?;
    if kind == Kind::Pin && name.pin_parts()?.1 != id {
        return Err(RefError::InvalidRef(path.into()));
    }
    Ok(Some(id))
}
fn temporary_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(".ref.") else {
        return false;
    };
    let Some((pid, counter)) = rest.split_once('.') else {
        return false;
    };
    !pid.is_empty()
        && !counter.is_empty()
        && pid.bytes().all(|b| b.is_ascii_digit())
        && counter.bytes().all(|b| b.is_ascii_digit())
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), RefError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().expect("ref has a parent directory");
    fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
    let (temporary, mut file) = loop {
        let temporary = parent.join(format!(
            ".ref.{}.{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => break (temporary, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(io_error(&temporary, source)),
        }
    };
    let result = (|| {
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|source| io_error(&temporary, source))?;
        drop(file);
        fs::rename(&temporary, path).map_err(|source| io_error(path, source))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
fn entries(path: &Path) -> Result<Vec<fs::DirEntry>, RefError> {
    check_directory(path)?;
    match fs::read_dir(path) {
        Ok(entries) => entries
            .map(|entry| entry.map_err(|source| io_error(path, source)))
            .collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(source) => Err(io_error(path, source)),
    }
}
fn walk(
    path: &Path,
    visit: &mut impl FnMut(&Path) -> Result<(), RefError>,
) -> Result<(), RefError> {
    for entry in entries(path)? {
        let path = entry.path();
        let ty = entry
            .file_type()
            .map_err(|source| io_error(&path, source))?;
        if ty.is_dir() {
            walk(&path, visit)?;
        } else if ty.is_file() && entry.file_name().to_str().is_some_and(temporary_name) {
            continue;
        } else if ty.is_file() {
            visit(&path)?;
        } else {
            return Err(RefError::InvalidRef(path));
        }
    }
    Ok(())
}
fn clean_directory(path: &Path, recursive: bool) -> Result<(), RefError> {
    for entry in entries(path)? {
        let child = entry.path();
        let ty = entry
            .file_type()
            .map_err(|source| io_error(&child, source))?;
        if ty.is_file() && entry.file_name().to_str().is_some_and(temporary_name) {
            fs::remove_file(&child).map_err(|source| io_error(&child, source))?;
        } else if recursive && ty.is_dir() {
            clean_directory(&child, true)?;
        } else if recursive && !ty.is_file() {
            return Err(RefError::InvalidRef(child));
        }
    }
    Ok(())
}
fn sync_directory(path: &Path) -> Result<(), RefError> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|source| io_error(path, source))
}
fn sync_directories(path: &Path) -> Result<(), RefError> {
    for entry in entries(path)? {
        let child = entry.path();
        if entry
            .file_type()
            .map_err(|source| io_error(&child, source))?
            .is_dir()
        {
            sync_directories(&child)?;
        }
    }
    if metadata(path)?.is_some() {
        sync_directory(path)?;
    }
    Ok(())
}

pub(crate) fn inspect_refs(root: &Path) -> Result<(), RefError> {
    let refs = root.join("refs");
    for entry in entries(&refs)? {
        let path = entry.path();
        let kind = match entry.file_name().to_str() {
            Some("heads") => Kind::Branch,
            Some("tags") => Kind::Tag,
            Some("pins") => Kind::Pin,
            _ => return Err(RefError::InvalidRef(path)),
        };
        check_directory(&path)?;
        walk(&path, &mut |file| {
            let relative = file.strip_prefix(&path).expect("walk stays beneath base");
            let value = relative
                .components()
                .map(|part| part.as_os_str().to_str())
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| RefError::InvalidRef(file.into()))?
                .join("/");
            let name = kind.name(&value)?;
            read_value(file, kind, &name)?;
            Ok(())
        })?;
    }
    Ok(())
}
fn io_error(path: &Path, source: io::Error) -> RefError {
    RefError::Io {
        path: path.into(),
        source,
    }
}

#[derive(Debug)]
pub enum RefError {
    InvalidName(String),
    InvalidRef(PathBuf),
    Stale { actual: Option<ObjectId> },
    Conflict { existing: PathBuf },
    MissingTarget(ObjectId),
    WrongKind { id: ObjectId, actual: ObjectKind },
    Format(FormatError),
    Store(StoreError),
    Io { path: PathBuf, source: io::Error },
}
impl fmt::Display for RefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName(name) => write!(f, "invalid ref name: {name}"),
            Self::InvalidRef(path) => write!(f, "invalid ref at {}", path.display()),
            Self::Stale { actual } => write!(f, "stale ref; actual value: {actual:?}"),
            Self::Conflict { existing } => write!(f, "ref conflicts with {}", existing.display()),
            Self::MissingTarget(id) => write!(f, "missing ref target: {id}"),
            Self::WrongKind { id, actual } => write!(f, "invalid ref target {id}: {actual:?}"),
            Self::Format(error) => error.fmt(f),
            Self::Store(error) => error.fmt(f),
            Self::Io { path, source } => write!(f, "ref I/O at {}: {source}", path.display()),
        }
    }
}
impl Error for RefError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Format(error) => Some(error),
            Self::Store(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
