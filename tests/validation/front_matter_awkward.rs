use folt_core::folio_dir::FolioId;
use folt_core::front_matter::{
    Block, Edit, FrontMatterError, Reference, SCAN_LIMIT, apply_edit, locate, read_id, read_name,
    read_references, set_id, set_name, set_reference, set_references,
};

const ID: &str = "12345678-1234-4234-8234-123456789abc";
const NEW_ID: &str = "12345678-1234-4234-8234-123456789abd";
type Result<T> = std::result::Result<T, FrontMatterError>;

#[derive(Clone, Debug, PartialEq)]
struct Reads {
    name: Result<Option<String>>,
    id: Result<Option<FolioId>>,
    refs: Result<Vec<Reference<'static>>>,
}

impl Reads {
    fn empty() -> Self {
        Self {
            name: Ok(None),
            id: Ok(None),
            refs: Ok(vec![]),
        }
    }

    fn refused(error: FrontMatterError) -> Self {
        Self {
            name: Err(error),
            id: Err(error),
            refs: Err(error),
        }
    }
}

fn reads(bytes: &[u8]) -> Reads {
    Reads {
        name: read_name(bytes).map(|v| v.map(|v| v.into_owned())),
        id: read_id(bytes),
        refs: read_references(bytes).map(|rs| {
            rs.into_iter()
                .map(|r| Reference {
                    alias: r.alias.into_owned().into(),
                    name: r.name.map(|s| s.into_owned().into()),
                    id: r.id,
                    pin: r.pin.map(|s| s.into_owned().into()),
                })
                .collect()
        }),
    }
}

fn logo(name: &str, pin: Option<&str>) -> Reference<'static> {
    Reference {
        alias: "logo".into(),
        name: Some(name.to_owned().into()),
        id: None,
        pin: pin.map(|s| s.to_owned().into()),
    }
}

#[derive(Clone, Copy, Debug)]
enum Operation {
    Name(bool),
    Id(bool),
    References(bool),
    Alias(bool),
}

impl Operation {
    fn plan(self, bytes: &[u8]) -> Result<Option<Edit>> {
        self.plan_with_create(bytes, false)
    }

    fn plan_with_create(self, bytes: &[u8], create: bool) -> Result<Option<Edit>> {
        match self {
            Self::Name(set) => set_name(bytes, set.then_some("new"), create),
            Self::Id(set) => set_id(bytes, set.then(|| FolioId::parse(NEW_ID).unwrap()), create),
            Self::References(set) => set_references(
                bytes,
                &if set {
                    vec![logo("new", Some("pin"))]
                } else {
                    vec![]
                },
                create,
            ),
            Self::Alias(set) => set_reference(
                bytes,
                "logo",
                set.then(|| logo("new", Some("pin"))).as_ref(),
                create,
            ),
        }
    }

    fn written(self, before: &Reads) -> Reads {
        let mut after = before.clone();
        match self {
            Self::Name(set) => after.name = Ok(set.then(|| "new".into())),
            Self::Id(set) => after.id = Ok(set.then(|| FolioId::parse(NEW_ID).unwrap())),
            Self::References(set) | Self::Alias(set) => {
                after.refs = Ok(if set {
                    vec![logo("new", Some("pin"))]
                } else {
                    vec![]
                })
            }
        }
        after
    }
}

struct ExpectEdit {
    operation: Operation,
    error: Option<FrontMatterError>,
    output: Option<Vec<u8>>,
}

struct Case {
    name: String,
    input: Vec<u8>,
    reads: Reads,
    locate_error: Option<FrontMatterError>,
    edits: Vec<ExpectEdit>,
    preserved: Vec<Vec<u8>>,
}

fn edits(operations: &[Operation], error: Option<FrontMatterError>) -> Vec<ExpectEdit> {
    operations
        .iter()
        .map(|&operation| ExpectEdit {
            operation,
            error,
            output: None,
        })
        .collect()
}

fn name_case(name: &str, input: &str, value: Option<&str>) -> Case {
    Case {
        name: name.into(),
        input: input.as_bytes().to_vec(),
        reads: Reads {
            name: Ok(value.map(str::to_owned)),
            ..Reads::empty()
        },
        locate_error: None,
        edits: edits(&[Operation::Name(true), Operation::Name(false)], None),
        preserved: vec![],
    }
}

