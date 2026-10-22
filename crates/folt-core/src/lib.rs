pub mod folio_dir;
pub mod home;
pub mod object;
pub mod placement;
pub mod registry;

/// Returns the version of `folt`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
