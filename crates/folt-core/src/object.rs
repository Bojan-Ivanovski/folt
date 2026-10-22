use std::error::Error;
use std::fmt;
use std::io::{self, Read, Write};
use std::str::FromStr;

use sha2::{Digest, Sha256};

const UNCOMPRESSED: u8 = 0x00;
const ZSTD: u8 = 0x01;
const MAX_ZSTD_WINDOW_LOG: u32 = 23;

/// The SHA-256 name of an object.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ObjectId([u8; 32]);

impl ObjectId {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for ObjectId {
    type Err = ObjectError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(ObjectError::InvalidHash);
        }
        let mut bytes = [0; 32];
        for (index, chunk) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
            let digit = |b: u8| {
                if b.is_ascii_digit() {
                    b - b'0'
                } else {
                    b - b'a' + 10
                }
            };
            bytes[index] = digit(chunk[0]) * 16 + digit(chunk[1]);
        }
        Ok(Self(bytes))
    }
}

/// The kind prefix in a canonical object encoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectKind {
    Blob,
    Commit,
    Tag,
}

impl ObjectKind {
    fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Blob => b"blob",
            Self::Commit => b"commit",
            Self::Tag => b"tag",
        }
    }
}

/// Incrementally hashes a framed object without buffering its body.
pub struct ObjectHasher {
    hasher: Sha256,
}

impl ObjectHasher {
    pub fn new(kind: ObjectKind, length: u64) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(kind.as_bytes());
        hasher.update(b" ");
        hasher.update(length.to_string().as_bytes());
        hasher.update([0]);
        Self { hasher }
    }
    pub fn update(&mut self, bytes: &[u8]) {
        self.hasher.update(bytes);
    }
    pub fn finish(self) -> ObjectId {
        ObjectId(self.hasher.finalize().into())
    }
}

pub fn hash_object(kind: ObjectKind, body: &[u8]) -> ObjectId {
    let mut hasher = ObjectHasher::new(kind, body.len() as u64);
    hasher.update(body);
    hasher.finish()
}

pub fn hash_blob(body: &[u8]) -> ObjectId {
    hash_object(ObjectKind::Blob, body)
}

