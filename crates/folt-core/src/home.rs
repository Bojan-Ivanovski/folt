use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::{self, Read};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::layout::{HOME_FORMAT_VERSION, LayoutSupport, LayoutVersion};
use crate::registry::{Registry, RegistryError};

const APPLICATION_ID: u32 = 0x464f_4c54;
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";

/// A resolved Folt home directory.
///
/// The home and every Folio directory are trusted: whoever can change them can
/// change everything Folt writes. A process running as the same user is not
/// defended against. On Unix, the home is created private, a home writable by
/// group or others is refused, and a home readable by them produces a warning.
/// On other platforms this trust boundary is documentation only.
pub struct Home {
    path: PathBuf,
    warnings: Vec<HomeWarning>,
}

impl Home {
    /// Resolves the home path using the process environment, without touching
    /// the filesystem.
    ///
    /// The platform home directory may change behavior on Windows, and redirected
    /// Windows profiles are not handled.
    pub fn locate() -> Result<PathBuf, HomeError> {
        let folt_home = std::env::var_os("FOLT_HOME");
        let platform_home = std::env::home_dir();
        resolve_home(folt_home, platform_home)
    }

    /// Opens or creates a home directory and validates its contents and format.
    ///
    /// Concurrent first runs are not serialised until the home lock exists.
    pub fn open_or_create(path: PathBuf) -> Result<Self, HomeError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| HomeError::CannotCreate {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        match private_directory_builder().create(&path) {
            Ok(()) => {}
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => {}
            Err(source) => {
                return Err(HomeError::CannotCreate {
                    path: path.clone(),
                    source,
                });
            }
        }

        recognize_home(&path)?;
        let home = Self {
            warnings: home_warnings(&path)?,
            path: path.clone(),
        };
        let registry = Registry::open_existing(&home)?;
        if let Some(text) = registry.format_version()? {
            let version =
                LayoutVersion::from_str(&text).map_err(|_| HomeError::InvalidFormatVersion {
                    path: path.join("registry.db"),
                    found: text,
                })?;
            match version.support_as_home_format() {
                LayoutSupport::Supported => {}
                LayoutSupport::Newer => {
                    return Err(HomeError::NewerFormatVersion {
                        found: version,
                        supported: HOME_FORMAT_VERSION,
                    });
                }
                LayoutSupport::Older => {
                    return Err(HomeError::OlderFormatVersion {
                        found: version,
                        supported: HOME_FORMAT_VERSION,
                    });
                }
            }
        } else if path.join("registry.db").exists() {
            return Err(HomeError::MissingFormatVersion {
                path: path.join("registry.db"),
            });
        }

        Ok(home)
    }

    /// Returns the home directory path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns nonfatal warnings found when opening the home.
    pub fn warnings(&self) -> &[HomeWarning] {
        &self.warnings
    }

    /// Acquires the exclusive home lock, waiting until it is available.
    ///
    /// Waits forever if this process already holds the lock, so keep one guard
    /// per process and pass it down.
    /// Share `&HomeLock` for operations that are safe side by side: object puts,
    /// `create_folio` and registry writes. Exclusive operations take `&mut HomeLock`.
    /// If threads need both, wrap the guard in a [`std::sync::RwLock`] (read for
    /// puts, write for exclusive operations) or a [`std::sync::Mutex`].
    pub fn lock(&self) -> Result<HomeLock, HomeError> {
        self.acquire_lock(false)
    }

    /// Attempts to acquire the exclusive home lock without waiting.
    pub fn try_lock(&self) -> Result<HomeLock, HomeError> {
        self.acquire_lock(true)
    }

    fn acquire_lock(&self, nonblocking: bool) -> Result<HomeLock, HomeError> {
        let lock_path = self.path.join("lock");
        match fs::metadata(&lock_path) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => return Err(HomeError::NotRegularFile { path: lock_path }),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(HomeError::LockIo {
                    path: lock_path,
                    source,
                });
            }
        }
        let file = private_file_options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| HomeError::LockIo {
                path: lock_path.clone(),
                source,
            })?;
        if !file
            .metadata()
            .map_err(|source| HomeError::LockIo {
                path: lock_path.clone(),
                source,
            })?
            .is_file()
        {
            return Err(HomeError::NotRegularFile { path: lock_path });
        }
        if nonblocking {
            match file.try_lock() {
                Ok(()) => {}
                Err(TryLockError::WouldBlock) => {
                    return Err(HomeError::Locked {
                        path: self.path.clone(),
                    });
                }
                Err(TryLockError::Error(source)) => {
                    return Err(HomeError::LockIo {
                        path: lock_path,
                        source,
                    });
                }
            }
        } else if let Err(source) = file.lock() {
            return Err(HomeError::LockIo {
                path: lock_path,
                source,
            });
        }

        let temporary_directory = self.path.join("tmp");
        match fs::symlink_metadata(&temporary_directory) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(HomeError::CannotCreate {
                    path: temporary_directory,
                    source: io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "temporary path must be a real directory",
                    ),
                });
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                private_directory_builder()
                    .create(&temporary_directory)
                    .map_err(|source| HomeError::CannotCreate {
                        path: temporary_directory.clone(),
                        source,
                    })?;
            }
            Err(source) => {
                return Err(HomeError::CannotCreate {
                    path: temporary_directory,
                    source,
                });
            }
        }
        for entry in
            fs::read_dir(&temporary_directory).map_err(|source| HomeError::CannotCreate {
                path: temporary_directory.clone(),
                source,
            })?
        {
            let entry = entry.map_err(|source| HomeError::CannotCreate {
                path: temporary_directory.clone(),
                source,
            })?;
            let path = entry.path();
            let result = if entry
                .file_type()
                .map_err(|source| HomeError::CannotCreate {
                    path: path.clone(),
                    source,
                })?
                .is_dir()
            {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
            result.map_err(|source| HomeError::CannotCreate {
                path: path.clone(),
                source,
            })?;
        }
        Ok(HomeLock {
            file,
            temporary_directory,
        })
    }
}

