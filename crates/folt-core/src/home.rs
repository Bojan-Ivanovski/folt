use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

const FORMAT_VERSION: u64 = 1;

/// A resolved Folt home directory.
pub struct Home {
    path: PathBuf,
}

impl Home {
    /// Resolves the home using the process environment.
    ///
    /// The platform home directory may change behavior on Windows. Redirected
    /// Windows profiles are not handled, and a missing home directory fails
    /// when the directory is created. Directory permissions follow the umask.
    pub fn locate() -> Result<Self, HomeError> {
        let folt_home = std::env::var_os("FOLT_HOME");
        let platform_home = std::env::home_dir();
        let path = resolve_home(folt_home, platform_home)?;
        Self::open_or_create(path)
    }

    /// Opens or creates a home directory and validates its format marker.
    pub fn open_or_create(path: PathBuf) -> Result<Self, HomeError> {
        fs::create_dir_all(&path).map_err(|source| HomeError::CannotCreate {
            path: path.clone(),
            source,
        })?;

        let marker = path.join("format");
        match File::open(&marker) {
            Ok(mut file) => {
                let mut contents = String::new();
                file.read_to_string(&mut contents).map_err(|source| {
                    HomeError::UnreadableMarker {
                        path: marker.clone(),
                        source,
                    }
                })?;
                let found = contents.trim().parse::<u64>().map_err(|error| {
                    HomeError::UnreadableMarker {
                        path: marker.clone(),
                        source: io::Error::new(io::ErrorKind::InvalidData, error),
                    }
                })?;
                if found > FORMAT_VERSION {
                    return Err(HomeError::UnsupportedFormatVersion {
                        found,
                        supported: FORMAT_VERSION,
                    });
                }
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                let mut entries =
                    fs::read_dir(&path).map_err(|source| HomeError::CannotCreate {
                        path: path.clone(),
                        source,
                    })?;
                if entries.next().is_some() {
                    return Err(HomeError::UnreadableMarker {
                        path: marker,
                        source,
                    });
                }
                write_format_marker(&path, &marker)?;
            }
            Err(source) => {
                return Err(HomeError::UnreadableMarker {
                    path: marker,
                    source,
                });
            }
        }

        Ok(Self { path })
    }

    /// Returns the home directory path.
    pub fn path(&self) -> &Path {
        &self.path
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

fn write_format_marker(directory: &Path, marker: &Path) -> Result<(), HomeError> {
    let temporary = directory.join("format.tmp");
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        writeln!(file, "{FORMAT_VERSION}")?;
        file.sync_all()?;
        fs::rename(&temporary, marker)
    })();
    if let Err(source) = result {
        let _ = fs::remove_file(&temporary);
        return Err(HomeError::CannotCreate {
            path: directory.to_path_buf(),
            source,
        });
    }
    Ok(())
}

/// Errors that can occur while resolving or opening a Folt home.
#[derive(Debug)]
pub enum HomeError {
    NoHomeDirectory,
    InvalidFoltHome(OsString),
    CannotCreate { path: PathBuf, source: io::Error },
    UnreadableMarker { path: PathBuf, source: io::Error },
    UnsupportedFormatVersion { found: u64, supported: u64 },
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
            Self::UnreadableMarker { path, source } => {
                write!(
                    formatter,
                    "cannot read Folt format marker {}: {source}",
                    path.display()
                )
            }
            Self::UnsupportedFormatVersion { found, supported } => write!(
                formatter,
                "Folt format version {found} is newer than supported version {supported}"
            ),
        }
    }
}

impl Error for HomeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CannotCreate { source, .. } | Self::UnreadableMarker { source, .. } => {
                Some(source)
            }
            _ => None,
        }
    }
}
