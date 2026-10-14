//! A small deterministic character, built through the real part-kind contract (spec v3 §39 rule 6).
//!
//! Importing this crate registers four kinds (`mock-head`, `mock-hair`, `mock-body`, `mock-garment`) that take the
//! **real** parameter types (`HeadParams`, `HairParams`, `BodyParams`), so tests exercise the file format, validation,
//! variants and the build pipeline end to end without the real generators:
//! - [`mock_character`]: a head, hair (cap and a simulated curtain), a torso and two layered garments
//! - [`mock_variants`]: a `game` and a `close-up` variant, selected by shot
//! - [`MockLive`]: a frame loop that steps the hair, maps channels to paint parameters, paints the face and reports
//!   what changed
//! - [`MemAssets`]: an in-memory [`AssetSource`](omuzaki_char_api::AssetSource) for the build services
//!
//! The geometry is ellipsoids, tubes and ribbons; the hair sway is a sine of the shot clock. Everything is a pure
//! function of the character and its seed.
#![deny(missing_docs)]

mod assets;
mod body;
mod hair;
mod head;
mod live;
mod shapes;
mod util;

use omuzaki_char_api::{CharacterDef, VariantSet};

pub use assets::MemAssets;
pub use body::{GarmentParams, MockBody, MockGarment, OffsetFit, TorsoFit};
pub use hair::MockHair;
pub use head::{EllipsoidHead, MockHead};
pub use live::{MockLive, RestPose, TickReport};

/// The mock character as it is stored: `fixtures/character.json`, a readable example of the project format.
pub const CHARACTER_JSON: &str = include_str!("../fixtures/character.json");

/// The mock variants as they are stored: `fixtures/variants.json`.
pub const VARIANTS_JSON: &str = include_str!("../fixtures/variants.json");

/// The mock character: `body`, `head`, `hair` (cap and a simulated curtain), a `shirt` (layer 20) and a `vest`
/// (layer 30). The fringe is not in the base; the `close-up` variant adds it.
///
/// # Errors
/// Only if the fixture stops matching the character types (a test catches that).
pub fn mock_character() -> Result<CharacterDef, serde_json::Error> {
    serde_json::from_str(CHARACTER_JSON)
}

/// Two variants: `game` (wide shots) and `close-up` (the head fills at least a third of the frame): a longer chin, a
/// lower mouth, performing eyes and the fringe on.
///
/// # Errors
/// Only if the fixture stops matching the variant types.
pub fn mock_variants() -> Result<VariantSet, serde_json::Error> {
    serde_json::from_str(VARIANTS_JSON)
}

omuzaki_core::define_link!();
