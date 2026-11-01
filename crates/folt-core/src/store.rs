use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::folio_dir::{self, FolioDir};
use crate::home::{HomeLock, is_regular_file};
use crate::layout::LayoutVersion;
use crate::object::{
    self, ObjectError, ObjectHasher, ObjectId, ObjectKind, ObjectReader, ObjectWriter,
    STREAM_BUFFER_SIZE,
};
use crate::registry::FolioFormat;

trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

/// Immutable objects stored beneath one Folio's `blobs` directory.
pub struct ObjectStore {
    root: PathBuf,
    format: FolioFormat,
}

impl ObjectStore {
    pub fn open(folio: &FolioDir, format: &FolioFormat) -> Result<Self, StoreError> {
        folio_dir::check_format(format).map_err(|error| match error {
            folio_dir::FormatError::Newer { found, supported } => {
                StoreError::NewerFormat { found, supported }
            }
            folio_dir::FormatError::Older { found, supported } => {
                StoreError::OlderFormat { found, supported }
            }
            folio_dir::FormatError::UnsupportedAlgorithm(value) => {
                StoreError::UnsupportedAlgorithm(value)
            }
        })?;
        Ok(Self {
            root: folio.path().join("blobs"),
            format: format.clone(),
        })
    }
    pub fn format(&self) -> FolioFormat {
        self.format.clone()
    }
    fn path(&self, id: ObjectId) -> PathBuf {
        self.root.join(format!("{id}.blob"))
    }
    pub fn contains(&self, id: ObjectId) -> Result<bool, StoreError> {
        match fs::metadata(self.path(id)) {
            Ok(meta) => Ok(meta.is_file()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(StoreError::Io {
                path: self.root.clone(),
                source,
            }),
        }
    }
    /// Reads only the codec and header; this does not verify the object's body or hash.
    pub fn kind_of(&self, id: ObjectId) -> Result<ObjectKind, StoreError> {
        let path = self.path(id);
        if !is_regular_file(&path).map_err(|source| StoreError::Io {
            path: path.clone(),
            source,
        })? {
            return Err(StoreError::NotRegularFile(path));
        }
        let file = File::open(&path).map_err(|source| StoreError::Io { path, source })?;
        object::peek_kind(file).map_err(StoreError::Object)
    }
    pub fn put(
        &self,
        lock: &HomeLock,
        kind: ObjectKind,
        body: &[u8],
    ) -> Result<ObjectId, StoreError> {
        self.put_source(
            lock,
            kind,
            body.len() as u64,
            || Ok(Box::new(Cursor::new(body))),
            Path::new("<slice>"),
        )
    }
    pub fn put_commit(
        &self,
        lock: &HomeLock,
        commit: &object::Commit,
    ) -> Result<ObjectId, StoreError> {
        let body = commit.encode_body()?;
        let id = self.put(lock, ObjectKind::Commit, &body)?;
        if id != object::hash_object(ObjectKind::Commit, &body) {
            return Err(ObjectError::HashMismatch.into());
        }
        Ok(id)
    }
    pub fn get_commit(&self, id: ObjectId, limit: u64) -> Result<object::Commit, StoreError> {
        Ok(object::Commit::parse_body(&self.get(
            id,
            ObjectKind::Commit,
            limit,
        )?)?)
    }
    pub fn put_tag(&self, lock: &HomeLock, tag: &object::Tag) -> Result<ObjectId, StoreError> {
        self.put(lock, ObjectKind::Tag, &tag.encode_body()?)
    }
    pub fn get_tag(&self, id: ObjectId, limit: u64) -> Result<object::Tag, StoreError> {
        Ok(object::Tag::parse_body(&self.get(
            id,
            ObjectKind::Tag,
            limit,
        )?)?)
    }
    pub fn put_file(
        &self,
        lock: &HomeLock,
        kind: ObjectKind,
        path: &Path,
    ) -> Result<ObjectId, StoreError> {
        let len = File::open(path)
            .map_err(|source| StoreError::Io {
                path: path.into(),
                source,
            })?
            .metadata()
            .map_err(|source| StoreError::Io {
                path: path.into(),
                source,
            })?
            .len();
        self.put_source(
            lock,
            kind,
            len,
            || {
                File::open(path)
                    .map(|file| Box::new(file) as Box<dyn ReadSeek>)
                    .map_err(|source| StoreError::Io {
                        path: path.into(),
                        source,
                    })
            },
            path,
        )
    }
    /// Stores a non-seekable reader through a temporary spool so it can be read twice.
    pub fn put_reader<R: Read>(
        &self,
        _lock: &HomeLock,
        kind: ObjectKind,
        length: u64,
        mut reader: R,
    ) -> Result<ObjectId, StoreError> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let spool = self.root.join(format!(
            ".input.{}.{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&spool)
                .map_err(|source| StoreError::Io {
                    path: spool.clone(),
                    source,
                })?;
            let mut count = 0u64;
            let mut buffer = vec![0; STREAM_BUFFER_SIZE];
            loop {
                let n = reader.read(&mut buffer).map_err(|source| StoreError::Io {
                    path: spool.clone(),
                    source,
                })?;
                if n == 0 {
                    break;
                }
                count = count
                    .checked_add(n as u64)
                    .ok_or(StoreError::SourceChanged)?;
                if count > length {
                    return Err(StoreError::SourceChanged);
                }
                file.write_all(&buffer[..n])
                    .map_err(|source| StoreError::Io {
                        path: spool.clone(),
                        source,
                    })?;
            }
            if count != length {
                return Err(StoreError::SourceChanged);
            }
            drop(file);
            self.put_source(
                _lock,
                kind,
                length,
                || {
                    File::open(&spool)
                        .map(|f| Box::new(f) as Box<dyn ReadSeek>)
                        .map_err(|source| StoreError::Io {
                            path: spool.clone(),
                            source,
                        })
                },
                &spool,
            )
        })();
        let _ = fs::remove_file(&spool);
        result
    }

