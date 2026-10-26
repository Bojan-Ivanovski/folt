use std::error::Error;
use std::fmt;
use std::io::{self, Read, Write};
use std::str::FromStr;

use sha2::{Digest, Sha256};

const UNCOMPRESSED: u8 = 0x00;
const ZSTD: u8 = 0x01;
const MAX_ZSTD_WINDOW_LOG: u32 = 23;
// Bounds streaming object memory use independently of object size.
// Measured streaming buffer size.
pub const STREAM_BUFFER_SIZE: usize = 1024 * 1024;
// Small objects avoid the overhead of probing.
pub(crate) const COMPRESSION_PROBE_MIN_SIZE: usize = 4 * 1024;
// Three bounded samples keep the probe cheap for large objects.
pub(crate) const COMPRESSION_SAMPLE_SIZE: usize = 64 * 1024;
// Require at least ten percent sampled savings before full compression.
pub(crate) const COMPRESSION_PROBE_MIN_SAVING_PERCENT: usize = 10;
// These body prefixes identify formats that are already compressed.
const ALREADY_COMPRESSED_MAGIC: &[&[u8]] = &[
    b"\x1f\x8b",           // gzip
    b"\x28\xb5\x2f\xfd",   // zstd
    b"\xfd7zXZ\x00",       // xz
    b"BZh",                // bzip2
    b"PK\x03\x04",         // zip, docx, jar
    b"7z\xbc\xaf\x27\x1c", // 7z
    b"\x89PNG\r\n\x1a\n",  // PNG
    b"\xff\xd8\xff",       // JPEG
    b"GIF87a",
    b"GIF89a",
];

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

/// Streams a framed body into its stored representation while hashing it.
pub struct ObjectWriter<W: Write> {
    output: Option<W>,
    encoder: Option<zstd::Encoder<'static, W>>,
    hasher: ObjectHasher,
    expected: u64,
    written: u64,
}

impl<W: Write> ObjectWriter<W> {
    pub fn new(
        mut output: W,
        kind: ObjectKind,
        length: u64,
        compressed: bool,
    ) -> Result<Self, ObjectError> {
        let header = [kind.as_bytes(), b" ", length.to_string().as_bytes(), &[0]].concat();
        let hasher = ObjectHasher::new(kind, length);
        if compressed {
            output.write_all(&[ZSTD]).map_err(ObjectError::Io)?;
            let mut encoder = zstd::Encoder::new(output, 3).map_err(ObjectError::Io)?;
            encoder.write_all(&header).map_err(ObjectError::Io)?;
            Ok(Self {
                output: None,
                encoder: Some(encoder),
                hasher,
                expected: length,
                written: 0,
            })
        } else {
            output
                .write_all(&[UNCOMPRESSED])
                .and_then(|()| output.write_all(&header))
                .map_err(ObjectError::Io)?;
            Ok(Self {
                output: Some(output),
                encoder: None,
                hasher,
                expected: length,
                written: 0,
            })
        }
    }
    pub fn write_body(&mut self, bytes: &[u8]) -> Result<(), ObjectError> {
        self.written = self
            .written
            .checked_add(bytes.len() as u64)
            .ok_or(ObjectError::InvalidFrame)?;
        if self.written > self.expected {
            return Err(ObjectError::InvalidFrame);
        }
        self.hasher.update(bytes);
        if let Some(encoder) = &mut self.encoder {
            encoder.write_all(bytes).map_err(ObjectError::Io)
        } else {
            self.output
                .as_mut()
                .expect("uncompressed output")
                .write_all(bytes)
                .map_err(ObjectError::Io)
        }
    }
    pub fn finish(mut self) -> Result<(W, ObjectId), ObjectError> {
        if self.written != self.expected {
            return Err(ObjectError::Truncated);
        }
        let output = if let Some(encoder) = self.encoder.take() {
            encoder.finish().map_err(ObjectError::Io)?
        } else {
            self.output.take().expect("uncompressed output")
        };
        Ok((output, self.hasher.finish()))
    }
}

