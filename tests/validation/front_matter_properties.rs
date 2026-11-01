use std::cell::Cell;

use folt_core::folio_dir::FolioId;
use folt_core::front_matter::{
    Block, Edit, FrontMatterError, Reference, apply_edit, locate, read_id, read_name,
    read_references, set_id, set_name, set_reference, set_references,
};
use proptest::prelude::*;
use proptest::test_runner::{RngSeed, TestRunner};

const ID: &str = "12345678-1234-4234-8234-123456789abc";
const OWNED: [&str; 3] = ["folio-name", "folio-id", "folio-references"];

// Indices into these pools shrink before rendering, so every failure below includes
// the escaped, rendered bytes as well as the operation.
#[derive(Clone, Copy, Debug)]
struct Shape {
    text: &'static str,
    // None means every operation must accept this shape, including in a
    // composition of known-valid shapes. Reasons are fixed expectations, never
    // inferred from the editor during a property run.
    refusal: Option<&'static str>,
    accepts_alone: bool,
}

const fn known_valid(text: &'static str) -> Shape {
    Shape {
        text,
        refusal: None,
        accepts_alone: true,
    }
}

const fn may_refuse(text: &'static str, reason: &'static str, accepts_alone: bool) -> Shape {
    Shape {
        text,
        refusal: Some(reason),
        accepts_alone,
    }
}

const POOLS: [&[Shape]; 4] = [FOREIGN, NAMES, IDS, REFERENCES];

#[derive(Debug)]
struct Document {
    bytes: Vec<u8>,
    must_accept: bool,
    entries: Vec<(usize, usize)>,
}

// Bias toward accepted compositions so random refusals cannot consume most of
// the budget. Both arms shrink indices, and every pool entry remains reachable.
fn pool_index(pool: &'static [Shape]) -> impl Strategy<Value = usize> {
    let valid: Vec<_> = (0..pool.len())
        .filter(|&i| pool[i].refusal.is_none())
        .collect();
    prop_oneof![1 => prop::sample::select(valid), 1 => 0..pool.len()]
}