    fn put_source<'a, F>(
        &self,
        _lock: &HomeLock,
        kind: ObjectKind,
        length: u64,
        mut fresh: F,
        source_path: &Path,
    ) -> Result<ObjectId, StoreError>
    where
        F: FnMut() -> Result<Box<dyn ReadSeek + 'a>, StoreError>,
    {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut buffer = vec![0; STREAM_BUFFER_SIZE];
        let mut first = fresh()?;
        let mut hash = ObjectHasher::new(kind, length);
        let mut count = 0u64;
        loop {
            let n = first.read(&mut buffer).map_err(|source| StoreError::Io {
                path: source_path.into(),
                source,
            })?;
            if n == 0 {
                break;
            }
            count = count
                .checked_add(n as u64)
                .ok_or(StoreError::SourceChanged)?;
            if count > length {
                return Err(StoreError::SourceChanged);
            }
            hash.update(&buffer[..n]);
        }
        if count != length {
            return Err(StoreError::SourceChanged);
        }
        let id = hash.finish();
        let dest = self.path(id);
        match is_regular_file(&dest) {
            Ok(true) => return Ok(id),
            Ok(false) => return Err(StoreError::NotRegularFile(dest)),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(StoreError::Io { path: dest, source }),
        }
        let mut input = fresh()?;
        let mut head = [0; 8];
        let n = input.read(&mut head).map_err(|source| StoreError::Io {
            path: source_path.into(),
            source,
        })?;
        let compressed = !object::already_compressed(&head[..n])
            && compression_probe_is_worthwhile(&mut *input, length, source_path)?;
        input
            .seek(SeekFrom::Start(0))
            .map_err(|source| StoreError::Io {
                path: source_path.into(),
                source,
            })?;
        let temp = self.root.join(format!(
            ".object.{}.{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|source| StoreError::Io {
                path: temp.clone(),
                source,
            })?;
        let write_result = (|| {
            let mut writer = ObjectWriter::new(out, kind, length, compressed)?;
            let mut verify = ObjectHasher::new(kind, length);
            let mut total = 0u64;
            loop {
                let n = input.read(&mut buffer).map_err(ObjectError::Io)?;
                if n == 0 {
                    break;
                }
                total += n as u64;
                verify.update(&buffer[..n]);
                writer.write_body(&buffer[..n])?;
            }
            if total != length || verify.finish() != id {
                return Err(ObjectError::InvalidFrame);
            }
            let (mut out, _) = writer.finish()?;
            out.flush().map_err(ObjectError::Io)?;
            out.sync_all().map_err(ObjectError::Io)?;
            Ok(())
        })();
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temp);
            return Err(StoreError::Object(error));
        }
        match is_regular_file(&dest) {
            Ok(true) => {
                fs::remove_file(&temp).map_err(|source| StoreError::Io { path: temp, source })?;
            }
            Ok(false) => {
                let _ = fs::remove_file(&temp);
                return Err(StoreError::NotRegularFile(dest));
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                fs::rename(&temp, &dest).map_err(|source| {
                    let _ = fs::remove_file(&temp);
                    StoreError::Io {
                        path: dest.clone(),
                        source,
                    }
                })?;
            }
            Err(source) => {
                let _ = fs::remove_file(&temp);
                return Err(StoreError::Io { path: dest, source });
            }
        }
        Ok(id)
    }
    /// The destination may contain provisional bytes if verification fails; publish it only after `Ok`.
    pub fn get_to<W: Write>(
        &self,
        id: ObjectId,
        kind: ObjectKind,
        mut output: W,
    ) -> Result<(), StoreError> {
        let path = self.path(id);
        if !is_regular_file(&path).map_err(|source| StoreError::Io {
            path: path.clone(),
            source,
        })? {
            return Err(StoreError::NotRegularFile(path));
        }
        let file = File::open(&path).map_err(|source| StoreError::Io {
            path: path.clone(),
            source,
        })?;
        let mut reader = ObjectReader::new(file, id, kind)?;
        let mut buffer = vec![0; STREAM_BUFFER_SIZE];
        loop {
            let n = reader.read_body(&mut buffer)?;
            if n == 0 {
                break;
            }
            output
                .write_all(&buffer[..n])
                .map_err(|source| StoreError::Io {
                    path: path.clone(),
                    source,
                })?;
        }
        reader.finish()?;
        Ok(())
    }
    /// Returns a bounded decoded prefix without rehashing or verifying the tail.
    pub fn get_prefix(
        &self,
        id: ObjectId,
        kind: ObjectKind,
        limit: usize,
    ) -> Result<Vec<u8>, StoreError> {
        let path = self.path(id);
        if !is_regular_file(&path).map_err(|source| StoreError::Io {
            path: path.clone(),
            source,
        })? {
            return Err(StoreError::NotRegularFile(path));
        }
        let file = File::open(&path).map_err(|source| StoreError::Io { path, source })?;
        Ok(object::read_prefix(file, kind, limit)?)
    }
    pub fn get(&self, id: ObjectId, kind: ObjectKind, limit: u64) -> Result<Vec<u8>, StoreError> {
        let path = self.path(id);
        if !is_regular_file(&path).map_err(|source| StoreError::Io {
            path: path.clone(),
            source,
        })? {
            return Err(StoreError::NotRegularFile(path));
        }
        let file = File::open(&path).map_err(|source| StoreError::Io {
            path: path.clone(),
            source,
        })?;
        let mut reader = ObjectReader::new(file, id, kind)?;
        let mut out = Vec::new();
        let mut buffer =
            vec![0; STREAM_BUFFER_SIZE.min(usize::try_from(limit).unwrap_or(usize::MAX).max(1))];
        loop {
            let count = reader.read_body(&mut buffer)?;
            if count == 0 {
                break;
            }
            if out.len() as u64 + count as u64 > limit {
                return Err(StoreError::Object(ObjectError::LimitExceeded));
            }
            out.extend_from_slice(&buffer[..count]);
        }
        reader.finish()?;
        Ok(out)
    }
    pub fn sync_blobs(&self) -> Result<(), StoreError> {
        crate::fs::sync_directory(&self.root).map_err(|source| StoreError::Io {
            path: self.root.clone(),
            source,
        })
    }

    /// Removes abandoned staging files with exclusive access to the home lock guard.
    /// Call once before a batch of puts.
    pub fn clean_temporary_files(&self, _lock: &mut HomeLock) -> Result<(), StoreError> {
        for entry in fs::read_dir(&self.root).map_err(|source| StoreError::Io {
            path: self.root.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| StoreError::Io {
                path: self.root.clone(),
                source,
            })?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(".input.") || name.starts_with(".object.") {
                fs::remove_file(entry.path()).map_err(|source| StoreError::Io {
                    path: entry.path(),
                    source,
                })?;
            }
        }
        Ok(())
    }
}