/// Streams the verified body; callers must treat output as provisional until `finish` succeeds.
pub struct ObjectReader {
    reader: BoxReader,
    hasher: ObjectHasher,
    expected: u64,
    read: u64,
    id: ObjectId,
}

impl ObjectReader {
    pub fn new<R: Read + 'static>(
        stored: R,
        id: ObjectId,
        kind: ObjectKind,
    ) -> Result<Self, ObjectError> {
        let (reader, header) = read_stored_header(stored)?;
        let expected = decode_header(&header, kind)?;
        Ok(Self {
            reader,
            hasher: ObjectHasher::new(kind, expected),
            expected,
            read: 0,
            id,
        })
    }
    pub fn read_body(&mut self, output: &mut [u8]) -> Result<usize, ObjectError> {
        let allowance = output
            .len()
            .min(usize::try_from(self.expected.saturating_sub(self.read)).unwrap_or(usize::MAX));
        if allowance == 0 {
            let mut extra = [0];
            if self.reader.read(&mut extra).map_err(ObjectError::Io)? != 0 {
                return Err(ObjectError::InvalidFrame);
            }
            return Ok(0);
        }
        let count = self
            .reader
            .read(&mut output[..allowance])
            .map_err(ObjectError::Io)?;
        self.read += count as u64;
        self.hasher.update(&output[..count]);
        Ok(count)
    }
    pub fn finish(mut self) -> Result<(), ObjectError> {
        if self.read != self.expected {
            return Err(ObjectError::Truncated);
        }
        let mut extra = [0];
        if self.reader.read(&mut extra).map_err(ObjectError::Io)? != 0 {
            return Err(ObjectError::InvalidFrame);
        }
        if self.hasher.finish() != self.id {
            return Err(ObjectError::HashMismatch);
        }
        Ok(())
    }
}

type BoxReader = Box<dyn Read>;

/// Reads the stored codec and canonical header without verifying the body or hash.
pub fn peek_kind<R: Read + 'static>(stored: R) -> Result<ObjectKind, ObjectError> {
    let (_, header) = read_stored_header(stored)?;
    let kind = match header.split(|b| *b == b' ').next() {
        Some(b"blob") => ObjectKind::Blob,
        Some(b"commit") => ObjectKind::Commit,
        Some(b"tag") => ObjectKind::Tag,
        _ => return Err(ObjectError::InvalidFrame),
    };
    decode_header(&header, kind)?;
    Ok(kind)
}

fn read_stored_header<R: Read + 'static>(
    mut stored: R,
) -> Result<(BoxReader, Vec<u8>), ObjectError> {
    let mut codec = [0];
    stored.read_exact(&mut codec).map_err(ObjectError::Io)?;
    let mut reader: BoxReader = match codec[0] {
        UNCOMPRESSED => Box::new(stored),
        ZSTD => {
            let mut decoder = zstd::stream::read::Decoder::new(stored).map_err(ObjectError::Io)?;
            decoder
                .window_log_max(MAX_ZSTD_WINDOW_LOG)
                .map_err(ObjectError::Io)?;
            Box::new(decoder)
        }
        value => return Err(ObjectError::UnknownCodec(value)),
    };
    let mut header = Vec::new();
    for _ in 0..128 {
        let mut byte = [0];
        if reader.read(&mut byte).map_err(ObjectError::Io)? == 0 {
            break;
        }
        if byte[0] == 0 {
            return Ok((reader, header));
        }
        header.push(byte[0]);
    }
    Err(ObjectError::InvalidFrame)
}

