use std::error::Error;
use std::fmt;
use std::str::FromStr;

/// The home format version understood by this build.
pub const HOME_FORMAT_VERSION: LayoutVersion = LayoutVersion {
    major: 0,
    minor: 1,
    patch: 0,
};

/// The Folio layout version understood by this build.
pub const FOLIO_LAYOUT_VERSION: LayoutVersion = LayoutVersion {
    major: 0,
    minor: 1,
    patch: 0,
};

/// The fixed default branch for Folios.
pub const DEFAULT_BRANCH: &str = "main";

/// The working content file for the default branch and target of link placements.
pub const CONTENT_FILE: &str = "folio.main";

/// Object identifier algorithm used by Folios created by this build.
pub const OBJECT_ALGORITHM: &str = "sha256";

/// A canonical semantic layout version.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LayoutVersion {
    major: u64,
    minor: u64,
    patch: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutSupport {
    Supported,
    Newer,
    Older,
}

impl LayoutVersion {
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Classifies a Folio layout using the major-only compatibility rule.
    pub fn support(self) -> LayoutSupport {
        self.support_as_folio_layout()
    }

    pub fn support_as_folio_layout(self) -> LayoutSupport {
        self.support_version(FOLIO_LAYOUT_VERSION)
    }

    pub fn support_as_home_format(self) -> LayoutSupport {
        self.support_version(HOME_FORMAT_VERSION)
    }

    fn support_version(self, supported: Self) -> LayoutSupport {
        match self.major.cmp(&supported.major) {
            std::cmp::Ordering::Greater => LayoutSupport::Newer,
            std::cmp::Ordering::Less => LayoutSupport::Older,
            std::cmp::Ordering::Equal => LayoutSupport::Supported,
        }
    }
}

impl fmt::Display for LayoutVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl FromStr for LayoutVersion {
    type Err = LayoutVersionError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut parts = value.split('.');
        let parse = |part: Option<&str>| -> Result<u64, LayoutVersionError> {
            let part = part.ok_or(LayoutVersionError)?;
            if part.is_empty()
                || (part.len() > 1 && part.starts_with('0'))
                || !part.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(LayoutVersionError);
            }
            part.parse().map_err(|_| LayoutVersionError)
        };
        let version = Self {
            major: parse(parts.next())?,
            minor: parse(parts.next())?,
            patch: parse(parts.next())?,
        };
        if parts.next().is_some() {
            return Err(LayoutVersionError);
        }
        Ok(version)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LayoutVersionError;

impl fmt::Display for LayoutVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid layout version")
    }
}

impl Error for LayoutVersionError {}
