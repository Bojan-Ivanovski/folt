use std::error::Error;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction};

use crate::folio_dir::FolioId;
use crate::home::{Home, HomeLock};
use crate::layout::{HOME_FORMAT_VERSION, LayoutVersion};
use crate::object::ObjectId;
use crate::placement::Signature;

const SCHEMA_VERSION: i64 = 2;
const APPLICATION_ID: i64 = 0x464f_4c54;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const MARKER: &str = "__folio__.md";

/// A placement kind stored by the registry as an integer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Placement {
    pub path: PlacementPath,
    pub id: FolioId,
    pub kind: i64,
    /// Missing when the placement's baseline has not been captured or was cleared.
    pub base: Option<PlacementBase>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FolioFormat {
    pub layout: LayoutVersion,
    pub algorithm: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewFolio {
    pub id: FolioId,
    pub format: FolioFormat,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FolioRow {
    pub id: FolioId,
    pub format: FolioFormat,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlacementBase {
    pub hash: ObjectId,
    pub signature: Signature,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewPlacement {
    pub path: PlacementPath,
    pub id: FolioId,
    pub kind: i64,
    pub base: PlacementBase,
}

/// A path whose filesystem spelling has been resolved before registry access.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PlacementPath(Vec<u8>);

impl PlacementPath {
    /// Resolves a path to an absolute key without following its final component.
    /// A Folio directory resolves to its `__folio__.md` record.
    pub fn resolve(path: impl AsRef<Path>) -> Result<Self, RegistryError> {
        resolve_path(path.as_ref(), true).map(Self)
    }

    fn from_stored(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// A resolved absolute directory prefix used to query or move placements.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PlacementPrefix(Vec<u8>);

impl PlacementPrefix {
    /// Resolves a directory path without appending a Folio record name.
    pub fn resolve(path: impl AsRef<Path>) -> Result<Self, RegistryError> {
        resolve_path(path.as_ref(), false).map(Self)
    }
}

fn resolve_path(input: &Path, append_marker: bool) -> Result<Vec<u8>, RegistryError> {
    let absolute = if input.is_absolute() {
        input.to_path_buf()
    } else {
        std::env::current_dir()?.join(input)
    };
    let metadata = fs::symlink_metadata(&absolute);
    let candidate = match metadata {
        Ok(metadata) if metadata.is_dir() && append_marker => absolute.join(MARKER),
        _ => absolute,
    };
    let parent = candidate
        .parent()
        .ok_or_else(|| RegistryError::InvalidPath(candidate.clone()))?;
    let name = candidate
        .file_name()
        .ok_or_else(|| RegistryError::InvalidPath(candidate.clone()))?;
    let canonical_parent = fs::canonicalize(parent)?;
    let resolved_name = on_disk_spelling(&canonical_parent, name)?;
    let resolved = canonical_parent.join(resolved_name);
    Ok(path_bytes(&resolved))
}

/// SQLite registry for Folios and their placements.
pub struct Registry {
    connection: Option<Connection>,
    path: PathBuf,
}

impl Registry {
    /// Opens or creates the registry while the caller holds the home lock.
    pub fn open_or_create(home: &Home, _lock: &HomeLock) -> Result<Self, RegistryError> {
        let path = home.path().join("registry.db");
        let connection = Connection::open(&path)?;
        configure(&connection, true)?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let app: i64 = connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(RegistryError::UnsupportedSchemaVersion {
                found: version,
                supported: SCHEMA_VERSION,
            });
        }
        if version != 0 && app != APPLICATION_ID {
            return Err(RegistryError::ForeignDatabase(path));
        }
        if version == 1 {
            // This refusal must be removed before the first release, when migrations are required.
            return Err(RegistryError::OutdatedSchema {
                found: version,
                required: SCHEMA_VERSION,
                path: path.clone(),
            });
        }
        if version == 0 {
            if app != 0 {
                return Err(RegistryError::ForeignDatabase(path));
            }
            let tx = connection.unchecked_transaction()?;
            tx.execute_batch("CREATE TABLE folios(id BLOB PRIMARY KEY, layout TEXT NOT NULL, algorithm TEXT NOT NULL) WITHOUT ROWID;\
                CREATE TABLE placements(path BLOB PRIMARY KEY, id BLOB NOT NULL REFERENCES folios(id), kind INTEGER NOT NULL, base BLOB, size INTEGER, mtime INTEGER, ctime INTEGER, inode INTEGER) WITHOUT ROWID;\
                CREATE INDEX placements_id ON placements(id);")?;
            tx.execute_batch("CREATE TABLE folt_metadata(key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;")?;
            tx.execute(
                "INSERT INTO folt_metadata(key,value) VALUES ('format_version',?1)",
                [HOME_FORMAT_VERSION.to_string()],
            )?;
            tx.pragma_update(None, "application_id", APPLICATION_ID)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
        }
        Ok(Self {
            connection: Some(connection),
            path,
        })
    }

    /// Opens an existing registry read-only. A missing registry is an empty view.
    pub fn open_existing(home: &Home) -> Result<Self, RegistryError> {
        let path = home.path().join("registry.db");
        let connection = if path.exists() {
            let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            configure(&connection, false)?;
            check_schema(&connection, &path)?;
            Some(connection)
        } else {
            None
        };
        Ok(Self { connection, path })
    }

    pub fn placement_at(&self, path: &PlacementPath) -> Result<Option<Placement>, RegistryError> {
        let Some(connection) = &self.connection else {
            return Ok(None);
        };
        query_placement(
            connection,
            "SELECT path,id,kind,base,size,mtime,ctime,inode FROM placements WHERE path=?1",
            &[&path.0],
        )
    }

    pub fn format_version(&self) -> Result<Option<String>, RegistryError> {
        let Some(connection) = &self.connection else {
            return Ok(None);
        };
        Ok(connection
            .query_row(
                "SELECT value FROM folt_metadata WHERE key='format_version'",
                [],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn placements_under(
        &self,
        prefix: &PlacementPrefix,
    ) -> Result<Vec<Placement>, RegistryError> {
        let Some(connection) = &self.connection else {
            return Ok(Vec::new());
        };
        let lower = prefix_boundary(&prefix.0);
        let upper = prefix_successor(&lower);
        let mut statement = match upper {
            Some(_) => connection.prepare("SELECT path,id,kind,base,size,mtime,ctime,inode FROM placements WHERE path >= ?1 AND path < ?2 ORDER BY path")?,
            None => connection.prepare("SELECT path,id,kind,base,size,mtime,ctime,inode FROM placements WHERE path >= ?1 ORDER BY path")?,
        };
        let mut rows = if let Some(upper) = upper {
            statement.query(rusqlite::params![lower, upper])?
        } else {
            statement.query([&lower])?
        };
        collect_placements(&mut rows)
    }

    pub fn placements_of(&self, id: FolioId) -> Result<Vec<Placement>, RegistryError> {
        let Some(connection) = &self.connection else {
            return Ok(Vec::new());
        };
        let mut statement = connection
            .prepare("SELECT path,id,kind,base,size,mtime,ctime,inode FROM placements WHERE id = ?1 ORDER BY path")?;
        let mut rows = statement.query([id.as_bytes().as_slice()])?;
        collect_placements(&mut rows)
    }

    pub fn folio_exists(&self, id: FolioId) -> Result<bool, RegistryError> {
        let Some(connection) = &self.connection else {
            return Ok(false);
        };
        Ok(connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM folios WHERE id = ?1)",
            [id.as_bytes().as_slice()],
            |row| row.get(0),
        )?)
    }

    pub fn folio_row(&self, id: FolioId) -> Result<Option<FolioRow>, RegistryError> {
        let Some(connection) = &self.connection else {
            return Ok(None);
        };
        let row = connection
            .query_row(
                "SELECT layout,algorithm FROM folios WHERE id=?1",
                [id.as_bytes().as_slice()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        row.map(|(text, algorithm)| {
            let layout = text
                .parse()
                .map_err(|_| RegistryError::InvalidLayout { id, text })?;
            Ok(FolioRow {
                id,
                format: FolioFormat { layout, algorithm },
            })
        })
        .transpose()
    }

    pub fn folios(&self) -> Result<Vec<FolioId>, RegistryError> {
        let Some(connection) = &self.connection else {
            return Ok(Vec::new());
        };
        let mut statement = connection.prepare("SELECT id FROM folios ORDER BY id")?;
        let mut rows = statement.query([])?;
        let mut result = Vec::new();
        while let Some(row) = rows.next()? {
            result.push(FolioId::from_bytes(read_id(row.get(0)?)?));
        }
        Ok(result)
    }

    /// Runs a caller's read/decision/write operation in one immediate transaction.
    pub fn write<T>(
        &mut self,
        _lock: &HomeLock,
        operation: impl FnOnce(&mut RegistryTransaction<'_>) -> Result<T, RegistryError>,
    ) -> Result<T, RegistryError> {
        let connection = self
            .connection
            .as_mut()
            .ok_or_else(|| RegistryError::MissingDatabase(self.path.clone()))?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut registry_tx = RegistryTransaction { tx };
        let value = operation(&mut registry_tx)?;
        registry_tx.tx.commit()?;
        Ok(value)
    }

    /// Writes a consistent SQLite snapshot to a destination path.
    pub fn backup_to(
        &self,
        _lock: &HomeLock,
        destination: impl AsRef<Path>,
    ) -> Result<(), RegistryError> {
        let connection = self
            .connection
            .as_ref()
            .ok_or_else(|| RegistryError::MissingDatabase(self.path.clone()))?;
        let destination = destination.as_ref();
        let temporary = destination.with_extension(format!("{}.tmp", std::process::id()));
        let escaped = temporary.to_string_lossy().replace('\'', "''");
        connection.execute(&format!("VACUUM INTO '{escaped}'"), [])?;
        fs::rename(&temporary, destination)?;
        Ok(())
    }
}

/// Operations available inside a single registry transaction.
pub struct RegistryTransaction<'a> {
    tx: Transaction<'a>,
}

impl RegistryTransaction<'_> {
    pub fn folio_exists(&self, id: FolioId) -> Result<bool, RegistryError> {
        Ok(self.tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM folios WHERE id=?1)",
            [id.as_bytes().as_slice()],
            |row| row.get(0),
        )?)
    }
    pub fn placement_at(&self, path: &PlacementPath) -> Result<Option<Placement>, RegistryError> {
        query_placement(
            &self.tx,
            "SELECT path,id,kind,base,size,mtime,ctime,inode FROM placements WHERE path=?1",
            &[&path.0],
        )
    }
    pub fn add_folio(&mut self, folio: &NewFolio) -> Result<(), RegistryError> {
        self.tx.execute(
            "INSERT INTO folios(id,layout,algorithm) VALUES (?1,?2,?3)",
            rusqlite::params![
                folio.id.as_bytes().as_slice(),
                folio.format.layout.to_string(),
                folio.format.algorithm
            ],
        )?;
        Ok(())
    }
    pub fn remove_folio(&mut self, id: FolioId) -> Result<(), RegistryError> {
        let count: i64 = self.tx.query_row(
            "SELECT count(*) FROM placements WHERE id=?1",
            [id.as_bytes().as_slice()],
            |r| r.get(0),
        )?;
        if count != 0 {
            return Err(RegistryError::FolioHasPlacements(id));
        }
        self.tx
            .execute("DELETE FROM folios WHERE id=?1", [id.as_bytes().as_slice()])?;
        Ok(())
    }
    pub fn add_placement(&mut self, placement: &NewPlacement) -> Result<(), RegistryError> {
        self.add_placement_with_base(
            placement.path.clone(),
            placement.id,
            placement.kind,
            Some(&placement.base),
        )
    }
    fn add_placement_with_base(
        &mut self,
        path: PlacementPath,
        id: FolioId,
        kind: i64,
        base: Option<&PlacementBase>,
    ) -> Result<(), RegistryError> {
        let encoded = encode_base(base)?;
        self.tx.execute(
            "INSERT INTO placements(path,id,kind,base,size,mtime,ctime,inode) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            rusqlite::params![path.0, id.as_bytes().as_slice(), kind, encoded.hash, encoded.size, encoded.mtime, encoded.ctime, encoded.inode],
        )?;
        Ok(())
    }
    pub fn update_placement_base(
        &mut self,
        path: &PlacementPath,
        base: Option<&PlacementBase>,
    ) -> Result<(), RegistryError> {
        let encoded = encode_base(base)?;
        self.tx.execute(
            "UPDATE placements SET base=?2,size=?3,mtime=?4,ctime=?5,inode=?6 WHERE path=?1",
            rusqlite::params![
                path.0,
                encoded.hash,
                encoded.size,
                encoded.mtime,
                encoded.ctime,
                encoded.inode
            ],
        )?;
        Ok(())
    }
    pub fn remove_placement(&mut self, path: &PlacementPath) -> Result<(), RegistryError> {
        self.tx
            .execute("DELETE FROM placements WHERE path=?1", [&path.0])?;
        Ok(())
    }
    pub fn move_placement(
        &mut self,
        old: &PlacementPath,
        new: &PlacementPath,
    ) -> Result<(), RegistryError> {
        self.tx.execute(
            "UPDATE placements SET path=?2 WHERE path=?1",
            rusqlite::params![old.0, new.0],
        )?;
        Ok(())
    }
    pub fn move_prefix(
        &mut self,
        old: &PlacementPrefix,
        new: &PlacementPrefix,
    ) -> Result<(), RegistryError> {
        let lower = prefix_boundary(&old.0);
        let upper = prefix_successor(&lower);
        let rows = match upper {
            Some(upper) => {
                let mut s = self
                    .tx
                    .prepare("SELECT path FROM placements WHERE path>=?1 AND path<?2")?;
                s.query_map(rusqlite::params![lower, upper], |r| r.get::<_, Vec<u8>>(0))?
                    .collect::<Result<Vec<_>, _>>()?
            }
            None => {
                let mut s = self
                    .tx
                    .prepare("SELECT path FROM placements WHERE path>=?1")?;
                s.query_map([&lower], |r| r.get::<_, Vec<u8>>(0))?
                    .collect::<Result<Vec<_>, _>>()?
            }
        };
        for path in rows {
            if !path.starts_with(&lower) {
                continue;
            }
            let suffix = &path[old.0.len()..];
            let mut destination = new.0.clone();
            destination.extend_from_slice(suffix);
            self.tx.execute(
                "UPDATE placements SET path=?2 WHERE path=?1",
                rusqlite::params![path, destination],
            )?;
        }
        Ok(())
    }
    pub fn set_kind(&mut self, path: &PlacementPath, kind: i64) -> Result<(), RegistryError> {
        self.tx.execute(
            "UPDATE placements SET kind=?2 WHERE path=?1",
            rusqlite::params![path.0, kind],
        )?;
        Ok(())
    }
    pub fn replace_all(
        &mut self,
        folios: &[FolioRow],
        placements: &[Placement],
    ) -> Result<(), RegistryError> {
        self.tx.execute("DELETE FROM placements", [])?;
        self.tx.execute("DELETE FROM folios", [])?;
        for row in folios {
            self.add_folio(&NewFolio {
                id: row.id,
                format: row.format.clone(),
            })?;
        }
        for placement in placements {
            self.add_placement_with_base(
                placement.path.clone(),
                placement.id,
                placement.kind,
                placement.base.as_ref(),
            )?;
        }
        Ok(())
    }
}

fn configure(connection: &Connection, writer: bool) -> Result<(), RegistryError> {
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    if writer {
        connection.pragma_update(None, "journal_mode", "WAL")?;
    }
    Ok(())
}
fn check_schema(connection: &Connection, path: &Path) -> Result<(), RegistryError> {
    let version: i64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let app: i64 = connection.pragma_query_value(None, "application_id", |r| r.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(RegistryError::UnsupportedSchemaVersion {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }
    if app != APPLICATION_ID {
        return Err(RegistryError::ForeignDatabase(path.to_path_buf()));
    }
    if version == 1 {
        return Err(RegistryError::OutdatedSchema {
            found: version,
            required: SCHEMA_VERSION,
            path: path.to_path_buf(),
        });
    }
    if version != SCHEMA_VERSION {
        return Err(RegistryError::ForeignDatabase(path.to_path_buf()));
    }
    Ok(())
}
fn query_placement(
    connection: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Option<Placement>, RegistryError> {
    connection
        .query_row(sql, params, |row| {
            Ok(Placement {
                path: PlacementPath::from_stored(row.get(0)?),
                id: FolioId::from_bytes(read_id(row.get(1)?).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Blob,
                        Box::new(e),
                    )
                })?),
                kind: row.get(2)?,
                base: decode_base(
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                )
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Blob,
                        Box::new(e),
                    )
                })?,
            })
        })
        .optional()
        .map_err(Into::into)
}
fn collect_placements(rows: &mut rusqlite::Rows<'_>) -> Result<Vec<Placement>, RegistryError> {
    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        result.push(Placement {
            path: PlacementPath::from_stored(row.get(0)?),
            id: FolioId::from_bytes(read_id(row.get(1)?)?),
            kind: row.get(2)?,
            base: decode_base(
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
            )?,
        });
    }
    Ok(result)
}
struct EncodedBase {
    hash: Option<Vec<u8>>,
    size: Option<i64>,
    mtime: Option<i64>,
    ctime: Option<i64>,
    inode: Option<i64>,
}

fn encode_base(base: Option<&PlacementBase>) -> Result<EncodedBase, RegistryError> {
    let Some(base) = base else {
        return Ok(EncodedBase {
            hash: None,
            size: None,
            mtime: None,
            ctime: None,
            inode: None,
        });
    };
    let size = i64::try_from(base.signature.size)
        .map_err(|_| RegistryError::SizeOutOfRange(base.signature.size))?;
    let time = |value: Option<i128>| value.and_then(|n| i64::try_from(n).ok());
    Ok(EncodedBase {
        hash: Some(base.hash.as_bytes().to_vec()),
        size: Some(size),
        mtime: time(base.signature.mtime),
        ctime: time(base.signature.ctime),
        inode: base.signature.inode.map(|n| n as i64),
    })
}
fn decode_base(
    hash: Option<Vec<u8>>,
    size: Option<i64>,
    mtime: Option<i64>,
    ctime: Option<i64>,
    inode: Option<i64>,
) -> Result<Option<PlacementBase>, RegistryError> {
    let Some(hash) = hash else {
        return Ok(None);
    };
    let hash: [u8; 32] = hash
        .try_into()
        .map_err(|_| RegistryError::InvalidBaseHash)?;
    let size = size.ok_or(RegistryError::InvalidBaseSignature)?;
    Ok(Some(PlacementBase {
        hash: ObjectId::from_bytes(hash),
        signature: Signature {
            size: u64::try_from(size).map_err(|_| RegistryError::InvalidBaseSignature)?,
            mtime: mtime.map(i128::from),
            ctime: ctime.map(i128::from),
            inode: inode.map(|n| n as u64),
        },
    }))
}
fn read_id(bytes: Vec<u8>) -> Result<[u8; 16], RegistryError> {
    bytes.try_into().map_err(|_| RegistryError::InvalidId)
}
fn prefix_successor(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut upper = prefix.to_vec();
    for index in (0..upper.len()).rev() {
        if upper[index] != u8::MAX {
            upper[index] += 1;
            upper.truncate(index + 1);
            return Some(upper);
        }
    }
    None
}
fn prefix_boundary(prefix: &[u8]) -> Vec<u8> {
    let mut boundary = prefix.to_vec();
    #[cfg(unix)]
    boundary.push(b'/');
    #[cfg(windows)]
    boundary.extend_from_slice(&[b'\\', 0]);
    boundary
}
fn path_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect()
    }
}
fn on_disk_spelling(parent: &Path, name: &OsStr) -> Result<PathBuf, RegistryError> {
    let name_path = Path::new(name);
    let Some(text) = name.to_str() else {
        return Ok(name_path.to_path_buf());
    };
    let has_letter = text.chars().any(char::is_alphabetic);
    if !has_letter || !case_insensitive_probe(parent, text) {
        return Ok(name_path.to_path_buf());
    }
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(text)
        {
            return Ok(entry.file_name().into());
        }
    }
    Ok(name_path.to_path_buf())
}
fn case_insensitive_probe(parent: &Path, name: &str) -> bool {
    let flipped: String = name
        .chars()
        .map(|character| {
            if character.is_lowercase() {
                character.to_uppercase().next().unwrap_or(character)
            } else if character.is_uppercase() {
                character.to_lowercase().next().unwrap_or(character)
            } else {
                character
            }
        })
        .collect();
    flipped != name && fs::symlink_metadata(parent.join(flipped)).is_ok()
}

