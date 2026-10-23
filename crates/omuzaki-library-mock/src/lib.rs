//! The independent oracle for the library (design `docs/design/library.md` section 6), as `omuzaki-store-mock` is
//! for the store:
//!
//! - [`fixtures`]: items under `test/` (and two publishers' items), one of each kind on the character target, a
//!   scene-target item (refused on insert), an item whose op is missing, and signed and unverified publishers
//! - [`MockLibrary`]: an in-memory [`Library`](omuzaki_library_api::Library) with a naive linear search, which the
//!   real index must agree with on the fixture set. `compatible_with` is answered from [`CharacterFacts`] the test
//!   hands it ([`fixtures::characters`])
//! - [`apply_for_test`]: expands a recipe and plans each step on a [`Scratch`](omuzaki_doc_api::Scratch) with
//!   `plan_step`; no records and no slots, which is what seed tests need without linking `omuzaki-library`
//! - [`seed::check_source`]: the per-crate seed checks (design D15)
//! - [`conformance::run`]: what every `Library` must do on the fixture set
//!
//! The ops themselves live in the owning crates: a test registers the ops it plans, as the other mocks' tests do.
#![deny(missing_docs)]

mod apply;
pub mod conformance;
mod facts;
pub mod fixtures;
mod library;
pub mod seed;
mod text;

pub use apply::{apply_for_test, apply_with_library};
pub use library::MockLibrary;
pub use omuzaki_library_api::CharacterFacts;

omuzaki_core::define_link! {
    omuzaki_char_api::link();
    omuzaki_library_api::link();
}
