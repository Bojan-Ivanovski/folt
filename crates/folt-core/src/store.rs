use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::folio_dir::FolioDir;
use crate::home::HomeLock;
use crate::object::{
    self, ObjectError, ObjectHasher, ObjectId, ObjectKind, ObjectReader, ObjectWriter,
    STREAM_BUFFER_SIZE,
};

const SUPPORTED_LAYOUT: u64 = 1;
const OBJECT_ALGORITHM: &str = "sha256";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FolioFormat {
    pub layout: u64,
    pub algorithm: String,
}

/// Immutable objects stored beneath one Folio's `blobs` directory.
pub struct ObjectStore {
    root: PathBuf,
    format: FolioFormat,
}

impl ObjectStore {
    pub fn open(folio: &FolioDir) -> Result<Self, StoreError> {
        let format = parse_format(&folio.path().join("record"))?;
        if format.layout > SUPPORTED_LAYOUT {
            return Err(StoreError::NewerFormat {
                found: format.layout,
                supported: SUPPORTED_LAYOUT,
            });
        }
        if format.algorithm != OBJECT_ALGORITHM {
            return Err(StoreError::UnsupportedAlgorithm(format.algorithm.clone()));
        }
        Ok(Self {
            root: folio.path().join("blobs"),
            format,
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
    pub fn put(
        &self,
        lock: &HomeLock,
        kind: ObjectKind,
        body: &[u8],
    ) -> Result<ObjectId, StoreError> {
        self.put_reader(lock, kind, body.len() as u64, Cursor::new(body))
    }
    pub fn put_file(
        &self,
        lock: &HomeLock,
        kind: ObjectKind,
        path: &Path,
    ) -> Result<ObjectId, StoreError> {
        let mut file = File::open(path).map_err(|source| StoreError::Io {
            path: path.into(),
            source,
        })?;
        let len = file
            .metadata()
            .map_err(|source| StoreError::Io {
                path: path.into(),
                source,
            })?
            .len();
        self.put_reader(lock, kind, len, &mut file)
    }
    /// Streams input through a disk staging file so it can be hashed and encoded in two passes.
    pub fn put_reader<R: Read>(
        &self,
        _lock: &HomeLock,
        kind: ObjectKind,
        length: u64,
        mut reader: R,
    ) -> Result<ObjectId, StoreError> {
        self.clean_temporary_files()?;
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
            let mut hash = ObjectHasher::new(kind, length);
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
                hash.update(&buffer[..n]);
                file.write_all(&buffer[..n])
                    .map_err(|source| StoreError::Io {
                        path: spool.clone(),
                        source,
                    })?;
            }
            if count != length {
                return Err(StoreError::SourceChanged);
            }
            file.sync_all().map_err(|source| StoreError::Io {
                path: spool.clone(),
                source,
            })?;
            drop(file);
            let id = hash.finish();
            let dest = self.path(id);
            if dest.exists() {
                return Ok(id);
            }
            let mut input = File::open(&spool).map_err(|source| StoreError::Io {
                path: spool.clone(),
                source,
            })?;
            let mut head = [0; 8];
            let n = input.read(&mut head).map_err(|source| StoreError::Io {
                path: spool.clone(),
                source,
            })?;
            let compressed = !object::already_compressed(&head[..n])
                && compression_probe_is_worthwhile(&mut input, length, &spool)?;
            input
                .seek(SeekFrom::Start(0))
                .map_err(|source| StoreError::Io {
                    path: spool.clone(),
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
            if dest.exists() {
                fs::remove_file(&temp).map_err(|source| StoreError::Io { path: temp, source })?;
            } else {
                fs::rename(&temp, &dest).map_err(|source| {
                    let _ = fs::remove_file(&temp);
                    StoreError::Io {
                        path: dest.clone(),
                        source,
                    }
                })?;
            }
            Ok(id)
        })();
        let _ = fs::remove_file(&spool);
        result
    }
    /// The destination may contain provisional bytes if verification fails; publish it only after `Ok`.
    pub fn get_to<W: Write>(
        &self,
        id: ObjectId,
        kind: ObjectKind,
        mut output: W,
    ) -> Result<(), StoreError> {
        let path = self.path(id);
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
    pub fn get(&self, id: ObjectId, kind: ObjectKind, limit: u64) -> Result<Vec<u8>, StoreError> {
        let path = self.path(id);
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
        File::open(&self.root)
            .and_then(|file| file.sync_all())
            .map_err(|source| StoreError::Io {
                path: self.root.clone(),
                source,
            })
    }

    fn clean_temporary_files(&self) -> Result<(), StoreError> {
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
    file: &mut File,
    length: u64,
    path: &Path,
) -> Result<bool, StoreError> {
    const MIN_SIZE: u64 = 4 * 1024;
    const SAMPLE_SIZE: usize = 64 * 1024;
    const MIN_SAVING_PERCENT: usize = 10;
    if length <= MIN_SIZE {
        return Ok(true);
    }
    let sample_size = SAMPLE_SIZE.min((length / 3) as usize);
    let starts = [
        0,
        (length - sample_size as u64) / 2,
        length - sample_size as u64,
    ];
    let mut sample_bytes = 0usize;
    let mut compressed_bytes = 0usize;
    let mut sample = vec![0; sample_size];
    for start in starts {
        file.seek(SeekFrom::Start(start))
            .map_err(|source| StoreError::Io {
                path: path.into(),
                source,
            })?;
        file.read_exact(&mut sample)
            .map_err(|source| StoreError::Io {
                path: path.into(),
                source,
            })?;
        let compressed = zstd::encode_all(&sample[..], 1).map_err(ObjectError::Io)?;
        sample_bytes += sample.len();
        compressed_bytes += compressed.len();
    }
    Ok(compressed_bytes * 100 < sample_bytes * (100 - MIN_SAVING_PERCENT))
}

pub fn parse_format(record: &Path) -> Result<FolioFormat, StoreError> {
    let mut text = String::new();
    File::open(record)
        .and_then(|mut f| f.read_to_string(&mut text))
        .map_err(|source| StoreError::Io {
            path: record.into(),
            source,
        })?;
    let mut lines = text.lines();
    let layout = lines
        .next()
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| StoreError::InvalidFormat(record.into()))?;
    let algorithm = lines
        .next()
        .ok_or_else(|| StoreError::InvalidFormat(record.into()))?;
    if algorithm.is_empty() {
        return Err(StoreError::InvalidFormat(record.into()));
    }
    Ok(FolioFormat {
        layout,
        algorithm: algorithm.to_owned(),
    })
}

#[derive(Debug)]
pub enum StoreError {
    Io { path: PathBuf, source: io::Error },
    Object(ObjectError),
    InvalidFormat(PathBuf),
    NewerFormat { found: u64, supported: u64 },
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
            Self::InvalidFormat(p) => write!(f, "invalid Folio record header: {}", p.display()),
            Self::NewerFormat { found, supported } => write!(
                f,
                "Folio format {found} is newer than supported format {supported}"
            ),
            Self::UnsupportedAlgorithm(a) => write!(f, "unsupported object algorithm: {a}"),
            Self::SourceChanged => write!(f, "object source changed while being stored"),
        }
    }
}
impl Error for StoreError {}
