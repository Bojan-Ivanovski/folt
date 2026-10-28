use std::io;
use std::path::Path;

/// Syncs directory entries on Unix, propagating open and sync errors.
///
/// This is a no-op on Windows: `File::open` cannot open a directory there.
/// Directory durability is therefore not guaranteed on Windows; callers still
/// sync regular files separately.
pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(test)]
    if SYNC_FAILURE.with(|failure| {
        let mut failure = failure.borrow_mut();
        if let Some((target, remaining)) = failure.as_mut()
            && (target == path
                || target
                    .canonicalize()
                    .is_ok_and(|target| path.canonicalize().is_ok_and(|path| target == path)))
        {
            if *remaining == 0 {
                return true;
            }
            *remaining -= 1;
        }
        false
    }) {
        return Err(io::Error::other("injected directory sync failure"));
    }
    sync_directory_impl(path)
}

#[cfg(test)]
thread_local! {
    static SYNC_FAILURE: std::cell::RefCell<Option<(std::path::PathBuf, usize)>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(test)]
pub(crate) fn with_sync_failure<T>(path: &Path, run: impl FnOnce() -> T) -> T {
    with_sync_failure_after(path, 0, run)
}

#[cfg(test)]
pub(crate) fn with_sync_failure_after<T>(
    path: &Path,
    successful: usize,
    run: impl FnOnce() -> T,
) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            SYNC_FAILURE.with(|failure| *failure.borrow_mut() = None);
        }
    }
    SYNC_FAILURE.with(|failure| {
        assert!(failure.borrow().is_none());
        *failure.borrow_mut() = Some((path.to_path_buf(), successful));
    });
    let _reset = Reset;
    run()
}

#[cfg(unix)]
fn sync_directory_impl(path: &Path) -> io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

#[cfg(windows)]
fn sync_directory_impl(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syncs_a_real_directory() {
        let path = std::env::temp_dir().join(format!("folt-sync-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        let result = sync_directory(&path);
        std::fs::remove_dir(&path).unwrap();
        result.unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn sync_failure_matches_canonical_paths_after_directory_creation() {
        let root = std::env::temp_dir().join(format!("folt-sync-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let alias = root.join("alias");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let target = alias.join("directory");
        with_sync_failure_after(&target, 1, || {
            std::fs::create_dir(&target).unwrap();
            let canonical = target.canonicalize().unwrap();
            sync_directory(&canonical).unwrap();
            let error = sync_directory(&canonical).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("injected directory sync failure")
            );
        });
        sync_directory(&target).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn propagates_directory_open_errors() {
        let path = std::env::temp_dir().join(format!("folt-sync-{}", uuid::Uuid::new_v4()));
        assert_eq!(
            sync_directory(&path).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }
}
