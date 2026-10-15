//! The real face painter: the prototype's `face.js` as pure functions from paint parameters to vector draw lists.
//!
//! [`RealPainter`] runs the [`Section`]s in draw order, each a function `paint(ctx: &mut PaintCtx<'_>)` in its own
//! module. A section reads the profile and the paint parameters from the [`PaintCtx`] and appends commands to the
//! layer lists it draws on; it never allocates once the lists are warm, never reads a clock and never keeps state, so
//! the same input paints the same lists on every platform. Sections are private to the crate: tools reach one through
//! [`RealPainter::paint_section`].

mod brows;
mod cheeks;
mod eyes;
mod marks;
mod mouth;
mod nose;
mod overlay;
mod real;

use omuzaki_core::time::Seconds;
use omuzaki_face_api::{
    DEFAULT_PX_PER_M, DrawList, FaceDrawing, FaceInput, FaceLayerId, FaceLayerSpec, FaceProfile, FaceView, PaintParams,
};

pub use mouth::{MouthPiece, OpenMouth};
pub use real::RealPainter;

/// One part of the face, in the order [`RealPainter`] paints them.
///
/// The order is the prototype's (cheek wash, brows, eyes, nose, mouth) with the two sections the prototype lacked
/// slotted where they belong: marks are skin decoration, so they sit under the features, and the overlay layer is in
/// front of the hair, so its position among the others does not matter.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    /// Blush, the warm wash and the fringe shade (Base layer).
    Cheeks,
    /// Freckles, beauty marks, face paint and scars (Base layer).
    Marks,
    /// Both brows (Base and Brows layers).
    Brows,
    /// Both eyes (Base layer; some effects on Overlay).
    Eyes,
    /// The nose (Base layer).
    Nose,
    /// The mouth (Base layer).
    Mouth,
    /// Effects in front of the hair: sweat, gloom lines, anger veins (Overlay layer).
    Overlay,
}

impl Section {
    /// Every section, in draw order.
    pub const ALL: [Section; 7] =
        [Self::Cheeks, Self::Marks, Self::Brows, Self::Eyes, Self::Nose, Self::Mouth, Self::Overlay];

    fn paint(self, ctx: &mut PaintCtx<'_>) {
        match self {
            Self::Cheeks => cheeks::paint(ctx),
            Self::Marks => marks::paint(ctx),
            Self::Brows => brows::paint(ctx),
            Self::Eyes => eyes::paint(ctx),
            Self::Nose => nose::paint(ctx),
            Self::Mouth => mouth::paint(ctx),
            Self::Overlay => overlay::paint(ctx),
        }
    }
}

/// What a [`Section`] reads and draws into for one frame.
pub struct PaintCtx<'a> {
    /// The face design, with palette slots already resolved.
    pub profile: &'a FaceProfile,
    /// The frame's paint parameters.
    pub params: &'a PaintParams,
    /// View-dependent inputs.
    pub view: FaceView,
    /// The shot clock, for twinkle and shimmer.
    pub time: Seconds,
    /// Test hook: when set, only this section paints ([`RealPainter::paint_section`]).
    pub only: Option<Section>,
    /// The open mouth's outline, recorded by the mouth section before any of its pieces paints (`record_outline`), so
    /// the pieces that need it (gloss, interior, rim, tongue) have it even when painted alone. `None` before the mouth
    /// paints, and for a closed mouth or a kiss.
    pub open_mouth: Option<OpenMouth>,
    /// Test hook: when set, only this piece of the mouth paints ([`RealPainter::paint_mouth_piece`]).
    mouth_only: Option<MouthPiece>,
    specs: &'a [FaceLayerSpec; FaceLayerId::COUNT],
    drawing: &'a mut FaceDrawing,
}

impl<'a> PaintCtx<'a> {
    fn new(
        input: &FaceInput<'a>,
        specs: &'a [FaceLayerSpec; FaceLayerId::COUNT],
        drawing: &'a mut FaceDrawing,
        only: Option<Section>,
    ) -> Self {
        Self {
            profile: input.profile,
            params: input.params,
            view: input.view,
            time: input.time,
            only,
            open_mouth: None,
            mouth_only: None,
            specs,
            drawing,
        }
    }

    /// The draw list of `layer`. A section picks its layer per command: the brows draw on two.
    pub fn list(&mut self, layer: FaceLayerId) -> &mut DrawList {
        &mut self.drawing[layer]
    }

    /// The raster density of `layer` right now, in pixels per metre. Draw lists are in metres and do not depend on it;
    /// a section reads it only to keep something visible at any density (a hairline never thinner than one pixel).
    pub fn raster_px_per_m(&self, layer: FaceLayerId) -> f32 {
        self.specs[layer as usize].px_per_m
    }

    /// Metres from prototype pixels, the unit the prototype's constants are written in (its face canvas is
    /// [`DEFAULT_PX_PER_M`] pixels per metre). Fixed, not the live raster density, so a quality change never changes
    /// the draw list.
    pub fn metres_from_px(&self, px: f32) -> f32 {
        px / DEFAULT_PX_PER_M
    }

    /// Prototype pixels from metres; the inverse of [`metres_from_px`](Self::metres_from_px).
    pub fn px_from_metres(&self, metres: f32) -> f32 {
        metres * DEFAULT_PX_PER_M
    }

    fn runs(&self, section: Section) -> bool {
        self.only.is_none_or(|only| only == section)
    }
}
