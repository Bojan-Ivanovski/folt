pub mod folio_dir;
pub mod home;
pub mod placement;

/// Returns the version of `folt`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
