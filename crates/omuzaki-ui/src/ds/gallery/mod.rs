//! The gallery: every component in every state, in every theme (`design-system.md` section 10).
//!
//! Each component registers its own story (see [`register_story!`]), so the gallery lists whatever is compiled in
//! and a new component never edits a shared file. The `gallery` binary shows it in a window, writes screenshots of it
//! and measures its frame time; the tests walk its accessibility tree.

mod app;
pub mod bench;
#[cfg(feature = "capture")]
pub mod capture;
mod layout;
mod stories;
mod story;
pub(crate) mod text;

pub use app::Gallery;
pub use story::{Group, Story, all};

#[cfg(test)]
mod tests;
