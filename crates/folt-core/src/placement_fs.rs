use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::folio_dir::{FolioId, content_filename};
use crate::home::Home;
use crate::object::{ObjectHasher, ObjectId, ObjectKind, STREAM_BUFFER_SIZE};
use crate::placement::{
    Observation, Placement, PlacementError, PlacementKind, PlacementOps, Signature, Written,
};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Filesystem implementation of link and copy placement operations.
pub struct PlacementFs<'a> {
    home: &'a Home,
}

impl<'a> PlacementFs<'a> {
    pub fn new(home: &'a Home) -> Self {
        Self { home }
    }

    /// Returns the canonical derived link target spelling used by create and read.
    pub fn link_target(&self, id: FolioId) -> Result<PathBuf, PlacementError> {
        self.content_path(id)
    }

    /// Checks whether the path's filesystem permits creating a symbolic link.
    pub fn probe_link(&self, path: &Path) -> Result<(), PlacementError> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let probe = temporary_path(parent, "folt-link-probe");
        create_symlink(&self.home.path().join("folios"), &probe)
            .map_err(|source| symlink_failure(path, source))?;
        fs::remove_file(&probe).map_err(|source| failure(&probe, source))
    }

    fn content_path(&self, id: FolioId) -> Result<PathBuf, PlacementError> {
        let directory = self.home.path().join("folios").join(id.to_string());
        let metadata = directory.join("folio.toml");
        let text = fs::read_to_string(&metadata).map_err(|source| failure(&metadata, source))?;
        let name = read_original_name(&text).map_err(|source| failure(&metadata, source))?;
        Ok(directory.join(content_filename(std::ffi::OsStr::new(&name))))
    }

    fn copy_content(
        &self,
        id: FolioId,
        placement: &Placement,
        replace: bool,
    ) -> Result<Written, PlacementError> {
        let source = self.content_path(id)?;
        let metadata = fs::metadata(&source).map_err(|error| failure(&source, error))?;
        let length = metadata.len();
        let mut input = File::open(&source).map_err(|e| failure(&source, e))?;
        let parent = placement.path.parent().unwrap_or_else(|| Path::new("."));
        let temporary = temporary_path(parent, "folt-copy");
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| failure(&temporary, e))?;
        let result = stream_copy(&mut input, &mut output, length, &temporary);
        let (hash, read) = match result {
            Ok(value) => value,
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                return Err(error);
            }
        };
        if read != length {
            let _ = fs::remove_file(&temporary);
            return Err(failure(
                &source,
                io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Folio content changed while copying",
                ),
            ));
        }
        output.flush().map_err(|e| failure(&temporary, e))?;
        output.sync_all().map_err(|e| failure(&temporary, e))?;
        drop(output);
        if replace {
            match fs::symlink_metadata(&placement.path) {
                Ok(existing) if existing.is_file() => {
                    fs::set_permissions(&temporary, existing.permissions())
                        .map_err(|error| failure(&temporary, error))?;
                }
                Ok(_) => {
                    let _ = fs::remove_file(&temporary);
                    return Err(failure(
                        &placement.path,
                        io::Error::other("cannot refresh a copy placement over a non-regular file"),
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    let _ = fs::remove_file(&temporary);
                    return Err(failure(&placement.path, error));
                }
            }
        } else if let Err(error) = fs::symlink_metadata(&placement.path) {
            if error.kind() != io::ErrorKind::NotFound {
                let _ = fs::remove_file(&temporary);
                return Err(failure(&placement.path, error));
            }
        } else {
            let _ = fs::remove_file(&temporary);
            return Err(failure(
                &placement.path,
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "placement path already exists",
                ),
            ));
        }
        let publish = if replace {
            fs::rename(&temporary, &placement.path)
        } else {
            match fs::hard_link(&temporary, &placement.path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(error),
                Err(error) if hard_links_unsupported(&error) => {
                    copy_new(&temporary, &placement.path)
                }
                Err(error) => Err(error),
            }
        };
        if let Err(error) = publish {
            let _ = fs::remove_file(&temporary);
            return Err(failure(&placement.path, error));
        }
        let _ = fs::remove_file(&temporary);
        let metadata = fs::symlink_metadata(&placement.path)
            .map_err(|error| failure(&placement.path, error))?;
        Ok(Written {
            hash,
            signature: signature(&metadata),
        })
    }
}

