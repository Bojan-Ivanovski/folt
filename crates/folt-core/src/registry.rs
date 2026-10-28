use std::error::Error;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction};

use crate::folio_dir::FolioId;
use crate::home::{Home, HomeLock, is_regular_file};
use crate::layout::{DEFAULT_BRANCH, HOME_FORMAT_VERSION, LayoutVersion};
use crate::object::ObjectId;
pub use crate::placement::PlacementBase;
use crate::placement::{PlacementKind, Signature, StagedBlob};

const SCHEMA_VERSION: i64 = 2;
const APPLICATION_ID: i64 = 0x464f_4c54;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const MARKER: &str = "__folio__.md";

/// A placement stored by the registry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Placement {
    pub path: PlacementPath,
    pub id: FolioId,
    pub kind: PlacementKind,
    pub head: String,
    pub commit_id: Option<ObjectId>,
    /// Links have no base; working files may have an uncaptured or cleared base.
    pub base: Option<PlacementBase>,
    pub staged: Option<StagedBlob>,
}

impl Placement {
    /// Converts a registry row to filesystem facts while retaining its Folio id.
    pub fn to_placement(&self) -> (FolioId, crate::placement::Placement) {
        (
            self.id,
            crate::placement::Placement {
                path: self.path.to_path_buf(),
                kind: self.kind,
                head: self.head.clone(),
                commit_id: self.commit_id,
                base: self.base.clone(),
            },
        )
    }
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
pub struct NewPlacement {
    pub path: PlacementPath,
    pub id: FolioId,
    pub kind: PlacementKind,
    /// Defaults to the layout's default branch when omitted.
    pub head: Option<String>,
    pub commit_id: Option<ObjectId>,
    pub base: Option<PlacementBase>,
    pub staged: Option<StagedBlob>,
}

/// Builds the initial internal row without registering it or capturing a base.
pub fn internal_placement(home: &Home, id: FolioId) -> Result<NewPlacement, RegistryError> {
    Ok(NewPlacement {
        path: PlacementPath::resolve(crate::placement_fs::internal_path(home, id))?,
        id,
        kind: PlacementKind::Internal,
        head: Some(DEFAULT_BRANCH.into()),
        commit_id: None,
        base: None,
        staged: None,
    })
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

    /// Resolves a directory itself, without appending a Folio record name.
    pub fn resolve_directory(path: impl AsRef<Path>) -> Result<Self, RegistryError> {
        resolve_path(path.as_ref(), false).map(Self)
    }

    pub(crate) fn replace_prefix(&self, old: &Self, new: &Self) -> Self {
        let mut path = new.0.clone();
        path.extend_from_slice(&self.0[old.0.len()..]);
        Self(path)
    }

    fn from_stored(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn to_path_buf(&self) -> PathBuf {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            std::ffi::OsString::from_vec(self.0.clone()).into()
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            let wide: Vec<_> = self
                .0
                .as_chunks::<2>()
                .0
                .iter()
                .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
                .collect();
            std::ffi::OsString::from_wide(&wide).into()
        }
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
    pub(crate) fn home_path(&self) -> &Path {
        self.path.parent().expect("registry path has a home")
    }

    /// Opens or creates the registry while the caller holds the home lock.
    pub fn open_or_create(home: &Home, _lock: &HomeLock) -> Result<Self, RegistryError> {
        let path = home.path().join("registry.db");
        match is_regular_file(&path) {
            Ok(true) => {}
            Ok(false) => return Err(RegistryError::NotRegularFile(path)),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(RegistryError::Io(source)),
        }
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
                CREATE TABLE placements(path BLOB PRIMARY KEY, id BLOB NOT NULL REFERENCES folios(id), kind TEXT NOT NULL CHECK (kind IN ('link','copy','internal')), head TEXT NOT NULL DEFAULT 'main', commit_id BLOB, base BLOB, size INTEGER, mtime INTEGER, ctime INTEGER, inode INTEGER, staged BLOB, staged_size INTEGER, CHECK ((staged IS NULL) = (staged_size IS NULL)), CHECK (staged_size IS NULL OR staged_size >= 0), CHECK (kind != 'internal' OR head = 'main'), CHECK (kind != 'link' OR (head = 'main' AND commit_id IS NULL AND base IS NULL AND staged IS NULL))) WITHOUT ROWID;\
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
        let connection = match is_regular_file(&path) {
            Ok(true) => {
                let connection =
                    Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
                configure(&connection, false)?;
                check_schema(&connection, &path)?;
                Some(connection)
            }
            Ok(false) => return Err(RegistryError::NotRegularFile(path)),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => return Err(RegistryError::Io(source)),
        };
        Ok(Self { connection, path })
    }

    pub fn placement_at(&self, path: &PlacementPath) -> Result<Option<Placement>, RegistryError> {
        let Some(connection) = &self.connection else {
            return Ok(None);
        };
        query_placement(
            connection,
            "SELECT path,id,kind,head,commit_id,base,size,mtime,ctime,inode,staged,staged_size FROM placements WHERE path=?1",
            &[&path.0],
        )
    }

    /// Returns the Folio's internal working-file row, if registered.
    pub fn internal_placement(&self, id: FolioId) -> Result<Option<Placement>, RegistryError> {
        let Some(connection) = &self.connection else {
            return Ok(None);
        };
        query_placement(
            connection,
            "SELECT path,id,kind,head,commit_id,base,size,mtime,ctime,inode,staged,staged_size FROM placements WHERE id=?1 AND kind='internal'",
            &[&id.as_bytes().as_slice()],
        )
    }

    #[cfg(test)]
    pub(crate) fn fail_writes_at(&self, path: &PlacementPath, insert: bool) {
        let operation = if insert { "INSERT" } else { "DELETE" };
        let row = if insert { "NEW" } else { "OLD" };
        let hex: String = path
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        self.connection.as_ref().unwrap().execute_batch(&format!(
            "CREATE TRIGGER injected_failure BEFORE {operation} ON placements WHEN {row}.path = X'{hex}' BEGIN SELECT RAISE(ABORT, 'injected failure'); END;"
        )).unwrap();
    }

    #[cfg(test)]
    pub(crate) fn clear_write_failure(&self) {
        self.connection
            .as_ref()
            .unwrap()
            .execute_batch("DROP TRIGGER injected_failure")
            .unwrap();
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
            Some(_) => connection.prepare("SELECT path,id,kind,head,commit_id,base,size,mtime,ctime,inode,staged,staged_size FROM placements WHERE kind != 'internal' AND path >= ?1 AND path < ?2 ORDER BY path")?,
            None => connection.prepare("SELECT path,id,kind,head,commit_id,base,size,mtime,ctime,inode,staged,staged_size FROM placements WHERE kind != 'internal' AND path >= ?1 ORDER BY path")?,
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
            .prepare("SELECT path,id,kind,head,commit_id,base,size,mtime,ctime,inode,staged,staged_size FROM placements WHERE id = ?1 AND kind != 'internal' ORDER BY path")?;
        let mut rows = statement.query([id.as_bytes().as_slice()])?;
        collect_placements(&mut rows)
    }

    /// Guards branch deletion, so includes the internal working-file row.
    pub fn placements_of_head(
        &self,
        id: FolioId,
        head: &str,
    ) -> Result<Vec<Placement>, RegistryError> {
        let Some(connection) = &self.connection else {
            return Ok(Vec::new());
        };
        let mut statement = connection.prepare(
            "SELECT path,id,kind,head,commit_id,base,size,mtime,ctime,inode,staged,staged_size FROM placements WHERE id=?1 AND head=?2 ORDER BY path",
        )?;
        let mut rows = statement.query(rusqlite::params![id.as_bytes().as_slice(), head])?;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MoveResult {
    Moved,
    NotRegistered,
    DestinationRegistered,
    InsideSource,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoveResult {
    Removed,
    NotRegistered,
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
            "SELECT path,id,kind,head,commit_id,base,size,mtime,ctime,inode,staged,staged_size FROM placements WHERE path=?1",
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
            "SELECT count(*) FROM placements WHERE id=?1 AND kind != 'internal'",
            [id.as_bytes().as_slice()],
            |r| r.get(0),
        )?;
        if count != 0 {
            return Err(RegistryError::FolioHasPlacements(id));
        }
        self.tx.execute(
            "DELETE FROM placements WHERE id=?1 AND kind='internal'",
            [id.as_bytes().as_slice()],
        )?;
        self.tx
            .execute("DELETE FROM folios WHERE id=?1", [id.as_bytes().as_slice()])?;
        Ok(())
    }
    pub fn add_placement(&mut self, placement: &NewPlacement) -> Result<(), RegistryError> {
        self.insert_placement(&Placement {
            path: placement.path.clone(),
            id: placement.id,
            kind: placement.kind,
            head: placement.head.as_deref().unwrap_or(DEFAULT_BRANCH).into(),
            commit_id: placement.commit_id,
            base: placement.base.clone(),
            staged: placement.staged,
        })
    }
    pub(crate) fn insert_placement(&mut self, placement: &Placement) -> Result<(), RegistryError> {
        let encoded = encode_base(placement.base.as_ref())?;
        let (staged, staged_size) = encode_staged(placement.staged)?;
        self.tx.execute(
            "INSERT INTO placements(path,id,kind,head,commit_id,base,size,mtime,ctime,inode,staged,staged_size) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            rusqlite::params![placement.path.0, placement.id.as_bytes().as_slice(), placement.kind.as_str(), placement.head, placement.commit_id.map(|id| id.as_bytes().to_vec()), encoded.hash, encoded.size, encoded.mtime, encoded.ctime, encoded.inode, staged, staged_size],
        )?;
        Ok(())
    }
    pub fn update_placement(&mut self, placement: &Placement) -> Result<(), RegistryError> {
        let encoded = encode_base(placement.base.as_ref())?;
        let (staged, staged_size) = encode_staged(placement.staged)?;
        self.tx.execute(
            "UPDATE placements SET id=?2,kind=?3,head=?4,commit_id=?5,base=?6,size=?7,mtime=?8,ctime=?9,inode=?10,staged=?11,staged_size=?12 WHERE path=?1",
            rusqlite::params![placement.path.0, placement.id.as_bytes().as_slice(), placement.kind.as_str(), placement.head, placement.commit_id.map(|id| id.as_bytes().to_vec()), encoded.hash, encoded.size, encoded.mtime, encoded.ctime, encoded.inode, staged, staged_size],
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
    pub fn remove_placement(
        &mut self,
        path: &PlacementPath,
    ) -> Result<RemoveResult, RegistryError> {
        let count = self
            .tx
            .execute("DELETE FROM placements WHERE path=?1", [&path.0])?;
        Ok(if count == 0 {
            RemoveResult::NotRegistered
        } else {
            RemoveResult::Removed
        })
    }
    pub fn move_placement(
        &mut self,
        old: &PlacementPath,
        new: &PlacementPath,
    ) -> Result<MoveResult, RegistryError> {
        let exists = |path: &PlacementPath| -> Result<bool, RegistryError> {
            Ok(self.tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM placements WHERE path=?1)",
                [&path.0],
                |row| row.get(0),
            )?)
        };
        if !exists(old)? {
            return Ok(MoveResult::NotRegistered);
        }
        if exists(new)? {
            return Ok(MoveResult::DestinationRegistered);
        }
        self.tx.execute(
            "UPDATE placements SET path=?2 WHERE path=?1",
            rusqlite::params![old.0, new.0],
        )?;
        Ok(MoveResult::Moved)
    }
    pub fn move_prefix(
        &mut self,
        old: &PlacementPrefix,
        new: &PlacementPrefix,
    ) -> Result<MoveResult, RegistryError> {
        let lower = prefix_boundary(&old.0);
        if new.0 == old.0 || new.0.starts_with(&lower) {
            return Ok(MoveResult::InsideSource);
        }
        let rows = {
            let mut statement = self
                .tx
                .prepare("SELECT path FROM placements ORDER BY path")?;
            statement
                .query_map([], |row| row.get::<_, Vec<u8>>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let moves: Vec<_> = rows
            .iter()
            .filter(|path| path.starts_with(&lower))
            .map(|path| {
                let mut destination = new.0.clone();
                destination.extend_from_slice(&path[old.0.len()..]);
                (path, destination)
            })
            .collect();
        if moves.is_empty() {
            return Ok(MoveResult::NotRegistered);
        }
        if moves
            .iter()
            .any(|(_, destination)| rows.contains(destination))
        {
            return Ok(MoveResult::DestinationRegistered);
        }
        for (path, destination) in moves {
            self.tx.execute(
                "UPDATE placements SET path=?2 WHERE path=?1",
                rusqlite::params![path, destination],
            )?;
        }
        Ok(MoveResult::Moved)
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
            self.insert_placement(placement)?;
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
        .query_row(sql, params, read_placement)
        .optional()
        .map_err(Into::into)
}
fn read_placement(row: &rusqlite::Row<'_>) -> rusqlite::Result<Placement> {
    let invalid = |index, kind, error: RegistryError| {
        rusqlite::Error::FromSqlConversionFailure(index, kind, Box::new(error))
    };
    let kind: String = row.get(2)?;
    let commit: Option<Vec<u8>> = row.get(4)?;
    Ok(Placement {
        path: PlacementPath::from_stored(row.get(0)?),
        id: FolioId::from_bytes(
            read_id(row.get(1)?).map_err(|e| invalid(1, rusqlite::types::Type::Blob, e))?,
        ),
        kind: PlacementKind::parse(&kind).ok_or_else(|| {
            invalid(
                2,
                rusqlite::types::Type::Text,
                RegistryError::InvalidPlacementKind(kind),
            )
        })?,
        head: row.get(3)?,
        commit_id: commit
            .map(|bytes| {
                bytes.try_into().map(ObjectId::from_bytes).map_err(|_| {
                    invalid(
                        4,
                        rusqlite::types::Type::Blob,
                        RegistryError::InvalidCommitId,
                    )
                })
            })
            .transpose()?,
        base: decode_base(
            row.get(5)?,
            row.get(6)?,
            row.get(7)?,
            row.get(8)?,
            row.get(9)?,
        )
        .map_err(|e| invalid(5, rusqlite::types::Type::Blob, e))?,
        staged: decode_staged(row.get(10)?, row.get(11)?)
            .map_err(|e| invalid(10, rusqlite::types::Type::Blob, e))?,
    })
}
fn collect_placements(rows: &mut rusqlite::Rows<'_>) -> Result<Vec<Placement>, RegistryError> {
    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        result.push(read_placement(row)?);
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
fn encode_staged(
    staged: Option<StagedBlob>,
) -> Result<(Option<Vec<u8>>, Option<i64>), RegistryError> {
    match staged {
        None => Ok((None, None)),
        Some(staged) => Ok((
            Some(staged.hash.as_bytes().to_vec()),
            Some(
                i64::try_from(staged.size)
                    .map_err(|_| RegistryError::SizeOutOfRange(staged.size))?,
            ),
        )),
    }
}
fn decode_staged(
    hash: Option<Vec<u8>>,
    size: Option<i64>,
) -> Result<Option<StagedBlob>, RegistryError> {
    match (hash, size) {
        (None, None) => Ok(None),
        (Some(hash), Some(size)) => Ok(Some(StagedBlob {
            hash: ObjectId::from_bytes(hash.try_into().map_err(|_| RegistryError::InvalidStaged)?),
            size: u64::try_from(size).map_err(|_| RegistryError::InvalidStaged)?,
        })),
        _ => Err(RegistryError::InvalidStaged),
    }
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
    NotRegularFile(PathBuf),
    FolioHasPlacements(FolioId),
    SizeOutOfRange(u64),
    InvalidPlacementKind(String),
    InvalidCommitId,
    InvalidBaseHash,
    InvalidBaseSignature,
    InvalidStaged,
    PlacementChanged(PlacementPath),
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
            Self::NotRegularFile(path) => {
                write!(
                    f,
                    "registry database is not a regular file: {}",
                    path.display()
                )
            }
            Self::FolioHasPlacements(_) => {
                write!(f, "cannot remove a Folio while placements refer to it")
            }
            Self::SizeOutOfRange(size) => {
                write!(f, "placement size {size} exceeds SQLite integer range")
            }
            Self::InvalidPlacementKind(kind) => {
                write!(f, "registry contains an invalid placement kind: {kind}")
            }
            Self::InvalidCommitId => write!(f, "registry contains an invalid placement commit id"),
            Self::InvalidBaseHash => write!(f, "registry contains an invalid placement base hash"),
            Self::InvalidStaged => write!(f, "registry contains an invalid staged blob"),
            Self::PlacementChanged(path) => write!(
                f,
                "registry placement changed: {}",
                path.to_path_buf().display()
            ),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_writers_report_outcomes_without_partial_prefix_moves() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE placements(path BLOB PRIMARY KEY);")
            .unwrap();
        let key = |text: &str| {
            PlacementPath(path_bytes(Path::new(
                &text.replace('/', std::path::MAIN_SEPARATOR_STR),
            )))
        };
        let prefix = |text: &str| {
            PlacementPrefix(path_bytes(Path::new(
                &text.replace('/', std::path::MAIN_SEPARATOR_STR),
            )))
        };
        let a = key("/old/a");
        let b = key("/old/b");
        let collision = key("/new/b");
        for path in [&a, &b, &collision] {
            connection
                .execute("INSERT INTO placements(path) VALUES (?1)", [&path.0])
                .unwrap();
        }
        // These writers only need the path column; full row reads are not needed.
        let tx = connection.transaction().unwrap();
        let mut writer = RegistryTransaction { tx };
        assert_eq!(
            writer.remove_placement(&key("/missing")).unwrap(),
            RemoveResult::NotRegistered
        );
        assert_eq!(
            writer
                .move_prefix(&prefix("/old"), &prefix("/old/child"))
                .unwrap(),
            MoveResult::InsideSource
        );
        assert_eq!(
            writer
                .move_prefix(&prefix("/missing"), &prefix("/new"))
                .unwrap(),
            MoveResult::NotRegistered
        );
        assert_eq!(
            writer
                .move_prefix(&prefix("/old"), &prefix("/new"))
                .unwrap(),
            MoveResult::DestinationRegistered
        );
        assert_eq!(
            writer
                .tx
                .query_row(
                    "SELECT count(*) FROM placements WHERE path=?1",
                    [&a.0],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(
            writer.move_placement(&key("/missing"), &a).unwrap(),
            MoveResult::NotRegistered
        );
        assert_eq!(
            writer.move_placement(&a, &b).unwrap(),
            MoveResult::DestinationRegistered
        );
        assert_eq!(
            writer
                .move_placement(&collision, &key("/elsewhere"))
                .unwrap(),
            MoveResult::Moved
        );
        assert_eq!(
            writer.remove_placement(&key("/elsewhere")).unwrap(),
            RemoveResult::Removed
        );
        assert_eq!(
            writer
                .move_prefix(&prefix("/old"), &prefix("/new"))
                .unwrap(),
            MoveResult::Moved
        );
    }
}