// Compare the actual foreign spelling/value and non-comment continuation, in order.
type ForeignBytes = (String, Vec<u8>, Vec<u8>, Vec<u8>);

fn foreign(bytes: &[u8], block: &Block) -> Vec<ForeignBytes> {
    block
        .nodes
        .iter()
        .filter(|n| !["folio-name", "folio-id", "folio-references"].contains(&n.key.as_str()))
        .map(|n| {
            let mut continuation = vec![];
            for line in bytes[n.continuation.clone()].split_inclusive(|b| *b == b'\n') {
                let first = line.iter().find(|b| !b.is_ascii_whitespace());
                if first.is_some_and(|b| *b != b'#') {
                    continuation.extend_from_slice(line);
                }
            }
            (
                n.key.clone(),
                bytes[n.key_range.clone()].to_vec(),
                bytes[n.value.clone()].to_vec(),
                continuation,
            )
        })
        .collect()
}

fn check(case: &Case) {
    let context = format!(
        "case={}, bytes=\"{}\"",
        case.name,
        case.input.escape_ascii()
    );
    assert_eq!(reads(&case.input), case.reads, "{context}");
    let original = locate(&case.input);
    assert_eq!(
        original.as_ref().err().copied(),
        case.locate_error,
        "{context}"
    );
    if case.locate_error.is_none() {
        assert!(
            original.as_ref().unwrap().is_some(),
            "{context}: missing original block"
        );
    }
    for expected in &case.edits {
        let context = format!("{context}, operation={:?}", expected.operation);
        let planned = expected.operation.plan(&case.input);
        if let Some(error) = expected.error {
            // Result's Err arm cannot contain an Edit.
            assert_eq!(planned, Err(error), "{context}");
            continue;
        }
        let edit = planned
            .unwrap_or_else(|e| panic!("{context}: {e}"))
            .unwrap_or_else(|| panic!("{context}: expected an edit"));
        assert!(
            edit.range.start <= edit.range.end && edit.range.end <= case.input.len(),
            "{context}: invalid edit range"
        );
        let output = apply_edit(&case.input, &edit).unwrap_or_else(|e| panic!("{context}: {e}"));
        assert_eq!(
            output,
            [
                &case.input[..edit.range.start],
                &edit.replacement,
                &case.input[edit.range.end..]
            ]
            .concat(),
            "{context}"
        );
        if let Some(exact) = &expected.output {
            assert_eq!(&output, exact, "{context}");
        }
        let block = locate(&output)
            .unwrap_or_else(|e| panic!("{context}: {e}"))
            .unwrap_or_else(|| panic!("{context}: missing block"));
        if let Ok(Some(old)) = &original {
            assert!(output.ends_with(&case.input[old.range.end..]), "{context}");
            assert_eq!(
                foreign(&case.input, old),
                foreign(&output, &block),
                "{context}"
            );
        }
        for fragment in &case.preserved {
            assert!(
                output.windows(fragment.len()).any(|w| w == fragment),
                "{context}: lost {}",
                fragment.escape_ascii()
            );
        }
        assert_eq!(
            reads(&output),
            expected.operation.written(&case.reads),
            "{context}"
        );
        if let Some(again) = expected
            .operation
            .plan(&output)
            .unwrap_or_else(|e| panic!("{context}: repeated: {e}"))
        {
            assert_eq!(
                apply_edit(&output, &again).unwrap(),
                output,
                "{context}: repeated"
            );
        }
    }
}

