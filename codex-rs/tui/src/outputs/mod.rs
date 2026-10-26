//! Read-only, thread-scoped output browser.
//!
//! Loading intentionally lives in `data`: this module owns only the retained UI
//! state, rendering and keyboard interaction. Keeping that boundary means a
//! late workspace or app-server response cannot draw itself into a newer view.

pub(crate) mod data;
mod diff;
mod preferences;
mod tree;
mod types;
mod ui;

pub(crate) use preferences::BranchAssociation;
pub(crate) use preferences::Preferences;
pub(crate) use ui::OutputsView;
