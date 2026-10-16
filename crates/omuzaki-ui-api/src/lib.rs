//! Contract for the UI shell (`docs/design/ux/shell.md` section 3, spec v3 §39 rule 2).
//!
//! Workspaces, panels and gallery stories register themselves next to their code, so the tab bar, the View menu and
//! the gallery show whatever is compiled in and no pull request edits a shared list:
//!
//! - [`registry`]: [`register_workspace!`], [`register_panel!`], [`register_story!`] and the [`Registry`] that
//!   collects them, ordered by group (a panel's region) then `order` then id, with duplicate ids reported
//! - [`panel`]: the [`Panel`](panel::Panel) trait and [`PanelCtx`](panel::PanelCtx), what a panel may read and run
//! - [`layout`]: the tree a workspace's default layout is written in
//!
//! Panels change the document only through the `Runner` in their context (`cargo xtask lint-ui-writes`).
//!
//! [`Registry`]: registry::Registry
#![deny(missing_docs)]

pub mod layout;
pub mod panel;
pub mod registry;

#[doc(hidden)]
pub use egui as __egui;
#[doc(hidden)]
pub use inventory as __inventory;
#[doc(hidden)]
pub use omuzaki_core::__record_registration;
