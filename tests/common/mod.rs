use std::path::{Path, PathBuf};

use folt_core::home::{Home, HomeLock};

/// Owns a private Folt home and removes it when the test finishes.
pub struct TempHome {
    pub home: Home,
    directory: tempfile::TempDir,
}

impl TempHome {
    pub fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let home = Home::open_or_create(directory.path().join("home")).unwrap();
        Self { home, directory }
    }

    pub fn path(&self) -> &Path {
        self.home.path()
    }

    pub fn root(&self) -> &Path {
        self.directory.path()
    }

    pub fn lock(&self) -> HomeLock {
        self.home.try_lock().unwrap()
    }
}

/// Resolves suite data independently of the process working directory.
pub fn fixture(relative: impl AsRef<Path>) -> PathBuf {
    Path::new(env!("FOLT_TESTS_DIR"))
        .join("fixtures")
        .join(relative)
}
