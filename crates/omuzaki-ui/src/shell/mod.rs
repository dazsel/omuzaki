//! The app shell (`docs/design/ux/shell.md`): workspaces, docking, panels, the top bar and the status bar.
//!
//! This module is built in layers so each can be tested without a window:
//!
//! - [`class`]: the width classes and what each changes, and the order docks collapse in as the window shrinks;
//! - [`registry`]: what the shell reads from `omuzaki_ui_api::registry`, where workspaces and panels register themselves
//!   (`register_workspace!`, `register_panel!`), so the tab bar and the View menu show whatever is compiled in and
//!   nothing edits a central list (spec §39 rule 2);
//! - [`layout`]: a layout is a small serialisable tree of panel ids; it converts to and from the docking widget's tree;
//! - [`persist`]: layouts belong to the person, not the project, and are saved per workspace;
//! - [`text`], [`keys`], [`panels`], [`stub`]: the shell's words, its global keyboard map, the panels it ships with and
//!   the runner it starts with;
//! - [`host`], [`dock`], [`saver`]: the panels built and how one is drawn, how panels are docked and constrained, and
//!   the background thread that writes layouts;
//! - [`app`]: the [`Shell`] that draws a frame from all of them.

// Layout arithmetic converts between small integers and `f32` (window sizes, counts); every value is bounded by a screen.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

pub mod app;
pub mod class;
pub mod dock;
pub mod history;
pub mod host;
pub mod keys;
pub mod layout;
pub mod palette;
pub mod panels;
pub mod persist;
pub mod registry;
pub mod requests;
pub mod saver;
pub mod stub;
pub mod text;

pub use app::Shell;

#[cfg(test)]
mod tests;
