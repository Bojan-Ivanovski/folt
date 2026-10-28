use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::folio_dir::FolioId;
use crate::home::Home;
use crate::layout::CONTENT_FILE;
use crate::object::{ObjectHasher, ObjectId, ObjectKind, STREAM_BUFFER_SIZE};
use crate::placement::{
    Observation, Placement, PlacementBase, PlacementError, PlacementKind, PlacementOps, Signature,
    Written,
};
use crate::store::ObjectStore;

/// Returns the working file path for a Folio's internal placement.
pub fn internal_path(home: &Home, id: FolioId) -> PathBuf {
    home.path()
        .join("folios")
        .join(id.to_string())
        .join(CONTENT_FILE)
}

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
    pub fn link_target(&self, id: FolioId) -> PathBuf {
        internal_path(self.home, id)
    }

    /// Checks whether the path's filesystem permits creating a symbolic link.
    /// Runs only when a link is explicitly requested; a copy never calls it.
    pub fn probe_link(&self, path: &Path) -> Result<(), PlacementError> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let probe = temporary_path(parent, "folt-link-probe");
        create_symlink(&self.home.path().join("folios"), &probe)
            .map_err(|source| symlink_failure(path, source))?;
        fs::remove_file(&probe).map_err(|source| failure(&probe, source))
    }

    fn relocation_receipt(&self, placement: &Placement, to: &Path) -> PathBuf {
        use crate::object::hash_blob;
        self.home.path().join("relocations").join(format!(
            "{}-{}",
            hash_blob(placement.path.as_os_str().as_encoded_bytes()),
            hash_blob(to.as_os_str().as_encoded_bytes()),
        ))
    }

    fn relocation_evidence(
        &self,
        id: FolioId,
        placement: &Placement,
        to: &Path,
    ) -> Result<String, PlacementError> {
        let mut destination = placement.clone();
        destination.path = to.to_path_buf();
        let observation = self.read(id, &destination)?;
        let hash = if matches!(observation, Observation::File { .. }) {
            Some(hash_file(to)?)
        } else {
            None
        };
        Ok(format!("{id}\n{placement:?}\n{observation:?}\n{hash:?}\n"))
    }

    fn record_relocation(
        &self,
        id: FolioId,
        placement: &Placement,
        to: &Path,
    ) -> Result<(), PlacementError> {
        let receipt = self.relocation_receipt(placement, to);
        let parent = receipt.parent().expect("receipt has a parent");
        fs::create_dir_all(parent).map_err(|error| failure(parent, error))?;
        let temporary = temporary_path(parent, "folt-relocation");
        let result = (|| {
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|error| failure(&temporary, error))?;
            output
                .write_all(self.relocation_evidence(id, placement, to)?.as_bytes())
                .map_err(|error| failure(&temporary, error))?;
            output
                .sync_all()
                .map_err(|error| failure(&temporary, error))?;
            drop(output);
            fs::rename(&temporary, &receipt).map_err(|error| failure(&receipt, error))
        })();
        let _ = fs::remove_file(temporary);
        result
    }

    fn relocate_across_devices(
        &self,
        id: FolioId,
        placement: &Placement,
        to: &Path,
        expected: &Observation,
    ) -> Result<Written, PlacementError> {
        let prepared = match expected {
            Observation::IntactLink | Observation::OtherLink { .. } => {
                let target = fs::read_link(&placement.path)
                    .map_err(|error| failure(&placement.path, error))?;
                let target = if target.is_relative() {
                    resolved_path(
                        &placement
                            .path
                            .parent()
                            .unwrap_or(Path::new("."))
                            .join(target),
                    )?
                } else {
                    target
                };
                create_symlink(&target, to).map_err(|error| symlink_failure(to, error))?;
                Ok(())
            }
            Observation::File { .. } => {
                let hash = hash_file(&placement.path)?;
                // create_new reserves the destination before any bytes are copied.
                let mut output = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(to)
                    .map_err(|error| failure(to, error))?;
                let result = (|| {
                    let mut input = File::open(&placement.path)?;
                    io::copy(&mut input, &mut output)?;
                    output.set_permissions(input.metadata()?.permissions())?;
                    output.sync_all()
                })()
                .map_err(|error| failure(to, error));
                drop(output);
                result.and_then(|()| {
                    if hash_file(to)? != hash || hash_file(&placement.path)? != hash {
                        return Err(PlacementError::PlacementChanged);
                    }
                    Ok(())
                })
            }
            _ => return Err(PlacementError::PlacementChanged),
        };
        if let Err(error) = prepared.and_then(|()| {
            if self.read(id, placement)? != *expected {
                return Err(PlacementError::PlacementChanged);
            }
            Ok(())
        }) {
            let _ = fs::remove_file(to);
            return Err(error);
        }
        // Persist the destination identity and content before removing the source.
        // A fresh process can then finish the filesystem-first registry update,
        // including when edited content has no matching base or stage hash.
        if let Err(error) = self.record_relocation(id, placement, to) {
            let _ = fs::remove_file(to);
            return Err(error);
        }
        fs::remove_file(&placement.path).map_err(|error| failure(&placement.path, error))?;
        relocated_base(placement, to)
    }

    fn relocate_with_publish(
        &self,
        id: FolioId,
        placement: &Placement,
        to: &Path,
        expected: &Observation,
        hard_link: impl FnOnce(&Path, &Path) -> io::Result<()>,
        reserve_directory: fn(&Path) -> io::Result<()>,
    ) -> Result<Written, PlacementError> {
        if self.read(id, placement)? != *expected {
            return Err(PlacementError::PlacementChanged);
        }
        require_absent(to)?;
        let metadata = fs::symlink_metadata(&placement.path)
            .map_err(|error| failure(&placement.path, error))?;
        if metadata.file_type().is_symlink() {
            // Recreate links with their resolved target so relative links keep
            // pointing at the same file from the destination's directory.
            return self.relocate_across_devices(id, placement, to, expected);
        }
        let moved = if metadata.is_dir() {
            relocate_directory(&placement.path, to, reserve_directory)
        } else {
            match hard_link(&placement.path, to) {
                Ok(()) => fs::remove_file(&placement.path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(error),
                // create_new in the copy fallback reserves the destination
                // atomically. An absence check followed by rename can clobber.
                Err(_) => return self.relocate_across_devices(id, placement, to, expected),
            }
        };
        moved.map_err(|error| failure(to, error))?;
        relocated_base(placement, to)
    }

    fn copy_content(
        &self,
        id: FolioId,
        placement: &Placement,
        objects: &ObjectStore,
        content: ObjectId,
        replace: bool,
        expected: Option<&Observation>,
    ) -> Result<Written, PlacementError> {
        let parent = placement.path.parent().unwrap_or_else(|| Path::new("."));
        let temporary = temporary_path(parent, "folt-copy");
        write_copy(&temporary, objects, content)?;
        // Defer destination errors so a changed observation is reported as such.
        let prepare = if replace {
            match fs::symlink_metadata(&placement.path) {
                Ok(existing) if existing.is_file() => {
                    fs::set_permissions(&temporary, existing.permissions())
                        .map_err(|error| failure(&temporary, error))
                }
                Ok(_) => Err(failure(
                    &placement.path,
                    io::Error::other("cannot refresh a copy placement over a non-regular file"),
                )),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(failure(&placement.path, error)),
            }
        } else {
            Ok(())
        };
        if !replace {
            if let Err(error) = fs::symlink_metadata(&placement.path) {
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
        }
        let publish = if replace {
            if let Some(expected) = expected {
                match self.read(id, placement) {
                    Ok(observation) if &observation == expected => {}
                    Ok(_) => {
                        let _ = fs::remove_file(&temporary);
                        return Err(PlacementError::PlacementChanged);
                    }
                    Err(error) => {
                        let _ = fs::remove_file(&temporary);
                        return Err(error);
                    }
                }
            }
            if let Err(error) = prepare {
                let _ = fs::remove_file(&temporary);
                return Err(error);
            }
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
            base: Some(PlacementBase {
                hash: content,
                signature: signature(&metadata),
            }),
        })
    }
}

