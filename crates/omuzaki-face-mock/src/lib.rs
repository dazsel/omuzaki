//! A deterministic stand-in for the face painter, for tests and the walking skeleton (spec v3 §39 rule 6).
//!
//! - [`MockPainter`] implements [`FacePainter`](omuzaki_face_api::FacePainter) with a simple geometric face: it reacts to
//!   about a third of the channels (eyes, gaze, brows, mouth, blush), enough to test every seam without the real
//!   painter. Channels it ignores are listed in [`MockPainter::SUPPORTED`]'s complement.
//! - [`MockRasterizer`] implements [`FaceRasterizer`](omuzaki_face_api::FaceRasterizer): a small, exact-enough scanline
//!   rasteriser (non-zero fill, clips, transforms, gradients, strokes) so a draw list can become pixels with no GPU.
//! - [`mock_profile`] and [`mock_projection`] give tests a face and a place to put it.
//! - [`MockFaceBakerFactory`] registers a trivial baker with the face registry, so `face_baker()` finds one.
#![deny(missing_docs)]

mod baker;
mod painter;
mod raster;

pub use baker::{MockFaceBaker, MockFaceBakerFactory};
pub use painter::{MockPainter, mock_profile, mock_projection};
pub use raster::MockRasterizer;

omuzaki_core::define_link!();
