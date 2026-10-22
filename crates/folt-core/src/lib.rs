pub mod home;
pub mod object;

/// Returns the version of `folt`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