/// An exclusive lock held for the lifetime of the guard.
///
/// Keep one guard per process. Share `&HomeLock` for operations that are safe
/// side by side: object puts, `create_folio` and registry writes. Exclusive
/// operations take `&mut HomeLock`. If threads need both, wrap the guard in a
/// [`std::sync::RwLock`] (read for puts, write for exclusive operations) or a
/// [`std::sync::Mutex`].
pub struct HomeLock {
    file: File,
    temporary_directory: PathBuf,
}

impl Drop for HomeLock {
    fn drop(&mut self) {
        drop(self.file.unlock());
    }
}

impl HomeLock {
    /// Writes a unique temporary file and renames it to the destination.
    /// The destination must be on the same filesystem as the home; an existing
    /// destination is replaced where the platform allows it.
    pub fn write_temp_file<F>(&self, destination: &Path, write: F) -> Result<(), HomeError>
    where
        F: FnOnce(&mut File) -> io::Result<()>,
    {
        static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
        let (temporary_path, mut file) = loop {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = self
                .temporary_directory
                .join(format!("{}.{}", std::process::id(), id));
            match private_file_options()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => break (path, file),
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => {
                    return Err(HomeError::WriteFailed {
                        path: destination.to_path_buf(),
                        source,
                    });
                }
            }
        };
        if let Err(source) = write(&mut file).and_then(|()| file.sync_all()) {
            let _ = fs::remove_file(&temporary_path);
            return Err(HomeError::WriteFailed {
                path: destination.to_path_buf(),
                source,
            });
        }
        drop(file);
        if let Err(source) = fs::rename(&temporary_path, destination) {
            let _ = fs::remove_file(&temporary_path);
            return Err(HomeError::WriteFailed {
                path: destination.to_path_buf(),
                source,
            });
        }
        Ok(())
    }
}

fn private_directory_builder() -> DirBuilder {
    let builder = DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    builder
}

fn private_file_options() -> OpenOptions {
    let options = OpenOptions::new();
    #[cfg(unix)]
    let options = {
        let mut options = options;
        options.mode(0o600);
        options
    };
    options
}

fn home_warnings(_path: &Path) -> Result<Vec<HomeWarning>, HomeError> {
    #[cfg(unix)]
    {
        let path = _path;
        let mode = fs::metadata(path)
            .map_err(|source| HomeError::CannotCreate {
                path: path.to_path_buf(),
                source,
            })?
            .mode()
            & 0o7777;
        if mode & 0o022 != 0 {
            return Err(HomeError::WritableByOthers {
                path: path.to_path_buf(),
                mode,
            });
        }
        if mode & 0o044 != 0 {
            return Ok(vec![HomeWarning::ReadableByOthers {
                path: path.to_path_buf(),
                mode,
            }]);
        }
    }
    Ok(Vec::new())
}

/// A nonfatal concern found when opening a Folt home.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HomeWarning {
    ReadableByOthers { path: PathBuf, mode: u32 },
}

impl fmt::Display for HomeWarning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadableByOthers { path, mode } => write!(
                formatter,
                "Folt home {} has mode {mode:04o}; group or others can read its contents; chmod 700 on that folder makes it private",
                path.display()
            ),
        }
    }
}

/// Resolves the home from an override and platform home directory.
pub fn resolve_home(
    folt_home: Option<OsString>,
    platform_home: Option<PathBuf>,
) -> Result<PathBuf, HomeError> {
    if let Some(value) = folt_home.filter(|value| !value.is_empty()) {
        let path = PathBuf::from(&value);
        if !path.is_absolute() {
            return Err(HomeError::InvalidFoltHome(value));
        }
        return Ok(path);
    }

    platform_home
        .map(|path| path.join(".folt"))
        .ok_or(HomeError::NoHomeDirectory)
}