pub fn encode(kind: ObjectKind, body: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(kind.as_bytes().len() + 22 + body.len());
    bytes.extend_from_slice(kind.as_bytes());
    bytes.push(b' ');
    bytes.extend_from_slice(body.len().to_string().as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(body);
    bytes
}

pub fn decode(framed: &[u8], expected: ObjectKind, limit: u64) -> Result<Vec<u8>, ObjectError> {
    let nul = framed
        .iter()
        .position(|b| *b == 0)
        .ok_or(ObjectError::InvalidFrame)?;
    let header = &framed[..nul];
    let mut split = header.split(|b| *b == b' ');
    let kind = split.next().ok_or(ObjectError::InvalidFrame)?;
    let length = split.next().ok_or(ObjectError::InvalidFrame)?;
    if split.next().is_some()
        || kind != expected.as_bytes()
        || length.is_empty()
        || (length.len() > 1 && length[0] == b'0')
        || !length.iter().all(u8::is_ascii_digit)
    {
        return Err(ObjectError::InvalidFrame);
    }
    let length = std::str::from_utf8(length)
        .map_err(|_| ObjectError::InvalidFrame)?
        .parse::<u64>()
        .map_err(|_| ObjectError::InvalidFrame)?;
    if length > limit {
        return Err(ObjectError::LimitExceeded);
    }
    let body = &framed[nul + 1..];
    if body.len() as u64 != length {
        return Err(ObjectError::InvalidFrame);
    }
    Ok(body.to_vec())
}

/// Adds a codec marker and stores a framed object, selecting compression only when smaller.
pub fn compress(framed: &[u8]) -> Result<Vec<u8>, ObjectError> {
    let mut encoder = zstd::Encoder::new(Vec::new(), 3).map_err(ObjectError::Io)?;
    encoder.write_all(framed).map_err(ObjectError::Io)?;
    let compressed = encoder.finish().map_err(ObjectError::Io)?;
    if compressed.len() < framed.len() {
        let mut stored = Vec::with_capacity(compressed.len() + 1);
        stored.push(ZSTD);
        stored.extend_from_slice(&compressed);
        Ok(stored)
    } else {
        let mut stored = Vec::with_capacity(framed.len() + 1);
        stored.push(UNCOMPRESSED);
        stored.extend_from_slice(framed);
        Ok(stored)
    }
}

pub fn open_stored(stored: &[u8], expected: ObjectId, limit: u64) -> Result<Vec<u8>, ObjectError> {
    let (&codec, payload) = stored.split_first().ok_or(ObjectError::InvalidFrame)?;
    let framed = match codec {
        UNCOMPRESSED => {
            if payload.len() as u64 > limit.saturating_add(128) {
                return Err(ObjectError::LimitExceeded);
            }
            payload.to_vec()
        }
        ZSTD => {
            let cursor = io::Cursor::new(payload);
            let mut decoder = zstd::stream::read::Decoder::new(cursor).map_err(ObjectError::Io)?;
            decoder
                .window_log_max(MAX_ZSTD_WINDOW_LOG)
                .map_err(ObjectError::Io)?;
            let mut output = Vec::new();
            decoder
                .take(limit.saturating_add(128).saturating_add(1))
                .read_to_end(&mut output)
                .map_err(ObjectError::Io)?;
            if output.len() as u64 > limit.saturating_add(128) {
                return Err(ObjectError::LimitExceeded);
            }
            output
        }
        other => return Err(ObjectError::UnknownCodec(other)),
    };
    let (kind, body) = parse_frame(&framed, limit)?;
    if hash_object(kind, &body) != expected {
        return Err(ObjectError::HashMismatch);
    }
    Ok(framed)
}

fn parse_frame(framed: &[u8], limit: u64) -> Result<(ObjectKind, Vec<u8>), ObjectError> {
    for kind in [ObjectKind::Blob, ObjectKind::Commit, ObjectKind::Tag] {
        if let Ok(body) = decode(framed, kind, limit) {
            return Ok((kind, body));
        }
    }
    Err(ObjectError::InvalidFrame)
}

/// A canonical commit body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Commit {
    pub content: Option<ObjectId>,
    pub meta: ObjectId,
    pub parents: Vec<ObjectId>,
    pub author: String,
    pub timestamp: i64,
    pub message: String,
    pub group: Option<String>,
}