const FOREIGN: &[Shape] = &[
    known_valid("foreign: plain\n"),
    known_valid("'quoted key': 'quoted value'\n"),
    known_valid("# comment\n\n"),
    known_valid("foreign: |\n  ---\n  literal\n"),
    known_valid("flow: {a: 1, b: [x, y]}\n"),
    // #167 exclusion lifted now that #170's Hidden audit is in the base.
    // Malformed flows may be refused; accepted edits must still read back.
    may_refuse(
        "flow: {a:]1,\n  b: 2}\n",
        "malformed foreign flow can hide following owned keys (Hidden)",
        true,
    ),
    may_refuse(
        "flow: {a: 1,\n  b: 2\n",
        "malformed foreign flow can hide following owned keys (Hidden)",
        true,
    ),
    may_refuse(
        "flow: [a, b\n",
        "malformed foreign flow can hide following owned keys (Hidden)",
        true,
    ),
    may_refuse(
        "flow: [a, b}\n",
        "malformed foreign flow can hide following owned keys (Hidden)",
        true,
    ),
    may_refuse(
        "flow: ]\n",
        "malformed foreign flow can hide following owned keys (Hidden)",
        true,
    ),
    known_valid("flow: {a: 1, # keep\n  b: [x, y]}\n"),
    known_valid("? other\n: value\n"),
    known_valid("? other\n"),
    known_valid("?\n  other\n: value\n"),
    known_valid("? other\n  key\n: value\n"),
    may_refuse(
        "!!str other: value\n",
        "tagged foreign key (Ambiguous)",
        false,
    ),
    may_refuse(
        "&a other: value\n",
        "anchored foreign key (Ambiguous)",
        false,
    ),
    known_valid("foreign: !!str value\n"),
    known_valid("foreign: &a value\n"),
    known_valid("foreign:\tvalue\n"),
];
const NAMES: &[Shape] = &[
    known_valid(""),
    known_valid("folio-name: old\n"),
    known_valid("folio-name :  v # c\n"),
    known_valid("folio-name: 'old'\n"),
    known_valid("folio-name: \"old\"\n"),
    may_refuse(
        "? folio-name\n: old\n",
        "explicit, quoted or escaped owned key (Ambiguous)",
        false,
    ),
    may_refuse(
        "? folio-name\n",
        "explicit, quoted or escaped owned key (Ambiguous)",
        false,
    ),
    may_refuse(
        "\"folio-name\": old\n",
        "explicit, quoted or escaped owned key (Ambiguous)",
        false,
    ),
    may_refuse(
        "\"\\u0066olio-name\": old\n",
        "explicit, quoted or escaped owned key (Ambiguous)",
        false,
    ),
    may_refuse(
        "folio-name: !!str old\n",
        "tagged owned value (Unsupported)",
        false,
    ),
    may_refuse(
        "folio-name: &a old\n",
        "anchored owned value (Unsupported)",
        false,
    ),
    may_refuse(
        "folio-name: old\n\tcontinued\n",
        "tab continuation on owned scalar (Unsupported)",
        false,
    ),
    may_refuse(
        "folio-name: a\nfolio-name: b\n",
        "duplicate owned key (Duplicate)",
        false,
    ),
];
const IDS: &[Shape] = &[
    known_valid(""),
    known_valid("folio-id: 12345678-1234-4234-8234-123456789abc\n"),
    known_valid("folio-id: '12345678-1234-4234-8234-123456789abc'\n"),
    may_refuse(
        "? folio-id\n: old\n",
        "explicit owned key (Ambiguous)",
        false,
    ),
    may_refuse(
        "folio-id: !!str old\n",
        "tagged owned value (Unsupported)",
        false,
    ),
];
const REFERENCES: &[Shape] = &[
    // #148 follow-up: each distinct flow comment position, an empty commented
    // header, and a comment on the closing line of a wrapped entry.
    known_valid("folio-references:\n  logo: {name: # keep\n    image}\n"),
    known_valid("folio-references:\n  logo: {name: # c\n image}\n"),
    known_valid("folio-references:\n  logo: { # c\n name: image}\n"),
    known_valid("folio-references:\n  logo: {name: image # c\n }\n"),
    known_valid("folio-references:\n  logo: {name # c\n : image}\n"),
    known_valid("folio-references: # header\n"),
    known_valid("folio-references:\n  logo: {name: old,\n    pin: abc} # keep\n"),
    // #148: nested key spacing, including tabs and flow alias spacing.
    known_valid("folio-references:\n  logo:\n    name : image\n"),
    known_valid("folio-references:\n  logo:\n    name\t: image\n"),
    known_valid("folio-references:\n  logo:\n    id : 12345678-1234-4234-8234-123456789abc\n"),
    known_valid("folio-references:\n  logo:\n    id\t: 12345678-1234-4234-8234-123456789abc\n"),
    known_valid("folio-references:\n  logo:\n    name: image\n    pin : abc\n"),
    known_valid("folio-references:\n  logo:\n    name: image\n    pin\t: abc\n"),
    known_valid("folio-references: {logo: {name : image}}\n"),
    known_valid("folio-references: {logo: {name\t: image}}\n"),
    known_valid("folio-references: {logo: {id : 12345678-1234-4234-8234-123456789abc}}\n"),
    known_valid("folio-references: {logo: {id\t: 12345678-1234-4234-8234-123456789abc}}\n"),
    known_valid("folio-references: {logo: {name: image, pin : abc}}\n"),
    known_valid("folio-references: {logo: {name: image, pin\t: abc}}\n"),
    known_valid("folio-references: {logo: {name\t: image, pin : abc}}\n"),
    known_valid("folio-references: {logo : {name: image}}\n"),
    known_valid("folio-references: {logo\t: {name: image}}\n"),
    known_valid(""),
    known_valid("folio-references: {}\n"),
    known_valid("folio-references: {logo: {name: old}, other: {name: other}}\n"),
    known_valid(
        "folio-references:\n  logo:\n    name: old\n    pin: abc\n  other: {name: other}\n",
    ),
    known_valid("folio-references: {logo: {id: 12345678-1234-4234-8234-123456789abc, pin: abc}}\n"),
    known_valid(
        "folio-references: {logo: {name: old, # inner\n pin: abc}, # outer\n other: {name: other}}\n",
    ),
    known_valid("folio-references:\n  logo: {name: old, # inner\n    pin: abc} # tail\n"),
    may_refuse(
        "folio-references:\n  logo:\n    name:\n",
        "empty nested name (InvalidValue for reference edits)",
        false,
    ),
    may_refuse(
        "folio-references: {logo: {name:}}\n",
        "empty flow name (InvalidValue for reference edits)",
        false,
    ),
    may_refuse(
        "? folio-references\n: {}\n",
        "explicit owned key (Ambiguous)",
        false,
    ),
];
fn documents() -> impl Strategy<Value = Document> {
    (
        prop::collection::vec(pool_index(FOREIGN), 0..4),
        pool_index(NAMES),
        pool_index(IDS),
        pool_index(REFERENCES),
        0..3usize,
        any::<bool>(),
        0..12usize,
        prop_oneof![38 => Just(0usize), 1 => 1..60_000usize, 1 => 65_400..66_001usize],
    )
        .prop_map(|(foreign, name, id, refs, ending, bom, shape, padding)| {
            let mut entries: Vec<_> = foreign.iter().map(|&i| (0, i)).collect();
            entries.extend([(1, name), (2, id), (3, refs)]);
            // No promise for absent/unterminated blocks or large padding that
            // can reach SCAN_LIMIT after line-ending conversion or an edit.
            let must_accept = shape > 1
                && padding == 0
                && entries
                    .iter()
                    .all(|&(pool, i)| POOLS[pool][i].refusal.is_none());
            let mut doc = if bom {
                "\u{feff}".to_owned()
            } else {
                String::new()
            };
            if shape != 0 {
                doc.push_str("---\n");
                for node in foreign {
                    doc.push_str(FOREIGN[node].text);
                }
                doc.push_str(NAMES[name].text);
                doc.push_str(IDS[id].text);
                doc.push_str(REFERENCES[refs].text);
                if padding > 0 {
                    doc.push_str("padding: ");
                    // Large values and closers on either side of the scan boundary.
                    let length = padding.saturating_sub(doc.len() + 1);
                    doc.extend(std::iter::repeat_n('x', length));
                    doc.push('\n');
                }
                if shape != 1 {
                    doc.push_str("---\n");
                }
            }
            doc.push_str("# Body\n---\nlater: document\n...\n");
            match ending {
                1 => doc = doc.replace('\n', "\r\n"),
                2 => {
                    doc = doc
                        .lines()
                        .enumerate()
                        .map(|(i, line)| {
                            format!("{line}{}", if i % 2 == 0 { "\r\n" } else { "\n" })
                        })
                        .collect()
                }
                _ => {}
            }
            Document {
                bytes: doc.into_bytes(),
                must_accept,
                entries: if shape == 0 { vec![] } else { entries },
            }
        })
}