fn decode_header(header: &[u8], expected: ObjectKind) -> Result<u64, ObjectError> {
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
    std::str::from_utf8(length)
        .map_err(|_| ObjectError::InvalidFrame)?
        .parse()
        .map_err(|_| ObjectError::InvalidFrame)
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

/// Adds a codec marker and stores a framed object, probing likely savings before compression.
pub fn compress(framed: &[u8]) -> Result<Vec<u8>, ObjectError> {
    let body_start = framed
        .iter()
        .position(|byte| *byte == 0)
        .map(|index| index + 1);
    if body_start.is_some_and(|start| {
        ALREADY_COMPRESSED_MAGIC
            .iter()
            .any(|magic| framed[start..].starts_with(magic))
    }) || (framed.len() > COMPRESSION_PROBE_MIN_SIZE
        && !compression_probe_is_worthwhile(framed)?)
    {
        return store_uncompressed(framed);
    }

    let mut encoder = zstd::Encoder::new(Vec::new(), 3).map_err(ObjectError::Io)?;
    encoder.write_all(framed).map_err(ObjectError::Io)?;
    let compressed = encoder.finish().map_err(ObjectError::Io)?;
    if compressed.len() < framed.len() {
        let mut stored = Vec::with_capacity(compressed.len() + 1);
        stored.push(ZSTD);
        stored.extend_from_slice(&compressed);
        Ok(stored)
    } else {
        store_uncompressed(framed)
    }
}

fn store_uncompressed(framed: &[u8]) -> Result<Vec<u8>, ObjectError> {
    let mut stored = Vec::with_capacity(framed.len() + 1);
    stored.push(UNCOMPRESSED);
    stored.extend_from_slice(framed);
    Ok(stored)
}

pub(crate) fn already_compressed(bytes: &[u8]) -> bool {
    ALREADY_COMPRESSED_MAGIC
        .iter()
        .any(|magic| bytes.starts_with(magic))
}

pub(crate) fn compression_probe_is_worthwhile(framed: &[u8]) -> Result<bool, ObjectError> {
    let sample_size = COMPRESSION_SAMPLE_SIZE.min(framed.len() / 3);
    let starts = [
        0,
        (framed.len() - sample_size) / 2,
        framed.len() - sample_size,
    ];
    compression_samples_are_worthwhile(starts.map(|start| &framed[start..start + sample_size]))
}

pub(crate) fn compression_samples_are_worthwhile(samples: [&[u8]; 3]) -> Result<bool, ObjectError> {
    let mut sample_bytes = 0;
    let mut compressed_bytes = 0;
    for sample in samples {
        let compressed = zstd::encode_all(sample, 1).map_err(ObjectError::Io)?;
        sample_bytes += sample.len();
        compressed_bytes += compressed.len();
    }
    Ok(compressed_bytes * 100 < sample_bytes * (100 - COMPRESSION_PROBE_MIN_SAVING_PERCENT))
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

/// A validated identity, encoded as `username` or `username <email>`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Author {
    username: String,
    email: Option<String>,
}

impl Author {
    pub fn new(username: &str, email: Option<&str>) -> Result<Self, ObjectError> {
        validate_value(username)?;
        if username.trim().is_empty() || username.contains(['<', '>']) {
            return Err(ObjectError::InvalidObject);
        }
        if let Some(email) = email {
            validate_value(email)?;
            if email.split('@').count() != 2
                || email.split('@').any(str::is_empty)
                || email.chars().any(char::is_whitespace)
                || email.contains(['<', '>'])
            {
                return Err(ObjectError::InvalidObject);
            }
        }
        Ok(Self {
            username: username.into(),
            email: email.map(String::from),
        })
    }

    pub fn anonymous() -> Self {
        Self {
            username: "anonymous".into(),
            email: None,
        }
    }

    pub fn username(&self) -> &str {
        &self.username
    }
    pub fn email(&self) -> Option<&str> {
        self.email.as_deref()
    }
}

impl fmt::Display for Author {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.email {
            Some(email) => write!(f, "{} <{email}>", self.username),
            None => write!(f, "{}", self.username),
        }
    }
}