fn recognize_home(path: &Path) -> Result<(), HomeError> {
    let entries = fs::read_dir(path).map_err(|source| HomeError::CannotCreate {
        path: path.to_path_buf(),
        source,
    })?;
    let mut allowed = true;
    for entry in entries {
        let entry = entry.map_err(|source| HomeError::CannotCreate {
            path: path.to_path_buf(),
            source,
        })?;
        if !matches!(
            entry.file_name().to_str(),
            Some("lock" | "tmp" | "folios" | "registry.db" | "format")
        ) {
            allowed = false;
            break;
        }
    }
    if allowed || has_folt_application_id(&path.join("registry.db")) {
        Ok(())
    } else {
        Err(HomeError::NotAHome {
            path: path.to_path_buf(),
        })
    }
}

pub(crate) fn is_regular_file(path: &Path) -> io::Result<bool> {
    Ok(fs::metadata(path)?.is_file())
}

fn has_folt_application_id(path: &Path) -> bool {
    if !is_regular_file(path).unwrap_or(false) {
        return false;
    }
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut header = [0; 72];
    if file.read_exact(&mut header).is_err() || &header[..16] != SQLITE_HEADER {
        return false;
    }
    u32::from_be_bytes(header[68..72].try_into().expect("four-byte header slice")) == APPLICATION_ID
}

/// Errors that can occur while resolving or opening a Folt home.
#[derive(Debug)]
pub enum HomeError {
    NoHomeDirectory,
    InvalidFoltHome(OsString),
    CannotCreate {
        path: PathBuf,
        source: io::Error,
    },
    WriteFailed {
        path: PathBuf,
        source: io::Error,
    },
    NotAHome {
        path: PathBuf,
    },
    WritableByOthers {
        path: PathBuf,
        mode: u32,
    },
    InvalidFormatVersion {
        path: PathBuf,
        found: String,
    },
    MissingFormatVersion {
        path: PathBuf,
    },
    NewerFormatVersion {
        found: LayoutVersion,
        supported: LayoutVersion,
    },
    OlderFormatVersion {
        found: LayoutVersion,
        supported: LayoutVersion,
    },
    Registry(RegistryError),
    Locked {
        path: PathBuf,
    },
    NotRegularFile {
        path: PathBuf,
    },
    LockIo {
        path: PathBuf,
        source: io::Error,
    },
}

fn may_be_in_use(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::PermissionDenied {
        return true;
    }

    #[cfg(windows)]
    {
        matches!(error.raw_os_error(), Some(32 | 33))
    }
    #[cfg(not(windows))]
    {
        false
    }
}

impl fmt::Display for HomeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoHomeDirectory => write!(
                formatter,
                "could not determine the home directory; set FOLT_HOME"
            ),
            Self::InvalidFoltHome(value) => {
                write!(formatter, "FOLT_HOME must be an absolute path: {value:?}")
            }
            Self::CannotCreate { path, source } => {
                write!(
                    formatter,
                    "cannot create Folt home {}: {source}",
                    path.display()
                )
            }
            Self::WriteFailed { path, source } => {
                if may_be_in_use(source) {
                    write!(
                        formatter,
                        "cannot write {}: {source}. The file may be open in another application or not writable; close other applications using it, check its permissions and try again.",
                        path.display()
                    )
                } else {
                    write!(formatter, "cannot write {}: {source}", path.display())
                }
            }
            Self::NotAHome { path } => {
                write!(formatter, "not a Folt home directory: {}", path.display())
            }
            Self::WritableByOthers { path, mode } => write!(
                formatter,
                "refusing Folt home {} with mode {mode:04o}: group or others can write to it; remove group and other write permission (chmod go-w) from that folder (chmod 700 on that folder makes it private)",
                path.display()
            ),
            Self::InvalidFormatVersion { path, found } => write!(
                formatter,
                "Folt registry {} contains invalid home format version '{found}'",
                path.display()
            ),
            Self::MissingFormatVersion { path } => write!(
                formatter,
                "Folt registry {} is missing its home format version",
                path.display()
            ),
            Self::NewerFormatVersion { found, supported } => write!(
                formatter,
                "Folt home format version {found} is newer than supported version {supported}"
            ),
            Self::OlderFormatVersion { found, supported } => write!(
                formatter,
                "Folt home format version {found} has an unsupported older major version (supported: {supported})"
            ),
            Self::Registry(source) => write!(formatter, "{source}"),
            Self::Locked { path } => {
                write!(formatter, "Folt home is locked: {}", path.display())
            }
            Self::NotRegularFile { path } => {
                write!(
                    formatter,
                    "Folt home lock is not a regular file: {}",
                    path.display()
                )
            }
            Self::LockIo { path, source } => {
                write!(
                    formatter,
                    "cannot lock Folt home file {}: {source}",
                    path.display()
                )
            }
        }
    }
}

impl Error for HomeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CannotCreate { source, .. }
            | Self::WriteFailed { source, .. }
            | Self::LockIo { source, .. } => Some(source),
            Self::Registry(source) => Some(source),
            _ => None,
        }
    }
}

impl From<RegistryError> for HomeError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(error)
    }
}