#[derive(Clone, Debug)]
enum Operation {
    Name(Option<&'static str>),
    Id(bool),
    References(bool),
    Alias(&'static str, bool),
}

fn reference(alias: &'static str) -> Reference<'static> {
    Reference {
        alias: alias.into(),
        name: Some("new-image".into()),
        id: None,
        pin: Some("pin with \"quotes\" and \\slashes".into()),
    }
}

impl Operation {
    fn kind(&self) -> usize {
        match self {
            Self::Name(_) => 0,
            Self::Id(_) => 1,
            Self::References(_) => 2,
            Self::Alias(..) => 3,
        }
    }

    fn setting(&self) -> bool {
        matches!(
            self,
            Self::Name(Some(_)) | Self::Id(true) | Self::References(true) | Self::Alias(_, true)
        )
    }

    fn key(&self) -> usize {
        match self {
            Self::Name(_) => 0,
            Self::Id(_) => 1,
            _ => 2,
        }
    }

    fn plan(&self, bytes: &[u8], create: bool) -> Result<Option<Edit>, FrontMatterError> {
        match self {
            Self::Name(value) => set_name(bytes, *value, create),
            Self::Id(set) => set_id(bytes, set.then(|| FolioId::parse(ID).unwrap()), create),
            Self::References(set) => set_references(
                bytes,
                &if *set {
                    vec![
                        reference("logo"),
                        Reference {
                            alias: "second".into(),
                            name: None,
                            id: Some(FolioId::parse(ID).unwrap()),
                            pin: None,
                        },
                    ]
                } else {
                    vec![]
                },
                create,
            ),
            Self::Alias(alias, set) => {
                set_reference(bytes, alias, set.then(|| reference(alias)).as_ref(), create)
            }
        }
    }
}

fn operations() -> Vec<Operation> {
    vec![
        Operation::Name(Some("new")),
        Operation::Name(Some("1234")),
        Operation::Name(None),
        Operation::Id(true),
        Operation::Id(false),
        Operation::References(true),
        Operation::References(false),
        Operation::Alias("logo", true),
        Operation::Alias("logo", false),
        Operation::Alias("missing", true),
        Operation::Alias("missing", false),
    ]
}

type ForeignNode = (String, Vec<u8>, Vec<u8>, Vec<u8>);

fn foreign(bytes: &[u8], block: &Block) -> Vec<ForeignNode> {
    block
        .nodes
        .iter()
        .filter(|node| !OWNED.contains(&node.key.as_str()))
        .map(|node| {
            let continuation = bytes[node.continuation.clone()]
                .split_inclusive(|b| *b == b'\n')
                .filter(|line| {
                    let trimmed = line
                        .iter()
                        .copied()
                        .skip_while(u8::is_ascii_whitespace)
                        .collect::<Vec<_>>();
                    !trimmed.is_empty() && !trimmed.starts_with(b"#")
                })
                .flatten()
                .copied()
                .collect();
            (
                node.key.clone(),
                bytes[node.key_range.clone()].to_vec(),
                bytes[node.value.clone()].to_vec(),
                continuation,
            )
        })
        .collect()
}

// Preserve standalone body lines, including trailing lines owned by a node.
// Whole-reference replacement promises its trailing standalone lines; clearing
// references promises all standalone continuation lines. Comments embedded in
// rewritten entry values are not promised by whole-collection replacement.
fn preserved_lines<'a>(bytes: &'a [u8], block: &Block, op: &Operation) -> Vec<&'a [u8]> {
    let mut offset = block.body.start;
    bytes[block.body.clone()]
        .split_inclusive(|b| *b == b'\n')
        .filter(|line| {
            let start = offset;
            offset += line.len();
            let standalone = line
                .iter()
                .find(|b| !b.is_ascii_whitespace())
                .is_none_or(|b| *b == b'#');
            if !standalone {
                return false;
            }
            let owner = block
                .nodes
                .iter()
                .find(|n| OWNED.contains(&n.key.as_str()) && n.range.contains(&start));
            let Some(node) = owner else { return true };
            if node.key != "folio-references" || op.key() != 2 {
                return true;
            }
            if matches!(op, Operation::References(false) | Operation::Alias(..)) {
                return start >= node.continuation.start;
            }
            // The tail after the last meaningful continuation line survives
            // whole-reference replacement. Alias edits preserve standalone
            // continuation lines as checked above.
            bytes[start..node.range.end]
                .split_inclusive(|b| *b == b'\n')
                .all(|line| {
                    line.iter()
                        .find(|b| !b.is_ascii_whitespace())
                        .is_none_or(|b| *b == b'#')
                })
        })
        .collect()
}

struct Context<'a> {
    bytes: &'a [u8],
    op: &'a Operation,
    create: bool,
}