impl FromStr for Author {
    type Err = ObjectError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Some((username, email)) = value
            .strip_suffix('>')
            .and_then(|value| value.rsplit_once(" <"))
        {
            Self::new(username, Some(email))
        } else {
            Self::new(value, None)
        }
    }
}

/// A UTC second in the proleptic Gregorian calendar, years 0000 through 9999.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommitTime(i64);

impl CommitTime {
    pub fn from_unix_seconds(seconds: i64) -> Result<Self, ObjectError> {
        // These bounds keep both the arithmetic and the four-digit year canonical.
        if !(-62_167_219_200..=253_402_300_799).contains(&seconds) {
            return Err(ObjectError::InvalidObject);
        }
        Ok(Self(seconds))
    }

    pub fn unix_seconds(self) -> i64 {
        self.0
    }
}

// Civil dates are decomposed into Gregorian 400-year eras, starting in March.
fn civil_date(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day = days.rem_euclid(146_097);
    let year = (day - day / 1_460 + day / 36_524 - day / 146_096) / 365;
    let ordinal = day - (365 * year + year / 4 - year / 100);
    let month = (5 * ordinal + 2) / 153;
    let date = ordinal - (153 * month + 2) / 5 + 1;
    let month = month + if month < 10 { 3 } else { -9 };
    (year + era * 400 + i64::from(month <= 2), month, date)
}

impl fmt::Display for CommitTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (year, month, day) = civil_date(self.0.div_euclid(86_400));
        let second = self.0.rem_euclid(86_400);
        write!(
            f,
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
            second / 3_600,
            second / 60 % 60,
            second % 60
        )
    }
}

impl FromStr for CommitTime {
    type Err = ObjectError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 20 || !value.is_ascii() {
            return Err(ObjectError::InvalidObject);
        }
        let number = |range: std::ops::Range<usize>| -> Result<i64, ObjectError> {
            let text = &value[range];
            if !text.bytes().all(|b| b.is_ascii_digit()) {
                return Err(ObjectError::InvalidObject);
            }
            text.parse().map_err(|_| ObjectError::InvalidObject)
        };
        let year = number(0..4)?;
        let month = number(5..7)?;
        let day = number(8..10)?;
        let hour = number(11..13)?;
        let minute = number(14..16)?;
        let second = number(17..19)?;
        if !(1..=12).contains(&month)
            || !(1..=31).contains(&day)
            || hour > 23
            || minute > 59
            || second > 59
        {
            return Err(ObjectError::InvalidObject);
        }
        let year = year - i64::from(month <= 2);
        let era = year.div_euclid(400);
        let year = year.rem_euclid(400);
        let month = month + if month > 2 { -3 } else { 9 };
        let ordinal = (153 * month + 2) / 5 + day - 1;
        let days = era * 146_097 + year * 365 + year / 4 - year / 100 + ordinal - 719_468;
        let time = Self::from_unix_seconds(days * 86_400 + hour * 3_600 + minute * 60 + second)?;
        if time.to_string() != value {
            return Err(ObjectError::InvalidObject);
        }
        Ok(time)
    }
}

/// A canonical commit body. Headers end in LF; a nonempty body is preserved byte for byte.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Commit {
    pub content: ObjectId,
    pub parents: Vec<ObjectId>,
    pub author: Author,
    pub time: CommitTime,
    pub message: String,
    pub body: Option<String>,
    pub group: Option<String>,
}

impl Commit {
    pub fn encode_body(&self) -> Result<Vec<u8>, ObjectError> {
        validate_message(&self.message)?;
        if let Some(group) = &self.group {
            validate_value(group)?;
        }
        let mut text = format!("content {}\n", self.content);
        for parent in &self.parents {
            text.push_str(&format!("parent {parent}\n"));
        }
        text.push_str(&format!(
            "author {}\ntime {}\nmessage {}\n",
            self.author, self.time, self.message
        ));
        if let Some(group) = &self.group {
            text.push_str(&format!("group {group}\n"));
        }
        if let Some(body) = self.body.as_ref().filter(|body| !body.is_empty()) {
            text.push('\n');
            text.push_str(body);
        }
        Ok(text.into_bytes())
    }

