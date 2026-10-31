use std::fs;

use folt_core::front_matter::{
    FrontMatterError, Reference, apply_edit, read_name, read_references, set_name, set_reference,
};

use crate::common::TempHome;

#[test]
fn edits_note_bytes_without_touching_foreign_yaml_or_body() {
    let home = TempHome::new();
    let path = home.root().join("note.md");
    let original =
        b"\xef\xbb\xbf---\r\nforeign: |\r\n  ---\r\nfolio-name :  old # note\r\n---\r\n# Body\n";
    fs::write(&path, original).unwrap();
    let bytes = fs::read(&path).unwrap();
    let edit = set_name(&bytes, Some("new"), false).unwrap().unwrap();
    fs::write(&path, apply_edit(&bytes, &edit).unwrap()).unwrap();
    let bytes = fs::read(&path).unwrap();
    assert_eq!(read_name(&bytes).unwrap().as_deref(), Some("new"));
    assert_eq!(
        bytes,
        String::from_utf8(original.to_vec())
            .unwrap()
            .replace("old", "new")
            .as_bytes()
    );
}

#[test]
fn creates_then_removes_a_reference_in_a_note() {
    let home = TempHome::new();
    let path = home.root().join("note.md");
    fs::write(&path, b"# Body\n").unwrap();
    let reference = Reference {
        alias: "logo".into(),
        name: Some("logo-image".into()),
        id: None,
        pin: None,
    };
    let bytes = fs::read(&path).unwrap();
    let edit = set_reference(&bytes, "logo", Some(&reference), true)
        .unwrap()
        .unwrap();
    fs::write(&path, apply_edit(&bytes, &edit).unwrap()).unwrap();
    let bytes = fs::read(&path).unwrap();
    assert_eq!(read_references(&bytes).unwrap(), vec![reference]);
    let edit = set_reference(&bytes, "logo", None, false).unwrap().unwrap();
    fs::write(&path, apply_edit(&bytes, &edit).unwrap()).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"---\n---\n# Body\n");
}

#[test]
fn unterminated_note_cannot_receive_a_second_block() {
    let home = TempHome::new();
    let path = home.root().join("note.md");
    let original = b"---\nfolio-name: old\n# Body\n";
    fs::write(&path, original).unwrap();
    assert_eq!(
        set_name(&fs::read(&path).unwrap(), Some("new"), true),
        Err(FrontMatterError::Unterminated)
    );
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn short_references_distinguish_explicit_ids_from_names() {
    use folt_core::folio_dir::FolioId;
    let id = "550e8400-e29b-41d4-a716-446655440000";
    for (target, expected_name, expected_id) in [
        (format!("id:{id}"), None, Some(FolioId::parse(id).unwrap())),
        (id.into(), Some(id), None),
        ("Logo-image".into(), Some("Logo-image"), None),
    ] {
        for value in [
            format!("\n  logo: {target}\n"),
            format!(" {{logo: {target}}}\n"),
        ] {
            let bytes = format!("---\nfolio-references:{value}---\n");
            let refs = read_references(bytes.as_bytes()).unwrap();
            assert_eq!(refs[0].name.as_deref(), expected_name);
            assert_eq!(refs[0].id, expected_id);
        }
    }
    for target in [
        "id:bad",
        "bad/name",
        ".bad",
        "bad-",
        "'bad name'",
        "'bad#name'",
        "other:value",
        &"a".repeat(64),
    ] {
        let bytes = format!("---\nfolio-references:\n  logo: {target}\n---\n");
        assert_eq!(
            read_references(bytes.as_bytes()),
            Err(FrontMatterError::InvalidValue),
            "{target}"
        );
    }
    // Long forms retain their existing name and id rules.
    let bytes = format!(
        "---\nfolio-references: {{logo: {{name: old/name, id: {id}, pin: abc123}}}}\n---\n"
    );
    let refs = read_references(bytes.as_bytes()).unwrap();
    assert_eq!(refs[0].name.as_deref(), Some("old/name"));
    assert_eq!(refs[0].id, Some(FolioId::parse(id).unwrap()));
    assert_eq!(refs[0].pin.as_deref(), Some("abc123"));
}