impl std::fmt::Display for Context<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "bytes=\"{}\", operation={:?}, create={}",
            self.bytes.escape_ascii(),
            self.op,
            self.create
        )
    }
}

// Attach the rendered input and operation even to unexpected API panics.
fn check(
    bytes: &[u8],
    op: &Operation,
    create: bool,
    refused: &Cell<usize>,
    must_accept: bool,
) -> Result<bool, TestCaseError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        check_inner(bytes, op, create, refused, must_accept)
    }))
    .unwrap_or_else(|_| {
        Err(TestCaseError::fail(format!(
            "panic: {}",
            Context { bytes, op, create }
        )))
    })
}

fn check_inner(
    bytes: &[u8],
    op: &Operation,
    create: bool,
    refused: &Cell<usize>,
    must_accept: bool,
) -> Result<bool, TestCaseError> {
    let context = Context { bytes, op, create };
    let original_block = locate(bytes);
    let before_name = read_name(bytes);
    let before_id = read_id(bytes);
    let before_refs = read_references(bytes);
    let planned = op.plan(bytes, create);
    if must_accept {
        prop_assert!(
            planned.is_ok(),
            "known-valid shape refused: {planned:?}: {context}"
        );
        if op.setting() {
            prop_assert!(
                matches!(&planned, Ok(Some(_))),
                "setting must produce an edit: {context}"
            );
        }
    }
    if let Err(error) = original_block {
        prop_assert_eq!(&planned, &Err(error), "{}", context);
        prop_assert_eq!(&before_name, &Err(error), "{}", context);
        prop_assert_eq!(before_id, Err(error), "{}", context);
        prop_assert_eq!(&before_refs, &Err(error), "{}", context);
    }
    // Structural/unsupported read refusals also prohibit editing that owned key.
    // Invalid ids may be replaced without parsing the old id, so InvalidValue
    // is compared for reference edits only.
    let read_error = match op.key() {
        0 => before_name.as_ref().err(),
        1 => before_id.as_ref().err(),
        _ => before_refs.as_ref().err(),
    };
    if let Some(error) =
        read_error.filter(|e| **e != FrontMatterError::InvalidValue || op.key() == 2)
    {
        prop_assert!(planned.is_err(), "read refusal {error:?}: {context}");
    }
    let edit = match planned {
        Err(_) => {
            refused.set(refused.get() + 1);
            return Ok(false);
        }
        Ok(None) => {
            if must_accept {
                // An absent edit is allowed only when removal has nothing to do.
                let absent = match op {
                    Operation::Name(None) => before_name.as_ref().is_ok_and(Option::is_none),
                    Operation::Id(false) => before_id.as_ref().is_ok_and(Option::is_none),
                    Operation::References(false) => before_refs.as_ref().is_ok_and(Vec::is_empty),
                    Operation::Alias(alias, false) => before_refs
                        .as_ref()
                        .is_ok_and(|refs| refs.iter().all(|r| r.alias != *alias)),
                    _ => false,
                };
                prop_assert!(absent, "unexpected no-op: {context}");
            }
            return Ok(false);
        }
        Ok(Some(edit)) => edit,
    };
    prop_assert!(
        edit.range.start <= edit.range.end && edit.range.end <= bytes.len(),
        "{}",
        context
    );
    let result =
        apply_edit(bytes, &edit).map_err(|e| TestCaseError::fail(format!("{e}: {context}")))?;
    let expected = [
        &bytes[..edit.range.start],
        &edit.replacement,
        &bytes[edit.range.end..],
    ]
    .concat();
    prop_assert_eq!(&result, &expected, "{}", context);
    let block =
        locate(&result).map_err(|e| TestCaseError::fail(format!("locate: {e}: {context}")))?;
    prop_assert!(block.is_some(), "{}", context);
    if let Ok(Some(old)) = original_block {
        let new = block.as_ref().unwrap();
        if edit.range.is_empty() {
            let pos = edit.range.start;
            prop_assert!(
                pos >= old.body.start
                    && pos <= old.body.end
                    && !(old.preamble.start < pos && pos < old.preamble.end)
                    && !old.nodes.iter().any(|n| !OWNED.contains(&n.key.as_str())
                        && n.range.start < pos
                        && pos < n.range.end),
                "insertion outside allowed body boundary: {edit:?}: {context}"
            );
        } else {
            // Check containment in the union, allowing adjacent owned nodes.
            let mut covered = edit.range.start;
            for node in &old.nodes {
                if OWNED.contains(&node.key.as_str())
                    && node.range.start <= covered
                    && covered < node.range.end
                {
                    covered = node.range.end;
                }
            }
            prop_assert!(
                covered >= edit.range.end,
                "edit outside original owned nodes: {edit:?}: {context}"
            );
        }
        prop_assert_eq!(
            &result[..new.body.start],
            &bytes[..old.body.start],
            "{}",
            context
        );
        prop_assert!(
            result[new.body.clone()].starts_with(&bytes[old.preamble.clone()]),
            "preamble changed: {context}"
        );
        // Include the original closer line, not only the bytes after the block.
        prop_assert!(
            result.ends_with(&bytes[old.body.end..]),
            "closer changed: {context}"
        );
        let original_lines = preserved_lines(bytes, &old, op);
        let result_lines: Vec<_> = result[new.body.clone()]
            .split_inclusive(|b| *b == b'\n')
            .filter(|line| {
                line.iter()
                    .find(|b| !b.is_ascii_whitespace())
                    .is_none_or(|b| *b == b'#')
            })
            .collect();
        // New standalone comments can be introduced by alias rewrites. Compare
        // the original sequence in order, permitting those additional lines.
        let mut remaining = result_lines.as_slice();
        for line in original_lines {
            let index = remaining.iter().position(|candidate| *candidate == line);
            prop_assert!(
                index.is_some(),
                "lost/reordered standalone line {}: {context}",
                line.escape_ascii()
            );
            remaining = &remaining[index.unwrap() + 1..];
        }
        prop_assert!(result.ends_with(&bytes[old.range.end..]), "{}", context);
        prop_assert_eq!(
            foreign(bytes, &old),
            foreign(&result, block.as_ref().unwrap()),
            "{}",
            context
        );
    } else {
        prop_assert!(create, "{}", context);
        prop_assert_eq!(&original_block, &Ok(None), "{}", context);
        let position = if bytes.starts_with(b"\xef\xbb\xbf") {
            3
        } else {
            0
        };
        prop_assert_eq!(
            &edit.range,
            &(position..position),
            "block creation must insert at the start after any BOM: {}",
            context
        );
        // Remove only the insertion from the result: every original byte,
        // including the BOM and any later YAML documents, must survive.
        prop_assert_eq!(
            [
                &result[..position],
                &result[position + edit.replacement.len()..],
            ]
            .concat(),
            bytes,
            "block creation discarded original bytes: {}",
            context
        );
    }
    let again = op
        .plan(&result, create)
        .map_err(|e| TestCaseError::fail(format!("repeat: {e}: {context}")))?;
    if let Some(edit) = again {
        prop_assert_eq!(
            apply_edit(&result, &edit).unwrap(),
            result.clone(),
            "{}",
            context
        );
    }
    let name = read_name(&result);
    let id = read_id(&result);
    let refs = read_references(&result);
    if op.key() != 0 {
        prop_assert_eq!(&before_name, &name, "{}", context);
    }
    if op.key() != 1 {
        prop_assert_eq!(&before_id, &id, "{}", context);
    }
    if op.key() != 2 {
        prop_assert_eq!(&before_refs, &refs, "{}", context);
    }
    for (before, after) in [
        (before_name.as_ref().err(), name.as_ref().err()),
        (before_id.as_ref().err(), id.as_ref().err()),
        (before_refs.as_ref().err(), refs.as_ref().err()),
    ] {
        if before != Some(&FrontMatterError::Duplicate) {
            prop_assert_ne!(after, Some(&FrontMatterError::Duplicate), "{}", context);
        }
    }
    match op {
        Operation::Name(value) => {
            prop_assert_eq!(
                name.as_ref().map(|v| v.as_deref()),
                Ok(*value),
                "{}",
                context
            );
            if value.is_some() {
                let node = block
                    .as_ref()
                    .unwrap()
                    .nodes
                    .iter()
                    .find(|n| n.key == "folio-name")
                    .unwrap();
                prop_assert!(result[node.value.clone()].starts_with(b"\""), "{}", context);
            }
        }
        Operation::Id(set) => prop_assert_eq!(
            id,
            Ok(set.then(|| FolioId::parse(ID).unwrap())),
            "{}",
            context
        ),
        Operation::References(set) => {
            let expected = if *set {
                vec![
                    reference("logo"),
                    Reference {
                        alias: "second".into(),
                        name: None,
                        id: Some(FolioId::parse(ID).unwrap()),
                        pin: None,
                    },
                ]
            } else {
                vec![]
            };
            prop_assert_eq!(refs, Ok(expected), "{}", context);
        }
        Operation::Alias(alias, set) => {
            let mut expected = before_refs.unwrap();
            if let Some(index) = expected.iter().position(|r| r.alias == *alias) {
                if *set {
                    expected[index] = reference(alias);
                } else {
                    expected.remove(index);
                }
            } else if *set {
                expected.push(reference(alias));
            }
            prop_assert_eq!(refs, Ok(expected), "{}", context);
        }
    }
    Ok(true)
}