    pub fn parse_body(body: &[u8]) -> Result<Self, ObjectError> {
        let text = std::str::from_utf8(body).map_err(|_| ObjectError::InvalidObject)?;
        let (headers, body_text) = match text.split_once("\n\n") {
            Some((headers, body)) => (headers, Some(body.to_owned())),
            None => (
                text.strip_suffix('\n').ok_or(ObjectError::InvalidObject)?,
                None,
            ),
        };
        let mut lines = headers.split('\n');
        let content = field(lines.next(), "content ")?.parse()?;
        let mut parents = Vec::new();
        let mut line = lines.next();
        while let Some(value) = line.and_then(|line| line.strip_prefix("parent ")) {
            parents.push(value.parse()?);
            line = lines.next();
        }
        let author = field(line, "author ")?.parse()?;
        let time = field(lines.next(), "time ")?.parse()?;
        let message = field(lines.next(), "message ")?.to_owned();
        let group = lines
            .next()
            .map(|line| field(Some(line), "group ").map(str::to_owned))
            .transpose()?;
        if lines.next().is_some() {
            return Err(ObjectError::InvalidObject);
        }
        let commit = Self {
            content,
            parents,
            author,
            time,
            message,
            body: body_text,
            group,
        };
        if commit.encode_body()? != body {
            return Err(ObjectError::InvalidObject);
        }
        Ok(commit)
    }

    pub fn encode(&self) -> Result<Vec<u8>, ObjectError> {
        Ok(encode(ObjectKind::Commit, &self.encode_body()?))
    }

    pub fn parse(framed: &[u8], limit: u64) -> Result<Self, ObjectError> {
        Self::parse_body(&decode(framed, ObjectKind::Commit, limit)?)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tag {
    pub target: ObjectId,
    pub name: String,
    pub tagger: Author,
    pub time: CommitTime,
    pub message: String,
}

impl Tag {
    pub fn encode_body(&self) -> Result<Vec<u8>, ObjectError> {
        crate::refs::RefName::parse(&self.name).map_err(|_| ObjectError::InvalidObject)?;
        validate_value(&self.message)?;
        Ok(format!(
            "target {}\nname {}\ntagger {}\ntime {}\nmessage {}\n",
            self.target, self.name, self.tagger, self.time, self.message
        )
        .into_bytes())
    }

    pub fn parse_body(body: &[u8]) -> Result<Self, ObjectError> {
        let text = std::str::from_utf8(body).map_err(|_| ObjectError::InvalidObject)?;
        let mut lines = text.split_terminator('\n');
        let tag = Self {
            target: field(lines.next(), "target ")?.parse()?,
            name: field(lines.next(), "name ")?.into(),
            tagger: field(lines.next(), "tagger ")?.parse()?,
            time: field(lines.next(), "time ")?.parse()?,
            message: field(lines.next(), "message ")?.into(),
        };
        if lines.next().is_some() || tag.encode_body()? != body {
            return Err(ObjectError::InvalidObject);
        }
        Ok(tag)
    }

    pub fn encode(&self) -> Result<Vec<u8>, ObjectError> {
        Ok(encode(ObjectKind::Tag, &self.encode_body()?))
    }
    pub fn parse(framed: &[u8], limit: u64) -> Result<Self, ObjectError> {
        Self::parse_body(&decode(framed, ObjectKind::Tag, limit)?)
    }
}

fn field<'a>(line: Option<&'a str>, prefix: &str) -> Result<&'a str, ObjectError> {
    line.and_then(|line| line.strip_prefix(prefix))
        .ok_or(ObjectError::InvalidObject)
}