// #148: whitespace before a nested key's colon must not become part of the key.
#[test]
fn nested_reference_key_spacing() {
    let mut rows = vec![];
    for spacing in [" ", "\t"] {
        for flow in [false, true] {
            for (field, value, expected) in [
                ("name", "image", logo("image", None)),
                (
                    "id",
                    ID,
                    Reference {
                        alias: "logo".into(),
                        name: None,
                        id: Some(FolioId::parse(ID).unwrap()),
                        pin: None,
                    },
                ),
                ("pin", "abc", logo("image", Some("abc"))),
            ] {
                let target = if field == "pin" { "name: image" } else { "" };
                let collection = if flow {
                    let separator = if target.is_empty() { "" } else { ", " };
                    format!(
                        "folio-references: {{logo: {{{target}{separator}{field}{spacing}: {value}}}}}\n"
                    )
                } else {
                    let target = if target.is_empty() {
                        String::new()
                    } else {
                        format!("    {target}\n")
                    };
                    format!("folio-references:\n  logo:\n{target}    {field}{spacing}: {value}\n")
                };
                rows.push((collection, expected));
            }
        }
    }
    rows.push((
        "folio-references: {logo: {name\t: image, pin : abc}}\n".into(),
        logo("image", Some("abc")),
    ));
    for spacing in [" ", "\t"] {
        rows.push((
            format!("folio-references: {{logo{spacing}: {{name: image}}}}\n"),
            logo("image", None),
        ));
    }
    for (collection, expected) in rows {
        let mut case = Case {
            name: "#148 nested reference key spacing".into(),
            input: format!("---\n{collection}---\n").into_bytes(),
            reads: Reads {
                refs: Ok(vec![expected]),
                ..Reads::empty()
            },
            locate_error: None,
            edits: edits(
                &[
                    Operation::References(true),
                    Operation::References(false),
                    Operation::Alias(true),
                    Operation::Alias(false),
                ],
                None,
            ),
            preserved: vec![],
        };
        let spacing = if collection.starts_with("folio-references:\n") {
            ""
        } else {
            " "
        };
        case.edits[0].output = Some(format!("---\nfolio-references:{spacing}\n  logo:\n    name: \"new\"\n    pin: \"pin\"\n---\n").into_bytes());
        case.edits[1].output = Some(b"---\n---\n".to_vec());
        check(&case);
    }
}

#[test]
fn reference_comment_positions() {
    for (name, collection, expected, comment) in [
        (
            "comment before flow value",
            "folio-references:\n  logo: {name: # keep\n    image}\n",
            logo("image", None),
            "# keep",
        ),
        (
            "comment after colon",
            "folio-references:\n  logo: {name: # c\n image}\n",
            logo("image", None),
            "# c",
        ),
        (
            "comment after opening brace",
            "folio-references:\n  logo: { # c\n name: image}\n",
            logo("image", None),
            "# c",
        ),
        (
            "comment before closing brace",
            "folio-references:\n  logo: {name: image # c\n }\n",
            logo("image", None),
            "# c",
        ),
        (
            "comment between key and colon",
            "folio-references:\n  logo: {name # c\n : image}\n",
            logo("image", None),
            "# c",
        ),
        (
            "wrapped entry closing-line comment",
            "folio-references:\n  logo: {name: old,\n    pin: abc} # keep\n",
            logo("old", Some("abc")),
            "# keep",
        ),
    ] {
        let mut case = Case {
            name: format!("#148 {name}"),
            input: format!("---\n{collection}---\n").into_bytes(),
            reads: Reads {
                refs: Ok(vec![expected]),
                ..Reads::empty()
            },
            locate_error: None,
            edits: edits(
                &[
                    Operation::Name(true),
                    Operation::Id(true),
                    Operation::References(true),
                    Operation::References(false),
                ],
                None,
            ),
            preserved: vec![],
        };
        check(&case);
        // Alias edits preserve the entry's comments even when removing it.
        // Clearing the whole collection also removes its comments and header.
        case.edits = edits(&[Operation::Alias(true), Operation::Alias(false)], None);
        case.preserved.push(comment.as_bytes().to_vec());
        check(&case);
    }
}

#[test]
fn empty_reference_header_comment_stays_separated() {
    let mut case = Case {
        name: "#148 empty reference collection with header comment".into(),
        input: b"---\nfolio-references: # header\n---\n".to_vec(),
        reads: Reads::empty(),
        locate_error: None,
        edits: edits(&[Operation::References(true), Operation::Alias(true)], None),
        preserved: vec![b"# header\n".to_vec()],
    };
    for expected in &mut case.edits {
        expected.output = Some(
            b"---\nfolio-references: # header\n  logo:\n    name: \"new\"\n    pin: \"pin\"\n---\n"
                .to_vec(),
        );
    }
    check(&case);
}

