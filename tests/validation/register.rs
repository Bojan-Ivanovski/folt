use std::fs;
use std::path::{Path, PathBuf};

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

fn resolved_path(path: impl AsRef<Path>) -> PathBuf {
    fs::canonicalize(path).unwrap()
}

#[test]
fn files_are_captured_without_history_and_repeated_registration_is_a_noop() {
    let home = TempHome::new();
    let paths: Vec<_> = ["note.md", "data.bin", "empty"]
        .map(|name| home.root().join(name))
        .into();
    let contents: [&[u8]; 3] = [
        b"---\ntitle: untouched\nfolio-name: note\n---\nbody",
        &[0, 255, 7],
        b"",
    ];
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
        let applied = apply_register(&home.home, &plan, false).unwrap();
        assert_eq!(applied.summary.refused, 0);
        assert_eq!(
            fs::read(&marker).unwrap(),
            b"---\nfolio-name: \"notes\"\n---\n"
        );
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
fn distinct_case_variant_markers_register_in_one_unchanged_batch() {
    let home = TempHome::new();
    let dir = home.root().join("notes");
    fs::create_dir(&dir).unwrap();
    let paths = [dir.join("__folio__.md"), dir.join("__Folio__.md")];
    let contents = [
        b"---\nfolio-name: first\n---\n".as_slice(),
        b"---\nfolio-name: second\n---\n".as_slice(),
    ];
    for (path, bytes) in paths.iter().zip(contents) {
        fs::write(path, bytes).unwrap();
    }
    // Case-insensitive filesystems cannot hold both distinct marker files.
    if fs::read(&paths[0]).unwrap() != contents[0] || fs::read(&paths[1]).unwrap() != contents[1] {
        return;
    }

    let plan = plan_register(&home.home, &paths).unwrap();
    assert_eq!(plan.entries.len(), 2);
    assert_eq!(plan.summary.new, 2);
    assert_ne!(plan.entries[0].path, plan.entries[1].path);
    let applied = apply_register(&home.home, &plan, false).unwrap();
    assert_eq!(applied, plan);
    let registry = Registry::open_existing(&home.home).unwrap();
    assert_eq!(registry.folios().unwrap().len(), 2);
    for (entry, bytes) in plan.entries.iter().zip(contents) {
        let row = registry
            .placement_at(&PlacementPath::resolve(&entry.path).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(row.base.unwrap().hash, hash_blob(bytes));
        assert_eq!(fs::read(&entry.path).unwrap(), bytes);
    }
}

#[test]
fn directory_names_use_the_resolved_directory_for_equivalent_spellings() {
    for spelling in ["notes", "notes/child/..", "notes/", "notes/."] {
        let home = TempHome::new();
        let root = resolved_path(home.root());
        let dir = root.join("notes");
        fs::create_dir_all(dir.join("child")).unwrap();
        let request = root.join(spelling);
        assert!(request.is_absolute());

        let plan = plan_register(&home.home, [&request]).unwrap();
        assert_eq!(plan.summary.new, 1, "{spelling}: {plan:?}");
        assert_eq!(
            plan.entries[0].name.as_ref().unwrap().as_str(),
            "notes",
            "{spelling}"
        );
        let applied = apply_register(&home.home, &plan, false).unwrap();
        assert_eq!(applied.summary.new, 1, "{spelling}: {applied:?}");
        assert_eq!(
            fs::read(dir.join(register::DIRECTORY_MARKER)).unwrap(),
            b"---\nfolio-name: \"notes\"\n---\n",
            "{spelling}"
        );
        let registry = Registry::open_existing(&home.home).unwrap();
        let row = registry
            .placement_at(&PlacementPath::resolve(&dir).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(
            registry.folio_name(row.id).unwrap().unwrap().as_str(),
            "notes",
            "{spelling}"
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
    fs::write(&marker, b"---\nfolio-name: notes\n---\nappeared").unwrap();
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
        b"---\nfolio-name: notes\n---\nappeared"
    );
    assert_eq!(
        fs::read(marker).unwrap(),
        b"---\nfolio-name: notes\n---\nappeared"
    );
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
    let changed = apply_register(&home.home, &plan, false).unwrap();
    assert_eq!(
        changed.entries[0].status,
        RegisterStatus::Refused(RefusalReason::FileChanged)
    );
    let updated = plan_register(&home.home, [&path]).unwrap();
    let refused = apply_register(&home.home, &updated, false).unwrap();
    assert_eq!(
        refused.entries[0].status,
        RegisterStatus::NeedsConfirmation { size: 4 }
    );
    assert!(!home.path().join("registry.db").exists());
    apply_register(&home.home, &updated, true).unwrap();
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
fn repointed_parent_blocks_file_registration() {
    use std::os::unix::fs::symlink;

    for links in [false, true] {
        let home = TempHome::new();
        let a = home.root().join("a");
        let b = home.root().join("b");
        fs::create_dir(&a).unwrap();
        fs::create_dir(&b).unwrap();
        fs::write(a.join("note.md"), b"original a").unwrap();
        fs::write(b.join("note.md"), b"original b").unwrap();
        let current = home.root().join("current");
        symlink(&a, &current).unwrap();
        let request = current.join("note.md");
        let plan = plan_register(&home.home, [&request]).unwrap();
        assert_eq!(plan.summary.new, 1);
        fs::remove_file(&current).unwrap();
        symlink(&b, &current).unwrap();
        let destination = plan_register(&home.home, [&request]).unwrap().entries[0]
            .path
            .clone();
        assert_ne!(destination, plan.entries[0].path);

        let blocked = if links {
            let outcome = register::apply_register_links(&home.home, &plan, false).unwrap();
            assert!(outcome.paths.is_empty());
            outcome.registration
        } else {
            apply_register(&home.home, &plan, false).unwrap()
        };
        assert_eq!(blocked.summary.refused, 1);
        assert_eq!(blocked.entries[0].path, destination);
        let reason = RefusalReason::DestinationChanged {
            path: destination.clone(),
        };
        assert_eq!(
            blocked.entries[0].status,
            RegisterStatus::Refused(reason.clone())
        );
        assert_eq!(
            reason.to_string(),
            format!(
                "{}: destination changed since the plan",
                destination.display()
            )
        );
        assert_eq!(fs::read(&plan.entries[0].path).unwrap(), b"original a");
        assert_eq!(fs::read(&destination).unwrap(), b"original b");
        assert!(!home.path().join("registry.db").exists());
        assert!(
            Registry::open_existing(&home.home)
                .unwrap()
                .folios()
                .unwrap()
                .is_empty()
        );
    }
}

#[cfg(unix)]
#[test]
fn repointed_parent_blocks_directory_registration() {
    use std::os::unix::fs::symlink;

    for existing_marker in [false, true] {
        let home = TempHome::new();
        let a = home.root().join("a");
        let b = home.root().join("b");
        fs::create_dir_all(a.join("dir")).unwrap();
        fs::create_dir_all(b.join("dir")).unwrap();
        if existing_marker {
            fs::write(
                a.join("dir").join(register::DIRECTORY_MARKER),
                b"original a",
            )
            .unwrap();
            fs::write(
                b.join("dir").join(register::DIRECTORY_MARKER),
                b"original b",
            )
            .unwrap();
        }
        let current = home.root().join("current");
        symlink(&a, &current).unwrap();
        let request = current.join("dir");
        let plan = plan_register(&home.home, [&request]).unwrap();
        assert_eq!(plan.summary.new, 1);
        fs::remove_file(&current).unwrap();
        symlink(&b, &current).unwrap();
        let destination = plan_register(&home.home, [&request]).unwrap().entries[0]
            .path
            .clone();
        assert_ne!(destination, plan.entries[0].path);

        let blocked = apply_register(&home.home, &plan, false).unwrap();
        assert_eq!(blocked.summary.refused, 1);
        assert_eq!(blocked.entries[0].path, destination);
        let reason = RefusalReason::DestinationChanged {
            path: destination.clone(),
        };
        assert_eq!(
            blocked.entries[0].status,
            RegisterStatus::Refused(reason.clone())
        );
        assert_eq!(
            reason.to_string(),
            format!(
                "{}: destination changed since the plan",
                destination.display()
            )
        );
        if existing_marker {
            assert_eq!(fs::read(&plan.entries[0].path).unwrap(), b"original a");
            assert_eq!(fs::read(&destination).unwrap(), b"original b");
        } else {
            assert!(!plan.entries[0].path.exists());
            assert!(!destination.exists());
        }
        assert!(!home.path().join("registry.db").exists());
        assert!(
            Registry::open_existing(&home.home)
                .unwrap()
                .folios()
                .unwrap()
                .is_empty()
        );
    }
}

#[cfg(unix)]
#[test]
fn unchanged_parent_registers_collapsed_file_and_directory_requests() {
    use std::os::unix::fs::symlink;

    for directory in [false, true] {
        let home = TempHome::new();
        let a = home.root().join("a");
        fs::create_dir(&a).unwrap();
        let target = if directory {
            a.join("dir")
        } else {
            a.join("note.md")
        };
        if directory {
            fs::create_dir(&target).unwrap();
        } else {
            fs::write(&target, b"original").unwrap();
        }
        let current = home.root().join("current");
        symlink(&a, &current).unwrap();
        let request = current.join(target.file_name().unwrap());
        let plan = plan_register(&home.home, [&request, &target, &request]).unwrap();
        assert_eq!(plan.entries.len(), 1);
        let applied = apply_register(&home.home, &plan, false).unwrap();
        assert_eq!(applied.summary.refused, 0);
        assert_eq!(applied.entries[0].path, plan.entries[0].path);
        let registry = Registry::open_existing(&home.home).unwrap();
        assert_eq!(registry.folios().unwrap().len(), 1);
        let again = plan_register(&home.home, [&request, &target]).unwrap();
        assert_eq!(again.summary.already_registered, 1);
        assert_eq!(apply_register(&home.home, &again, false).unwrap(), again);
    }
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
    let plan = plan_register(
        &home.home,
        [register::RegisterRequest::new(
            &path,
            folt_core::folio_name::FolioName::parse("explicit").unwrap(),
        )],
    )
    .unwrap();
    apply_register(&home.home, &plan, false).unwrap();
    let registry = Registry::open_existing(&home.home).unwrap();
    let row = registry
        .placement_at(&PlacementPath::resolve(&path).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        row.path.as_bytes(),
        resolved_path(&path).as_os_str().as_bytes()
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

#[test]
fn link_dry_run_creates_no_marker_folio_registry_or_probe() {
    let home = TempHome::new();
    let directory = home.root().join("notes");
    fs::create_dir(&directory).unwrap();
    let file = home.root().join("file");
    fs::write(&file, b"content").unwrap();
    let names = || {
        let mut names: Vec<_> = fs::read_dir(home.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let before = names();
    let plan = plan_register(&home.home, [&directory, &file]).unwrap();
    assert_eq!(plan.summary.new, 2);
    assert!(plan.entries[0].creates_marker);
    assert_eq!(names(), before);
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
    assert_eq!(fs::read_dir(home.root()).unwrap().count(), 3);
    assert_eq!(fs::read(&file).unwrap(), b"content");
}

#[cfg_attr(windows, ignore = "real symlinks require privileges")]
#[test]
fn link_registration_without_a_tip_and_repeated_requests_use_the_same_folio() {
    assert_link_registration(&TempHome::new());
}

#[cfg(unix)]
#[test]
fn link_registration_through_a_symlinked_home_uses_the_home_spelling() {
    let mut home = TempHome::new();
    let alias = home.root().join("home-alias");
    std::os::unix::fs::symlink(home.path(), &alias).unwrap();
    home.home = folt_core::home::Home::open_or_create(alias).unwrap();
    assert_link_registration(&home);
}

fn assert_link_registration(home: &TempHome) {
    use folt_core::placement::PlacementKind;
    use folt_core::register::{RegisterLinkResult, apply_register_links};
    let path = home.root().join("file");
    fs::write(&path, b"content").unwrap();
    let expected_path = resolved_path(&path);
    let plan = plan_register(&home.home, [&path]).unwrap();
    let result = apply_register_links(&home.home, &plan, false).unwrap();
    assert_eq!(
        result.paths,
        vec![(expected_path, RegisterLinkResult::Linked)]
    );
    let registry = Registry::open_existing(&home.home).unwrap();
    let row = registry
        .placement_at(&PlacementPath::resolve(&path).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(row.kind, PlacementKind::Link);
    assert_eq!(row.base, None);
    assert_eq!(row.commit_id, None);
    let internal = registry.internal_placement(row.id).unwrap().unwrap();
    assert_eq!(internal.commit_id, None);
    let target = home
        .path()
        .join("folios")
        .join(row.id.to_string())
        .join(CONTENT_FILE);
    // Links use the home's spelling; registry keys resolve parent symlinks.
    assert_eq!(fs::read_link(&path).unwrap(), target);
    assert_eq!(internal.path.to_path_buf(), resolved_path(&target));
    fs::write(&path, b"shared edit").unwrap();
    assert_eq!(
        fs::read(internal.path.to_path_buf()).unwrap(),
        b"shared edit"
    );
    let result = apply_register_links(&home.home, &plan, false).unwrap();
    assert_eq!(result.paths[0].1, RegisterLinkResult::Linked);
    assert_eq!(registry.folios().unwrap(), vec![row.id]);
}

#[cfg_attr(windows, ignore = "real symlinks require privileges")]
#[test]
fn link_request_retries_an_already_registered_copy() {
    use folt_core::register::{RegisterLinkResult, apply_register_links};
    let home = TempHome::new();
    let path = home.root().join("file");
    fs::write(&path, b"content").unwrap();
    let plan = plan_register(&home.home, [&path]).unwrap();
    apply_register(&home.home, &plan, false).unwrap();
    let registry = Registry::open_existing(&home.home).unwrap();
    let ids = registry.folios().unwrap();
    let result = apply_register_links(&home.home, &plan, false).unwrap();
    assert_eq!(result.registration.summary.already_registered, 1);
    assert_eq!(result.paths[0].1, RegisterLinkResult::Linked);
    assert_eq!(registry.folios().unwrap(), ids);
    assert!(fs::symlink_metadata(path).unwrap().is_symlink());
}

#[cfg(unix)]
#[test]
fn hard_linked_source_remains_a_registered_copy_while_other_paths_convert() {
    assert_hard_linked_source_remains_a_registered_copy();
}

#[cfg(windows)]
#[test]
fn hard_linked_source_remains_a_registered_copy_while_other_paths_convert() {
    assert_hard_linked_source_remains_a_registered_copy();
}

fn assert_hard_linked_source_remains_a_registered_copy() {
    use folt_core::placement::PlacementKind;
    use folt_core::register::{RegisterLinkResult, apply_register_links};
    let home = TempHome::new();
    let path = home.root().join("file");
    let alias = home.root().join("alias");
    let other = home.root().join("other");
    fs::write(&path, b"content").unwrap();
    fs::hard_link(&path, &alias).unwrap();
    fs::write(&other, b"other").unwrap();
    let plan = plan_register(&home.home, [&path, &other]).unwrap();
    let result = apply_register_links(&home.home, &plan, false).unwrap();
    assert!(
        matches!(&result.paths[0].1, RegisterLinkResult::LeftAsCopy { reason }
        if reason.contains("other hard links"))
    );
    assert_eq!(result.paths[1].1, RegisterLinkResult::Linked);
    assert!(!fs::symlink_metadata(&path).unwrap().is_symlink());
    let registry = Registry::open_existing(&home.home).unwrap();
    let row = registry
        .placement_at(&PlacementPath::resolve(&path).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(row.kind, PlacementKind::Copy);
    fs::write(&alias, b"shared inode").unwrap();
    assert_eq!(fs::read(path).unwrap(), b"shared inode");
    assert_eq!(registry.folios().unwrap().len(), 2);
}

fn name(value: &str) -> folt_core::folio_name::FolioName {
    folt_core::folio_name::FolioName::parse(value).unwrap()
}

fn named(path: impl AsRef<Path>, value: &str) -> register::RegisterRequest {
    register::RegisterRequest::new(path, name(value))
}

#[test]
fn name_sources_and_exact_edits_are_planned_without_writing() {
    use register::{NameSource, NameWrite};
    let home = TempHome::new();
    let paths =
        ["Front.MD", "Option.Markdown", "File Name.md", "binary.txt"].map(|p| home.root().join(p));
    let contents: [&[u8]; 4] = [
        b"---\nfolio-name: FromFile\n---\nbody",
        b"\xef\xbb\xbf---\r\ntitle: preserved\r\n---\r\nbody",
        b"body",
        b"---\nfolio-name: invalid name\nfolio-name: duplicate\n---\n\0",
    ];
    for (path, bytes) in paths.iter().zip(contents) {
        fs::write(path, bytes).unwrap();
    }
    let plan = plan_register(
        &home.home,
        [
            paths[0].clone().into(),
            named(&paths[1], "Chosen"),
            paths[2].clone().into(),
            paths[3].clone().into(),
        ],
    )
    .unwrap();
    assert_eq!(plan.summary.refused, 0);
    assert_eq!(plan.summary.files_to_write, 2);
    for (i, expected, source) in [
        (0, "FromFile", NameSource::FrontMatter),
        (1, "Chosen", NameSource::Option),
        (2, "File-Name", NameSource::FileName { suffix: 0 }),
        (3, "binary", NameSource::FileName { suffix: 0 }),
    ] {
        assert_eq!(plan.entries[i].name, Some(name(expected)));
        assert_eq!(plan.entries[i].name_source, Some(source));
    }
    assert_eq!(plan.entries[0].name_write, NameWrite::None);
    assert_eq!(plan.entries[3].name_write, NameWrite::None);
    for i in [1, 2] {
        let edit = match &plan.entries[i].name_write {
            NameWrite::Insert(edit) if i == 1 => edit,
            NameWrite::InsertBlock(edit) if i == 2 => edit,
            other => panic!("wrong edit: {other:?}"),
        };
        assert_eq!(
            Some(edit.clone()),
            folt_core::front_matter::set_name(
                contents[i],
                Some(plan.entries[i].name.as_ref().unwrap().as_str()),
                true
            )
            .unwrap()
        );
    }
    let text = plan.to_string();
    assert!(text.contains(&format!(
        "will write folio-name: \"Chosen\" into {}",
        plan.entries[1].path.display()
    )));
    assert!(text.contains("2 files to write"));
    assert!(text.contains("name: FromFile"));
    for (path, bytes) in paths.iter().zip(contents) {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn explicit_names_then_front_matter_are_reserved_before_stems() {
    use register::NameSource;
    let home = TempHome::new();
    let paths =
        ["same.txt", "same.bin", "front.md", "option.txt", "other.md"].map(|p| home.root().join(p));
    for path in &paths {
        fs::write(path, b"body").unwrap();
    }
    fs::write(&paths[2], b"---\nfolio-name: same\n---\n").unwrap();
    fs::write(&paths[4], b"---\nfolio-name: same_2\n---\n").unwrap();
    let plan = plan_register(
        &home.home,
        [
            paths[0].clone().into(),
            paths[1].clone().into(),
            paths[2].clone().into(),
            named(&paths[3], "same_1"),
            paths[4].clone().into(),
        ],
    )
    .unwrap();
    assert_eq!(plan.summary.refused, 0);
    for (entry, expected) in plan
        .entries
        .iter()
        .zip(["same_3", "same_4", "same", "same_1", "same_2"])
    {
        assert_eq!(entry.name, Some(name(expected)));
    }
    assert_eq!(
        plan.entries[0].name_source,
        Some(NameSource::FileName { suffix: 3 })
    );
    assert_eq!(
        plan.entries[1].name_source,
        Some(NameSource::FileName { suffix: 4 })
    );
    apply_register(&home.home, &plan, false).unwrap();
}

#[test]
fn registry_collisions_suffix_only_stems_and_explicit_names_block_the_batch() {
    use register::NameHolder;
    let home = TempHome::new();
    let held = home.root().join("held.txt");
    fs::write(&held, b"held").unwrap();
    let plan = plan_register(&home.home, [named(&held, "same")]).unwrap();
    apply_register(&home.home, &plan, false).unwrap();
    let registry = Registry::open_existing(&home.home).unwrap();
    let id = registry.find_by_name(&name("same")).unwrap().unwrap();
    let paths = ["same.txt", "same.bin", "front.md", "good.txt"].map(|p| home.root().join(p));
    for path in &paths {
        fs::write(path, b"body").unwrap();
    }
    let suffixes = plan_register(&home.home, [&paths[0], &paths[1]]).unwrap();
    assert_eq!(suffixes.entries[0].name, Some(name("same_1")));
    assert_eq!(suffixes.entries[1].name, Some(name("same_2")));
    for from_front in [false, true] {
        fs::write(
            &paths[2],
            if from_front {
                b"---\nfolio-name: same\n---\n".as_slice()
            } else {
                b"body"
            },
        )
        .unwrap();
        let request = if from_front {
            paths[2].clone().into()
        } else {
            named(&paths[2], "same")
        };
        let plan = plan_register(&home.home, [request, paths[3].clone().into()]).unwrap();
        assert_eq!(
            plan.entries[0].status,
            RegisterStatus::Refused(RefusalReason::NameTaken {
                name: name("same"),
                holder: NameHolder::Folio(id),
            })
        );
        assert_eq!(plan.entries[0].name, None);
        assert_eq!(apply_register(&home.home, &plan, false).unwrap(), plan);
        assert_eq!(registry.folios().unwrap(), vec![id]);
    }
}

#[test]
fn explicit_collisions_between_paths_never_get_a_suffix() {
    use register::NameHolder;
    let home = TempHome::new();
    let paths = ["one.txt", "two.txt", "front.md", "front2.md"].map(|p| home.root().join(p));
    for path in &paths {
        fs::write(path, b"---\nfolio-name: Taken\n---\n").unwrap();
    }
    for requests in [
        vec![named(&paths[0], "Taken"), named(&paths[1], "Taken")],
        vec![paths[2].clone().into(), named(&paths[0], "Taken")],
        vec![paths[2].clone().into(), paths[3].clone().into()],
    ] {
        let plan = plan_register(&home.home, requests).unwrap();
        assert_eq!(plan.summary.refused, 1);
        let refused = plan
            .entries
            .iter()
            .find(|e| matches!(e.status, RegisterStatus::Refused(_)))
            .unwrap();
        assert!(
            matches!(&refused.status, RegisterStatus::Refused(RefusalReason::NameTaken {
            name: taken, holder: NameHolder::Path(_),
        }) if taken == &name("Taken"))
        );
        assert_eq!(apply_register(&home.home, &plan, false).unwrap(), plan);
        assert!(!home.path().join("registry.db").exists());
    }
}

#[test]
fn repeated_paths_merge_options_but_refuse_conflicting_names_even_on_replay() {
    let home = TempHome::new();
    let path = home.root().join("file.txt");
    fs::write(&path, b"body").unwrap();
    for requests in [
        vec![path.clone().into(), named(&path, "Chosen")],
        vec![named(&path, "Chosen"), path.clone().into()],
        vec![
            named(&path, "Chosen"),
            named(home.root().join("./file.txt"), "Chosen"),
        ],
    ] {
        let plan = plan_register(&home.home, requests).unwrap();
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].name, Some(name("Chosen")));
    }
    let plan = plan_register(
        &home.home,
        [
            named(&path, "First"),
            named(&path, "Second"),
            path.clone().into(),
        ],
    )
    .unwrap();
    assert_eq!(
        plan.entries[0].status,
        RegisterStatus::Refused(RefusalReason::ConflictingNames)
    );
    assert_eq!(apply_register(&home.home, &plan, false).unwrap(), plan);
    let valid = plan_register(&home.home, [named(&path, "Chosen")]).unwrap();
    apply_register(&home.home, &valid, false).unwrap();
    let registered = plan_register(&home.home, [named(&path, "Chosen")]).unwrap();
    assert_eq!(
        registered.entries[0].status,
        RegisterStatus::Refused(RefusalReason::NameOnRegisteredPath)
    );
    assert!(registered.to_string().contains("rename (#155)"));
    assert_eq!(
        apply_register(&home.home, &valid, false).unwrap(),
        registered
    );
}

#[test]
fn front_matter_names_refuse_options_even_when_equal_and_invalid_grammar() {
    let home = TempHome::new();
    let path = home.root().join("file.md");
    fs::write(&path, b"---\nfolio-name: Existing\n---\n").unwrap();
    for option in ["Existing", "Different"] {
        let plan = plan_register(&home.home, [named(&path, option)]).unwrap();
        assert_eq!(
            plan.entries[0].status,
            RegisterStatus::Refused(RefusalReason::NameOptionWithFrontMatter)
        );
        assert_eq!(apply_register(&home.home, &plan, false).unwrap(), plan);
    }
    for invalid in ["bad name", ".bad", "bad/", &"g".repeat(65), &"a".repeat(64)] {
        fs::write(&path, format!("---\nfolio-name: \"{invalid}\"\n---\n")).unwrap();
        let plan = plan_register(&home.home, [&path]).unwrap();
        assert!(matches!(
            plan.entries[0].status,
            RegisterStatus::Refused(RefusalReason::InvalidFrontMatterName(_))
        ));
        assert!(
            plan.to_string()
                .contains(&plan.entries[0].path.display().to_string())
        );
    }
}

#[test]
fn editor_refusals_are_reported_with_the_path() {
    use folt_core::front_matter::FrontMatterError as E;
    let home = TempHome::new();
    let path = home.root().join("refused.md");
    for (bytes, reason) in [
        (
            b"---\nfolio-name: a\nfolio-name: b\n---\n".to_vec(),
            E::Duplicate,
        ),
        (b"---\n\"folio-name\": a\n---\n".to_vec(), E::Ambiguous),
        (b"---\nnever closes".to_vec(), E::Unterminated),
        (
            format!("---\n{}\n---\n", "x".repeat(65536)).into_bytes(),
            E::Unterminated,
        ),
        (b"body\0".to_vec(), E::Binary),
        (b"---\nfolio-name: [a]\n---\n".to_vec(), E::Unsupported),
        (b"---\nfolio-name:\n---\n".to_vec(), E::InvalidValue),
    ] {
        fs::write(&path, &bytes).unwrap();
        let plan = plan_register(&home.home, [&path]).unwrap();
        assert_eq!(
            plan.entries[0].status,
            RegisterStatus::Refused(RefusalReason::FrontMatter(reason))
        );
        assert!(
            plan.to_string()
                .contains(&plan.entries[0].path.display().to_string())
        );
        assert_eq!(apply_register(&home.home, &plan, false).unwrap(), plan);
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn stem_slugging_preserves_case_and_shortens_suffixes_to_64_bytes() {
    let home = TempHome::new();
    for (filename, expected) in [
        ("..Hello  World!!.txt".to_owned(), "Hello-World".to_owned()),
        ("A-z_0.thing.txt".to_owned(), "A-z_0.thing".to_owned()),
        (format!("{}.txt", "g".repeat(80)), "g".repeat(64)),
    ] {
        let path = home.root().join(filename);
        fs::write(&path, b"body").unwrap();
        let plan = plan_register(&home.home, [&path]).unwrap();
        assert_eq!(plan.entries[0].name, Some(name(&expected)));
    }
    for filename in ["日本語.txt".to_owned(), format!("{}.txt", "a".repeat(64))] {
        let path = home.root().join(filename);
        fs::write(&path, b"body").unwrap();
        let plan = plan_register(&home.home, [&path]).unwrap();
        assert!(matches!(
            plan.entries[0].status,
            RegisterStatus::Refused(RefusalReason::NoUsableStem(_))
        ));
        assert!(plan.to_string().contains("--name"));
    }
    let paths =
        ["txt", "bin", "dat"].map(|ext| home.root().join(format!("{}.{ext}", "g".repeat(64))));
    for path in &paths {
        fs::write(path, b"body").unwrap();
    }
    let first = plan_register(&home.home, [&paths[0]]).unwrap();
    apply_register(&home.home, &first, false).unwrap();
    let rest = plan_register(&home.home, [&paths[1], &paths[2]]).unwrap();
    assert_eq!(
        rest.entries[0].name,
        Some(name(&format!("{}_1", "g".repeat(62))))
    );
    assert_eq!(
        rest.entries[1].name,
        Some(name(&format!("{}_2", "g".repeat(62))))
    );
}

#[test]
fn directory_names_use_marker_then_option_then_directory_name() {
    use register::{NameSource, NameWrite};
    for (marker, option, expected, source) in [
        (
            Some("---\nfolio-name: FromMarker\n---\n"),
            None,
            "FromMarker",
            NameSource::FrontMatter,
        ),
        (Some("body"), Some("Option"), "Option", NameSource::Option),
        (None, Some("Option"), "Option", NameSource::Option),
        (None, None, "My-Notes", NameSource::FileName { suffix: 0 }),
        (
            Some("body"),
            None,
            "My-Notes",
            NameSource::FileName { suffix: 0 },
        ),
    ] {
        let home = TempHome::new();
        let dir = home.root().join("My Notes");
        fs::create_dir(&dir).unwrap();
        let path = dir.join(register::DIRECTORY_MARKER);
        if let Some(bytes) = marker {
            fs::write(&path, bytes).unwrap();
        }
        let request = option.map_or_else(|| dir.clone().into(), |n| named(&dir, n));
        let plan = plan_register(&home.home, [request]).unwrap();
        assert_eq!(plan.entries[0].name, Some(name(expected)));
        assert_eq!(plan.entries[0].name_source, Some(source.clone()));
        assert_eq!(plan.entries[0].creates_marker, marker.is_none());
        assert_eq!(
            plan.entries[0].name_write == NameWrite::None,
            source == NameSource::FrontMatter
        );
        assert_eq!(path.exists(), marker.is_some());
    }
}

#[test]
fn apply_stores_all_names_in_the_registry_without_changing_source_bytes() {
    let home = TempHome::new();
    let paths = ["front.md", "explicit.txt", "Stem.txt"].map(|p| home.root().join(p));
    let bytes: [&[u8]; 3] = [b"---\nfolio-name: Front\n---\nbody", b"\0binary", b"body"];
    for (path, content) in paths.iter().zip(bytes) {
        fs::write(path, content).unwrap();
    }
    let plan = plan_register(
        &home.home,
        [
            paths[0].clone().into(),
            named(&paths[1], "Explicit"),
            paths[2].clone().into(),
        ],
    )
    .unwrap();
    assert_eq!(plan.summary.files_to_write, 0);
    assert_eq!(apply_register(&home.home, &plan, false).unwrap(), plan);
    let registry = Registry::open_existing(&home.home).unwrap();
    for ((path, content), expected) in paths.iter().zip(bytes).zip(["Front", "Explicit", "Stem"]) {
        let id = registry
            .placement_at(&PlacementPath::resolve(path).unwrap())
            .unwrap()
            .unwrap()
            .id;
        assert_eq!(registry.folio_name(id).unwrap(), Some(name(expected)));
        assert_eq!(registry.find_by_name(&name(expected)).unwrap(), Some(id));
        assert_eq!(fs::read(path).unwrap(), content);
    }
}

#[test]
fn markdown_prefix_is_bounded_and_changed_markdown_blocks_apply() {
    let home = TempHome::new();
    let path = home.root().join("file.md");
    let mut bytes = b"---\nfolio-name: File\n---\n".to_vec();
    bytes.resize(65544, b'x');
    bytes.push(0); // NUL past the prefix must not be examined.
    fs::write(&path, bytes).unwrap();
    let plan = plan_register(&home.home, [&path]).unwrap();
    assert_eq!(plan.summary.refused, 0);
    fs::write(&path, b"---\nfolio-name: Changed\n---\nchanged content").unwrap();
    let changed = apply_register(&home.home, &plan, false).unwrap();
    assert_eq!(
        changed.entries[0].status,
        RegisterStatus::Refused(RefusalReason::FileChanged)
    );
    assert!(!home.path().join("registry.db").exists());
}

#[cfg(any(unix, windows))]
#[test]
fn markdown_needing_a_write_refuses_readonly_and_multiple_hard_links() {
    let home = TempHome::new();
    let path = home.root().join("file.md");
    fs::write(&path, b"body").unwrap();
    let original = fs::metadata(&path).unwrap().permissions();
    let mut readonly = original.clone();
    readonly.set_readonly(true);
    fs::set_permissions(&path, readonly).unwrap();
    let plan = plan_register(&home.home, [&path]).unwrap();
    assert!(
        matches!(&plan.entries[0].status, RegisterStatus::Refused(RefusalReason::NameNotWritable { reason }) if reason.contains("read-only"))
    );
    fs::set_permissions(&path, original).unwrap();
    let alias = home.root().join("alias");
    fs::hard_link(&path, &alias).unwrap();
    let plan = plan_register(&home.home, [&path]).unwrap();
    assert!(
        matches!(&plan.entries[0].status, RegisterStatus::Refused(RefusalReason::NameNotWritable { reason }) if reason.contains("hard links"))
    );
    // Existing names need no write, so hard links are allowed.
    fs::write(&path, b"---\nfolio-name: Existing\n---\n").unwrap();
    assert_eq!(
        plan_register(&home.home, [&path]).unwrap().summary.refused,
        0
    );
}

#[cfg_attr(windows, ignore = "real symlinks require privileges")]
#[test]
fn link_apply_replays_explicit_names_and_stores_them() {
    let home = TempHome::new();
    let path = home.root().join("file.txt");
    fs::write(&path, b"body").unwrap();
    let plan = plan_register(&home.home, [named(&path, "Chosen")]).unwrap();
    let linked = register::apply_register_links(&home.home, &plan, false).unwrap();
    assert_eq!(linked.paths[0].1, register::RegisterLinkResult::Linked);
    let registry = Registry::open_existing(&home.home).unwrap();
    assert!(registry.find_by_name(&name("Chosen")).unwrap().is_some());
    assert_eq!(registry.find_by_name(&name("file")).unwrap(), None);
    let refused = register::apply_register_links(&home.home, &plan, false).unwrap();
    assert!(refused.paths.is_empty());
    assert_eq!(
        refused.registration.entries[0].status,
        RegisterStatus::Refused(RefusalReason::NameOnRegisteredPath)
    );
}

#[test]
fn directory_and_marker_requests_merge_names_and_refuse_override() {
    let home = TempHome::new();
    let dir = home.root().join("notes");
    fs::create_dir(&dir).unwrap();
    let marker = dir.join(register::DIRECTORY_MARKER);
    for requests in [
        vec![named(&marker, "Chosen"), dir.clone().into()],
        vec![dir.clone().into(), named(&marker, "Chosen")],
    ] {
        let plan = plan_register(&home.home, requests).unwrap();
        assert_eq!(plan.entries.len(), 1);
        assert!(plan.entries[0].creates_marker);
        assert_eq!(plan.entries[0].name, Some(name("Chosen")));
    }
    fs::write(&marker, b"---\nfolio-name: Chosen\n---\n").unwrap();
    let plan = plan_register(&home.home, [named(&dir, "Chosen")]).unwrap();
    assert_eq!(
        plan.entries[0].status,
        RegisterStatus::Refused(RefusalReason::NameOptionWithFrontMatter)
    );
}

#[test]
fn markdown_name_writes_preserve_surrounding_bytes_and_store_the_new_content() {
    for (filename, original, explicit, expected) in [
        (
            "block.md",
            b"---\ntitle: keep\n---\nbody".as_slice(),
            None,
            b"---\ntitle: keep\nfolio-name: \"block\"\n---\nbody".as_slice(),
        ),
        (
            "new.md",
            b"body\n",
            None,
            b"---\nfolio-name: \"new\"\n---\nbody\n",
        ),
        (
            "crlf.MARKDOWN",
            b"---\r\ntitle: keep\r\n---\r\nbody\r\n",
            None,
            b"---\r\ntitle: keep\r\nfolio-name: \"crlf\"\r\n---\r\nbody\r\n",
        ),
        (
            "bom.MD",
            b"\xef\xbb\xbfbody\r\n",
            None,
            b"\xef\xbb\xbf---\nfolio-name: \"bom\"\n---\nbody\r\n",
        ),
        (
            "explicit.md",
            b"body\r\n",
            Some("true"),
            b"---\nfolio-name: \"true\"\n---\nbody\r\n",
        ),
    ] {
        let home = TempHome::new();
        let path = home.root().join(filename);
        fs::write(&path, original).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        }
        let permissions = fs::metadata(&path).unwrap().permissions();
        let request = explicit.map_or_else(|| path.clone().into(), |n| named(&path, n));
        let plan = plan_register(&home.home, [request]).unwrap();
        assert_eq!(plan.summary.files_to_write, 1);
        assert_eq!(fs::read(&path).unwrap(), original);
        let applied = apply_register(&home.home, &plan, false).unwrap();
        assert_eq!(applied.summary.refused, 0);
        assert_eq!(applied.entries[0].size, expected.len() as u64);
        assert_eq!(fs::read(&path).unwrap(), expected);
        assert_eq!(fs::metadata(&path).unwrap().permissions(), permissions);
        let registry = Registry::open_existing(&home.home).unwrap();
        let row = registry
            .placement_at(&PlacementPath::resolve(&path).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(registry.folio_name(row.id).unwrap(), plan.entries[0].name);
        assert_eq!(
            folt_core::front_matter::read_name(expected)
                .unwrap()
                .as_deref(),
            registry
                .folio_name(row.id)
                .unwrap()
                .as_ref()
                .map(|n| n.as_str()),
        );
        assert_eq!(row.base.as_ref().unwrap().hash, hash_blob(expected));
        let Observation::File { signature } = PlacementFs::new(&home.home)
            .read(row.id, &row.to_placement().1)
            .unwrap()
        else {
            panic!("expected file")
        };
        assert_eq!(row.base.as_ref().unwrap().signature, signature);
        let internal = registry.internal_placement(row.id).unwrap().unwrap();
        assert_eq!(fs::read(internal.path.to_path_buf()).unwrap(), expected);
        assert_eq!(internal.base.unwrap().hash, hash_blob(expected));
        // Successful atomic replacement leaves no sibling temporary files.
        assert!(
            !fs::read_dir(home.root())
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".folt-name-"))
        );
    }
}

#[test]
fn existing_name_at_scan_limit_registers_without_rewriting() {
    use folt_core::register::{NameSource, NameWrite};

    let home = TempHome::new();
    let path = home.root().join("note.md");
    let bytes = format!("---\n#{}\nfolio-name: x\n---\n", "a".repeat(65515));
    assert_eq!(&bytes.as_bytes()[65535..], b"---\n");
    fs::write(&path, &bytes).unwrap();

    let plan = plan_register(&home.home, [&path]).unwrap();
    assert_eq!(plan.summary.refused, 0);
    assert_eq!(plan.entries[0].status, RegisterStatus::New);
    assert_eq!(plan.entries[0].name.as_ref().unwrap().as_str(), "x");
    assert_eq!(plan.entries[0].name_source, Some(NameSource::FrontMatter));
    assert_eq!(plan.entries[0].name_write, NameWrite::None);
    assert_eq!(plan.summary.files_to_write, 0);
    assert_eq!(apply_register(&home.home, &plan, false).unwrap(), plan);
    let registry = Registry::open_existing(&home.home).unwrap();
    let row = registry
        .placement_at(&PlacementPath::resolve(&path).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(registry.folio_name(row.id).unwrap().unwrap().as_str(), "x");
    assert_eq!(fs::read(&path).unwrap(), bytes.as_bytes());
}

#[test]
fn needed_name_write_past_scan_limit_is_refused_with_the_path() {
    use folt_core::front_matter::FrontMatterError;

    let home = TempHome::new();
    let path = home.root().join("note.md");
    let bytes = format!("---\n#{}\n---\n", "a".repeat(65529));
    assert_eq!(&bytes.as_bytes()[65535..], b"---\n");
    fs::write(&path, &bytes).unwrap();

    let plan = plan_register(&home.home, [&path]).unwrap();
    assert_eq!(plan.summary.refused, 1);
    assert_eq!(
        plan.entries[0].status,
        RegisterStatus::Refused(RefusalReason::FrontMatter(FrontMatterError::Unterminated))
    );
    assert!(
        plan.to_string()
            .contains(&plan.entries[0].path.display().to_string())
    );
    assert_eq!(apply_register(&home.home, &plan, false).unwrap(), plan);
    assert_eq!(fs::read(&path).unwrap(), bytes.as_bytes());
}

#[test]
fn existing_name_does_not_rewrite_the_file_even_if_its_spelling_is_unquoted() {
    let home = TempHome::new();
    let path = home.root().join("file.md");
    let bytes = b"\xef\xbb\xbf---\r\nfolio-name: Existing # keep comment\r\n---\r\nbody";
    fs::write(&path, bytes).unwrap();
    let alias = home.root().join("alias");
    fs::hard_link(&path, &alias).unwrap();
    #[cfg(unix)]
    let inode = {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(&path).unwrap().ino()
    };
    let plan = plan_register(&home.home, [&path]).unwrap();
    assert_eq!(plan.summary.refused, 0);
    assert_eq!(plan.entries[0].name_write, register::NameWrite::None);
    assert_eq!(plan.summary.files_to_write, 0);
    assert_eq!(apply_register(&home.home, &plan, false).unwrap(), plan);
    let registry = Registry::open_existing(&home.home).unwrap();
    let row = registry
        .placement_at(&PlacementPath::resolve(&path).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        registry.folio_name(row.id).unwrap().unwrap().as_str(),
        "Existing"
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(fs::read(&alias).unwrap(), bytes);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
        assert_eq!(fs::metadata(&path).unwrap().nlink(), 2);
    }
}

#[test]
fn source_changed_after_planning_a_name_write_is_blocked_without_rewriting() {
    let home = TempHome::new();
    let path = home.root().join("file.md");
    fs::write(&path, b"old body").unwrap();
    let plan = plan_register(&home.home, [&path]).unwrap();
    assert_eq!(plan.summary.files_to_write, 1);
    fs::write(&path, b"user changed body").unwrap();
    let result = apply_register(&home.home, &plan, false).unwrap();
    assert_eq!(
        result.entries[0].status,
        RegisterStatus::Refused(RefusalReason::FileChanged)
    );
    assert_eq!(fs::read(path).unwrap(), b"user changed body");
    assert!(!home.path().join("registry.db").exists());
}