impl Commit {
    pub fn encode(&self) -> Result<Vec<u8>, ObjectError> {
        validate_value(&self.author)?;
        validate_value(&self.message)?;
        if let Some(group) = &self.group {
            validate_value(group)?;
        }
        let mut body = String::new();
        if let Some(content) = self.content {
            body.push_str(&format!("content {content}\n"));
        }
        body.push_str(&format!("meta {}\n", self.meta));
        for parent in &self.parents {
            body.push_str(&format!("parent {parent}\n"));
        }
        body.push_str(&format!(
            "author {}\ntimestamp {}\nmessage {}\n",
            self.author, self.timestamp, self.message
        ));
        if let Some(group) = &self.group {
            body.push_str(&format!("group {group}\n"));
        }
        Ok(encode(ObjectKind::Commit, body.as_bytes()))
    }
    pub fn parse(framed: &[u8], limit: u64) -> Result<Self, ObjectError> {
        let body = decode(framed, ObjectKind::Commit, limit)?;
        let text = std::str::from_utf8(&body).map_err(|_| ObjectError::InvalidObject)?;
        let mut lines = text.split_terminator('\n');
        let mut content = None;
        let mut line = lines.next().ok_or(ObjectError::InvalidObject)?;
        if let Some(value) = line.strip_prefix("content ") {
            content = Some(value.parse()?);
            line = lines.next().ok_or(ObjectError::InvalidObject)?;
        }
        let meta = line
            .strip_prefix("meta ")
            .ok_or(ObjectError::InvalidObject)?
            .parse()?;
        let mut parents = Vec::new();
        let mut next = lines.next().ok_or(ObjectError::InvalidObject)?;
        while let Some(value) = next.strip_prefix("parent ") {
            parents.push(value.parse()?);
            next = lines.next().ok_or(ObjectError::InvalidObject)?;
        }
        let author = next
            .strip_prefix("author ")
            .ok_or(ObjectError::InvalidObject)?
            .to_owned();
        let timestamp = lines
            .next()
            .and_then(|s| s.strip_prefix("timestamp "))
            .ok_or(ObjectError::InvalidObject)?
            .parse()
            .map_err(|_| ObjectError::InvalidObject)?;
        let message = lines
            .next()
            .and_then(|s| s.strip_prefix("message "))
            .ok_or(ObjectError::InvalidObject)?
            .to_owned();
        let group = lines
            .next()
            .map(|s| {
                s.strip_prefix("group ")
                    .ok_or(ObjectError::InvalidObject)
                    .map(str::to_owned)
            })
            .transpose()?;
        if lines.next().is_some() {
            return Err(ObjectError::InvalidObject);
        }
        validate_value(&author)?;
        validate_value(&message)?;
        if let Some(value) = &group {
            validate_value(value)?;
        }
        Ok(Self {
            content,
            meta,
            parents,
            author,
            timestamp,
            message,
            group,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tag {
    pub target: ObjectId,
    pub name: String,
    pub tagger: String,
    pub message: String,
}

impl Tag {
    pub fn encode(&self) -> Result<Vec<u8>, ObjectError> {
        validate_value(&self.name)?;
        validate_value(&self.tagger)?;
        validate_value(&self.message)?;
        Ok(encode(
            ObjectKind::Tag,
            format!(
                "target {}\nname {}\ntagger {}\nmessage {}\n",
                self.target, self.name, self.tagger, self.message
            )
            .as_bytes(),
        ))
    }
    pub fn parse(framed: &[u8], limit: u64) -> Result<Self, ObjectError> {
        let body = decode(framed, ObjectKind::Tag, limit)?;
        let text = std::str::from_utf8(&body).map_err(|_| ObjectError::InvalidObject)?;
        let mut lines = text.split_terminator('\n');
        let target = lines
            .next()
            .and_then(|s| s.strip_prefix("target "))
            .ok_or(ObjectError::InvalidObject)?
            .parse()?;
        let name = lines
            .next()
            .and_then(|s| s.strip_prefix("name "))
            .ok_or(ObjectError::InvalidObject)?
            .to_owned();
        let tagger = lines
            .next()
            .and_then(|s| s.strip_prefix("tagger "))
            .ok_or(ObjectError::InvalidObject)?
            .to_owned();
        let message = lines
            .next()
            .and_then(|s| s.strip_prefix("message "))
            .ok_or(ObjectError::InvalidObject)?
            .to_owned();
        if lines.next().is_some() {
            return Err(ObjectError::InvalidObject);
        }
        validate_value(&name)?;
        validate_value(&tagger)?;
        validate_value(&message)?;
        Ok(Self {
            target,
            name,
            tagger,
            message,
        })
    }
}

fn validate_value(value: &str) -> Result<(), ObjectError> {
    if value.contains('\n') || value.contains('\r') || value.as_bytes().contains(&0) {
        Err(ObjectError::InvalidObject)
    } else {
        Ok(())
    }
}

#[derive(Debug)]
pub enum ObjectError {
    InvalidHash,
    InvalidFrame,
    InvalidObject,
    LimitExceeded,
    UnknownCodec(u8),
    HashMismatch,
    Io(io::Error),
}

impl fmt::Display for ObjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHash => write!(f, "invalid object hash"),
            Self::InvalidFrame => write!(f, "invalid object framing"),
            Self::InvalidObject => write!(f, "invalid canonical object"),
            Self::LimitExceeded => write!(f, "object exceeds size limit"),
            Self::UnknownCodec(codec) => write!(f, "unknown object codec 0x{codec:02x}"),
            Self::HashMismatch => write!(f, "object hash does not match its name"),
            Self::Io(error) => write!(f, "object codec error: {error}"),
        }
    }
}
impl Error for ObjectError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}