fn runner(cases: u32, seed: u64) -> TestRunner {
    TestRunner::new(ProptestConfig {
        cases,
        rng_seed: RngSeed::Fixed(seed),
        failure_persistence: None,
        ..ProptestConfig::default()
    })
}

#[test]
fn arbitrary_bytes_never_panic() {
    let refused = Cell::new(0);
    runner(100, 15301)
        .run(&any::<Vec<u8>>(), |bytes| {
            for bytes in [bytes.clone(), [b"---\n".as_slice(), &bytes].concat()] {
                for op in operations() {
                    for create in [false, true] {
                        check(&bytes, &op, create, &refused, false)?;
                    }
                }
            }
            Ok(())
        })
        .unwrap();
    eprintln!("arbitrary-byte refused operations: {}", refused.get());
}

// Count successful edits, not merely attempts or non-panicking refusals.
struct Coverage {
    attempted: [usize; 4],
    accepted: [usize; 4],
    entries: Vec<Vec<usize>>,
}

impl Coverage {
    fn new() -> Self {
        Self {
            attempted: [0; 4],
            accepted: [0; 4],
            entries: POOLS.iter().map(|pool| vec![0; pool.len()]).collect(),
        }
    }

    fn record(&mut self, doc: &Document, op: &Operation, accepted: bool) {
        self.attempted[op.kind()] += 1;
        if accepted {
            self.accepted[op.kind()] += 1;
            for &(pool, index) in &doc.entries {
                self.entries[pool][index] += 1;
            }
        }
    }
}

