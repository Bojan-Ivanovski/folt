use std::fs;

use folt_core::config::{Config, DEFAULT_LARGE_FILE_BYTES};
use folt_core::layout::CONTENT_FILE;
use folt_core::object::hash_blob;
use folt_core::placement::{Drift, Observation, PlacementOps};
use folt_core::placement_fs::PlacementFs;
use folt_core::placement_rules::{self, Behind};
use folt_core::refs::RefStore;
use folt_core::register::{self, RefusalReason, RegisterStatus, apply_register, plan_register};
use folt_core::registry::{PlacementPath, Registry};

use crate::common::TempHome;

#[test]
fn files_are_captured_without_history_and_repeated_registration_is_a_noop() {
    let home = TempHome::new();
    let paths: Vec<_> = ["note.md", "data.bin", "empty"]
        .map(|name| home.root().join(name))
        .into();
    let contents: [&[u8]; 3] = [b"---\ntitle: untouched\n---\nbody", &[0, 255, 7], b""];
    for (path, bytes) in paths.iter().zip(contents) {
        fs::write(path, bytes).unwrap();
    }
    let plan = plan_register(&home.home, &paths).unwrap();
    assert_eq!(plan.summary.new, 3);
    assert!(!home.path().join("registry.db").exists());
    assert_eq!(apply_register(&home.home, &plan, false).unwrap(), plan);
    let registry = Registry::open_existing(&home.home).unwrap();
    assert_eq!(registry.folios().unwrap().len(), 3);
    for (path, content) in paths.iter().zip(contents) {
        let row = registry
            .placement_at(&PlacementPath::resolve(path).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(row.base.as_ref().unwrap().hash, hash_blob(content));
        assert!(row.commit_id.is_none());
        let internal = registry.internal_placement(row.id).unwrap().unwrap();
        assert_eq!(internal.base.as_ref().unwrap().hash, hash_blob(content));
        assert_eq!(fs::read(internal.path.to_path_buf()).unwrap(), content);
        let format = registry.folio_row(row.id).unwrap().unwrap().format;
        let dir = folt_core::folio_dir::validate(
            home.path(),
            &registry.folio_row(row.id).unwrap().unwrap(),
        )
        .unwrap();
        let refs = RefStore::open(&dir, &format).unwrap();
        let facts =
            placement_rules::gather(&[row, internal], &PlacementFs::new(&home.home), |_| {
                Ok(&refs)
            })
            .unwrap();
        for status in placement_rules::judge_status(&facts) {
            assert_eq!(status.drift, Drift::InSync);
            assert_eq!(status.behind, Some(Behind::NoHistory));
        }
    }
    let again = plan_register(&home.home, &paths).unwrap();
    assert_eq!(again.summary.already_registered, 3);
    assert_eq!(apply_register(&home.home, &again, false).unwrap(), again);
    assert_eq!(registry.folios().unwrap().len(), 3);
}

#[test]
fn directories_create_real_markers_and_deduplicate_in_both_orders() {
    for marker_first in [false, true] {
        let home = TempHome::new();
        let dir = home.root().join("notes");
        fs::create_dir(&dir).unwrap();
        let marker = dir.join(register::DIRECTORY_MARKER);
        let paths = if marker_first {
            vec![marker.clone(), dir.clone(), dir.join(".")]
        } else {
            vec![dir.clone(), marker.clone(), dir.join(".")]
        };
        let plan = plan_register(&home.home, paths).unwrap();
        assert_eq!(plan.entries.len(), 1);
        assert!(plan.entries[0].creates_marker);
        assert_eq!(plan.entries[0].status, RegisterStatus::New);
        assert!(!marker.exists());
        assert_eq!(apply_register(&home.home, &plan, false).unwrap(), plan);
        assert_eq!(fs::read(&marker).unwrap(), b"");
        let registry = Registry::open_existing(&home.home).unwrap();
        assert_eq!(registry.folios().unwrap().len(), 1);
        let row = registry
            .placement_at(&PlacementPath::resolve(&dir).unwrap())
            .unwrap()
            .unwrap();
        assert!(matches!(
            PlacementFs::new(&home.home)
                .read(row.id, &row.to_placement().1)
                .unwrap(),
            Observation::File { .. }
        ));
        let again = plan_register(&home.home, [&dir]).unwrap();
        assert_eq!(again.summary.already_registered, 1);
        apply_register(&home.home, &again, false).unwrap();
        assert_eq!(registry.folios().unwrap().len(), 1);
        let child = dir.join("child.txt");
        fs::write(&child, b"child").unwrap();
        assert_eq!(
            plan_register(&home.home, [&child]).unwrap().entries[0].status,
            RegisterStatus::Refused(RefusalReason::InsideDirectoryFolio)
        );
    }
}

#[test]
fn apply_uses_an_appearing_marker_without_clobbering_it() {
    let home = TempHome::new();
    let dir = home.root().join("notes");
    fs::create_dir(&dir).unwrap();
    let plan = plan_register(&home.home, [&dir]).unwrap();
    let marker = dir.join(register::DIRECTORY_MARKER);
    fs::write(&marker, b"appeared").unwrap();
    let applied = apply_register(&home.home, &plan, false).unwrap();
    assert!(!applied.entries[0].creates_marker);
    let registry = Registry::open_existing(&home.home).unwrap();
    let row = registry
        .placement_at(&PlacementPath::resolve(&marker).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        fs::read(
            home.path()
                .join("folios")
                .join(row.id.to_string())
                .join(CONTENT_FILE)
        )
        .unwrap(),
        b"appeared"
    );
    assert_eq!(fs::read(marker).unwrap(), b"appeared");
}

#[test]
fn refusals_stop_the_entire_batch_without_creating_markers_or_registry() {
    let home = TempHome::new();
    let dir = home.root().join("notes");
    fs::create_dir(&dir).unwrap();
    let good = home.root().join("good");
    fs::write(&good, b"ok").unwrap();
    let bad = home.root().join("missing-parent/no-file");
    let plan = plan_register(&home.home, [&good, &dir, &bad]).unwrap();
    assert_eq!(plan.summary.refused, 1);
    assert_eq!(
        plan.entries[2].status,
        RegisterStatus::Refused(RefusalReason::Missing)
    );
    assert_eq!(apply_register(&home.home, &plan, true).unwrap(), plan);
    assert!(!dir.join(register::DIRECTORY_MARKER).exists());
    assert!(!home.path().join("registry.db").exists());
    let own = home.path().join("config.toml");
    assert_eq!(
        plan_register(&home.home, [&own]).unwrap().entries[0].status,
        RegisterStatus::Refused(RefusalReason::InsideHome)
    );
    let marker = dir.join(register::DIRECTORY_MARKER);
    fs::create_dir(&marker).unwrap();
    assert_eq!(
        plan_register(&home.home, [&dir]).unwrap().entries[0].status,
        RegisterStatus::Refused(RefusalReason::NotAFile)
    );
}

#[test]
fn configuration_confirmation_and_changed_files_are_rechecked() {
    let home = TempHome::new();
    assert_eq!(
        Config::load(&home.home).unwrap().large_file_bytes(),
        DEFAULT_LARGE_FILE_BYTES
    );
    fs::write(home.path().join("config.toml"), "large_file_bytes = 4\n").unwrap();
    let path = home.root().join("file");
    fs::write(&path, b"abc").unwrap();
    let plan = plan_register(&home.home, [&path, &home.root().join("./file")]).unwrap();
    assert_eq!(plan.entries.len(), 1);
    fs::write(&path, b"new!").unwrap();
    let refused = apply_register(&home.home, &plan, false).unwrap();
    assert_eq!(
        refused.entries[0].status,
        RegisterStatus::NeedsConfirmation { size: 4 }
    );
    assert!(!home.path().join("registry.db").exists());
    apply_register(&home.home, &plan, true).unwrap();
    let registry = Registry::open_existing(&home.home).unwrap();
    let row = registry
        .placement_at(&PlacementPath::resolve(&path).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(row.base.unwrap().hash, hash_blob(b"new!"));
}

#[test]
fn batch_total_requires_confirmation_even_when_individual_files_do_not() {
    let home = TempHome::new();
    fs::write(
        home.path().join("config.toml"),
        "large_file_bytes = 999999999\n",
    )
    .unwrap();
    let paths = [home.root().join("a"), home.root().join("b")];
    for path in &paths {
        fs::File::create(path)
            .unwrap()
            .set_len(register::BATCH_TOTAL_BYTES / 2 + 1)
            .unwrap();
    }
    let plan = plan_register(&home.home, &paths).unwrap();
    assert_eq!(plan.summary.new, 2);
    assert_eq!(plan.notes.len(), 1);
    assert_eq!(apply_register(&home.home, &plan, false).unwrap(), plan);
    assert!(!home.path().join("registry.db").exists());
    fs::File::options()
        .write(true)
        .open(&paths[1])
        .unwrap()
        .set_len(register::BATCH_TOTAL_BYTES / 2 - 1)
        .unwrap();
    assert!(plan_register(&home.home, &paths).unwrap().notes.is_empty());
}

#[cfg(unix)]
#[test]
fn symlinks_and_devices_are_refused() {
    use std::os::unix::fs::symlink;
    let home = TempHome::new();
    let path = home.root().join("file");
    fs::write(&path, b"content").unwrap();
    let link = home.root().join("link");
    symlink(&path, &link).unwrap();
    assert_eq!(
        plan_register(&home.home, [&link]).unwrap().entries[0].status,
        RegisterStatus::Refused(RefusalReason::Symlink)
    );
    let dir = home.root().join("notes");
    fs::create_dir(&dir).unwrap();
    symlink(&path, dir.join(register::DIRECTORY_MARKER)).unwrap();
    assert_eq!(
        plan_register(&home.home, [&dir]).unwrap().entries[0].status,
        RegisterStatus::Refused(RefusalReason::Symlink)
    );
    let socket = std::path::Path::new("/dev/null");
    assert_eq!(
        plan_register(&home.home, [&socket]).unwrap().entries[0].status,
        RegisterStatus::Refused(RefusalReason::NotAFile)
    );
}

#[test]
fn two_case_spellings_deduplicate_when_the_filesystem_is_case_insensitive() {
    let home = TempHome::new();
    let path = home.root().join("Note");
    let other = home.root().join("note");
    fs::write(&path, b"bytes").unwrap();
    if other.exists() {
        assert_eq!(
            plan_register(&home.home, [&path, &other])
                .unwrap()
                .entries
                .len(),
            1
        );
    }
}

// APFS/HFS+ reject non-UTF-8 filenames before registration can run.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn non_utf8_names_are_registered() {
    use std::os::unix::ffi::OsStringExt;
    assert_unix_name_registered(&std::ffi::OsString::from_vec(vec![0xff]));
}

#[cfg(unix)]
#[test]
fn unicode_names_are_registered_with_byte_keys() {
    assert_unix_name_registered(std::ffi::OsStr::new("日本語.txt"));
}

#[cfg(unix)]
fn assert_unix_name_registered(name: &std::ffi::OsStr) {
    use std::os::unix::ffi::OsStrExt;

    let home = TempHome::new();
    let path = home.root().join(name);
    fs::write(&path, b"bytes").unwrap();
    let plan = plan_register(&home.home, [&path]).unwrap();
    apply_register(&home.home, &plan, false).unwrap();
    let registry = Registry::open_existing(&home.home).unwrap();
    let row = registry
        .placement_at(&PlacementPath::resolve(&path).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        row.path.as_bytes(),
        fs::canonicalize(&path).unwrap().as_os_str().as_bytes()
    );
    assert_eq!(
        plan_register(&home.home, [&path])
            .unwrap()
            .summary
            .already_registered,
        1
    );
}

#[test]
fn apply_rechecks_disappearing_sources_before_creating_any_marker() {
    let home = TempHome::new();
    let dir = home.root().join("directory");
    fs::create_dir(&dir).unwrap();
    let path = home.root().join("file");
    fs::write(&path, b"bytes").unwrap();
    let plan = plan_register(&home.home, [&dir, &path]).unwrap();
    fs::remove_file(&path).unwrap();
    let refused = apply_register(&home.home, &plan, false).unwrap();
    assert_eq!(
        refused.entries[1].status,
        RegisterStatus::Refused(RefusalReason::Missing)
    );
    assert!(!dir.join(register::DIRECTORY_MARKER).exists());
    assert!(!home.path().join("registry.db").exists());
}

#[cfg(unix)]
#[test]
fn unreadable_file_refuses_the_whole_call_when_access_is_denied() {
    use std::os::unix::fs::PermissionsExt;
    let home = TempHome::new();
    let path = home.root().join("unreadable");
    fs::write(&path, b"bytes").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o0)).unwrap();
    // Privileged users can still read mode-000 files. Test the OS access check
    // when it applies, without changing process identity or permissions globally.
    if fs::File::open(&path).is_err() {
        let plan = plan_register(&home.home, [&path]).unwrap();
        assert_eq!(
            plan.entries[0].status,
            RegisterStatus::Refused(RefusalReason::Unreadable)
        );
        assert_eq!(apply_register(&home.home, &plan, true).unwrap(), plan);
        assert!(!home.path().join("registry.db").exists());
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