#[test]
fn structured_awkward_inputs() {
    use FrontMatterError::{Ambiguous, Binary, InvalidValue, Unterminated};
    let all = [
        Operation::Name(true),
        Operation::Name(false),
        Operation::Id(true),
        Operation::Id(false),
        Operation::References(true),
        Operation::References(false),
        Operation::Alias(true),
        Operation::Alias(false),
    ];
    let mut cases = vec![];
    for (name, body) in [
        ("escaped hex owned key", "\"folio-\\x6eame\": v\n"),
        ("double quoted owned key", "\"folio-name\": v\n"),
        ("single quoted owned key", "'folio-name': v\n"),
        ("tagged owned key", "!!str folio-name: v\n"),
        ("anchored owned key", "&a folio-name: v\n"),
        ("explicit tab owned key", "?\tfolio-name\n: v\n"),
        ("explicit line end owned key", "? folio-name\n: v\n"),
        ("explicit CRLF owned key", "? folio-name\r\n: v\r\n"),
        ("explicit null owned key", "? folio-name\n"),
        ("explicit tab null owned key", "?\tfolio-name\n"),
        (
            "explicit multiline CRLF owned key",
            "?\r\n  folio-name\r\n: v\r\n",
        ),
        ("explicit multiline owned key", "?\n  folio-name\n: v\n"),
    ] {
        cases.push(Case {
            name: name.into(),
            input: format!("---\n{body}---\n").into_bytes(),
            reads: if name == "double quoted owned key" || name == "single quoted owned key" {
                Reads {
                    name: Err(Ambiguous),
                    ..Reads::empty()
                }
            } else {
                Reads::refused(Ambiguous)
            },
            locate_error: None,
            edits: edits(&all, Some(Ambiguous)),
            preserved: vec![],
        });
    }
    for (name, line, old) in [
        ("leading hyphen scalar", "folio-name: -x", "-x"),
        ("hyphen alone scalar", "folio-name: -", "-"),
        ("trailing scalar comment", "folio-name: old # note", "old"),
        ("scalar spacing", "folio-name :  old", "old"),
        ("spaces before colon", "folio-name   : v", "v"),
        ("tab value separator", "folio-name:\told", "old"),
    ] {
        let input = format!("---\n{line}\n# standalone\nforeign: keep\n---\nbody\n");
        let mut case = name_case(name, &input, Some(old));
        case.edits[0].output = Some(input.replacen(old, "\"new\"", 1).into_bytes());
        if name == "hyphen alone scalar" {
            case.reads.name = Err(InvalidValue);
            case.edits = edits(
                &[Operation::Name(true), Operation::Name(false)],
                Some(InvalidValue),
            );
        }
        case.preserved.push(b"# standalone\n".to_vec());
        cases.push(case);
    }
    for (name, input) in [
        (
            "CRLF",
            "---\r\nforeign: keep\r\nfolio-name: old\r\n---\r\nbody\r\n",
        ),
        (
            "mixed endings",
            "---\r\nforeign: keep\nfolio-name: old\r\n---\nbody\r\n",
        ),
        ("UTF-8 BOM", "\u{feff}---\nfolio-name: old\n---\nbody\n"),
        (
            "block scalar delimiter",
            "---\nforeign: |\n  ---\n  keep\nfolio-name: old\n---\nbody\n",
        ),
        (
            "multi-document stream",
            "---\nfolio-name: old\n---\nbody\n---\nfolio-name: later\n...\n",
        ),
        (
            "foreign multiline explicit key",
            "---\n?\n  other\n  key\n: value\nfolio-name: old\n---\n",
        ),
        (
            "foreign null explicit key",
            "---\n? other\nfolio-name: old\n---\n",
        ),
    ] {
        let mut case = name_case(name, input, Some("old"));
        case.edits[0].output = Some(input.replacen("old", "\"new\"", 1).into_bytes());
        cases.push(case);
    }
    cases.push(name_case(
        "60 KB foreign value",
        &format!(
            "---\nforeign: {}\nfolio-name: old\n---\nbody",
            "x".repeat(60_000)
        ),
        Some("old"),
    ));
    let mut id = name_case(
        "id spaces before colon",
        &format!("---\nfolio-id : {ID}\n---\n"),
        None,
    );
    id.reads.id = Ok(Some(FolioId::parse(ID).unwrap()));
    id.edits = edits(&[Operation::Id(true), Operation::Id(false)], None);
    id.edits[0].output = Some(format!("---\nfolio-id : {NEW_ID}\n---\n").into_bytes());
    id.edits[1].output = Some(b"---\n---\n".to_vec());
    cases.push(id);

    for (name, collection, refs) in [
        (
            "block references",
            "folio-references:\n  logo:\n    name: a\n    pin: b\n",
            Ok(vec![logo("a", Some("b"))]),
        ),
        (
            "block entry with flow reference",
            "folio-references:\n  logo: {name: a, pin: b}\n",
            Ok(vec![logo("a", Some("b"))]),
        ),
        (
            "reference id target with pin",
            "folio-references: {logo: {id: 12345678-1234-4234-8234-123456789abc, pin: b}}\n",
            Ok(vec![Reference {
                alias: "logo".into(),
                name: None,
                id: Some(FolioId::parse(ID).unwrap()),
                pin: Some("b".into()),
            }]),
        ),
        (
            "flow references",
            "folio-references: {logo: {name: a, pin: b}}\n",
            Ok(vec![logo("a", Some("b"))]),
        ),
        (
            "wrapped flow references",
            "folio-references: {logo: {name: a,\n    pin: b}}\n",
            Ok(vec![logo("a", Some("b"))]),
        ),
        (
            "comments inside flow references",
            "folio-references: {logo: {name: a, # c\n pin: b}}\n",
            Ok(vec![logo("a", Some("b"))]),
        ),
        (
            "reference header comment",
            "folio-references: # header\n  logo:\n    name: a\n",
            Ok(vec![logo("a", None)]),
        ),
        (
            "empty nested name",
            "folio-references:\n  logo:\n    name:\n",
            Err(InvalidValue),
        ),
        (
            "tilde nested name",
            "folio-references:\n  logo:\n    name: ~\n",
            Ok(vec![logo("~", None)]),
        ),
        (
            "null nested name",
            "folio-references:\n  logo:\n    name: null\n",
            Ok(vec![logo("null", None)]),
        ),
    ] {
        let error = refs.as_ref().err().copied();
        let mut case = Case {
            name: name.into(),
            input: format!("---\n{collection}# standalone\nforeign: keep\n---\nbody\n")
                .into_bytes(),
            reads: Reads {
                refs,
                ..Reads::empty()
            },
            locate_error: None,
            edits: edits(
                &[
                    Operation::References(true),
                    Operation::References(false),
                    Operation::Alias(true),
                    Operation::Alias(false),
                ],
                error,
            ),
            preserved: vec![b"# standalone\n".to_vec()],
        };
        if error.is_none() && !collection.contains('#') {
            let spacing = if collection.starts_with("folio-references:\n") {
                ""
            } else {
                " "
            };
            case.edits[0].output = Some(format!("---\nfolio-references:{spacing}\n  logo:\n    name: \"new\"\n    pin: \"pin\"\n# standalone\nforeign: keep\n---\nbody\n").into_bytes());
            case.edits[1].output = Some(b"---\n# standalone\nforeign: keep\n---\nbody\n".to_vec());
        }
        // Clearing the entire collection removes its header; alias edits retain it.
        if name == "reference header comment" {
            case.edits[0].output = Some(b"---\nfolio-references: # header\n  logo:\n    name: \"new\"\n    pin: \"pin\"\n# standalone\nforeign: keep\n---\nbody\n".to_vec());
        }
        if name == "reference header comment" {
            case.edits[2].output = case.edits[0].output.clone();
            case.edits[3].output = Some(
                b"---\nfolio-references: # header\n# standalone\nforeign: keep\n---\nbody\n"
                    .to_vec(),
            );
        }
        cases.push(case);
    }
    let mut combined = name_case(
        "other owned keys preserved",
        &format!(
            "---\nforeign: {{a: 1, b: [x, y]}}\nfolio-name: old\nfolio-id: {ID}\nfolio-references: {{logo: {{name: a, pin: b}}}}\n---\nbody"
        ),
        Some("old"),
    );
    combined.reads.id = Ok(Some(FolioId::parse(ID).unwrap()));
    combined.reads.refs = Ok(vec![logo("a", Some("b"))]);
    combined.edits = edits(&all, None);
    cases.push(combined);
    for offset in [65_535, 65_536, 65_537] {
        let prefix = "---\nfolio-name: x\npadding: ";
        let input = format!(
            "{prefix}{}\n---\nbody",
            "p".repeat(offset - prefix.len() - 1)
        );
        let mut case = name_case(&format!("64 KiB closer at {offset}"), &input, Some("x"));
        if offset >= SCAN_LIMIT {
            case.reads = Reads::refused(Unterminated);
            case.locate_error = Some(Unterminated);
        }
        case.edits = edits(&[Operation::Name(true)], Some(Unterminated));
        if offset < SCAN_LIMIT {
            case.edits.extend(edits(&[Operation::Name(false)], None));
        }
        cases.push(case);
    }
    for (name, input, error) in [
        (
            "missing closing delimiter",
            b"---\nfolio-name: old\n".to_vec(),
            Unterminated,
        ),
        ("binary prefix", b"---\nforeign: \0\n---\n".to_vec(), Binary),
    ] {
        cases.push(Case {
            name: name.into(),
            input,
            reads: Reads::refused(error),
            locate_error: Some(error),
            edits: edits(&all, Some(error)),
            preserved: vec![],
        });
    }
    let mut beyond = name_case(
        "NUL beyond scan limit",
        "---\nfolio-name: old\n---\n",
        Some("old"),
    );
    beyond.input.resize(SCAN_LIMIT + 100, b'x');
    beyond.input.push(0);
    cases.push(beyond);
    for case in cases {
        check(&case);
    }
}

