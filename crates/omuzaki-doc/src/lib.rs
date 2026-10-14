//! The project store (spec v3 §7): a folder of TOML files that loads into the document tree and saves back.
//!
//! [`Project`] implements [`DocRead`](omuzaki_doc_api::DocRead) and [`DocWrite`](omuzaki_doc_api::DocWrite), so ops, the
//! MCP server and the UI work against it exactly as they do against the in-memory mock in tests.
//!
//! # The folder is the tree
//!
//! ```text
//! my-show.omuzaki/                          document path
//!   project.toml                          project
//!   characters/mira/face.toml          characters/mira/face
//!   characters/mira/variants/game.toml characters/mira/variants/game
//!   scenes/opening.toml                   scenes/opening
//!   assets.lock, *.track                  (not in the tree)
//!   .omuzaki/                               (not in the tree)
//! ```
//!
//! - A folder is an object. A `<name>.toml` file is the object under `<name>` holding the file's table, so the
//!   file at `characters/mira/face.toml` is the node at `characters/mira/face`.
//! - Only `.toml` files and the folders that lead to them are in the tree. Other files (binary stores, locks), names
//!   starting with `.`, symbolic links and names that aren't UTF-8 are left alone and never touched. Saving never
//!   writes through a symbolic link or over one: it stops with [`StoreError::Symlink`].
//! - TOML dates and times appear as strings holding their text, and are written back unchanged until edited. `null`
//!   doesn't exist in TOML, so patches containing it are refused.
//! - A file and a folder with the same name (`scenes.toml` and `scenes/`) are an error: the tree can't hold both.
//!
//! # Which objects are files and which are folders
//!
//! The tree doesn't say, so the store decides, and the decision only affects how the project looks on disk:
//! reopening always gives back the same tree. Below the root, every object is a **file** (its whole subtree is one
//! TOML file) or a **folder** (its entries are files and folders). The top level is always a folder, so top-level
//! entries must be objects.
//!
//! 1. An object is a file if it is empty, or holds anything that isn't an object (a folder can't hold a number or
//!    a list). So a plain value can't be added next to the files of an existing folder: merging them into one file
//!    would lose their comments and the type of values like dates, and the patch is refused with
//!    [`PatchError::Unreachable`](omuzaki_doc_api::PatchError::Unreachable) (put the value inside one of the files).
//! 2. Otherwise it keeps what it was: a file that has content stays a file even if it holds only tables, and a
//!    folder stays a folder, so editing never reorganises a project.
//! 3. Otherwise (a new object holding only objects) it is a folder. That puts `characters/<id>/` and `variants/`
//!    in folders and `face`, `rig`, ... in files, as the spec lays them out.
//! 4. An empty object is saved as an empty file: an empty folder holds no TOML, so it would not come back on
//!    reopening. It has no layout of its own; once it gains entries, rule 3 applies.
//! 5. Names the store creates for files and folders must be slugs ([`omuzaki_core::id::Slug`]) and not device names
//!    like `con` or `com1` that Windows reserves; both are valid file names on every OS. A new object whose entries aren't slugs becomes a file instead (inside a file any key is
//!    fine). Adding such an entry to a folder that already exists is refused with
//!    [`PatchError::Unreachable`](omuzaki_doc_api::PatchError::Unreachable), rather than turning the folder into a
//!    file.
//!
//! # Saving
//!
//! [`Project::save`] touches only files whose part of the tree changed. A file is edited in place with `toml_edit`,
//! so comments, blank lines, key order, number formatting and line endings that a person wrote survive; new keys are
//! appended and new content is written in a plain, stable style (keys in the tree's sorted order, sub-tables as
//! `[a.b]`, lists of objects as `[[a]]`). Each file is replaced atomically (temporary file in the same folder, flush,
//! rename).
//!
//! # Schema versions and migrations
//!
//! `project.toml` holds `schema = N`. Opening an older project runs the migrations in `src/migrations/` in order
//! (each registers itself; see that module), in memory, until the next save. Opening a newer one is
//! [`StoreError::SchemaTooNew`].
#![deny(missing_docs)]

mod disk;
mod error;
mod known;
mod layout;
mod lock;
mod migrate;
mod migrations;
mod project;
mod toml_read;
mod toml_write;
mod tree;

#[cfg(test)]
#[path = "../build_support/migration_files.rs"]
mod migration_files;

pub use error::StoreError;
pub use lock::ProjectLock;
pub use migrate::{BASE_SCHEMA, Migration, MigrationError, supported_schema};
pub use project::{Project, RenderedFile, SaveReport};

#[doc(hidden)]
pub use inventory as __inventory;
#[doc(hidden)]
pub use omuzaki_core::__record_registration;
