pub mod home;
pub mod registry;

/// Returns the version of `folt`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