#[test]
fn known_valid_shapes_are_accepted() {
    let refused = Cell::new(0);
    let mut coverage = Coverage::new();
    // These representatives are themselves known-valid and exercise interactions
    // with every other pool. The sweep covers both reference forms individually.
    let representatives = [0, 1, 1, 23];
    for (pool, shapes) in POOLS.iter().enumerate() {
        for (index, shape) in shapes.iter().enumerate() {
            for combined in [false, true] {
                if shape.refusal.is_some() && (combined || !shape.accepts_alone) {
                    continue;
                }
                let mut entries = vec![(pool, index)];
                if combined {
                    entries.extend(
                        (0..POOLS.len())
                            .filter(|&p| p != pool)
                            .map(|p| (p, representatives[p])),
                    );
                }
                entries.sort_unstable();
                let content: String = entries.iter().map(|&(p, i)| POOLS[p][i].text).collect();
                let doc = Document {
                    bytes: format!("---\n{content}---\nbody\n").into_bytes(),
                    must_accept: true,
                    entries,
                };
                for op in operations() {
                    for create in [false, true] {
                        let accepted = check(&doc.bytes, &op, create, &refused, true).unwrap();
                        coverage.record(&doc, &op, accepted);
                    }
                }
            }
        }
    }
    for (pool, shapes) in POOLS.iter().enumerate() {
        for (index, shape) in shapes.iter().enumerate() {
            if shape.accepts_alone {
                // Six setting operations, two create values, at least alone.
                assert!(
                    coverage.entries[pool][index] >= 12,
                    "pool={pool}, entry={index}: {shape:?}"
                );
            }
        }
    }
    assert_eq!(refused.get(), 0);
}

