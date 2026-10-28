use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use folt_core::home::{Home, HomeError};
use folt_core::object::{ObjectKind, hash_blob};
use folt_core::registry::RegistryError;
use folt_core::store::StoreError;

use crate::common::TempHome;
use crate::support::TestFolio;

fn mkfifo(path: &Path) {
    assert!(Command::new("mkfifo").arg(path).status().unwrap().success());
}

fn private_home() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

fn assert_completes(case: &str, test: impl FnOnce() + Send + 'static) {
    let (sender, receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        test();
        sender.send(()).unwrap();
    });
    match receiver.recv_timeout(Duration::from_secs(8)) {
        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => worker.join().unwrap(),
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("{case} blocked for more than 8 seconds"),
    }
}

#[test]
fn fifo_registry_with_unrelated_file_is_not_a_home() {
    assert_completes("FIFO registry with unrelated file", || {
        let directory = private_home();
        mkfifo(&directory.path().join("registry.db"));
        fs::write(directory.path().join("notes.txt"), b"notes").unwrap();
        assert!(matches!(
            Home::open_or_create(directory.path().to_path_buf()),
            Err(HomeError::NotAHome { .. })
        ));
    });
}

#[test]
fn fifo_registry_with_only_allowed_names_is_refused() {
    assert_completes("FIFO registry with only allowed names", || {
        let directory = private_home();
        mkfifo(&directory.path().join("registry.db"));
        assert!(matches!(
            Home::open_or_create(directory.path().to_path_buf()),
            Err(HomeError::Registry(RegistryError::NotRegularFile(_)))
        ));
    });
}

#[test]
fn symlink_to_fifo_registry_with_unrelated_file_is_not_a_home() {
    assert_completes("symlink to FIFO registry with unrelated file", || {
        let target = tempfile::tempdir().unwrap();
        let fifo = target.path().join("fifo");
        mkfifo(&fifo);
        let directory = private_home();
        symlink(&fifo, directory.path().join("registry.db")).unwrap();
        fs::write(directory.path().join("notes.txt"), b"notes").unwrap();
        assert!(matches!(
            Home::open_or_create(directory.path().to_path_buf()),
            Err(HomeError::NotAHome { .. })
        ));
    });
}

#[test]
fn symlink_to_fifo_registry_with_only_allowed_names_is_refused() {
    assert_completes("symlink to FIFO registry with only allowed names", || {
        let target = tempfile::tempdir().unwrap();
        let fifo = target.path().join("fifo");
        mkfifo(&fifo);
        let directory = private_home();
        symlink(&fifo, directory.path().join("registry.db")).unwrap();
        assert!(matches!(
            Home::open_or_create(directory.path().to_path_buf()),
            Err(HomeError::Registry(RegistryError::NotRegularFile(_)))
        ));
    });
}

#[test]
fn fifo_lock_is_refused() {
    assert_completes("FIFO lock", || {
        let home = TempHome::new();
        drop(home.lock());
        let path = home.path().join("lock");
        fs::remove_file(&path).unwrap();
        mkfifo(&path);
        assert!(Home::open_or_create(home.path().to_path_buf()).is_ok());
        assert!(matches!(
            home.home.try_lock(),
            Err(HomeError::NotRegularFile { .. })
        ));
        assert!(matches!(
            home.home.lock(),
            Err(HomeError::NotRegularFile { .. })
        ));
    });
}

#[test]
fn fifo_blob_is_refused() {
    assert_completes("FIFO object blob", || {
        let home = TempHome::new();
        let lock = home.lock();
        let folio = TestFolio::new(&home, &lock);
        let id = hash_blob(b"fifo-blob");
        mkfifo(&folio.folio.path().join("blobs").join(format!("{id}.blob")));
        assert!(matches!(
            folio.objects.kind_of(id),
            Err(StoreError::NotRegularFile(_))
        ));
        assert!(matches!(
            folio.objects.get(id, ObjectKind::Blob, 1024),
            Err(StoreError::NotRegularFile(_))
        ));
        assert!(matches!(
            folio.objects.get_to(id, ObjectKind::Blob, &mut Vec::new()),
            Err(StoreError::NotRegularFile(_))
        ));
        assert!(matches!(
            folio.objects.put(&lock, ObjectKind::Blob, b"fifo-blob"),
            Err(StoreError::NotRegularFile(_))
        ));
    });
}
