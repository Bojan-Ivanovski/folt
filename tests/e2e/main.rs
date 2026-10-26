#[test]
fn binary_prints_version() {
    let output = assert_cmd::cargo::cargo_bin_cmd!("folt")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let output = String::from_utf8(output).unwrap();
    // The snapshot follows the workspace version without changing process state.
    assert_eq!(output, format!("folt {}\n", env!("CARGO_PKG_VERSION")));
    let normalized = output.replace(env!("CARGO_PKG_VERSION"), "<version>");
    insta::assert_snapshot!(normalized, @"folt <version>");
}

#[test]
fn tests_directory_contains_fixtures() {
    let tests = std::path::Path::new(env!("FOLT_TESTS_DIR"));
    assert!(tests.is_absolute());
    assert!(tests.join("fixtures/corpus/README.md").is_file());
    assert!(tests.join("fixtures/cases/README.md").is_file());
}
