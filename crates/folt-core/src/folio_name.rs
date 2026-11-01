//! Validated Folio labels.
use std::error::Error;
use std::ffi::OsStr;
use std::fmt;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct FolioName(String);

impl FolioName {
    /// Slugs a UTF-8 path stem, preserving ASCII case and allowed punctuation.
    pub fn from_stem(stem: impl AsRef<OsStr>) -> Result<Self, StemError> {
        let stem = stem.as_ref().to_str().ok_or(StemError::NonUtf8)?;
        let mut slug = String::new();
        let mut replacing_run = false;
        for ch in stem.chars() {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                slug.push(ch);
                replacing_run = false;
            } else if !replacing_run {
                slug.push('-');
                replacing_run = true;
            }
        }
        let slug = slug.trim_matches(['.', '-']);
        // Slugging produces only ASCII, so the byte limit is a character boundary.
        let slug = slug[..slug.len().min(64)].trim_end_matches(['.', '-']);
        if slug.is_empty() {
            return Err(StemError::Empty);
        }
        if slug.len() == 64 && slug.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(StemError::HexDigest);
        }
        Ok(Self(slug.into()))
    }

    /// Appends a numeric suffix, shortening the base to keep the 64-byte limit.
    pub fn with_suffix(&self, n: u32) -> Self {
        let suffix = format!("_{n}");
        let base = &self.0[..self.0.len().min(64 - suffix.len())];
        let base = base.trim_end_matches(['.', '-']);
        Self(format!("{base}{suffix}"))
    }

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StemError {
    NonUtf8,
    Empty,
    HexDigest,
}

impl fmt::Display for StemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NonUtf8 => "Path stem is not UTF-8; provide --name",
            Self::Empty => "Path stem produces an empty Folio name; provide --name",
            Self::HexDigest => {
                "Path stem produces a 64-digit hexadecimal Folio name; provide --name"
            }
        })
    }
}

impl Error for StemError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stem_slug_boundaries() {
        for (stem, expected) in [
            ("A-Z_a.b09", "A-Z_a.b09"),
            ("Hello  world!", "Hello-world"),
            ("a日本語 /\t🙂b", "a-b"),
            ("a - b", "a---b"),
            ("..--.bashrc.-", "bashrc"),
            ("__", "__"),
            ("..._---", "_"),
            ("日本語A日本語", "A"),
            (&"a".repeat(63), &"a".repeat(63)),
            (&"g".repeat(64), &"g".repeat(64)),
            (&"g".repeat(100), &"g".repeat(64)),
            (&format!("{}.-.z", "g".repeat(61)), &"g".repeat(61)),
            (&format!("{}日本語z", "g".repeat(63)), &"g".repeat(63)),
            (&format!("..{}..", "g".repeat(64)), &"g".repeat(64)),
        ] {
            let name = FolioName::from_stem(stem).unwrap();
            assert_eq!(name.as_str(), expected, "{stem:?}");
            assert_eq!(FolioName::parse(expected), Ok(name));
        }
    }

    #[test]
    fn unusable_stems_require_an_explicit_name() {
        for (stem, expected) in [
            ("", StemError::Empty),
            ("日本語", StemError::Empty),
            ("🙂é", StemError::Empty),
            (" .- /\t", StemError::Empty),
            (&"a".repeat(64), StemError::HexDigest),
            (&"ABCDEF0123456789".repeat(4), StemError::HexDigest),
            (&"a".repeat(65), StemError::HexDigest),
            (&format!("{}g", "a".repeat(64)), StemError::HexDigest),
        ] {
            assert_eq!(FolioName::from_stem(stem), Err(expected), "{stem:?}");
            assert!(expected.to_string().contains("--name"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_stem_requires_an_explicit_name() {
        use std::os::unix::ffi::OsStrExt;

        let stem = OsStr::from_bytes(b"valid-prefix\xff");
        assert_eq!(FolioName::from_stem(stem), Err(StemError::NonUtf8));
    }

    #[cfg(windows)]
    #[test]
    fn non_utf8_stem_requires_an_explicit_name() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;

        let stem = OsString::from_wide(&[0x61, 0xd800]);
        assert_eq!(FolioName::from_stem(stem), Err(StemError::NonUtf8));
    }

    #[test]
    fn suffix_boundaries() {
        for (base, n, expected) in [
            ("Note", 0, "Note_0".to_owned()),
            ("Note", 1, "Note_1".to_owned()),
            ("_", u32::MAX, "__4294967295".to_owned()),
            (&"g".repeat(62), 1, format!("{}_1", "g".repeat(62))),
            (&"g".repeat(64), 1, format!("{}_1", "g".repeat(62))),
            (&"g".repeat(64), 10, format!("{}_10", "g".repeat(61))),
            (
                &"g".repeat(64),
                u32::MAX,
                format!("{}_4294967295", "g".repeat(53)),
            ),
            (
                &format!("{}.-z", "g".repeat(61)),
                1,
                format!("{}_1", "g".repeat(61)),
            ),
            (
                &format!("{}-.z", "g".repeat(60)),
                1,
                format!("{}_1", "g".repeat(60)),
            ),
            (&"a".repeat(63), 1, format!("{}_1", "a".repeat(62))),
        ] {
            let base = FolioName::parse(base).unwrap();
            let name = base.with_suffix(n);
            assert_eq!(name.as_str(), expected, "{base:?}, {n}");
            assert_eq!(FolioName::parse(&expected), Ok(name));
        }
    }

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
