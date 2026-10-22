//! The feature-layout ops (CB-12): `face.layout.measure` reads where the features sit as ratios and sets them
//! beside reference ratios, `face.set_layout` sets one field of the layout, and `face.set_layout_ratios` sets the whole
//! layout from ratios.
//!
//! The layout is stored at `characters/<id>/face/layout` (`FaceLayout`, `omuzaki-face-api`). Ratios are head-size
//! independent, so they are read against the head's chin: the head part's `outline.chin_y()` (see `chin`). The
//! ratio math and its inverse live in `ratios`, the shared checks and patch-building in `edit`, and the ops in
//! `measure` and `set`.
//!
//! Every op plans against the document and returns a patch; none writes it. A value is checked on a copy of the layout,
//! and an edit is refused only for the problems it introduces, so a layout damaged elsewhere still takes unrelated edits.

mod chin;
mod edit;
mod measure;
mod ratios;
mod set;

pub use measure::MeasureLayout;
pub use ratios::{LayoutRatios, PartialLayoutRatios};
pub use set::{SetLayout, SetLayoutRatios};