/// Errors returned by registry operations.
#[derive(Debug)]
pub enum RegistryError {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
    UnsupportedSchemaVersion {
        found: i64,
        supported: i64,
    },
    OutdatedSchema {
        found: i64,
        required: i64,
        path: PathBuf,
    },
    ForeignDatabase(PathBuf),
    InvalidPath(PathBuf),
    InvalidId,
    InvalidLayout {
        id: FolioId,
        text: String,
    },
    MissingDatabase(PathBuf),
    FolioHasPlacements(FolioId),
    SizeOutOfRange(u64),
    InvalidBaseHash,
    InvalidBaseSignature,
}
impl From<rusqlite::Error> for RegistryError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}
impl From<std::io::Error> for RegistryError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(e) => write!(f, "registry database error: {e}"),
            Self::Io(e) => write!(f, "registry filesystem error: {e}"),
            Self::UnsupportedSchemaVersion { found, supported } => write!(
                f,
                "registry schema version {found} is newer than supported version {supported}"
            ),
            Self::OutdatedSchema {
                found,
                required,
                path,
            } => write!(
                f,
                "registry {} uses pre-release schema version {found}; required version is {required}; delete this registry before continuing",
                path.display()
            ),
            Self::ForeignDatabase(path) => {
                write!(f, "not a Folt registry database: {}", path.display())
            }
            Self::InvalidPath(path) => write!(f, "invalid placement path: {}", path.display()),
            Self::InvalidId => write!(f, "registry contains an invalid Folio id"),
            Self::InvalidLayout { id, text } => {
                write!(
                    f,
                    "registry contains invalid layout version '{text}' for Folio {id}"
                )
            }
            Self::MissingDatabase(path) => {
                write!(f, "registry database does not exist: {}", path.display())
            }
            Self::FolioHasPlacements(_) => {
                write!(f, "cannot remove a Folio while placements refer to it")
            }
            Self::SizeOutOfRange(size) => {
                write!(f, "placement size {size} exceeds SQLite integer range")
            }
            Self::InvalidBaseHash => write!(f, "registry contains an invalid placement base hash"),
            Self::InvalidBaseSignature => {
                write!(f, "registry contains an invalid placement signature")
            }
        }
    }
}
impl Error for RegistryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Sqlite(e) => Some(e),
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}
