//! Schema migrations, one per file (spec v3 §39 rule 5).
//!
//! To add one, create `m<NNNN>_<what>.rs` here, where `NNNN` is the schema it *produces* (the first is `m0002_...`,
//! then `m0003_...`, with no gaps), and declare it:
//!
//! ```ignore
//! use serde_json::Value;
//! use crate::{MigrationError, register_migration};
//!
//! fn up(project: &mut Value) -> Result<(), MigrationError> {
//!     // Change the whole project tree from the old shape to the new one. Don't touch `project/schema`.
//!     Ok(())
//! }
//!
//! register_migration!(2, "what", up);
//! ```
//!
//! A migration must be safe to run twice: `save` writes `project.toml` last, so a save that is interrupted leaves the
//! old schema over some files that are already migrated. And it must not add a plain value (a number, string or
//! list) directly to a folder of files such as `characters/<id>`; add it inside one of the files, or wrap it in an
//! object, because merging the files into one would lose their comments (see the crate docs).
//!
//! Nothing else needs editing: `build.rs` compiles every file named like that, and a test checks the numbers and
//! names agree and nothing is missing. Two PRs that add the same number fail to build, which is the signal to
//! renumber the later one. Write a test next to the migration with a small old-format project.

include!(concat!(env!("OUT_DIR"), "/migrations.rs"));
