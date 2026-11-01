//! Validated Folio labels.
use std::error::Error;
use std::fmt;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct FolioName(String);

impl FolioName {
    pub fn parse(value: &str) -> Result<Self, FolioNameError> {
        if !(1..=64).contains(&value.len()) {
            return Err(FolioNameError::Length);
        }
        if !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err(FolioNameError::Character);
        }
        if value.starts_with(['.', '-']) || value.ends_with(['.', '-']) {
            return Err(FolioNameError::Edge);
        }
        if value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(FolioNameError::HexDigest);
        }
        Ok(Self(value.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for FolioName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}
impl fmt::Display for FolioName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FolioNameError {
    Length,
    Character,
    Edge,
    HexDigest,
}
impl fmt::Display for FolioNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Length => "Folio name must contain 1 to 64 bytes",
            Self::Character => "Folio name allows only A-Z, a-z, 0-9, '.', '_' and '-'; @, #, :, / and whitespace are reserved",
            Self::Edge => "Folio name must not start or end with '.' or '-'",
            Self::HexDigest => "Folio name must not be a 64-digit hexadecimal string",
        })
    }
}
impl Error for FolioNameError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar_boundaries() {
        for value in [
            "a",
            "Logo",
            "logo",
            "a.b_c-d",
            "_x",
            "x_",
            "a..b",
            "1234",
            "deadbeef",
            "con",
            "NUL.txt",
            "550e8400-e29b-41d4-a716-446655440000",
            &"g".repeat(64),
            &"a".repeat(63),
        ] {
            let name = FolioName::parse(value).unwrap();
            assert_eq!(name.as_str(), value);
            assert_eq!(name.as_ref(), value);
            assert_eq!(name.to_string(), value);
        }
        for value in [
            "",
            ".a",
            "a.",
            "-a",
            "a-",
            ".",
            "-",
            "a@b",
            "a#b",
            "a:b",
            "a/b",
            "a b",
            "a\tb",
            "a\nb",
            "a\rb",
            "é",
            &"a".repeat(64),
            &"A".repeat(64),
            &"g".repeat(65),
            &"a".repeat(65),
        ] {
            assert!(FolioName::parse(value).is_err(), "{value:?}");
        }
        // 65 hex digits fail the length rule, rather than the digest rule.
        assert_eq!(
            FolioName::parse(&"a".repeat(65)),
            Err(FolioNameError::Length)
        );
        assert_ne!(
            FolioName::parse("Logo").unwrap(),
            FolioName::parse("logo").unwrap()
        );
    }
}
