use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::layout::{HOME_FORMAT_VERSION, LayoutSupport, LayoutVersion};
use crate::registry::{Registry, RegistryError};

const APPLICATION_ID: u32 = 0x464f_4c54;
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";

/// A resolved Folt home directory.
pub struct Home {
    path: PathBuf,
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
    /// Directory permissions follow the umask. Concurrent first runs are not
    /// serialised until the home lock exists.
    pub fn open_or_create(path: PathBuf) -> Result<Self, HomeError> {
        fs::create_dir_all(&path).map_err(|source| HomeError::CannotCreate {
            path: path.clone(),
            source,
        })?;

        recognize_home(&path)?;
        let registry = Registry::open_existing(&Self { path: path.clone() })?;
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

        Ok(Self { path })
    }

    /// Returns the home directory path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Acquires the exclusive home lock, waiting until it is available.
    ///
    /// Waits forever if this process already holds the lock, so keep one guard
    /// per process and pass it down.
    pub fn lock(&self) -> Result<HomeLock, HomeError> {
        self.acquire_lock(false)
    }

    /// Attempts to acquire the exclusive home lock without waiting.
    pub fn try_lock(&self) -> Result<HomeLock, HomeError> {
        self.acquire_lock(true)
    }

    fn acquire_lock(&self, nonblocking: bool) -> Result<HomeLock, HomeError> {
        let lock_path = self.path.join("lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| HomeError::LockIo {
                path: lock_path.clone(),
                source,
            })?;
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
                fs::create_dir(&temporary_directory).map_err(|source| HomeError::CannotCreate {
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
            match OpenOptions::new().write(true).create_new(true).open(&path) {
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

fn has_folt_application_id(path: &Path) -> bool {
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
