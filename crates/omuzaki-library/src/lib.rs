//! The library (spec v3 §28, ADR 0019, design `docs/design/library.md`): the implementation of the
//! [`Library`](omuzaki_library_api::Library) contract and the `library.*` ops.
//!
//! - [`HostLibrary`]: the registry and search index. It loads every first-party source linked into the binary
//!   ([`sources`](omuzaki_library_api::sources), design D14) and the person's library folder
//!   (`<data>/library/items/<publisher>/<name…>/<version>.toml`, D11), checks each manifest and its publisher, and
//!   indexes titles, tags, summaries and ids once (D13). Installing and uninstalling write that folder and update the
//!   index. The app injects it as a service, like the store.
//! - The ops (`src/ops`, one file each): `library.list`, `.search`, `.describe` and `.lock` read; `library.apply`,
//!   `.update` and `.detach` edit the document as one undoable step each; `library.install` and `.uninstall` change
//!   the person's library (`Undo::Explicit`, a person's call: an agent gets `needs_person`).
//!
//! **Applying materialises** (D5): the recipe's ops are planned one by one on a scratch copy of the document
//! ([`OpCtx::plan_step`](omuzaki_ops_api::OpCtx::plan_step)), their patches combined with the source rows of the
//! parts it created and the application record (`characters/<id>/applied/<app>`, the pin and the lock). A character
//! never reads the library to build. **Updating** is a three-way merge with the record's `wrote` as the base
//! (D10, [`omuzaki_doc_api::merge::merge3`]): the person's edits win unless they ask for the item's.
#![deny(missing_docs)]

mod acks;
mod apply;
mod common;
mod host;
pub mod ops;
mod records;
mod text;

pub use acks::Acknowledgement;
pub use host::HostLibrary;

omuzaki_core::define_link! {
    omuzaki_char_api::link();
    omuzaki_library_api::link();
}
