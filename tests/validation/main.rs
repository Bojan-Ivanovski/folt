#[allow(dead_code)]
#[path = "../common/mod.rs"]
mod common;

mod checkout;
mod declared_references;
mod folio;
mod front_matter;
mod front_matter_awkward;
mod front_matter_hidden;
mod front_matter_properties;
mod history;
mod home;
mod link_commit;
#[cfg(unix)]
mod non_regular;
mod placement;
mod placement_ops;
mod prune;
mod refs;
mod register;
mod registry;
mod support;
