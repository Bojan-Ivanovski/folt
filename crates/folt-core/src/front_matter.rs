//! Lossless, bounded front matter inspection and byte-span edits.
//!
//! This is a deliberately small YAML subset, not a general YAML parser. Foreign
//! nodes remain opaque. An edit is valid only for the bytes it was planned on;
//! plan it again after applying another edit.
use std::borrow::Cow;
use std::error::Error;
use std::fmt;
use std::ops::Range;

use crate::folio_dir::FolioId;

pub const SCAN_LIMIT: usize = 64 * 1024;
const OWNED: [&str; 3] = ["folio-id", "folio-name", "folio-references"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrontMatterError {
    Unterminated,
    Binary,
    Duplicate,
    Ambiguous,
    Unsupported,
    InvalidValue,
}
impl fmt::Display for FrontMatterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "front matter: {}",
            match self {
                Self::Unterminated => "no closing delimiter within 64 KiB",
                Self::Binary => "NUL in input prefix",
                Self::Duplicate => "duplicate key",
                Self::Ambiguous => "quoted or complex owned key",
                Self::Unsupported => "unsupported YAML construct",
                Self::InvalidValue => "invalid value",
            }
        )
    }
}
impl Error for FrontMatterError {}
type Result<T> = std::result::Result<T, FrontMatterError>;

/// A top-level node. Spans include original spelling and whitespace; `value`
/// excludes spacing after the colon and a trailing comment on its final line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Node {
    pub key: String,
    pub range: Range<usize>,
    pub key_range: Range<usize>,
    pub value: Range<usize>,
    pub comment: Range<usize>,
    pub continuation: Range<usize>,
    ambiguous: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Block {
    pub range: Range<usize>,
    pub body: Range<usize>,
    pub nodes: Vec<Node>,
    /// Bytes before the first key (comments and blank lines).
    pub preamble: Range<usize>,
    pub line_ending: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Edit {
    pub range: Range<usize>,
    pub replacement: Vec<u8>,
}

/// Applies one edit without I/O. Rejects an out-of-bounds or reversed range.
pub fn apply_edit(bytes: &[u8], edit: &Edit) -> Result<Vec<u8>> {
    if edit.range.start > edit.range.end || edit.range.end > bytes.len() {
        return Err(FrontMatterError::InvalidValue);
    }
    let mut result = Vec::with_capacity(bytes.len() - edit.range.len() + edit.replacement.len());
    result.extend_from_slice(&bytes[..edit.range.start]);
    result.extend_from_slice(&edit.replacement);
    result.extend_from_slice(&bytes[edit.range.end..]);
    Ok(result)
}

#[derive(Clone)]
struct Line {
    end: usize,
    next: usize,
}
fn line(bytes: &[u8], start: usize, end: usize) -> Line {
    let next = bytes[start..end]
        .iter()
        .position(|b| *b == b'\n')
        .map_or(end, |n| start + n + 1);
    let mut content = next;
    if content > start && bytes[content - 1] == b'\n' {
        content -= 1;
        if content > start && bytes[content - 1] == b'\r' {
            content -= 1;
        }
    }
    Line { end: content, next }
}
fn text(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes).map_err(|_| FrontMatterError::InvalidValue)
}
fn trim_end(bytes: &[u8], mut end: usize, start: usize) -> usize {
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    end
}

/// Finds a block only at the start (after an optional UTF-8 BOM). The closing
/// delimiter must start before byte 65536; its own bytes may cross that bound.
pub fn locate(bytes: &[u8]) -> Result<Option<Block>> {
    if bytes[..bytes.len().min(SCAN_LIMIT)].contains(&0) {
        return Err(FrontMatterError::Binary);
    }
    let start = if bytes.starts_with(b"\xef\xbb\xbf") {
        3
    } else {
        0
    };
    let opening = line(bytes, start, bytes.len().min(SCAN_LIMIT));
    if &bytes[start
        ..bytes[start..opening.end]
            .iter()
            .rposition(|b| !matches!(b, b' ' | b'\t'))
            .map_or(start, |n| start + n + 1)]
        != b"---"
    {
        return Ok(None);
    }
    let ending = if bytes.get(opening.end..opening.next) == Some(b"\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut pos = opening.next;
    while pos < bytes.len() && pos < SCAN_LIMIT {
        // Only inspect enough bytes past the bound to finish a delimiter.
        let current = line(bytes, pos, bytes.len().min(SCAN_LIMIT + 8));
        let content = &bytes[pos..current.end];
        if content == b"---" || content == b"..." {
            let mut block = Block {
                range: start..current.next,
                body: opening.next..pos,
                nodes: Vec::new(),
                preamble: opening.next..pos,
                line_ending: ending,
            };
            tokenize(bytes, &mut block)?;
            return Ok(Some(block));
        }
        pos = current.next;
    }
    Err(FrontMatterError::Unterminated)
}

// Scan punctuation only outside quotes/comments. This also lets wrapped flow
// mappings stay inside their node even when their lines start at column zero.
fn punctuation(s: &str, wanted: char) -> Vec<usize> {
    let mut result = Vec::new();
    let mut quote = None;
    let mut escape = false;
    let mut depth = 0i32;
    let mut comment = false;
    let chars: Vec<_> = s.char_indices().collect();
    let mut index = 0;
    while index < chars.len() {
        let (i, c) = chars[index];
        if comment {
            if c == '\n' {
                comment = false;
            }
            index += 1;
            continue;
        }
        if let Some(q) = quote {
            if escape {
                escape = false;
            } else if q == '"' && c == '\\' {
                escape = true;
            } else if c == q {
                if q == '\'' && chars.get(index + 1).is_some_and(|(_, c)| *c == '\'') {
                    index += 1;
                } else {
                    quote = None;
                }
            }
        } else if c == '#' && (i == 0 || s.as_bytes()[i - 1].is_ascii_whitespace()) {
            if wanted == '#' {
                result.push(i);
            }
            comment = true;
        } else if c == '\'' || c == '"' {
            quote = Some(c);
        } else {
            if c == wanted && depth == 0 {
                result.push(i);
            }
            if c == '{' || c == '[' {
                depth += 1;
            }
            if c == '}' || c == ']' {
                depth -= 1;
            }
        }
        index += 1;
    }
    result
}
fn colon(s: &str) -> Option<usize> {
    punctuation(s, ':').into_iter().find(|i| {
        s.as_bytes()
            .get(i + 1)
            .is_none_or(|b| b.is_ascii_whitespace() || *b == b'{' || *b == b'[')
    })
}
fn explicit_key(s: &str) -> Option<&str> {
    s.strip_prefix('?')
        .filter(|tail| tail.is_empty() || tail.starts_with([' ', '\t']))
}
fn tokenize(bytes: &[u8], block: &mut Block) -> Result<()> {
    let mut pos = block.body.start;
    while pos < block.body.end {
        let current = line(bytes, pos, block.body.end);
        let raw = text(&bytes[pos..current.end])?;
        let top = !raw.is_empty() && !raw.starts_with([' ', '\t', '#']) && !raw.starts_with("- ");
        if top {
            let key_end = colon(raw);
            let complex = explicit_key(raw);
            if key_end.is_some() || complex.is_some() {
                if let Some(previous) = block.nodes.last_mut() {
                    previous.range.end = pos;
                    previous.continuation.end = pos;
                } else {
                    block.preamble.end = pos;
                }
                let end = key_end
                    .unwrap_or_else(|| punctuation(raw, '#').first().copied().unwrap_or(raw.len()));
                let spelling = raw[..end].trim_end();
                let key = if let Some(key) = explicit_key(spelling) {
                    scalar(key.trim())
                        .map(Cow::into_owned)
                        .unwrap_or_else(|_| key.trim().into())
                } else {
                    scalar(spelling)
                        .map(Cow::into_owned)
                        .unwrap_or_else(|_| spelling.into())
                };
                let mut value_start = pos + key_end.map_or(raw.len(), |n| n + 1);
                while value_start < current.end && matches!(bytes[value_start], b' ' | b'\t') {
                    value_start += 1;
                }
                let comments = punctuation(text(&bytes[value_start..current.end])?, '#');
                let comment = comments.first().map_or(current.end, |n| value_start + n);
                let value_end = trim_end(bytes, comment, value_start);
                block.nodes.push(Node {
                    key,
                    key_range: pos..pos + end,
                    range: pos..block.body.end,
                    value: value_start..value_end,
                    comment: comment..current.end,
                    continuation: current.next..block.body.end,
                    ambiguous: complex.is_some() || spelling.starts_with(['\'', '"']),
                });
                // A flow value may wrap over several lines. Its final closing
                // brace, rather than its first line, determines the value end.
                if matches!(bytes.get(value_start), Some(b'{' | b'[')) {
                    let tail = text(&bytes[value_start..block.body.end])?;
                    if let Some(end) = flow_end(tail) {
                        let absolute = value_start + end;
                        let final_line = line(bytes, absolute, block.body.end);
                        let node = block.nodes.last_mut().unwrap();
                        node.value.end = absolute;
                        node.comment = absolute..final_line.end;
                        node.continuation.start = final_line.next;
                        pos = final_line.next;
                        continue;
                    }
                }
            }
        }
        pos = current.next;
    }
    Ok(())
}
fn flow_end(s: &str) -> Option<usize> {
    let closing = if s.starts_with('{') { '}' } else { ']' };
    punctuation(&s[1..], closing).first().map(|n| n + 2)
}

fn node<'a>(block: &'a Block, key: &str) -> Result<Option<&'a Node>> {
    let nodes: Vec<_> = block.nodes.iter().filter(|n| n.key == key).collect();
    if nodes.len() > 1 {
        return Err(FrontMatterError::Duplicate);
    }
    if nodes.first().is_some_and(|n| n.ambiguous) {
        return Err(FrontMatterError::Ambiguous);
    }
    Ok(nodes.first().copied())
}
fn supported(bytes: &[u8], node: &Node) -> Result<()> {
    let rest = text(&bytes[node.value.start..node.range.end])?;
    for l in rest.lines().skip(1) {
        if l.starts_with('\t') || l.trim_start_matches(' ').starts_with('\t') {
            return Err(FrontMatterError::Unsupported);
        }
    }
    // YAML properties are meaningful only as tokens, never inside quotes.
    for token in ['&', '*', '!'] {
        if !punctuation(rest, token).is_empty() {
            return Err(FrontMatterError::Unsupported);
        }
    }
    Ok(())
}
fn scalar(s: &str) -> Result<Cow<'_, str>> {
    let s = s.trim();
    if s.starts_with(['&', '*', '!', '|', '>', '[', '{']) {
        return Err(FrontMatterError::Unsupported);
    }
    if let Some(inner) = s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        return Ok(Cow::Owned(inner.replace("''", "'")));
    }
    if let Some(inner) = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        let mut out = String::new();
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            out.push(if c == '\\' {
                match chars.next() {
                    Some('"') => '"',
                    Some('\\') => '\\',
                    Some('n') => '\n',
                    Some('t') => '\t',
                    Some('r') => '\r',
                    _ => return Err(FrontMatterError::InvalidValue),
                }
            } else {
                c
            });
        }
        return Ok(Cow::Owned(out));
    }
    if s.contains(['\n', '\r', '\'', '"'])
        || s.starts_with(['?', ':'])
        || s.strip_prefix('-')
            .is_some_and(|tail| tail.is_empty() || tail.starts_with(char::is_whitespace))
    {
        return Err(FrontMatterError::InvalidValue);
    }
    Ok(Cow::Borrowed(s))
}
fn meaningful(s: &str) -> bool {
    !s.trim().is_empty() && !s.trim_start().starts_with('#')
}
fn read_scalar<'a>(bytes: &'a [u8], key: &str) -> Result<Option<Cow<'a, str>>> {
    let Some(block) = locate(bytes)? else {
        return Ok(None);
    };
    unresolved_key_checks(bytes, &block)?;
    let Some(n) = node(&block, key)? else {
        return Ok(None);
    };
    supported(bytes, n)?;
    if text(&bytes[n.continuation.clone()])?
        .lines()
        .any(meaningful)
    {
        return Err(FrontMatterError::Unsupported);
    }
    let value = scalar(text(&bytes[n.value.clone()])?)?;
    if value.is_empty() {
        return Err(FrontMatterError::InvalidValue);
    }
    Ok(Some(value))
}

