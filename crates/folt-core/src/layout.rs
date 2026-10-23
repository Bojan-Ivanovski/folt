use std::error::Error;
use std::fmt;
use std::str::FromStr;

/// The layout version understood by this build.
pub const LAYOUT_VERSION: LayoutVersion = LayoutVersion {
    major: 0,
    minor: 1,
    patch: 0,
};

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

    /// Classifies a version using the major-only compatibility rule.
    pub fn support(self) -> LayoutSupport {
        match self.major.cmp(&LAYOUT_VERSION.major) {
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