#[test]
fn composed_documents_preserve_and_round_trip() {
    let refused = Cell::new(0);
    let coverage = std::cell::RefCell::new(Coverage::new());
    runner(400, 15302)
        .run(
            &(
                documents(),
                prop::sample::select(operations()),
                any::<bool>(),
            ),
            |(doc, op, create)| {
                let accepted = check(&doc.bytes, &op, create, &refused, doc.must_accept)?;
                coverage.borrow_mut().record(&doc, &op, accepted);
                if locate(&doc.bytes) == Ok(None) {
                    // shape == 0 renders a body with a later YAML document.
                    // Exercise every creation API rather than relying on the
                    // sampled operation and create flag to reach this branch.
                    for op in operations().into_iter().filter(Operation::setting) {
                        prop_assert!(check(&doc.bytes, &op, true, &refused, true)?);
                    }
                }
                if doc.must_accept {
                    // Every generated known-valid document must accept every
                    // operation and both create values, not just the sampled one.
                    // Full edit properties run on the sampled operation here and
                    // every operation in the deterministic pool sweep.
                    for op in operations() {
                        for create in [false, true] {
                            let planned = op.plan(&doc.bytes, create);
                            let context = Context {
                                bytes: &doc.bytes,
                                op: &op,
                                create,
                            };
                            prop_assert!(
                                planned.is_ok(),
                                "known-valid shape refused: {planned:?}: {context}"
                            );
                            if op.setting() {
                                prop_assert!(
                                    matches!(planned, Ok(Some(_))),
                                    "setting must produce an edit: {context}"
                                );
                            }
                        }
                    }
                }
                Ok(())
            },
        )
        .unwrap();
    let coverage = coverage.into_inner();
    for kind in 0..4 {
        assert!(
            coverage.attempted[kind] >= 40,
            "operation kind {kind} under-exercised"
        );
        assert!(
            coverage.accepted[kind] * 4 >= coverage.attempted[kind],
            "operation kind {kind}: accepted {} / {} (minimum 1/4)",
            coverage.accepted[kind],
            coverage.attempted[kind]
        );
    }
    for (pool, shapes) in POOLS.iter().enumerate() {
        for (index, shape) in shapes.iter().enumerate() {
            if shape.refusal.is_none() {
                assert!(
                    coverage.entries[pool][index] >= 1,
                    "no accepted random edits for pool={pool}, entry={index}: {shape:?}"
                );
            }
        }
    }
    eprintln!(
        "composed accepted edits by kind: {:?} / {:?}; refused: {}; accepted edits per pool entry: {:?}",
        coverage.accepted,
        coverage.attempted,
        refused.get(),
        coverage.entries
    );
}

