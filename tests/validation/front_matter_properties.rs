use std::cell::Cell;

use folt_core::folio_dir::FolioId;
use folt_core::front_matter::{
    Block, Edit, FrontMatterError, Reference, apply_edit, locate, read_id, read_name,
    read_references, set_id, set_name, set_reference, set_references,
};
use proptest::prelude::*;
use proptest::test_runner::{RngSeed, TestCaseResult, TestRunner};

const ID: &str = "12345678-1234-4234-8234-123456789abc";
const OWNED: [&str; 3] = ["folio-name", "folio-id", "folio-references"];

// Indices into these pools shrink before rendering, so every failure below includes
// the escaped, rendered bytes as well as the operation.
const FOREIGN: &[&str] = &[
    "foreign: plain\n",
    "'quoted key': 'quoted value'\n",
    "# comment\n\n",
    "foreign: |\n  ---\n  literal\n",
    "flow: {a: 1, b: [x, y]}\n",
    // #167 exclusion lifted now that #170's Hidden audit is in the base.
    // Malformed flows may be refused; accepted edits must still read back.
    "flow: {a:]1,\n  b: 2}\n",
    "flow: {a: 1,\n  b: 2\n",
    "flow: [a, b\n",
    "flow: [a, b}\n",
    "flow: ]\n",
    "flow: {a: 1, # keep\n  b: [x, y]}\n",
    "? other\n: value\n",
    "? other\n",
    "?\n  other\n: value\n",
    "? other\n  key\n: value\n",
    "!!str other: value\n",
    "&a other: value\n",
    "foreign: !!str value\n",
    "foreign: &a value\n",
    "foreign:\tvalue\n",
];
const NAMES: &[&str] = &[
    "",
    "folio-name: old\n",
    "folio-name :  v # c\n",
    "folio-name: 'old'\n",
    "folio-name: \"old\"\n",
    "? folio-name\n: old\n",
    "? folio-name\n",
    "\"folio-name\": old\n",
    "\"\\u0066olio-name\": old\n",
    "folio-name: !!str old\n",
    "folio-name: &a old\n",
    "folio-name: old\n\tcontinued\n",
    "folio-name: a\nfolio-name: b\n",
];
const IDS: &[&str] = &[
    "",
    "folio-id: 12345678-1234-4234-8234-123456789abc\n",
    "folio-id: '12345678-1234-4234-8234-123456789abc'\n",
    "? folio-id\n: old\n",
    "folio-id: !!str old\n",
];
const REFERENCES: &[&str] = &[
    // #148 follow-up: each distinct flow comment position, an empty commented
    // header, and a comment on the closing line of a wrapped entry.
    "folio-references:\n  logo: {name: # keep\n    image}\n",
    "folio-references:\n  logo: {name: # c\n image}\n",
    "folio-references:\n  logo: { # c\n name: image}\n",
    "folio-references:\n  logo: {name: image # c\n }\n",
    "folio-references:\n  logo: {name # c\n : image}\n",
    "folio-references: # header\n",
    "folio-references:\n  logo: {name: old,\n    pin: abc} # keep\n",
    // #148: nested key spacing, including tabs and flow alias spacing.
    "folio-references:\n  logo:\n    name : image\n",
    "folio-references:\n  logo:\n    name\t: image\n",
    "folio-references:\n  logo:\n    id : 12345678-1234-4234-8234-123456789abc\n",
    "folio-references:\n  logo:\n    id\t: 12345678-1234-4234-8234-123456789abc\n",
    "folio-references:\n  logo:\n    name: image\n    pin : abc\n",
    "folio-references:\n  logo:\n    name: image\n    pin\t: abc\n",
    "folio-references: {logo: {name : image}}\n",
    "folio-references: {logo: {name\t: image}}\n",
    "folio-references: {logo: {id : 12345678-1234-4234-8234-123456789abc}}\n",
    "folio-references: {logo: {id\t: 12345678-1234-4234-8234-123456789abc}}\n",
    "folio-references: {logo: {name: image, pin : abc}}\n",
    "folio-references: {logo: {name: image, pin\t: abc}}\n",
    "folio-references: {logo: {name\t: image, pin : abc}}\n",
    "folio-references: {logo : {name: image}}\n",
    "folio-references: {logo\t: {name: image}}\n",
    "",
    "folio-references: {}\n",
    "folio-references: {logo: {name: old}, other: {name: other}}\n",
    "folio-references:\n  logo:\n    name: old\n    pin: abc\n  other: {name: other}\n",
    "folio-references: {logo: {id: 12345678-1234-4234-8234-123456789abc, pin: abc}}\n",
    "folio-references: {logo: {name: old, # inner\n pin: abc}, # outer\n other: {name: other}}\n",
    "folio-references:\n  logo: {name: old, # inner\n    pin: abc} # tail\n",
    "folio-references:\n  logo:\n    name:\n",
    "folio-references: {logo: {name:}}\n",
    "? folio-references\n: {}\n",
];

fn documents() -> impl Strategy<Value = Vec<u8>> {
    (
        prop::collection::vec(prop::sample::select(FOREIGN), 0..4),
        prop::sample::select(NAMES),
        prop::sample::select(IDS),
        prop::sample::select(REFERENCES),
        0..3usize,
        any::<bool>(),
        0..12usize,
        prop_oneof![38 => Just(0usize), 1 => 1..60_000usize, 1 => 65_400..66_001usize],
    )
        .prop_map(|(foreign, name, id, refs, ending, bom, shape, padding)| {
            let mut doc = if bom {
                "\u{feff}".to_owned()
            } else {
                String::new()
            };
            if shape != 0 {
                doc.push_str("---\n");
                for node in foreign {
                    doc.push_str(node);
                }
                doc.push_str(name);
                doc.push_str(id);
                doc.push_str(refs);
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
            doc.into_bytes()
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
fn check(bytes: &[u8], op: &Operation, create: bool, refused: &Cell<usize>) -> TestCaseResult {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        check_inner(bytes, op, create, refused)
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
) -> TestCaseResult {
    let context = Context { bytes, op, create };
    let original_block = locate(bytes);
    let before_name = read_name(bytes);
    let before_id = read_id(bytes);
    let before_refs = read_references(bytes);
    let planned = op.plan(bytes, create);
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
            return Ok(());
        }
        Ok(None) => return Ok(()),
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
        prop_assert!(result.ends_with(&bytes[old.range.end..]), "{}", context);
        prop_assert_eq!(
            foreign(bytes, &old),
            foreign(&result, block.as_ref().unwrap()),
            "{}",
            context
        );
    } else {
        prop_assert!(create, "{}", context);
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
    Ok(())
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
                        check(&bytes, &op, create, &refused)?;
                    }
                }
            }
            Ok(())
        })
        .unwrap();
    eprintln!("arbitrary-byte refused operations: {}", refused.get());
}

#[test]
fn composed_documents_preserve_and_round_trip() {
    let refused = Cell::new(0);
    runner(400, 15302)
        .run(
            &(
                documents(),
                prop::sample::select(operations()),
                any::<bool>(),
            ),
            |(bytes, op, create)| check(&bytes, &op, create, &refused),
        )
        .unwrap();
    eprintln!("composed-document refused operations: {}", refused.get());
}