#[test]
fn name_edits_preserve_preamble_and_standalone_lines() {
    for bom in ["", "\u{feff}"] {
        for ending in ["\n", "\r\n"] {
            let render = |text: &str| format!("{bom}{}", text.replace('\n', ending));
            let input = render("---\n# comment\n\n---\nbody\n");
            assert_eq!(set_name(input.as_bytes(), None, false), Ok(None));
            let mut case = name_case("comment-only preamble", &input, None);
            case.edits = edits(&[Operation::Name(true)], None);
            case.edits[0].output =
                Some(render("---\n# comment\n\nfolio-name: \"new\"\n---\nbody\n").into_bytes());
            check(&case);

            let input = render(
                "---\n# header comment\nforeign: 1\n\n# mid\nfolio-name: old\n\n# tail\n---\n",
            );
            let mut case = name_case("comments around name", &input, Some("old"));
            case.edits[0].output = Some(input.replace("old", "\"new\"").into_bytes());
            case.edits[1].output = Some(
                input
                    .replace(&format!("folio-name: old{ending}"), "")
                    .into_bytes(),
            );
            check(&case);
        }
    }
}

#[test]
fn creation_keeps_later_documents_exactly() {
    for bom in ["", "\u{feff}"] {
        for ending in ["\n", "\r\n"] {
            let original = format!(
                "{bom}{}",
                "# Body\n---\nlater: document\n...\n".replace('\n', ending)
            );
            let bytes = original.as_bytes();
            assert_eq!(locate(bytes), Ok(None));
            // Creation uses LF even when the original body uses CRLF.
            for (operation, inserted) in [
                (Operation::Name(true), "---\nfolio-name: \"new\"\n---\n"),
                (
                    Operation::Id(true),
                    "---\nfolio-id: 12345678-1234-4234-8234-123456789abd\n---\n",
                ),
                (
                    Operation::References(true),
                    "---\nfolio-references: \n  logo:\n    name: \"new\"\n    pin: \"pin\"\n---\n",
                ),
                (
                    Operation::Alias(true),
                    "---\nfolio-references: \n  logo:\n    name: \"new\"\n    pin: \"pin\"\n---\n",
                ),
            ] {
                let context = format!(
                    "bytes=\"{}\", operation={operation:?}, create=true",
                    bytes.escape_ascii()
                );
                let edit = operation.plan_with_create(bytes, true).unwrap().unwrap();
                let position = bom.len();
                assert_eq!(edit.range, position..position, "{context}");
                let output = apply_edit(bytes, &edit).unwrap();
                assert_eq!(
                    output,
                    format!("{bom}{inserted}{}", &original[position..]).as_bytes(),
                    "{context}"
                );
                assert_eq!(
                    [&output[..position], &output[position + inserted.len()..]].concat(),
                    bytes,
                    "{context}: original bytes lost"
                );
                assert!(locate(&output).unwrap().is_some(), "{context}");
                assert_eq!(
                    reads(&output),
                    operation.written(&Reads::empty()),
                    "{context}"
                );
                if let Some(again) = operation.plan_with_create(&output, true).unwrap() {
                    assert_eq!(apply_edit(&output, &again).unwrap(), output, "{context}");
                }
            }
        }
    }
}