impl PlacementOps for PlacementFs<'_> {
    fn create(
        &self,
        id: FolioId,
        placement: &Placement,
        objects: &ObjectStore,
        content: ObjectId,
    ) -> Result<Written, PlacementError> {
        let target = self.link_target(id);
        match placement.kind {
            PlacementKind::Link => {
                fs::metadata(&target).map_err(|e| failure(&target, e))?;
                create_symlink(&target, &placement.path)
                    .map_err(|e| failure(&placement.path, e))?;
                Ok(Written { base: None })
            }
            PlacementKind::Copy | PlacementKind::Internal => {
                self.copy_content(id, placement, objects, content, false, None)
            }
        }
    }

    fn read(&self, id: FolioId, placement: &Placement) -> Result<Observation, PlacementError> {
        let target = self.link_target(id);
        let metadata = match fs::symlink_metadata(&placement.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Observation::Missing);
            }
            Err(error) => return Err(failure(&placement.path, error)),
        };
        if metadata.file_type().is_symlink() {
            return match fs::read_link(&placement.path) {
                Ok(found)
                    if resolved_path(
                        &placement
                            .path
                            .parent()
                            .unwrap_or(Path::new("."))
                            .join(&found),
                    )? == resolved_path(&target)? =>
                {
                    Ok(Observation::IntactLink)
                }
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

    fn refresh(
        &self,
        id: FolioId,
        placement: &Placement,
        objects: &ObjectStore,
        content: ObjectId,
        expected: Option<&Observation>,
    ) -> Result<Written, PlacementError> {
        match placement.kind {
            PlacementKind::Copy | PlacementKind::Internal => {
                self.copy_content(id, placement, objects, content, true, expected)
            }
            PlacementKind::Link => {
                let target = self.link_target(id);
                fs::metadata(&target).map_err(|e| failure(&target, e))?;
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
                if let Some(expected) = expected
                    && self.read(id, placement)? != *expected
                {
                    let _ = fs::remove_file(&temporary);
                    return Err(PlacementError::PlacementChanged);
                }
                fs::rename(&temporary, &placement.path).map_err(|e| failure(&placement.path, e))?;
                Ok(Written { base: None })
            }
        }
    }

    fn convert(
        &self,
        id: FolioId,
        placement: &Placement,
        to_kind: PlacementKind,
        objects: &ObjectStore,
        content: ObjectId,
        expected: &Observation,
    ) -> Result<Written, PlacementError> {
        let parent = placement.path.parent().unwrap_or_else(|| Path::new("."));
        let temporary = temporary_path(parent, "folt-convert");
        let mut prepared = false;
        let result = (|| {
            match (placement.kind, to_kind, expected) {
                (PlacementKind::Link, PlacementKind::Copy, Observation::IntactLink) => {
                    write_copy(&temporary, objects, content)?;
                    prepared = true;
                    let output = OpenOptions::new()
                        .write(true)
                        .open(&temporary)
                        .map_err(|error| failure(&temporary, error))?;
                    let metadata = fs::metadata(&placement.path)
                        .map_err(|error| failure(&placement.path, error))?;
                    output
                        .set_permissions(metadata.permissions())
                        .map_err(|error| failure(&temporary, error))?;
                    output
                        .sync_all()
                        .map_err(|error| failure(&temporary, error))?;
                }
                (PlacementKind::Copy, PlacementKind::Link, Observation::File { .. }) => {
                    let target = self.link_target(id);
                    fs::metadata(&target).map_err(|error| failure(&target, error))?;
                    create_symlink(&target, &temporary)
                        .map_err(|error| symlink_failure(&placement.path, error))?;
                    prepared = true;
                }
                _ => {
                    return Err(failure(
                        &placement.path,
                        io::Error::other("invalid conversion"),
                    ));
                }
            }
            if self.read(id, placement)? != *expected {
                return Err(PlacementError::PlacementChanged);
            }
            // Rename replaces the entry atomically (MoveFileExW with
            // MOVEFILE_REPLACE_EXISTING on Windows). Never unlink first: a
            // failed replacement must leave the original entry available for retry.
            fs::rename(&temporary, &placement.path)
                .map_err(|error| failure(&placement.path, error))?;
            let base = if to_kind == PlacementKind::Copy {
                let metadata = fs::symlink_metadata(&placement.path)
                    .map_err(|error| failure(&placement.path, error))?;
                Some(PlacementBase {
                    hash: content,
                    signature: signature(&metadata),
                })
            } else {
                None
            };
            Ok(Written { base })
        })();
        if prepared {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    fn relocate(
        &self,
        id: FolioId,
        placement: &Placement,
        to: &Path,
        expected: &Observation,
    ) -> Result<Written, PlacementError> {
        self.relocate_with_publish(
            id,
            placement,
            to,
            expected,
            |source, destination| fs::hard_link(source, destination),
            |path| fs::create_dir(path),
        )
    }

    fn relocation_completed(
        &self,
        id: FolioId,
        placement: &Placement,
        to: &Path,
    ) -> Result<bool, PlacementError> {
        let receipt = self.relocation_receipt(placement, to);
        let evidence = match fs::read_to_string(&receipt) {
            Ok(evidence) => evidence,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(failure(&receipt, error)),
        };
        if !matches!(
            self.read(
                id,
                &Placement {
                    path: to.to_path_buf(),
                    ..placement.clone()
                }
            )?,
            Observation::File { .. } | Observation::IntactLink
        ) {
            return Ok(false);
        }
        Ok(evidence == self.relocation_evidence(id, placement, to)?)
    }

    fn forget_relocation(&self, placement: &Placement, to: &Path) {
        let _ = fs::remove_file(self.relocation_receipt(placement, to));
    }

    fn unlink(
        &self,
        id: FolioId,
        placement: &Placement,
        expected: &Observation,
    ) -> Result<(), PlacementError> {
        if self.read(id, placement)? != *expected {
            return Err(PlacementError::PlacementChanged);
        }
        match expected {
            Observation::IntactLink | Observation::OtherLink { .. } | Observation::File { .. } => {
                fs::remove_file(&placement.path).map_err(|error| failure(&placement.path, error))
            }
            _ => Err(PlacementError::PlacementChanged),
        }
    }

    fn hash_placement(&self, placement: &Placement) -> Result<ObjectId, PlacementError> {
        hash_file(&placement.path)
    }
}

/// Resolve existing ancestors even when a former home has been moved or removed.
pub(crate) fn resolved_path(path: &Path) -> Result<PathBuf, PlacementError> {
    let mut ancestor = path;
    let mut missing = Vec::new();
    loop {
        match fs::canonicalize(ancestor) {
            Ok(mut resolved) => {
                for name in missing.iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let Some(name) = ancestor.file_name() else {
                    return Err(failure(path, error));
                };
                missing.push(name);
                ancestor = ancestor.parent().ok_or_else(|| failure(path, error))?;
            }
            Err(error) => return Err(failure(path, error)),
        }
    }
}

// Reserve every directory with create_dir: rename can replace an empty directory
// that appears after require_absent, even on the same filesystem.
fn relocate_directory(
    source: &Path,
    destination: &Path,
    reserve: fn(&Path) -> io::Result<()>,
) -> io::Result<()> {
    reserve(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&from)?;
        if metadata.is_dir() {
            relocate_directory(&from, &to, |path| fs::create_dir(path))?;
        } else {
            if metadata.file_type().is_symlink() {
                create_symlink(&fs::read_link(&from)?, &to)?;
            } else {
                match fs::hard_link(&from, &to) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        return Err(error);
                    }
                    Err(_) => copy_new(&from, &to)?,
                }
            }
            fs::remove_file(&from)?;
        }
    }
    fs::set_permissions(destination, fs::metadata(source)?.permissions())?;
    fs::remove_dir(source)
}