fn compression_probe_is_worthwhile(
    file: &mut (impl Read + Seek + ?Sized),
    length: u64,
    path: &Path,
) -> Result<bool, StoreError> {
    if length as usize <= object::COMPRESSION_PROBE_MIN_SIZE {
        return Ok(true);
    }
    let sample_size = object::COMPRESSION_SAMPLE_SIZE.min((length / 3) as usize);
    let starts = [
        0,
        (length - sample_size as u64) / 2,
        length - sample_size as u64,
    ];
    let mut samples = Vec::with_capacity(3);
    for start in starts {
        file.seek(SeekFrom::Start(start))
            .map_err(|source| StoreError::Io {
                path: path.into(),
                source,
            })?;
        let mut sample = vec![0; sample_size];
        file.read_exact(&mut sample)
            .map_err(|source| StoreError::Io {
                path: path.into(),
                source,
            })?;
        samples.push(sample);
    }
    object::compression_samples_are_worthwhile([&samples[0], &samples[1], &samples[2]])
        .map_err(StoreError::Object)
}

#[derive(Debug)]
pub enum StoreError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Object(ObjectError),
    NotRegularFile(PathBuf),
    NewerFormat {
        found: LayoutVersion,
        supported: LayoutVersion,
    },
    OlderFormat {
        found: LayoutVersion,
        supported: LayoutVersion,
    },
    UnsupportedAlgorithm(String),
    SourceChanged,
}
impl From<ObjectError> for StoreError {
    fn from(e: ObjectError) -> Self {
        Self::Object(e)
    }
}
impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "object store I/O at {}: {source}", path.display())
            }
            Self::Object(e) => e.fmt(f),
            Self::NotRegularFile(path) => {
                write!(f, "object blob is not a regular file: {}", path.display())
            }
            Self::NewerFormat { found, supported } => write!(
                f,
                "Folio format {found} is newer than supported format {supported}"
            ),
            Self::OlderFormat { found, supported } => write!(
                f,
                "Folio format {found} is older than supported format {supported}"
            ),
            Self::UnsupportedAlgorithm(a) => write!(f, "unsupported object algorithm: {a}"),
            Self::SourceChanged => write!(f, "object source changed while being stored"),
        }
    }
}
impl Error for StoreError {}