/// Reads a scalar name, decoding minimal single/double quoted forms.
pub fn read_name(bytes: &[u8]) -> Result<Option<Cow<'_, str>>> {
    read_scalar(bytes, "folio-name")
}
/// Reads the canonical lowercase hyphenated Folio id.
pub fn read_id(bytes: &[u8]) -> Result<Option<FolioId>> {
    read_scalar(bytes, "folio-id")?
        .map(|s| FolioId::parse(&s).map_err(|_| FrontMatterError::InvalidValue))
        .transpose()
}

/// An alias and its optional name, canonical id and opaque pin. Name grammar
/// and commit validation belong to callers, not this YAML subset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reference<'a> {
    pub alias: Cow<'a, str>,
    pub name: Option<Cow<'a, str>>,
    pub id: Option<FolioId>,
    pub pin: Option<Cow<'a, str>>,
}
struct ReferenceNode<'a> {
    reference: Reference<'a>,
    range: Range<usize>,
    value: Range<usize>,
    nested: bool,
}
fn mapping(s: &str) -> Result<Vec<(&str, &str, usize)>> {
    let inner = s
        .trim()
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .ok_or(FrontMatterError::InvalidValue)?;
    let base = s.find('{').unwrap() + 1;
    let mut starts = vec![0];
    starts.extend(punctuation(inner, ',').into_iter().map(|n| n + 1));
    let mut result = Vec::new();
    for (i, start) in starts.iter().copied().enumerate() {
        let end = starts.get(i + 1).map_or(inner.len(), |n| n - 1);
        let part = &inner[start..end];
        // Remove comments without changing offsets used by the block editor.
        let mut clean = part.trim();
        while clean.starts_with('#') {
            clean = clean.split_once('\n').map_or("", |(_, tail)| tail.trim());
        }
        if clean.is_empty() {
            continue;
        }
        let c = colon(clean).ok_or(FrontMatterError::InvalidValue)?;
        let value = clean[c + 1..].trim();
        let end = punctuation(value, '#')
            .first()
            .copied()
            .unwrap_or(value.len());
        result.push((clean[..c].trim(), value[..end].trim(), base + start));
    }
    Ok(result)
}
fn reference<'a>(alias: &'a str, value: &'a str) -> Result<Reference<'a>> {
    let alias = scalar(alias)?;
    if alias.is_empty() {
        return Err(FrontMatterError::InvalidValue);
    }
    let mut result = Reference {
        alias,
        name: None,
        id: None,
        pin: None,
    };
    if value.trim_start().starts_with('{') {
        let mut seen = Vec::new();
        for (key, value, _) in mapping(value)? {
            if seen.contains(&key) {
                return Err(FrontMatterError::Duplicate);
            }
            seen.push(key);
            let value = scalar(value)?;
            if value.is_empty() {
                return Err(FrontMatterError::InvalidValue);
            }
            match key {
                "name" => result.name = Some(value),
                "id" => {
                    result.id =
                        Some(FolioId::parse(&value).map_err(|_| FrontMatterError::InvalidValue)?)
                }
                "pin" => result.pin = Some(value),
                _ => return Err(FrontMatterError::Unsupported),
            }
        }
    } else {
        let value = scalar(value)?;
        let (target, pin) = match value.split_once('@') {
            Some((target, pin)) if !pin.is_empty() && !pin.contains('@') => (target, Some(pin)),
            Some(_) => return Err(FrontMatterError::InvalidValue),
            None => (value.as_ref(), None),
        };
        if target.is_empty() {
            return Err(FrontMatterError::InvalidValue);
        }
        if let Ok(id) = FolioId::parse(target) {
            result.id = Some(id);
        } else {
            result.name = Some(Cow::Owned(target.into()));
        }
        result.pin = pin.map(|s| Cow::Owned(s.into()));
    }
    if result.name.is_none() && result.id.is_none() {
        return Err(FrontMatterError::InvalidValue);
    }
    Ok(result)
}
fn references<'a>(bytes: &'a [u8], n: &Node) -> Result<Vec<ReferenceNode<'a>>> {
    supported(bytes, n)?;
    let value = text(&bytes[n.value.clone()])?;
    let mut result = Vec::new();
    if value.starts_with('{') {
        let suffix = text(&bytes[n.comment.clone()])?.trim();
        if !suffix.is_empty() && !suffix.starts_with('#') {
            return Err(FrontMatterError::Unsupported);
        }
        if text(&bytes[n.continuation.clone()])?
            .lines()
            .any(meaningful)
        {
            return Err(FrontMatterError::Unsupported);
        }
        for (key, value, _) in mapping(value)? {
            result.push(ReferenceNode {
                reference: reference(key, value)?,
                range: n.value.clone(),
                value: n.value.clone(),
                nested: false,
            });
        }
    } else {
        if !value.is_empty() {
            return Err(FrontMatterError::Unsupported);
        }
        let mut pos = n.continuation.start;
        let mut indent = None;
        while pos < n.range.end {
            let l = line(bytes, pos, n.range.end);
            let raw = text(&bytes[pos..l.end])?;
            if !meaningful(raw) {
                pos = l.next;
                continue;
            }
            let spaces = raw.len() - raw.trim_start_matches(' ').len();
            if spaces == 0 {
                return Err(FrontMatterError::Unsupported);
            }
            let expected = *indent.get_or_insert(spaces);
            if spaces != expected {
                return Err(FrontMatterError::Unsupported);
            }
            let raw = &raw[spaces..];
            let c = colon(raw).ok_or(FrontMatterError::InvalidValue)?;
            let start = pos + spaces + c + 1;
            let start = start
                + bytes[start..l.end]
                    .iter()
                    .take_while(|b| matches!(b, b' ' | b'\t'))
                    .count();
            let comment = punctuation(text(&bytes[start..l.end])?, '#')
                .first()
                .map_or(l.end, |i| start + i);
            let end = trim_end(bytes, comment, start);
            let mut next = l.next;
            let mut entry_end = l.next;
            let nested = start == end;
            let parsed = if nested {
                let mut fields = Vec::new();
                while next < n.range.end {
                    let child = line(bytes, next, n.range.end);
                    let raw = text(&bytes[next..child.end])?;
                    if !meaningful(raw) {
                        next = child.next;
                        continue;
                    }
                    let child_indent = raw.len() - raw.trim_start_matches(' ').len();
                    if child_indent <= spaces {
                        break;
                    }
                    let clean = raw.trim();
                    let c = colon(clean).ok_or(FrontMatterError::InvalidValue)?;
                    let value = clean[c + 1..].trim();
                    let end = punctuation(value, '#')
                        .first()
                        .copied()
                        .unwrap_or(value.len());
                    fields.push((&clean[..c], value[..end].trim()));
                    next = child.next;
                    entry_end = next;
                }
                let mut r = Reference {
                    alias: scalar(raw[..c].trim())?,
                    name: None,
                    id: None,
                    pin: None,
                };
                let mut seen = Vec::new();
                for (key, value) in fields {
                    if seen.contains(&key) {
                        return Err(FrontMatterError::Duplicate);
                    }
                    seen.push(key);
                    let value = scalar(value)?;
                    match key {
                        "name" => r.name = Some(value),
                        "id" => {
                            r.id = Some(
                                FolioId::parse(&value)
                                    .map_err(|_| FrontMatterError::InvalidValue)?,
                            )
                        }
                        "pin" => r.pin = Some(value),
                        _ => return Err(FrontMatterError::Unsupported),
                    }
                }
                if r.name.is_none() && r.id.is_none() {
                    return Err(FrontMatterError::InvalidValue);
                }
                r
            } else {
                reference(raw[..c].trim(), text(&bytes[start..end])?)?
            };
            result.push(ReferenceNode {
                reference: parsed,
                range: pos..entry_end,
                value: start..end,
                nested,
            });
            pos = next;
        }
    }
    let mut aliases = Vec::new();
    for entry in &result {
        if aliases.contains(&entry.reference.alias.as_ref()) {
            return Err(FrontMatterError::Duplicate);
        }
        aliases.push(entry.reference.alias.as_ref());
    }
    Ok(result)
}
/// Reads block or flow reference mappings; absent metadata yields no entries.
pub fn read_references(bytes: &[u8]) -> Result<Vec<Reference<'_>>> {
    let Some(block) = locate(bytes)? else {
        return Ok(Vec::new());
    };
    unresolved_key_checks(bytes, &block)?;
    let Some(n) = node(&block, "folio-references")? else {
        return Ok(Vec::new());
    };
    Ok(references(bytes, n)?
        .into_iter()
        .map(|n| n.reference)
        .collect())
}