fn require_absent(path: &Path) -> Result<(), PlacementError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(failure(
            path,
            io::Error::new(io::ErrorKind::AlreadyExists, "destination exists"),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(failure(path, error)),
    }
}

fn relocated_base(placement: &Placement, to: &Path) -> Result<Written, PlacementError> {
    let mut base = placement.base.clone();
    if let Some(base) = &mut base {
        let metadata = fs::symlink_metadata(to).map_err(|error| failure(to, error))?;
        base.signature = signature(&metadata);
        // Size belongs to the base content, even when the moved file has edits.
        base.signature.size = placement.base.as_ref().expect("base exists").signature.size;
    }
    Ok(Written { base })
}

fn write_copy(path: &Path, objects: &ObjectStore, content: ObjectId) -> Result<(), PlacementError> {
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| failure(path, error))?;
    let result = (|| {
        objects
            .get_to(content, ObjectKind::Blob, &mut output)
            .map_err(PlacementError::Store)?;
        output.flush().map_err(|error| failure(path, error))?;
        output.sync_all().map_err(|error| failure(path, error))
    })();
    drop(output);
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
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
    #[cfg(windows)]
    let (mtime, ctime, inode) = {
        use std::os::windows::fs::MetadataExt;
        // FILETIME uses 100 ns ticks since 1601; registry timestamps use Unix ns.
        let unix_ns = |ticks| (i128::from(ticks) - 116_444_736_000_000_000) * 100;
        (
            Some(unix_ns(metadata.last_write_time())),
            Some(unix_ns(metadata.creation_time())),
            None,
        )
    };
    Signature {
        size: metadata.len(),
        mtime,
        ctime,
        inode,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::hash_blob;
    use crate::placement_rules::{self, PlacementFacts};
    use crate::registry::{self, PlacementPath, Registry};

    struct TestHome {
        home: Home,
        root: PathBuf,
    }

    impl TestHome {
        fn new() -> Self {
            let root = temporary_path(&std::env::temp_dir(), "folt-relocation-test");
            let home = Home::open_or_create(root.join("home")).unwrap();
            Self { home, root }
        }

        fn copy(&self) -> (FolioId, Placement, Observation) {
            let id = FolioId::from_bytes([38; 16]);
            let mut placement = Placement {
                path: fs::canonicalize(&self.root).unwrap().join("source"),
                kind: PlacementKind::Copy,
                head: "main".into(),
                commit_id: None,
                base: None,
            };
            fs::write(&placement.path, b"base").unwrap();
            let ops = PlacementFs::new(&self.home);
            let Observation::File { signature } = ops.read(id, &placement).unwrap() else {
                panic!("missing source")
            };
            placement.base = Some(PlacementBase {
                hash: hash_blob(b"base"),
                signature,
            });
            fs::write(&placement.path, b"edited content without a stage").unwrap();
            let observation = ops.read(id, &placement).unwrap();
            (id, placement, observation)
        }
    }

    impl Drop for TestHome {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn unsupported_hard_link_fallback_never_overwrites_a_racing_destination() {
        let home = TestHome::new();
        let (id, placement, observation) = home.copy();
        let destination = home.root.join("destination");
        let result = PlacementFs::new(&home.home).relocate_with_publish(
            id,
            &placement,
            &destination,
            &observation,
            |_, destination| {
                // The destination appears after the initial absence check.
                fs::write(destination, b"other process").unwrap();
                Err(io::Error::from(io::ErrorKind::Unsupported))
            },
            |path| fs::create_dir(path),
        );
        assert!(result.is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"other process");
        assert_eq!(
            fs::read(&placement.path).unwrap(),
            b"edited content without a stage"
        );
    }

    #[test]
    fn directory_relocation_never_overwrites_a_racing_empty_directory() {
        let home = TestHome::new();
        let source = home.root.join("source-directory");
        let destination = home.root.join("destination-directory");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("unmanaged"), b"keep me").unwrap();
        let placement = Placement {
            path: source.clone(),
            kind: PlacementKind::Copy,
            head: "main".into(),
            commit_id: None,
            base: None,
        };
        let result = PlacementFs::new(&home.home).relocate_with_publish(
            FolioId::from_bytes([38; 16]),
            &placement,
            &destination,
            &Observation::NotAFile,
            |_, _| panic!("directories cannot be hard linked"),
            |path| {
                // Another process creates an empty directory after the absence check.
                fs::create_dir(path).unwrap();
                fs::create_dir(path)
            },
        );
        assert!(result.is_err());
        assert!(destination.is_dir());
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
        assert_eq!(fs::read(source.join("unmanaged")).unwrap(), b"keep me");
    }

    #[test]
    fn copied_relocation_resumes_with_fresh_facts_after_restart() {
        for (source_present, changed) in
            [(false, false), (false, true), (true, false), (true, true)]
        {
            let home = TestHome::new();
            let (id, placement, observation) = home.copy();
            let to = PlacementPath::resolve(home.root.join("destination")).unwrap();
            let lock = home.home.try_lock().unwrap();
            let mut registry = Registry::open_or_create(&home.home, &lock).unwrap();
            let row = registry::Placement {
                path: PlacementPath::resolve(&placement.path).unwrap(),
                id,
                kind: placement.kind,
                head: placement.head.clone(),
                commit_id: None,
                base: placement.base.clone(),
                staged: None,
            };
            registry
                .write(&lock, |tx| {
                    tx.add_folio(&registry::NewFolio {
                        id,
                        format: registry::FolioFormat {
                            layout: crate::layout::FOLIO_LAYOUT_VERSION,
                            algorithm: crate::layout::OBJECT_ALGORITHM.into(),
                        },
                    })?;
                    tx.add_placement(&registry::NewPlacement {
                        path: row.path.clone(),
                        id,
                        kind: row.kind,
                        head: Some(row.head.clone()),
                        commit_id: None,
                        base: row.base.clone(),
                        staged: None,
                    })
                })
                .unwrap();
            {
                let ops = PlacementFs::new(&home.home);
                if source_present {
                    fs::copy(&placement.path, to.to_path_buf()).unwrap();
                    ops.record_relocation(id, &placement, &to.to_path_buf())
                        .unwrap();
                } else {
                    ops.relocate_with_publish(
                        id,
                        &placement,
                        &to.to_path_buf(),
                        &observation,
                        |_, _| Err(io::Error::from(io::ErrorKind::CrossesDevices)),
                        |path| fs::create_dir(path),
                    )
                    .unwrap();
                }
            }
            assert_eq!(placement.path.exists(), source_present);
            if changed {
                fs::write(to.to_path_buf(), b"replaced destination").unwrap();
            }
            // Reopen every handle and reconstruct facts, retaining no original plan.
            drop(registry);
            drop(lock);
            let reopened = Home::open_or_create(home.home.path().to_path_buf()).unwrap();
            let lock = reopened.try_lock().unwrap();
            let mut registry = Registry::open_or_create(&reopened, &lock).unwrap();
            let row = registry.placement_at(&row.path).unwrap().unwrap();
            let ops = PlacementFs::new(&reopened);
            let source = PlacementFacts {
                placement: row.clone(),
                observation: ops.read(id, &row.to_placement().1).unwrap(),
                placement_hash: None,
                tip: None,
            };
            let target =
                placement_rules::gather_relocate_target(&row, &to, &ops, &registry).unwrap();
            let plan = placement_rules::judge_relocate(&source, &target);
            if changed {
                assert!(plan.is_err());
                assert!(registry.placement_at(&row.path).unwrap().is_some());
                assert_eq!(placement.path.exists(), source_present);
            } else {
                placement_rules::apply_relocate(&plan.unwrap(), &ops, &mut registry, &lock)
                    .unwrap();
                assert!(registry.placement_at(&row.path).unwrap().is_none());
                assert!(!placement.path.exists());
                let moved = registry.placement_at(&to).unwrap().unwrap();
                assert_eq!(moved.base.as_ref().unwrap().hash, hash_blob(b"base"));
                assert_eq!(
                    fs::read(to.to_path_buf()).unwrap(),
                    b"edited content without a stage"
                );
                assert!(
                    !ops.relocation_receipt(&placement, &to.to_path_buf())
                        .exists()
                );
            }
        }
    }
}
