//! The character ops, `character.create` and `character.rename`, and the helpers the part ops share.
//!
//! These ops are what an agent or the UI calls to start a character and to arrange its parts, so each one follows
//! the same shape: read the character from the document, check the edit against it, return one minimal patch. A
//! dry-run is that plan shown without applying it, and undo is the patch's inverse, so an op never has to write
//! undo code of its own.
//!
//! | Module | Holds |
//! |---|---|
//! | [`create`], [`rename`] | the two `character.*` ops |
//! | `access` | reading a character from the document, building changes, and the shared error wording |
//! | `tree` | what makes the parts a buildable tree, and the layer convention |

pub mod create;
pub mod rename;

pub(crate) mod access;
pub(crate) mod tree;