// Resolve the entire explicit key, rather than trusting the tokenizer's
// best-effort key spelling. Only fully decoded scalar keys are safe to ignore.
fn resolved_explicit_key(bytes: &[u8], n: &Node, block: &Block) -> Result<String> {
    let ambiguous = FrontMatterError::Ambiguous;
    let spelling = text(&bytes[n.key_range.clone()])?;
    let inline = explicit_key(spelling).ok_or(ambiguous)?.trim();
    let first_line = line(bytes, n.range.start, block.body.end);
    let inline_value = colon(text(&bytes[n.range.start..first_line.end])?).is_some();
    let mut parts = Vec::new();
    if !inline.is_empty() {
        parts.push(inline);
    }
    if !inline_value {
        // The tokenizer ends this node at the next top-level key or explicit
        // value indicator. An omitted value indicator means a null value;
        // either way, only the continuation belongs to this key.
        for raw in text(&bytes[n.continuation.clone()])?.lines() {
            if !meaningful(raw) {
                continue;
            }
            if !raw.starts_with(' ') {
                return Err(ambiguous);
            }
            let end = punctuation(raw, '#').first().copied().unwrap_or(raw.len());
            let spelling = raw[..end].trim();
            if colon(spelling).is_some() {
                return Err(ambiguous);
            }
            parts.push(spelling);
        }
    }
    if parts.is_empty() || (parts.len() > 1 && parts.iter().any(|s| s.contains(['\'', '"']))) {
        return Err(ambiguous);
    }
    for part in &parts {
        // A closing quote must end the scalar. The general scalar decoder is
        // deliberately permissive about interior quotes in opaque values.
        if let Some(quote) = part.chars().next().filter(|c| matches!(c, '\'' | '"')) {
            let mut chars = part.char_indices().skip(1).peekable();
            let mut closed = false;
            while let Some((i, c)) = chars.next() {
                if quote == '"' && c == '\\' {
                    chars.next();
                } else if c == quote {
                    if quote == '\'' && chars.peek().is_some_and(|(_, c)| *c == quote) {
                        chars.next();
                    } else {
                        closed = i + c.len_utf8() == part.len();
                        break;
                    }
                }
            }
            if !closed {
                return Err(ambiguous);
            }
        } else if part.starts_with([',', ']', '}', '#', '%', '@', '`']) {
            return Err(ambiguous);
        }
        scalar(part).map_err(|_| ambiguous)?;
    }
    // Plain multiline scalars fold their line breaks into spaces.
    scalar(&parts.join(" "))
        .map(Cow::into_owned)
        .map_err(|_| ambiguous)
}

fn unresolved_key_checks(bytes: &[u8], block: &Block) -> Result<()> {
    for n in &block.nodes {
        let spelling = text(&bytes[n.key_range.clone()])?;
        if explicit_key(spelling).is_some() {
            let key = resolved_explicit_key(bytes, n, block)?;
            if OWNED.contains(&key.as_str()) {
                return Err(FrontMatterError::Ambiguous);
            }
            continue;
        }
        let spelling = spelling.trim();
        // Scalar decoding cannot resolve YAML key properties safely.
        if spelling.starts_with(['!', '&', '*']) {
            return Err(FrontMatterError::Ambiguous);
        }
        // An undecodable quoted key could hide an owned key.
        if spelling.starts_with(['\'', '"']) && scalar(spelling).is_err() {
            return Err(FrontMatterError::Ambiguous);
        }
    }
    Ok(())
}

fn edit_checks(bytes: &[u8], block: &Block) -> Result<()> {
    unresolved_key_checks(bytes, block)?;
    for n in &block.nodes {
        let spelling = text(&bytes[n.key_range.clone()])?;
        if explicit_key(spelling).is_some() {
            continue;
        }
        let spelling = spelling.trim();
        // Keep escaped quoted spellings opaque when editing foreign nodes.
        if spelling.starts_with(['\'', '"']) && spelling.contains('\\') {
            return Err(FrontMatterError::Ambiguous);
        }
    }
    for key in OWNED {
        if let Some(n) = node(block, key)? {
            supported(bytes, n)?;
        }
    }
    Ok(())
}
fn edit(range: Range<usize>, replacement: impl Into<Vec<u8>>) -> Edit {
    Edit {
        range,
        replacement: replacement.into(),
    }
}
// Replacing a value must retain YAML separation even when its original span
// started immediately after the colon (for example, an empty block header).
fn value_edit(bytes: &[u8], range: Range<usize>, value: impl AsRef<[u8]>) -> Edit {
    let mut replacement = Vec::new();
    if range.start > 0 && bytes[range.start - 1] == b':' {
        replacement.push(b' ');
    }
    replacement.extend_from_slice(value.as_ref());
    edit(range, replacement)
}
// Validate only the bounded prefix of the virtual result, including enough
// bytes past the scan limit to finish a closing delimiter.
fn checked_edit(bytes: &[u8], edit: Edit) -> Result<Option<Edit>> {
    let prefix: Vec<_> = bytes[..edit.range.start]
        .iter()
        .chain(&edit.replacement)
        .chain(&bytes[edit.range.end..])
        .take(SCAN_LIMIT + 8)
        .copied()
        .collect();
    locate(&prefix)?;
    Ok(Some(edit))
}

