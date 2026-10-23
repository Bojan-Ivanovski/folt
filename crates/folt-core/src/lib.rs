pub mod folio_dir;
pub mod home;
pub mod layout;
pub mod object;
pub mod placement;
pub mod registry;
pub mod store;

/// Returns the version of `folt`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
