pub mod checkout;
pub mod config;
pub mod folio_dir;
mod fs;
pub mod history;
pub mod home;
pub mod layout;
pub mod object;
pub mod placement;
pub mod placement_fs;
pub mod placement_rules;
pub mod prune;
pub mod refs;
pub mod register;
pub mod registry;
pub mod store;
pub mod working_state;

/// Returns the version of `folt`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
