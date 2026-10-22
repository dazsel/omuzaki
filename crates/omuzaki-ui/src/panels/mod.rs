//! The edit panels of the Build workspace (`shell.md` sections 5 to 7): the viewport, the outliner and the inspector.
//!
//! They share [`edit::EditFocus`] (the open character and the selection) and change the document only through the
//! runner they are given ([`io::Io`]), as every panel does (`cargo xtask lint-ui-writes`).

pub mod agent;
pub mod edit;
pub mod inspector;
pub mod io;
pub mod outliner;
pub mod setup;
pub mod text;
pub mod viewport;