fn insert(
    bytes: &[u8],
    block: Option<&Block>,
    key: &str,
    value: &str,
    create: bool,
) -> Result<Option<Edit>> {
    if let Some(block) = block {
        let position = block
            .nodes
            .iter()
            .rev()
            .find(|n| OWNED.contains(&n.key.as_str()))
            .map_or(block.body.end, |n| n.range.end);
        checked_edit(
            bytes,
            edit(
                position..position,
                format!("{key}: {value}{}", block.line_ending).into_bytes(),
            ),
        )
    } else if create {
        let position = if bytes.starts_with(b"\xef\xbb\xbf") {
            3
        } else {
            0
        };
        checked_edit(
            bytes,
            edit(
                position..position,
                format!("---\n{key}: {value}\n---\n").into_bytes(),
            ),
        )
    } else {
        Ok(None)
    }
}
fn plain(value: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._/".contains(&b))
    {
        return Err(FrontMatterError::InvalidValue);
    }
    scalar(value)?;
    Ok(())
}
fn set_scalar(bytes: &[u8], key: &str, value: Option<&str>, create: bool) -> Result<Option<Edit>> {
    if let Some(value) = value {
        plain(value)?;
    }
    let block = locate(bytes)?;
    if let Some(block) = &block {
        edit_checks(bytes, block)?;
        if let Some(n) = node(block, key)? {
            read_scalar(bytes, key)?;
            return checked_edit(
                bytes,
                match value {
                    Some(value) => value_edit(bytes, n.value.clone(), value.as_bytes()),
                    None => edit(n.range.start..n.continuation.start, Vec::new()),
                },
            );
        }
    }
    value.map_or(Ok(None), |value| {
        insert(bytes, block.as_ref(), key, value, create)
    })
}
/// Sets or removes the name. `None` means no edit is needed or block creation
/// was disabled. Written strings must be safe unquoted tokens; full name
/// validation remains the caller's responsibility.
pub fn set_name(bytes: &[u8], value: Option<&str>, create_block: bool) -> Result<Option<Edit>> {
    set_scalar(bytes, "folio-name", value, create_block)
}
/// Sets or removes the id, creating a missing block only when requested.
pub fn set_id(bytes: &[u8], value: Option<FolioId>, create_block: bool) -> Result<Option<Edit>> {
    let value = value.map(|id| id.to_string());
    set_scalar(bytes, "folio-id", value.as_deref(), create_block)
}
fn render_reference(r: &Reference<'_>) -> Result<String> {
    plain(&r.alias)?;
    let mut fields = Vec::new();
    if let Some(name) = &r.name {
        plain(name)?;
        fields.push(format!("name: {name}"));
    }
    if let Some(id) = r.id {
        fields.push(format!("id: {id}"));
    }
    if fields.is_empty() {
        return Err(FrontMatterError::InvalidValue);
    }
    if let Some(pin) = &r.pin {
        plain(pin)?;
        fields.push(format!("pin: {pin}"));
    }
    Ok(format!("{{{}}}", fields.join(", ")))
}
// Standalone comments and blank lines survive removal of their surrounding key.
fn remove_references(bytes: &[u8], node: &Node) -> Edit {
    let mut replacement = Vec::new();
    let mut pos = node.continuation.start;
    while pos < node.range.end {
        let l = line(bytes, pos, node.range.end);
        if !meaningful(text(&bytes[pos..l.end]).unwrap_or("")) {
            replacement.extend_from_slice(&bytes[pos..l.next]);
        }
        pos = l.next;
    }
    edit(node.range.clone(), replacement)
}
/// Replaces the entire references value, including nested block entries.
pub fn set_references(
    bytes: &[u8],
    values: &[Reference<'_>],
    create_block: bool,
) -> Result<Option<Edit>> {
    let mut parts = Vec::new();
    let mut seen = Vec::new();
    for r in values {
        if seen.contains(&r.alias.as_ref()) {
            return Err(FrontMatterError::Duplicate);
        }
        seen.push(r.alias.as_ref());
        parts.push(format!("{}: {}", r.alias, render_reference(r)?));
    }
    let block = locate(bytes)?;
    if let Some(block) = &block {
        edit_checks(bytes, block)?;
        if let Some(n) = node(block, "folio-references")? {
            references(bytes, n)?;
            if values.is_empty() {
                return checked_edit(bytes, remove_references(bytes, n));
            }
            // Keep the header's comment; remove only meaningful continuation
            // lines. Trailing blank/comment lines remain owned by the document.
            let entries = references(bytes, n)?;
            let end = entries.last().map_or(n.value.end, |e| {
                if text(&bytes[n.value.clone()]).unwrap().starts_with('{') {
                    n.value.end
                } else {
                    e.range.end
                }
            });
            let replacement = format!("{{{}}}", parts.join(", "));
            if end <= n.value.end {
                return checked_edit(bytes, value_edit(bytes, n.value.clone(), replacement));
            }
            let header = &bytes[n.value.end..n.continuation.start];
            let mut edit = value_edit(bytes, n.value.start..end, replacement);
            edit.replacement.extend_from_slice(header);
            return checked_edit(bytes, edit);
        }
    }
    if values.is_empty() {
        return Ok(None);
    }
    insert(
        bytes,
        block.as_ref(),
        "folio-references",
        &format!("{{{}}}", parts.join(", ")),
        create_block,
    )
}
/// Sets one alias, or removes it when `value` is `None`. Nested block entries
/// are readable but require `set_references` to rewrite the whole collection.
pub fn set_reference(
    bytes: &[u8],
    alias: &str,
    value: Option<&Reference<'_>>,
    create_block: bool,
) -> Result<Option<Edit>> {
    plain(alias)?;
    if value.is_some_and(|r| r.alias != alias) {
        return Err(FrontMatterError::InvalidValue);
    }
    let rendered = value.map(render_reference).transpose()?;
    let block = locate(bytes)?;
    if let Some(block) = &block {
        edit_checks(bytes, block)?;
        if let Some(n) = node(block, "folio-references")? {
            let entries = references(bytes, n)?;
            if entries.iter().any(|e| e.nested) {
                return Err(FrontMatterError::Unsupported);
            }
            if text(&bytes[n.value.clone()])?.starts_with('{') {
                let present = entries.iter().any(|e| e.reference.alias == alias);
                if !present && value.is_none() {
                    return Ok(None);
                }
                let mut refs = Vec::new();
                for entry in entries {
                    if entry.reference.alias == alias {
                        if let Some(value) = value {
                            refs.push(value.clone());
                        }
                    } else {
                        refs.push(entry.reference);
                    }
                }
                if !present && let Some(value) = value {
                    refs.push(value.clone());
                }
                return set_references(bytes, &refs, create_block);
            }
            if let Some(entry) = entries.iter().find(|e| e.reference.alias == alias) {
                return checked_edit(
                    bytes,
                    match rendered {
                        Some(value) => value_edit(bytes, entry.value.clone(), value),
                        None if entries.len() == 1 => remove_references(bytes, n),
                        None => edit(entry.range.clone(), Vec::new()),
                    },
                );
            }
            if let Some(value) = rendered {
                let position = entries.last().map_or(n.continuation.start, |e| e.range.end);
                return checked_edit(
                    bytes,
                    edit(
                        position..position,
                        format!(
                            "{}{alias}: {value}{}",
                            " ".repeat(entries.first().map_or(2, |e| {
                                bytes[e.range.start..]
                                    .iter()
                                    .take_while(|b| **b == b' ')
                                    .count()
                            })),
                            block.line_ending
                        )
                        .into_bytes(),
                    ),
                );
            }
            return Ok(None);
        }
    }
    match value {
        Some(value) => set_references(bytes, std::slice::from_ref(value), create_block),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "12345678-1234-4234-8234-123456789abc";
    fn logo() -> Reference<'static> {
        Reference {
            alias: "logo".into(),
            name: Some("logo-image".into()),
            id: None,
            pin: Some("abc123".into()),
        }
    }
    fn rename(bytes: &[u8], name: &str) -> Vec<u8> {
        apply_edit(bytes, &set_name(bytes, Some(name), true).unwrap().unwrap()).unwrap()
    }

    fn assert_ambiguous_key(bytes: &[u8]) {
        let error = FrontMatterError::Ambiguous;
        assert_eq!(read_name(bytes), Err(error));
        assert_eq!(read_id(bytes), Err(error));
        assert_eq!(read_references(bytes), Err(error));
        for name in [Some("new"), None] {
            assert_eq!(set_name(bytes, name, false), Err(error));
        }
        for id in [Some(FolioId::parse(ID).unwrap()), None] {
            assert_eq!(set_id(bytes, id, false), Err(error));
        }
        assert_eq!(set_references(bytes, &[logo()], false), Err(error));
        assert_eq!(set_references(bytes, &[], false), Err(error));
        assert_eq!(
            set_reference(bytes, "logo", Some(&logo()), false),
            Err(error)
        );
        assert_eq!(set_reference(bytes, "logo", None, false), Err(error));
    }

    #[test]
    fn foreign_null_explicit_keys_remain_readable_and_untouched() {
        for ending in ["\n", "\r\n"] {
            for foreign in ["? other\n", "?\n  other\n"] {
                let foreign = foreign.replace('\n', ending);
                let prefix = format!("---{ending}{foreign}");
                for (key, value) in [
                    ("folio-name", "old".to_owned()),
                    ("folio-id", ID.to_owned()),
                    ("folio-references", "{logo: old-image}".to_owned()),
                ] {
                    let input = format!("{prefix}{key}: {value}{ending}---{ending}body");
                    let bytes = input.as_bytes();
                    let id = FolioId::parse("12345678-1234-4234-8234-123456789abd").unwrap();
                    let edits = match key {
                        "folio-name" => {
                            assert_eq!(read_name(bytes).unwrap().as_deref(), Some("old"));
                            [
                                set_name(bytes, Some("new"), false),
                                set_name(bytes, None, false),
                            ]
                        }
                        "folio-id" => {
                            assert_eq!(read_id(bytes).unwrap(), Some(FolioId::parse(ID).unwrap()));
                            [set_id(bytes, Some(id), false), set_id(bytes, None, false)]
                        }
                        _ => {
                            let refs = read_references(bytes).unwrap();
                            assert_eq!(refs.len(), 1);
                            assert_eq!(refs[0].alias, "logo");
                            assert_eq!(refs[0].name.as_deref(), Some("old-image"));
                            [
                                set_references(bytes, &[logo()], false),
                                set_references(bytes, &[], false),
                            ]
                        }
                    };
                    for (index, edit) in edits.into_iter().enumerate() {
                        let edited = apply_edit(bytes, &edit.unwrap().unwrap()).unwrap();
                        assert!(edited.starts_with(prefix.as_bytes()));
                        let removed = index == 1;
                        match key {
                            "folio-name" => assert_eq!(
                                read_name(&edited).unwrap().as_deref(),
                                if removed { None } else { Some("new") }
                            ),
                            "folio-id" => assert_eq!(
                                read_id(&edited).unwrap(),
                                if removed { None } else { Some(id) }
                            ),
                            _ => assert_eq!(
                                read_references(&edited).unwrap(),
                                if removed { vec![] } else { vec![logo()] }
                            ),
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn owned_or_unresolved_null_explicit_keys_remain_ambiguous() {
        for ending in ["\n", "\r\n"] {
            for key in [
                "folio-name",
                "folio-id",
                "folio-references",
                "!!str other",
                "&a other",
                "*a",
                "[other]",
                "\"unterminated",
            ] {
                for foreign in [format!("? {key}\n"), format!("?\n  {key}\n")] {
                    let input =
                        format!("---\n{foreign}folio-name: old\n---\n").replace('\n', ending);
                    assert_ambiguous_key(input.as_bytes());
                }
            }
            assert_ambiguous_key(format!("---{ending}? folio-name{ending}---{ending}").as_bytes());
        }
    }

    #[test]
    fn foreign_multiline_explicit_keys_remain_readable_and_untouched() {
        for ending in ["\n", "\r\n"] {
            for key in [
                "other",
                "'other'",
                "\"other\"",
                "\"other\\nkey\"",
                r"'\u0066olio-name'",
                "'other''key'",
                "other\n  key",
            ] {
                for foreign in [
                    format!("? {key}\n: value\n"),
                    format!("?\n  {key}\n: value\n"),
                    format!("? {key} # key\n\n# before value\n: value\n"),
                    format!("? # key\n\n# before key\n  {key}\n\n# before value\n: value\n"),
                ] {
                    let foreign = foreign.replace('\n', ending);
                    let bytes = format!(
                        "---{ending}{foreign}folio-name: name{ending}folio-id: {ID}{ending}folio-references: {{logo: logo-image@abc123}}{ending}---{ending}body"
                    );
                    let bytes = bytes.as_bytes();
                    assert_eq!(read_name(bytes).unwrap().as_deref(), Some("name"));
                    assert_eq!(read_id(bytes).unwrap(), Some(FolioId::parse(ID).unwrap()));
                    assert_eq!(read_references(bytes).unwrap(), vec![logo()]);
                    let edits = [
                        set_name(bytes, Some("new"), false),
                        set_name(bytes, None, false),
                        set_id(bytes, Some(FolioId::parse(ID).unwrap()), false),
                        set_id(bytes, None, false),
                        set_references(bytes, &[logo()], false),
                        set_references(bytes, &[], false),
                        set_reference(bytes, "logo", Some(&logo()), false),
                        set_reference(bytes, "logo", None, false),
                    ];
                    for edit in edits {
                        let edited = apply_edit(bytes, &edit.unwrap().unwrap()).unwrap();
                        assert!(edited.starts_with(format!("---{ending}{foreign}").as_bytes()));
                    }
                    assert_eq!(
                        rename(bytes, "new"),
                        text(bytes)
                            .unwrap()
                            .replace("folio-name: name", "folio-name: new")
                            .as_bytes()
                    );
                }
            }
            // Ordinary foreign keys, including quoted property characters,
            // remain opaque and retain their spelling during edits.
            let bytes = format!(
                "---{ending}other: x{ending}'&a folio-name': x{ending}\"!!str folio-name\": x{ending}folio-name: name{ending}---{ending}"
            );
            assert_eq!(
                read_name(bytes.as_bytes()).unwrap().as_deref(),
                Some("name")
            );
            assert_eq!(
                rename(bytes.as_bytes(), "new"),
                bytes
                    .replace("folio-name: name", "folio-name: new")
                    .as_bytes()
            );
        }
    }

    #[test]
    fn refuses_owned_or_unresolved_multiline_explicit_keys() {
        for ending in ["\n", "\r\n"] {
            for key in OWNED {
                for spelling in [key.to_owned(), format!("'{key}'"), format!("\"{key}\"")] {
                    let bytes =
                        format!("---{ending}?{ending}  {spelling}{ending}: old{ending}---{ending}");
                    assert_ambiguous_key(bytes.as_bytes());
                }
            }
            for body in [
                "?\n: old\n",
                "?\n",
                "?\n  !!str other\n: old\n",
                "?\n  &a other\n: old\n",
                "?\n  *a\n: old\n",
                "?\n  \"\\u0066olio-name\"\n: old\n",
                "?\n  \"folio-\n  name\"\n: old\n",
            ] {
                let bytes = format!("---\n{body}---\n").replace('\n', ending);
                assert_ambiguous_key(bytes.as_bytes());
            }
        }
    }

    #[test]
    fn explicit_keys_require_fully_decoded_foreign_scalars() {
        for ending in ["\n", "\r\n"] {
            for key in OWNED {
                for spelling in [
                    key.to_owned(),
                    format!("'{key}'"),
                    format!("\"{key}\""),
                    format!("|-\n  {key}"),
                    format!(">-\n  {key}"),
                    format!("[{key}]"),
                    format!("{{{key}: other}}"),
                    format!("!!str {key}"),
                    format!("&a {key}"),
                    format!("*{key}"),
                    format!("'other' '{key}'"),
                    format!("\"other\" \"{key}\""),
                ] {
                    for body in [
                        format!("? {spelling}\n: old\n"),
                        format!("? {spelling}\n# before value\n\n: old\n"),
                        format!("?\n  {spelling}\n: old\n"),
                        format!("? # key\n\n# before key\n  {spelling}\n\n# before value\n: old\n"),
                    ] {
                        let input = format!("---\n{body}---\n").replace('\n', ending);
                        assert_ambiguous_key(input.as_bytes());
                    }
                    // Also exercise the inline value indicator for single-line keys.
                    if !spelling.contains('\n') {
                        let input = format!("---\n? {spelling}: old\n---\n").replace('\n', ending);
                        assert_ambiguous_key(input.as_bytes());
                    }
                }
            }
            for spelling in ["[other]", "{other: value}", "|-\n  other", ">-\n  other"] {
                let input = format!("---\n? {spelling}\n: old\n---\n").replace('\n', ending);
                assert_ambiguous_key(input.as_bytes());
            }
        }
    }

    #[test]
    fn refuses_key_properties_before_owned_key_lookup() {
        for ending in ["\n", "\r\n"] {
            for key in OWNED {
                for property in ["!!str", "!custom", "&a", "*a"] {
                    for spelling in [key.to_owned(), format!("'{key}'"), format!("\"{key}\"")] {
                        for body in [
                            format!("? {property} {spelling}\n: old\n"),
                            format!("? {property} {spelling}: old\n"),
                            format!("{property} {spelling}: old\n"),
                            format!("?\n  {property} {spelling}\n: old\n"),
                        ] {
                            let bytes = format!("---\n{body}---\n").replace('\n', ending);
                            assert_ambiguous_key(bytes.as_bytes());
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn reads_all_reference_forms() {
        let forms = [
            "folio-references:\n  logo: logo-image\n".to_owned(),
            "folio-references: {logo: logo-image}\n".to_owned(),
            "folio-references: {\n  logo: logo-image\n}\n".to_owned(),
            "folio-references:\n  logo: logo-image@abc123\n".to_owned(),
            format!("folio-references:\n  logo: {ID}\n"),
            format!("folio-references:\n  logo: {ID}@abc123\n"),
            format!("folio-references:\n  logo: {{name: logo-image, id: {ID}, pin: abc123}}\n"),
            format!(
                "folio-references:\n  logo:\n    name: logo-image\n    id: {ID}\n    pin: abc123\n"
            ),
        ];
        for form in forms {
            let bytes = format!("---\n{form}---\nbody").into_bytes();
            let refs = read_references(&bytes).unwrap();
            assert_eq!(refs.len(), 1, "{form}");
            assert_eq!(refs[0].alias, "logo");
            assert!(refs[0].name.is_some() || refs[0].id.is_some());
            if form.contains("abc123") {
                assert_eq!(refs[0].pin.as_deref(), Some("abc123"));
            }
            if form.contains("    name:") {
                assert_eq!(
                    set_reference(&bytes, "logo", Some(&logo()), false),
                    Err(FrontMatterError::Unsupported)
                );
                let edited = apply_edit(
                    &bytes,
                    &set_references(&bytes, &[logo()], false).unwrap().unwrap(),
                )
                .unwrap();
                assert_eq!(read_references(&edited).unwrap(), vec![logo()]);
            } else {
                let edited = apply_edit(
                    &bytes,
                    &set_reference(&bytes, "logo", Some(&logo()), false)
                        .unwrap()
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(read_references(&edited).unwrap(), vec![logo()]);
                let twice = apply_edit(
                    &edited,
                    &set_reference(&edited, "logo", Some(&logo()), false)
                        .unwrap()
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(edited, twice);
            }
        }
        for form in ["folio-references:\n", "folio-references: {}\n"] {
            let bytes = format!("---\n{form}---\n");
            assert!(read_references(bytes.as_bytes()).unwrap().is_empty());
            let edited = apply_edit(
                bytes.as_bytes(),
                &set_reference(bytes.as_bytes(), "logo", Some(&logo()), false)
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(read_references(&edited).unwrap(), vec![logo()]);
        }
    }

    #[test]
    fn preserves_bytes_and_owned_line_trivia() {
        for ending in ["\n", "\r\n"] {
            let bytes = format!("\u{feff}---{ending}title: |{ending}  ---{ending}tags:{ending}- a{ending}- b{ending}folio-name  :   'old'   # keep{ending}# separate{ending}other: yes{ending}...{ending}body\0").into_bytes();
            // Binary detection covers the prefix, including the body.
            assert_eq!(locate(&bytes), Err(FrontMatterError::Binary));
            let bytes = &bytes[..bytes.len() - 1];
            let result = rename(bytes, "new");
            let expected = text(bytes).unwrap().replace("'old'", "new");
            assert_eq!(result, expected.as_bytes());
            assert_eq!(rename(&result, "new"), result);
            let block = locate(bytes).unwrap().unwrap();
            assert_eq!(block.nodes[1].key, "tags");
            assert!(
                text(&bytes[block.nodes[1].range.clone()])
                    .unwrap()
                    .contains("- b")
            );
        }
        let bytes = b"---\r\nforeign: |\n  ---\r\nfolio-name: old # c\nother: x\r\n---\nbody\r\n";
        assert_eq!(
            rename(bytes, "new"),
            b"---\r\nforeign: |\n  ---\r\nfolio-name: new # c\nother: x\r\n---\nbody\r\n"
        );
    }

    #[test]
    fn refuses_unsafe_owned_nodes() {
        for (body, error) in [
            (
                "folio-name: a\nfolio-name: b\n",
                FrontMatterError::Duplicate,
            ),
            ("\"folio-name\": a\n", FrontMatterError::Ambiguous),
            ("'folio-name': a\n", FrontMatterError::Ambiguous),
            ("? folio-name\n: a\n", FrontMatterError::Ambiguous),
            ("? 'folio-name'\n: a\n", FrontMatterError::Ambiguous),
            ("? folio-name: a\n", FrontMatterError::Ambiguous),
            ("folio-name: &anchor a\n", FrontMatterError::Unsupported),
            ("folio-name: *anchor\n", FrontMatterError::Unsupported),
            ("folio-name: !!str a\n", FrontMatterError::Unsupported),
            (
                "folio-name: a\n\tcontinued\n",
                FrontMatterError::Unsupported,
            ),
            (
                "folio-name: a\n  continued\n",
                FrontMatterError::Unsupported,
            ),
        ] {
            let bytes = format!("---\n{body}---\n");
            assert_eq!(read_name(bytes.as_bytes()), Err(error), "{body}");
            assert_eq!(
                set_name(bytes.as_bytes(), Some("new"), true),
                Err(error),
                "{body}"
            );
        }
        for key in OWNED {
            let bytes = format!("---\n{key}: a\n{key}: b\n---\n");
            assert_eq!(
                set_name(bytes.as_bytes(), Some("new"), true),
                Err(FrontMatterError::Duplicate)
            );
        }
        assert_eq!(
            set_name(b"---\nno close", Some("new"), true),
            Err(FrontMatterError::Unterminated)
        );
        assert_eq!(
            set_name(b"body\0", Some("new"), true),
            Err(FrontMatterError::Binary)
        );
    }

    #[test]
    fn refuses_escaped_or_undecodable_quoted_keys_before_insertion() {
        for key in [
            r#""\u0066olio-name""#,
            r#""\x66olio-name""#,
            r#""\U00000066olio-name""#,
            r#""\qolio-name""#,
            "'folio-name'",
            "\"folio-name\"",
        ] {
            for prefix in ["", "? "] {
                let bytes = format!("---\n{prefix}{key}: old\n---\n");
                assert_eq!(
                    set_name(bytes.as_bytes(), Some("new"), false),
                    Err(FrontMatterError::Ambiguous),
                    "{bytes}"
                );
            }
        }
    }

    #[test]
    fn implicit_escaped_foreign_keys_remain_opaque_for_edits() {
        let bytes = br"---
'\u0066olio-name': old
---
";
        assert_eq!(read_name(bytes).unwrap(), None);
        assert_eq!(
            set_name(bytes, Some("new"), false),
            Err(FrontMatterError::Ambiguous)
        );
    }

    #[test]
    fn written_plain_scalars_read_back_and_repeat_identically() {
        let empty = b"---\n---\n";
        for name in ["-logo", "--", "logo"] {
            let output =
                apply_edit(empty, &set_name(empty, Some(name), false).unwrap().unwrap()).unwrap();
            assert_eq!(read_name(&output).unwrap().as_deref(), Some(name));
            assert_eq!(rename(&output, name), output);
        }
        let id = FolioId::parse(ID).unwrap();
        let output = apply_edit(empty, &set_id(empty, Some(id), false).unwrap().unwrap()).unwrap();
        assert_eq!(read_id(&output).unwrap(), Some(id));
        let twice =
            apply_edit(&output, &set_id(&output, Some(id), false).unwrap().unwrap()).unwrap();
        assert_eq!(twice, output);

        let reference = Reference {
            alias: "-logo".into(),
            name: Some("-image".into()),
            id: Some(id),
            pin: Some("-pin".into()),
        };
        let output = apply_edit(
            empty,
            &set_reference(empty, "-logo", Some(&reference), false)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(read_references(&output).unwrap(), vec![reference.clone()]);
        let twice = apply_edit(
            &output,
            &set_reference(&output, "-logo", Some(&reference), false)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(twice, output);
        assert_eq!(
            set_name(empty, Some("-"), false),
            Err(FrontMatterError::InvalidValue)
        );
        for field in ["alias", "name", "pin"] {
            let mut invalid = reference.clone();
            match field {
                "alias" => invalid.alias = "-".into(),
                "name" => invalid.name = Some("-".into()),
                "pin" => invalid.pin = Some("-".into()),
                _ => unreachable!(),
            }
            assert_eq!(
                set_references(empty, &[invalid], false),
                Err(FrontMatterError::InvalidValue)
            );
        }
        for value in ["-", "- logo"] {
            let bytes = format!("---\nfolio-name: {value}\n---\n");
            assert_eq!(
                read_name(bytes.as_bytes()),
                Err(FrontMatterError::InvalidValue)
            );
        }
    }

    #[test]
    fn explicit_owned_keys_with_comments_are_ambiguous() {
        let bytes = b"---\n? folio-name # keep\n: old\n---\n";
        assert_eq!(read_name(bytes), Err(FrontMatterError::Ambiguous));
        assert_eq!(
            set_name(bytes, Some("new"), false),
            Err(FrontMatterError::Ambiguous)
        );
    }

    #[test]
    fn reads_refuse_unresolved_owned_keys() {
        for ending in ["\n", "\r\n"] {
            for key in OWNED {
                for body in [
                    format!("?{ending}  {key}{ending}: old"),
                    format!("? # keep{ending}  {key}{ending}: old"),
                    format!(r#""\u0066{}": old"#, &key[1..]),
                    format!(r#"? "\u0066{}": old"#, &key[1..]),
                ] {
                    let input = format!("---{ending}{body}{ending}---{ending}");
                    let bytes = input.as_bytes();
                    assert_eq!(
                        read_name(bytes),
                        Err(FrontMatterError::Ambiguous),
                        "{input}"
                    );
                    assert_eq!(read_id(bytes), Err(FrontMatterError::Ambiguous), "{input}");
                    assert_eq!(
                        read_references(bytes),
                        Err(FrontMatterError::Ambiguous),
                        "{input}"
                    );
                }
            }
        }
    }

    #[test]
    fn reads_allow_decodable_foreign_explicit_and_quoted_keys() {
        for ending in ["\n", "\r\n"] {
            for key in [
                "? other",
                "?\tother",
                "? 'other'",
                r#"? "other\nkey""#,
                r#""other\nkey""#,
            ] {
                for owned in [false, true] {
                    let metadata = if owned {
                        format!(
                            "folio-name: name{ending}folio-id: {ID}{ending}folio-references: {{}}{ending}"
                        )
                    } else {
                        String::new()
                    };
                    let input = format!("---{ending}{key}: value{ending}{metadata}---{ending}");
                    let bytes = input.as_bytes();
                    assert_eq!(
                        read_name(bytes).unwrap().as_deref(),
                        owned.then_some("name"),
                        "{input}"
                    );
                    assert_eq!(
                        read_id(bytes).unwrap(),
                        owned.then(|| FolioId::parse(ID).unwrap()),
                        "{input}"
                    );
                    assert!(read_references(bytes).unwrap().is_empty(), "{input}");
                }
            }
        }
    }

    #[test]
    fn explicit_owned_keys_with_yaml_separation_refuse_edits() {
        let id = FolioId::parse(ID).unwrap();
        for key in OWNED {
            for prefix in ["? ", "?\t", "?\n  ", "?\r\n  "] {
                let bytes = format!("---\n{prefix}{key} # keep\n: old\n---\n");
                let bytes = bytes.as_bytes();
                assert_eq!(
                    set_name(bytes, Some("new"), false),
                    Err(FrontMatterError::Ambiguous)
                );
                assert_eq!(
                    set_id(bytes, Some(id), false),
                    Err(FrontMatterError::Ambiguous)
                );
                assert_eq!(
                    set_references(bytes, &[logo()], false),
                    Err(FrontMatterError::Ambiguous)
                );
                assert_eq!(
                    set_reference(bytes, "logo", Some(&logo()), false),
                    Err(FrontMatterError::Ambiguous)
                );
            }
        }
    }

    #[test]
    fn reference_value_replacement_preserves_yaml_separation() {
        let reference = Reference {
            alias: "logo".into(),
            name: Some("new".into()),
            id: None,
            pin: None,
        };
        for spacing in ["", " ", "\t"] {
            for value in ["", "{}", "\n  logo: old", "\n  logo:\n    name: old"] {
                let input = format!("---\nfolio-references:{spacing}{value}\n---\n");
                let output = apply_edit(
                    input.as_bytes(),
                    &set_references(input.as_bytes(), std::slice::from_ref(&reference), false)
                        .unwrap()
                        .unwrap(),
                )
                .unwrap();
                let expected_spacing = if spacing.is_empty() { " " } else { spacing };
                assert_eq!(
                    text(&output).unwrap(),
                    format!(
                        "---\nfolio-references:{expected_spacing}{{logo: {{name: new}}}}\n---\n"
                    )
                );
                assert_eq!(read_references(&output).unwrap(), vec![reference.clone()]);
            }
            // A single-alias replacement also writes immediately after a colon.
            let input = format!("---\nfolio-references:\n  logo:{spacing}{{name: old}}\n---\n");
            let output = apply_edit(
                input.as_bytes(),
                &set_reference(input.as_bytes(), "logo", Some(&reference), false)
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            let expected_spacing = if spacing.is_empty() { " " } else { spacing };
            assert_eq!(
                text(&output).unwrap(),
                format!("---\nfolio-references:\n  logo:{expected_spacing}{{name: new}}\n---\n")
            );
            assert_eq!(read_references(&output).unwrap(), vec![reference.clone()]);
        }
    }

    #[test]
    fn scalar_replacements_preserve_spacing_and_refuse_empty_values() {
        let id = FolioId::parse(ID).unwrap();
        for spacing in [" ", "\t"] {
            let input = format!("---\nfolio-name:{spacing}old\nfolio-id:{spacing}{ID}\n---\n");
            let output = apply_edit(
                input.as_bytes(),
                &set_name(input.as_bytes(), Some("new"), false)
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(
                text(&output).unwrap(),
                format!("---\nfolio-name:{spacing}new\nfolio-id:{spacing}{ID}\n---\n")
            );
            let output = apply_edit(
                input.as_bytes(),
                &set_id(input.as_bytes(), Some(id), false).unwrap().unwrap(),
            )
            .unwrap();
            assert_eq!(output, input.as_bytes());
        }
        // Empty scalar values are invalid and must not be rewritten.
        for spacing in ["", " ", "\t"] {
            let input = format!("---\nfolio-name:{spacing}\n---\n");
            assert_eq!(
                set_name(input.as_bytes(), Some("new"), false),
                Err(FrontMatterError::InvalidValue)
            );
            let input = format!("---\nfolio-id:{spacing}\n---\n");
            assert_eq!(
                set_id(input.as_bytes(), Some(id), false),
                Err(FrontMatterError::InvalidValue)
            );
        }
    }

    #[test]
    fn edits_keep_the_closing_delimiter_within_the_scan_limit() {
        let bytes = format!("---\n#{}\nfolio-name: x\n---\n", "a".repeat(65515));
        assert_eq!(locate(bytes.as_bytes()).unwrap().unwrap().body.end, 65535);
        assert_eq!(
            set_name(bytes.as_bytes(), Some("xx"), false),
            Err(FrontMatterError::Unterminated)
        );
        let unchanged = rename(bytes.as_bytes(), "x");
        assert_eq!(unchanged, bytes.as_bytes());
        assert_eq!(read_name(&unchanged).unwrap().as_deref(), Some("x"));

        fn at_boundary(body: &str) -> Vec<u8> {
            format!(
                "---\n#{}\n{body}---\n",
                "a".repeat(SCAN_LIMIT - 7 - body.len())
            )
            .into_bytes()
        }
        let id = FolioId::parse(ID).unwrap();
        for body in ["", "folio-id: 12345678-1234-4234-8234-123456789abc\n"] {
            let bytes = at_boundary(body);
            assert_eq!(
                set_name(&bytes, Some("new"), false),
                Err(FrontMatterError::Unterminated)
            );
        }
        let bytes = at_boundary("");
        assert_eq!(
            set_id(&bytes, Some(id), false),
            Err(FrontMatterError::Unterminated)
        );
        for body in [
            "",
            "folio-references: {}\n",
            "folio-references: {logo: x}\n",
            "folio-references:\n  logo: x\n",
            "folio-references:\n  logo:\n    name: x\n",
        ] {
            let bytes = at_boundary(body);
            assert_eq!(
                set_references(&bytes, &[logo()], false),
                Err(FrontMatterError::Unterminated),
                "{body}"
            );
            if !body.contains("    name:") {
                for alias in ["logo", "other"] {
                    let mut reference = logo();
                    reference.alias = alias.into();
                    assert_eq!(
                        set_reference(&bytes, alias, Some(&reference), false),
                        Err(FrontMatterError::Unterminated),
                        "{body}: {alias}"
                    );
                }
            }
        }
        assert_eq!(
            set_name(b"body", Some(&"x".repeat(SCAN_LIMIT)), true),
            Err(FrontMatterError::Unterminated)
        );
    }

    #[test]
    fn nested_reference_replacement_keeps_trailing_comments_and_blank_lines() {
        for tail in ["# keep\n", "\n# keep\n\n"] {
            let bytes = format!("---\nfolio-references:\n  logo:\n    name: old\n{tail}---\n");
            let output = apply_edit(
                bytes.as_bytes(),
                &set_references(bytes.as_bytes(), &[logo()], false)
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(
                text(&output).unwrap(),
                format!(
                    "---\nfolio-references: {{logo: {{name: logo-image, pin: abc123}}}}\n{tail}---\n"
                )
            );
            assert_eq!(read_references(&output).unwrap(), vec![logo()]);
            assert_eq!(
                set_reference(bytes.as_bytes(), "logo", Some(&logo()), false),
                Err(FrontMatterError::Unsupported)
            );

            let bytes = bytes.replace("  logo:\n    name: old", "  logo: old");
            let output = apply_edit(
                bytes.as_bytes(),
                &set_reference(bytes.as_bytes(), "logo", Some(&logo()), false)
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert!(text(&output).unwrap().ends_with(&format!("{tail}---\n")));
            let output = apply_edit(
                bytes.as_bytes(),
                &set_reference(bytes.as_bytes(), "logo", None, false)
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(text(&output).unwrap(), format!("---\n{tail}---\n"));
        }
    }

    #[test]
    fn enforces_scan_boundary_and_delimiters() {
        for (offset, success) in [(SCAN_LIMIT - 1, true), (SCAN_LIMIT, false)] {
            let mut bytes = b"---\n#".to_vec();
            bytes.resize(offset - 1, b'x');
            bytes.extend_from_slice(b"\n---\nbody");
            assert_eq!(locate(&bytes).is_ok(), success);
        }
        assert!(locate(b" ---\n---\n").unwrap().is_none());
        assert!(locate(b"--- \t\r\n...\r\n").unwrap().is_some());
        assert_eq!(locate(b"---\n  ---\n"), Err(FrontMatterError::Unterminated));
        assert_eq!(locate(b"---\rfolio-name: a\r---"), Ok(None));
        assert_eq!(locate(b"---\r"), Ok(None));
        assert_eq!(locate(b"---\x0b\n---\n"), Ok(None));
    }

    #[test]
    fn creates_inserts_and_removes() {
        assert!(set_name(b"body", Some("new"), false).unwrap().is_none());
        assert_eq!(
            rename(b"\xef\xbb\xbfbody", "new"),
            b"\xef\xbb\xbf---\nfolio-name: new\n---\nbody"
        );
        assert_eq!(
            rename(b"---\n---\nbody", "new"),
            b"---\nfolio-name: new\n---\nbody"
        );
        let input = b"---\r\nforeign: a\n---\r\nbody";
        assert_eq!(
            rename(input, "new"),
            b"---\r\nforeign: a\nfolio-name: new\r\n---\r\nbody"
        );
        let input = format!("---\nfolio-id: {ID}\nforeign: a\n---\n");
        let output = rename(input.as_bytes(), "new");
        assert!(text(&output).unwrap().contains("folio-name: new\nforeign:"));
        assert_eq!(read_id(&output).unwrap().unwrap().to_string(), ID);
        let id = FolioId::parse(ID).unwrap();
        let output = apply_edit(
            b"---\n---\n",
            &set_id(b"---\n---\n", Some(id), false).unwrap().unwrap(),
        )
        .unwrap();
        assert_eq!(read_id(&output).unwrap(), Some(id));
        assert_eq!(
            read_id(b"---\nfolio-id: INVALID\n---\n"),
            Err(FrontMatterError::InvalidValue)
        );
        for input in [
            b"---\nfolio-references:\n  logo: image\n---\nbody".as_slice(),
            b"---\nfolio-references: {logo: image}\n---\nbody",
        ] {
            let output = apply_edit(
                input,
                &set_reference(input, "logo", None, false).unwrap().unwrap(),
            )
            .unwrap();
            assert_eq!(output, b"---\n---\nbody");
        }
        assert_eq!(
            apply_edit(b"x", &edit(2..3, Vec::new())),
            Err(FrontMatterError::InvalidValue)
        );
    }

    #[test]
    fn reference_value_edits_keep_comments_and_other_entries() {
        let input = b"---\nfolio-references: # header\n  logo  :   old   # keep\n  other: other-image\n# tail\nforeign: x\n---\nbody";
        let output = apply_edit(
            input,
            &set_reference(input, "logo", Some(&logo()), false)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            text(&output).unwrap(),
            text(input)
                .unwrap()
                .replace("old", "{name: logo-image, pin: abc123}")
        );
        let output = apply_edit(
            &output,
            &set_reference(&output, "logo", None, false)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(read_references(&output).unwrap().len(), 1);
        assert!(text(&output).unwrap().contains("# tail\nforeign: x"));
    }
    #[test]
    fn wrapped_flow_comments_and_indentation_are_understood() {
        let bytes = b"---\nfolio-references: {\n# inside\nlogo: logo-image, # first\nother: other-image # last\n}  # outside\nforeign: [\n  {folio-name: foreign}\n]\n---\n";
        assert_eq!(read_references(bytes).unwrap().len(), 2);
        let output = apply_edit(
            bytes,
            &set_reference(bytes, "logo", Some(&logo()), false)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(read_references(&output).unwrap().len(), 2);
        assert!(
            text(&output)
                .unwrap()
                .contains("  # outside\nforeign: [\n  {folio-name: foreign}\n]")
        );

        let bytes = b"---\nfolio-references:\n    other: other-image\n# tail\n---\n";
        let output = apply_edit(
            bytes,
            &set_reference(bytes, "logo", Some(&logo()), false)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(read_references(&output).unwrap().len(), 2);
        assert!(text(&output).unwrap().contains("    logo: {name:"));
        let output = apply_edit(
            &output,
            &set_references(&output, &[], false).unwrap().unwrap(),
        )
        .unwrap();
        assert_eq!(output, b"---\n# tail\n---\n");
    }

    #[test]
    fn malformed_references_are_refused_without_partial_edits() {
        for (body, error) in [
            (
                "folio-references: {logo: one, logo: two}\n",
                FrontMatterError::Duplicate,
            ),
            (
                "folio-references:\n  logo: {name: one, name: two}\n",
                FrontMatterError::Duplicate,
            ),
            (
                "folio-references:\n  logo: *other\n",
                FrontMatterError::Unsupported,
            ),
            (
                "folio-references:\n  logo: !str one\n",
                FrontMatterError::Unsupported,
            ),
            (
                "folio-references:\n\tlogo: one\n",
                FrontMatterError::Unsupported,
            ),
            (
                "folio-references: {} garbage\n",
                FrontMatterError::Unsupported,
            ),
            (
                "folio-references:\n  logo: one@\n",
                FrontMatterError::InvalidValue,
            ),
        ] {
            let bytes = format!("---\n{body}---\n");
            assert_eq!(read_references(bytes.as_bytes()), Err(error), "{body}");
            assert_eq!(
                set_reference(bytes.as_bytes(), "logo", Some(&logo()), true),
                Err(error),
                "{body}"
            );
        }
    }
}