impl PlacementOps for PlacementFs<'_> {
    fn create(&self, id: FolioId, placement: &Placement) -> Result<Written, PlacementError> {
        let target = self.link_target(id)?;
        match placement.kind {
            PlacementKind::Link => {
                let metadata = fs::metadata(&target).map_err(|e| failure(&target, e))?;
                let hash = hash_file(&target)?;
                create_symlink(&target, &placement.path)
                    .map_err(|e| failure(&placement.path, e))?;
                Ok(Written {
                    hash,
                    signature: signature(&metadata),
                })
            }
            PlacementKind::Copy => self.copy_content(id, placement, false),
        }
    }

    fn read(&self, id: FolioId, placement: &Placement) -> Result<Observation, PlacementError> {
        let target = self.link_target(id)?;
        let metadata = match fs::symlink_metadata(&placement.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Observation::Missing);
            }
            Err(error) => return Err(failure(&placement.path, error)),
        };
        if metadata.file_type().is_symlink() {
            return match fs::read_link(&placement.path) {
                Ok(found) if found == target => Ok(Observation::IntactLink),
                Ok(found) => Ok(Observation::OtherLink { target: found }),
                Err(error) => Err(failure(&placement.path, error)),
            };
        }
        if metadata.is_file() {
            return Ok(Observation::File {
                signature: signature(&metadata),
            });
        }
        Ok(Observation::NotAFile)
    }

    fn refresh(&self, id: FolioId, placement: &Placement) -> Result<Written, PlacementError> {
        match placement.kind {
            PlacementKind::Copy => self.copy_content(id, placement, true),
            PlacementKind::Link => {
                let target = self.link_target(id)?;
                let metadata = fs::metadata(&target).map_err(|e| failure(&target, e))?;
                let hash = hash_file(&target)?;
                match fs::symlink_metadata(&placement.path) {
                    Ok(existing) if existing.file_type().is_symlink() => {}
                    Ok(_) => {
                        return Err(failure(
                            &placement.path,
                            io::Error::other("cannot refresh a link placement over a non-symlink"),
                        ));
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(failure(&placement.path, error)),
                }
                let temporary = temporary_path(
                    placement.path.parent().unwrap_or_else(|| Path::new(".")),
                    "folt-link",
                );
                create_symlink(&target, &temporary).map_err(|e| failure(&temporary, e))?;
                #[cfg(windows)]
                if fs::symlink_metadata(&placement.path)
                    .is_ok_and(|metadata| metadata.file_type().is_symlink())
                {
                    fs::remove_file(&placement.path).map_err(|e| failure(&placement.path, e))?;
                }
                fs::rename(&temporary, &placement.path).map_err(|e| failure(&placement.path, e))?;
                Ok(Written {
                    hash,
                    signature: signature(&metadata),
                })
            }
        }
    }

    fn hash_placement(&self, placement: &Placement) -> Result<ObjectId, PlacementError> {
        hash_file(&placement.path)
    }
}

fn stream_copy(
    input: &mut File,
    output: &mut File,
    length: u64,
    path: &Path,
) -> Result<(ObjectId, u64), PlacementError> {
    let mut hasher = ObjectHasher::new(ObjectKind::Blob, length);
    let mut buffer = vec![0; STREAM_BUFFER_SIZE];
    let mut total = 0u64;
    loop {
        let count = input.read(&mut buffer).map_err(|e| failure(path, e))?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| failure(path, io::Error::other("content length overflow")))?;
        output
            .write_all(&buffer[..count])
            .map_err(|e| failure(path, e))?;
        hasher.update(&buffer[..count]);
    }
    Ok((hasher.finish(), total))
}

fn hash_file(path: &Path) -> Result<ObjectId, PlacementError> {
    let metadata = fs::metadata(path).map_err(|e| failure(path, e))?;
    let expected = metadata.len();
    let mut input = File::open(path).map_err(|e| failure(path, e))?;
    let mut hasher = ObjectHasher::new(ObjectKind::Blob, expected);
    let mut buffer = vec![0; STREAM_BUFFER_SIZE];
    let mut total = 0u64;
    loop {
        let count = input.read(&mut buffer).map_err(|e| failure(path, e))?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| failure(path, io::Error::other("content length overflow")))?;
        hasher.update(&buffer[..count]);
    }
    if total != expected {
        return Err(failure(
            path,
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "placement content changed while hashing",
            ),
        ));
    }
    Ok(hasher.finish())
}

fn signature(metadata: &fs::Metadata) -> Signature {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    #[cfg(unix)]
    let (mtime, ctime, inode) = (
        Some(metadata.mtime() as i128 * 1_000_000_000 + metadata.mtime_nsec() as i128),
        Some(metadata.ctime() as i128 * 1_000_000_000 + metadata.ctime_nsec() as i128),
        Some(metadata.ino()),
    );
    #[cfg(not(unix))]
    let (mtime, ctime, inode) = (None, None, None);
    Signature {
        size: metadata.len(),
        mtime,
        ctime,
        inode,
    }
}

fn read_original_name(text: &str) -> io::Result<String> {
    let value = text
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("original_name = "))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "missing original_name in folio.toml",
            )
        })?;
    if !value.starts_with('"') || !value.ends_with('"') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid original_name in folio.toml",
        ));
    }
    let mut result = String::new();
    let mut chars = value[1..value.len() - 1].chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            result.push(match chars.next() {
                Some('\\') => '\\',
                Some('"') => '"',
                Some('n') => '\n',
                Some('r') => '\r',
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid original_name escape",
                    ));
                }
            });
        } else {
            result.push(ch);
        }
    }
    Ok(result)
}

fn temporary_path(parent: &Path, tag: &str) -> PathBuf {
    parent.join(format!(
        ".{tag}-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ))
}

fn copy_new(source: &Path, destination: &Path) -> io::Result<()> {
    let mut input = File::open(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    io::copy(&mut input, &mut output)?;
    output.sync_all()
}

fn hard_links_unsupported(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::Unsupported {
        return true;
    }
    #[cfg(windows)]
    {
        error.raw_os_error() == Some(1314)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn symlink_failure(path: &Path, source: io::Error) -> PlacementError {
    let unavailable = source.kind() == io::ErrorKind::Unsupported
        || cfg!(windows)
            && (source.kind() == io::ErrorKind::PermissionDenied
                || source.raw_os_error() == Some(1314));
    if unavailable {
        PlacementError::LinkUnavailable {
            path: path.to_path_buf(),
            source,
        }
    } else {
        failure(path, source)
    }
}

fn failure(path: &Path, source: io::Error) -> PlacementError {
    PlacementError::OperationFailed {
        path: path.to_path_buf(),
        source,
    }
}
#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}
#[cfg(windows)]
fn create_symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}
