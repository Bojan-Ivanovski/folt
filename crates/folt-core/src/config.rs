use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::PathBuf;

use serde::Deserialize;

use crate::home::Home;
use crate::object::{Author, ObjectError};

/// Settings read from the Folt home's `config.toml`.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct Config {
    pub username: Option<String>,
    pub email: Option<String>,
}

impl Config {
    /// Loads settings, using defaults only when the file is missing.
    pub fn load(home: &Home) -> Result<Self, ConfigError> {
        let path = home.path().join("config.toml");
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => return Err(ConfigError::Read { path, source }),
        };
        let metadata = if metadata.is_symlink() {
            fs::metadata(&path).map_err(|source| ConfigError::Read {
                path: path.clone(),
                source,
            })?
        } else {
            metadata
        };
        if !metadata.is_file() {
            return Err(ConfigError::NotAFile { path });
        }
        let text = fs::read_to_string(&path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        let config: Self = toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.clone(),
            source,
        })?;
        config
            .validated_author()
            .map_err(|source| ConfigError::Invalid { path, source })?;
        Ok(config)
    }

    /// Returns the configured author, or `anonymous` when no username is set.
    pub fn author(&self) -> Result<Author, ConfigError> {
        self.validated_author().map_err(ConfigError::Author)
    }

    fn validated_author(&self) -> Result<Author, ObjectError> {
        let author = Author::new(
            self.username.as_deref().unwrap_or("anonymous"),
            self.email.as_deref(),
        )?;
        if self.username.is_some() {
            Ok(author)
        } else {
            Ok(Author::anonymous())
        }
    }
}

/// Errors that can occur while loading settings or building an author.
#[derive(Debug)]
pub enum ConfigError {
    Read {
        path: PathBuf,
        source: io::Error,
    },
    NotAFile {
        path: PathBuf,
    },
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    Invalid {
        path: PathBuf,
        source: ObjectError,
    },
    Author(ObjectError),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(formatter, "cannot read config {}: {source}", path.display())
            }
            Self::NotAFile { path } => {
                write!(
                    formatter,
                    "config is not a regular file: {}",
                    path.display()
                )
            }
            Self::Parse { path, source } => {
                write!(
                    formatter,
                    "cannot parse config {}: {source}",
                    path.display()
                )
            }
            Self::Invalid { path, source } => {
                write!(
                    formatter,
                    "invalid author in config {}: {source}",
                    path.display()
                )
            }
            Self::Author(source) => write!(formatter, "invalid config author: {source}"),
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
            Self::Invalid { source, .. } | Self::Author(source) => Some(source),
            Self::NotAFile { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_username_uses_anonymous_even_with_email() {
        for email in [None, Some("a@example.test".into())] {
            assert_eq!(
                Config {
                    username: None,
                    email
                }
                .author()
                .unwrap(),
                Author::anonymous()
            );
        }
    }

    #[test]
    fn username_only_has_no_email() {
        let config = Config {
            username: Some("Alice".into()),
            email: None,
        };
        assert_eq!(config.author().unwrap().to_string(), "Alice");
    }

    #[test]
    fn configured_identity_uses_both_fields() {
        let config = Config {
            username: Some("Alice".into()),
            email: Some("a@example.test".into()),
        };
        assert_eq!(
            config.author().unwrap().to_string(),
            "Alice <a@example.test>"
        );
    }

    #[test]
    fn invalid_identity_is_not_silently_replaced() {
        for username in ["<alice>", "ali\nce", ""] {
            assert!(
                Config {
                    username: Some(username.into()),
                    email: None
                }
                .author()
                .is_err()
            );
        }
        assert!(
            Config {
                username: None,
                email: Some("invalid".into())
            }
            .author()
            .is_err()
        );
    }
}