fn validate_message(value: &str) -> Result<(), ObjectError> {
    validate_value(value)?;
    if value.is_empty() {
        return Err(ObjectError::InvalidObject);
    }
    Ok(())
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
    Truncated,
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
            Self::Truncated => write!(f, "object body is truncated"),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(body: Option<&str>) -> Commit {
        Commit {
            content: hash_blob(b"content"),
            parents: vec![hash_blob(b"parent")],
            author: Author::new("Alice", Some("a@example.test")).unwrap(),
            time: CommitTime::from_unix_seconds(0).unwrap(),
            message: "subject".into(),
            body: body.map(str::to_owned),
            group: Some("group-1".into()),
        }
    }

    #[test]
    fn framed_objects_round_trip() {
        for kind in [ObjectKind::Blob, ObjectKind::Commit, ObjectKind::Tag] {
            for body in [b"".as_slice(), b"hello\0world\n".as_slice()] {
                assert_eq!(decode(&encode(kind, body), kind, 100).unwrap(), body);
            }
        }
    }

    #[test]
    fn wrong_kind_is_refused() {
        assert!(matches!(
            decode(&encode(ObjectKind::Blob, b"x"), ObjectKind::Tag, 10),
            Err(ObjectError::InvalidFrame)
        ));
    }

    #[test]
    fn wrong_length_is_refused() {
        for framed in [b"blob 2\0x".as_slice(), b"blob 0\0x", b"blob 01\0x"] {
            assert!(matches!(
                decode(framed, ObjectKind::Blob, 10),
                Err(ObjectError::InvalidFrame)
            ));
        }
    }

    #[test]
    fn decode_honors_limit() {
        assert!(matches!(
            decode(&encode(ObjectKind::Blob, b"ab"), ObjectKind::Blob, 1),
            Err(ObjectError::LimitExceeded)
        ));
    }

    #[test]
    fn author_refuses_delimiters_and_newlines() {
        for name in ["a<b", "a>b", "a\nb"] {
            assert!(Author::new(name, None).is_err());
        }
        for email in ["a<b@example.test", "a>b@example.test", "a\nb@example.test"] {
            assert!(Author::new("Alice", Some(email)).is_err());
        }
    }

    #[test]
    fn anonymous_has_no_email() {
        let author = Author::anonymous();
        assert_eq!(author.username(), "anonymous");
        assert_eq!(author.email(), None);
        assert_eq!(author.to_string(), "anonymous");
    }

    #[test]
    fn commit_body_round_trip() {
        let original = commit(Some("First paragraph.\n\nSecond paragraph.\n"));
        assert_eq!(
            Commit::parse_body(&original.encode_body().unwrap()).unwrap(),
            original
        );
    }

    #[test]
    fn empty_commit_body_is_none() {
        let original = commit(Some(""));
        assert_eq!(
            Commit::parse_body(&original.encode_body().unwrap()).unwrap(),
            commit(None)
        );
    }

    #[test]
    fn tag_body_round_trip() {
        let tag = Tag {
            target: hash_blob(b"commit"),
            name: "v1".into(),
            tagger: Author::anonymous(),
            time: CommitTime::from_unix_seconds(0).unwrap(),
            message: "release".into(),
        };
        assert_eq!(Tag::parse_body(&tag.encode_body().unwrap()).unwrap(), tag);
    }

    #[test]
    fn utc_time_text_is_canonical() {
        for (seconds, text) in [
            (0, "1970-01-01T00:00:00Z"),
            (-1, "1969-12-31T23:59:59Z"),
            (951782400, "2000-02-29T00:00:00Z"),
        ] {
            let time = CommitTime::from_unix_seconds(seconds).unwrap();
            assert_eq!(time.to_string(), text);
            assert_eq!(text.parse::<CommitTime>().unwrap(), time);
        }
    }
}
