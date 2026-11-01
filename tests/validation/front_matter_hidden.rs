use folt_core::folio_dir::FolioId;
use folt_core::front_matter::{
    FrontMatterError, Reference, apply_edit, locate, read_id, read_name, read_references, set_id,
    set_name, set_reference, set_references,
};

const ID: &str = "12345678-1234-4234-8234-123456789abc";
const NEW_ID: &str = "12345678-1234-4234-8234-123456789abd";
const OWNED: &[&str] = &[
    "folio-name: x\n",
    "folio-id: 12345678-1234-4234-8234-123456789abc\n",
    "folio-references:\n  a:\n    name: x\n",
];

fn reference(name: &'static str) -> Reference<'static> {
    Reference {
        alias: "a".into(),
        name: Some(name.into()),
        id: None,
        pin: None,
    }
}

#[test]
fn malformed_foreign_flows_cannot_hide_owned_keys() {
    // #167/#170: the five bare prefixes requested by #153 are accepted and
    // their following owned keys remain readable, and all four setters read
    // back correctly. Excluded from Hidden assertions without a later closer:
    // `flow: {a:]1,\n  b: 2}`, `flow: {a: 1,\n  b: 2`, `flow: [a, b`,
    // `flow: [a, b}`, and `flow: ]`. Later closers reproduce swallowing below.
    // In particular `flow: ]` cannot start a flow node; leave that Hidden row
    // out rather than enshrining its acceptance as the required behaviour.
    let rows = [
        (
            "stray bracket in mapping",
            "flow: {a:]1,\n  b: 2}\n",
            "tail: {{{}}}\n",
        ),
        ("unclosed mapping", "flow: {a: 1,\n  b: 2\n", "}\n"),
        ("unclosed sequence", "flow: [a, b\n", "]\n"),
        ("mismatched sequence closer", "flow: [a, b}\n", "tail: []\n"),
    ];
    for (row, prefix, suffix) in rows {
        // Exercise each owned key alone as well as all three together, so a
        // swallowed name cannot mask a missing audit for ids or references.
        for owned in OWNED.iter().copied().chain(std::iter::once(
            "folio-name: x\nfolio-id: 12345678-1234-4234-8234-123456789abc\nfolio-references:\n  a:\n    name: x\n",
        )) {
            for ending in ["\n", "\r\n"] {
                let input = format!("---\n{prefix}{owned}{suffix}---\nbody\n")
                    .replace('\n', ending);
                let bytes = input.as_bytes();
                let context = format!("row={row}, bytes=\"{}\"", bytes.escape_ascii());
                assert_eq!(locate(bytes), Err(FrontMatterError::Hidden), "{context}");
                assert_eq!(read_name(bytes), Err(FrontMatterError::Hidden), "{context}");
                assert_eq!(read_id(bytes), Err(FrontMatterError::Hidden), "{context}");
                assert_eq!(read_references(bytes), Err(FrontMatterError::Hidden), "{context}");
                for create in [false, true] {
                    for set in [false, true] {
                        let refs = if set { vec![reference("new")] } else { vec![] };
                        // Err contains no Edit by construction, for sets and removals.
                        for (operation, planned) in [
                            ("set_name", set_name(bytes, set.then_some("new"), create)),
                            ("set_id", set_id(bytes, set.then(|| FolioId::parse(NEW_ID).unwrap()), create)),
                            ("set_references", set_references(bytes, &refs, create)),
                            ("set_reference", set_reference(bytes, "a", set.then(|| reference("new")).as_ref(), create)),
                        ] {
                            assert_eq!(planned, Err(FrontMatterError::Hidden), "{context}, operation={operation}, set={set}, create={create}");
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn well_formed_foreign_flow_counterparts_read_and_edit_normally() {
    let rows = [
        ("mapping with sequence", "flow: {a: [1],\n  b: 2}\n"),
        ("closed mapping", "flow: {a: 1,\n  b: 2}\n"),
        ("closed sequence", "flow: [a, b]\n"),
        ("matching sequence closer", "flow: [a,\n  b]\n"),
        ("empty sequence", "flow: []\n"),
        (
            "commented wrapped mapping",
            "flow: {a: 1, # keep\n  b: [x, y]}\n",
        ),
    ];
    for (row, foreign) in rows {
        for ending in ["\n", "\r\n"] {
            let input =
                format!("---\n{foreign}{}---\nbody\n", OWNED.concat()).replace('\n', ending);
            let bytes = input.as_bytes();
            let context = format!("row={row}, bytes=\"{}\"", bytes.escape_ascii());
            let old_id = FolioId::parse(ID).unwrap();
            assert_eq!(read_name(bytes).unwrap().as_deref(), Some("x"), "{context}");
            assert_eq!(read_id(bytes).unwrap(), Some(old_id), "{context}");
            assert_eq!(
                read_references(bytes).unwrap(),
                vec![reference("x")],
                "{context}"
            );
            for create in [false, true] {
                for set in [false, true] {
                    let new_id = set.then(|| FolioId::parse(NEW_ID).unwrap());
                    let refs = if set { vec![reference("new")] } else { vec![] };
                    for (operation, planned) in [
                        ("set_name", set_name(bytes, set.then_some("new"), create)),
                        ("set_id", set_id(bytes, new_id, create)),
                        ("set_references", set_references(bytes, &refs, create)),
                        (
                            "set_reference",
                            set_reference(
                                bytes,
                                "a",
                                set.then(|| reference("new")).as_ref(),
                                create,
                            ),
                        ),
                    ] {
                        let context =
                            format!("{context}, operation={operation}, set={set}, create={create}");
                        let edit = planned
                            .unwrap_or_else(|e| panic!("{context}: {e}"))
                            .unwrap_or_else(|| panic!("{context}: missing edit"));
                        let output = apply_edit(bytes, &edit).unwrap();
                        assert!(locate(&output).unwrap().is_some(), "{context}");
                        assert!(
                            output.starts_with(
                                format!("---{ending}{}", foreign.replace('\n', ending)).as_bytes()
                            ),
                            "{context}"
                        );
                        assert!(
                            output.ends_with(format!("---{ending}body{ending}").as_bytes()),
                            "{context}"
                        );
                        assert_eq!(
                            read_name(&output).unwrap().as_deref(),
                            if operation == "set_name" {
                                set.then_some("new")
                            } else {
                                Some("x")
                            },
                            "{context}"
                        );
                        assert_eq!(
                            read_id(&output).unwrap(),
                            if operation == "set_id" {
                                new_id
                            } else {
                                Some(old_id)
                            },
                            "{context}"
                        );
                        assert_eq!(
                            read_references(&output).unwrap(),
                            if operation == "set_references" || operation == "set_reference" {
                                refs.clone()
                            } else {
                                vec![reference("x")]
                            },
                            "{context}"
                        );
                    }
                }
            }
        }
    }
}
