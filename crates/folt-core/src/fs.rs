use std::io;
use std::path::Path;

/// Syncs directory entries on Unix, propagating open and sync errors.
///
/// This is a no-op on Windows: `File::open` cannot open a directory there.
/// Directory durability is therefore not guaranteed on Windows; callers still
/// sync regular files separately.
#[cfg(unix)]
pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

#[cfg(windows)]
pub(crate) fn sync_directory(_path: &Path) -> io::Result<()> {
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
    fn propagates_directory_open_errors() {
        let path = std::env::temp_dir().join(format!("folt-sync-{}", uuid::Uuid::new_v4()));
        assert_eq!(
            sync_directory(&path).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }
}