#[test]
fn standalone_lines_survive_edits_including_owned_node_tails() {
    let refused = Cell::new(0);
    for body in [
        "# comment\n\n",
        "# header comment\nforeign: 1\n\n# mid\nfolio-name: old\n\n# tail\n",
        "folio-references:\n  logo:\n    name: old\n\n# trailing reference comment\n\n",
        "folio-references:\n  logo:\n    name: old\n\n# between aliases\n  other:\n    name: other\n\n# tail\n",
    ] {
        for bom in ["", "\u{feff}"] {
            for ending in ["\n", "\r\n"] {
                let bytes = format!(
                    "{bom}{}",
                    format!("---\n{body}---\nbody\n").replace('\n', ending)
                )
                .into_bytes();
                for op in operations() {
                    for create in [false, true] {
                        check(&bytes, &op, create, &refused, true).unwrap();
                    }
                }
            }
        }
    }
    assert_eq!(refused.get(), 0);
}

#[test]
fn block_creation_preserves_all_original_bytes() {
    let refused = Cell::new(0);
    for body in ["", "# Body\n", "# Body\n---\nlater: document\n...\n"] {
        for bom in ["", "\u{feff}"] {
            for ending in ["\n", "\r\n"] {
                let bytes = format!("{bom}{}", body.replace('\n', ending)).into_bytes();
                assert_eq!(locate(&bytes), Ok(None));
                for op in operations() {
                    for create in [false, true] {
                        let accepted = check(&bytes, &op, create, &refused, create).unwrap();
                        assert_eq!(
                            accepted,
                            create && op.setting(),
                            "{}",
                            Context {
                                bytes: &bytes,
                                op: &op,
                                create
                            }
                        );
                    }
                }
            }
        }
    }
    assert_eq!(refused.get(), 0);
}
