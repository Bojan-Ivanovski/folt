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

    fn relocate_with_rename(
        &self,
        id: FolioId,
        placement: &Placement,
        to: &Path,
        expected: &Observation,
        rename: impl FnOnce(&Path, &Path) -> io::Result<()>,
    ) -> Result<Written, PlacementError> {
        if self.read(id, placement)? != *expected {
            return Err(PlacementError::PlacementChanged);
        }
        require_absent(to)?;
        rename(&placement.path, to).map_err(|error| move_failure(to, error))?;
        Ok(Written {
            base: placement.base.clone(),
        })
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
        Ok(if metadata.is_dir() {
            Observation::Directory
        } else {
            Observation::NotAFile
        })
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
        self.relocate_with_rename(id, placement, to, expected, rename_no_replace)
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    renameat_with(CWD, source, CWD, destination, RenameFlags::NOREPLACE).map_err(io::Error::from)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    match fs::symlink_metadata(destination) {
        Ok(_) => return Err(io::ErrorKind::AlreadyExists.into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::rename(source, destination)
}

fn move_failure(path: &Path, source: io::Error) -> PlacementError {
    if source.kind() == io::ErrorKind::CrossesDevices {
        failure(
            path,
            io::Error::new(
                io::ErrorKind::CrossesDevices,
                "moves across filesystems are not supported",
            ),
        )
    } else {
        failure(path, source)
    }
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

    struct TestHome {
        home: Home,
        root: PathBuf,
    }

    impl TestHome {
        fn new() -> Self {
            let root = temporary_path(&std::env::temp_dir(), "folt-rename-test");
            let home = Home::open_or_create(root.join("home")).unwrap();
            let root = fs::canonicalize(root).unwrap();
            Self { home, root }
        }

        fn source(&self, directory: bool) -> Placement {
            let path = self.root.join("source");
            if directory {
                fs::create_dir(&path).unwrap();
            } else {
                fs::write(&path, b"source").unwrap();
            }
            Placement {
                path,
                kind: PlacementKind::Copy,
                head: "main".into(),
                commit_id: None,
                base: None,
            }
        }
    }

    impl Drop for TestHome {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn no_clobber_rename_refuses_destinations_created_after_absence_check() {
        for directory in [false, true] {
            for target_directory in [false, true] {
                let home = TestHome::new();
                let placement = home.source(directory);
                let fs = PlacementFs::new(&home.home);
                let id = FolioId::from_bytes([38; 16]);
                let observation = fs.read(id, &placement).unwrap();
                let to = home.root.join("destination");
                let result =
                    fs.relocate_with_rename(id, &placement, &to, &observation, |from, to| {
                        if target_directory {
                            std::fs::create_dir(to).unwrap();
                        } else {
                            std::fs::write(to, b"foreign file").unwrap();
                        }
                        rename_no_replace(from, to)
                    });
                assert!(result.is_err());
                assert_eq!(placement.path.is_dir(), directory);
                assert_eq!(to.is_dir(), target_directory);
                if !directory {
                    assert_eq!(std::fs::read(&placement.path).unwrap(), b"source");
                }
                if !target_directory {
                    assert_eq!(std::fs::read(to).unwrap(), b"foreign file");
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn no_clobber_rename_refuses_a_link_created_after_absence_check() {
        let home = TestHome::new();
        let placement = home.source(false);
        let fs = PlacementFs::new(&home.home);
        let id = FolioId::from_bytes([38; 16]);
        let observation = fs.read(id, &placement).unwrap();
        let to = home.root.join("destination");
        let result = fs.relocate_with_rename(id, &placement, &to, &observation, |from, to| {
            create_symlink(Path::new("foreign target"), to).unwrap();
            rename_no_replace(from, to)
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&placement.path).unwrap(), b"source");
        assert_eq!(std::fs::read_link(to).unwrap(), Path::new("foreign target"));
    }

    #[test]
    fn cross_device_and_permission_errors_are_reported_without_moving() {
        for directory in [false, true] {
            for kind in [
                io::ErrorKind::CrossesDevices,
                io::ErrorKind::PermissionDenied,
            ] {
                let home = TestHome::new();
                let placement = home.source(directory);
                let fs = PlacementFs::new(&home.home);
                let id = FolioId::from_bytes([38; 16]);
                let observation = fs.read(id, &placement).unwrap();
                let to = home.root.join("destination");
                let error =
                    fs.relocate_with_rename(id, &placement, &to, &observation, |_, _| {
                        Err(kind.into())
                    })
                    .unwrap_err();
                if kind == io::ErrorKind::CrossesDevices {
                    assert!(
                        error
                            .to_string()
                            .contains("moves across filesystems are not supported")
                    );
                }
                let PlacementError::OperationFailed { source, .. } = error else {
                    panic!("wrong error")
                };
                assert_eq!(source.kind(), kind);
                assert!(placement.path.exists());
                assert!(!to.exists());
            }
        }
        #[cfg(unix)]
        {
            let error = move_failure(
                Path::new("destination"),
                io::Error::from_raw_os_error(rustix::io::Errno::XDEV.raw_os_error()),
            );
            assert!(
                error
                    .to_string()
                    .contains("moves across filesystems are not supported")
            );
        }
    }
}
