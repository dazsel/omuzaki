//! Pictures of a painted face for the tools: the channel sweep, the expression range test and the coverage tests paint a
//! face for a channel buffer and look at the pixels, on the CPU raster, with no renderer.
//!
//! A [`Preview`] keeps one painter, one rasteriser and one raster per layer, so painting many buffers in a row reuses
//! its memory. [`Preview::picture`] is the face as it reads on the head: the Base layer over the skin with the Overlay
//! layer (in front of the hair) over that. The Brows layer is the brows again, for hair to show them through bangs, so it
//! is measured on its own ([`Preview::layer`]) but never composited.

use image::{Rgba, RgbaImage};
use omuzaki_anim_api::ChannelBuf;
use omuzaki_core::colour::Colour;
use omuzaki_core::time::Seconds;
use omuzaki_face_api::{
    FaceDrawing, FaceError, FaceInput, FaceLayerId, FacePainter, FaceProfile, FaceProjection, FaceRasterizer, FaceView,
    Raster,
};

use crate::painter::RealPainter;
use crate::raster::SkiaRasterizer;

pub mod direction;
pub mod metrics;
pub mod range;
pub mod sheet;
pub mod sweep;

/// What a preview paints: a face with its palette resolved, where it sits on the head, and the skin under it.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceSubject {
    /// The face design, with every palette slot resolved (`FaceProfile::resolve_colours`).
    pub profile: FaceProfile,
    /// The head's face window: what the layers cover.
    pub projection: FaceProjection,
    /// The opaque skin colour the face is composited over.
    pub skin: Colour,
}

/// The smallest and largest picture width a preview paints, in pixels: below 8 a face is a smudge, and above 4096 a
/// sweep of every channel would take minutes.
pub const MIN_WIDTH_PX: u32 = 8;
/// See [`MIN_WIDTH_PX`].
pub const MAX_WIDTH_PX: u32 = 4096;

/// Paints a subject for channel buffers, at a fixed width.
pub struct Preview {
    subject: FaceSubject,
    painter: RealPainter,
    raster: SkiaRasterizer,
    drawing: FaceDrawing,
    layers: [Raster; FaceLayerId::COUNT],
}

impl Preview {
    /// A preview of `subject` whose pictures are `width_px` wide (clamped to [`MIN_WIDTH_PX`]..=[`MAX_WIDTH_PX`]); the
    /// height follows the face window's aspect.
    pub fn new(subject: FaceSubject, width_px: u32) -> Self {
        let rect = subject.projection.rect;
        let width = width_px.clamp(MIN_WIDTH_PX, MAX_WIDTH_PX);
        // A face window is at least a few centimetres wide (the head kind checks it), so this never divides by zero.
        #[allow(clippy::cast_precision_loss)] // the width is at most 4096
        let px_per_m = width as f32 / (rect.x1 - rect.x0).max(f32::EPSILON);
        Self {
            painter: RealPainter::new(subject.projection, px_per_m),
            subject,
            raster: SkiaRasterizer::new(),
            drawing: FaceDrawing::new(),
            layers: std::array::from_fn(|_| Raster::new(1, 1)),
        }
    }

    /// The subject this preview paints.
    pub fn subject(&self) -> &FaceSubject {
        &self.subject
    }

    /// Paints the face for `bus`, mapped through the subject's own mapping, and rasterises every layer.
    ///
    /// # Errors
    /// What the painter or the rasteriser returns.
    pub fn paint(&mut self, bus: &ChannelBuf) -> Result<(), FaceError> {
        self.paint_in(bus, FaceView::default())
    }

    /// [`paint`](Self::paint) as the face would be seen in `view` (a camera off to one side, a wide shot).
    ///
    /// # Errors
    /// What the painter or the rasteriser returns.
    pub fn paint_in(&mut self, bus: &ChannelBuf, view: FaceView) -> Result<(), FaceError> {
        let params = self.subject.profile.mapping.apply(bus);
        let input = FaceInput { profile: &self.subject.profile, params: &params, time: Seconds(0.0), view };
        self.painter.paint(&input, &mut self.drawing)?;
        for spec in self.painter.layers() {
            let out = &mut self.layers[spec.id as usize];
            self.raster.rasterize(spec, &self.drawing[spec.id], spec.px_per_m, out)?;
        }
        Ok(())
    }

    /// The raster of `layer` from the last [`paint`](Self::paint): premultiplied sRGB, transparent where nothing is
    /// drawn. A 1 x 1 transparent raster before the first paint.
    pub fn layer(&self, layer: FaceLayerId) -> &Raster {
        &self.layers[layer as usize]
    }

    /// The face as it reads on the head, from the last [`paint`](Self::paint): the Base layer over the skin, the Overlay
    /// layer over that. Opaque.
    pub fn picture(&self) -> RgbaImage {
        let base = self.layer(FaceLayerId::Base);
        let skin = self.subject.skin;
        let mut out = RgbaImage::from_pixel(base.width, base.height, Rgba([skin.r, skin.g, skin.b, 255]));
        over(&mut out, base);
        over(&mut out, self.layer(FaceLayerId::Overlay));
        out
    }
}

/// Premultiplied `src` over `dst`, channel by channel: `src + dst * (1 - alpha)`. A source of another size (a layer at
/// another density) is sampled at the nearest pixel.
fn over(dst: &mut RgbaImage, src: &Raster) {
    let (w, h) = dst.dimensions();
    if src.width == 0 || src.height == 0 {
        return;
    }
    let same = src.width == w && src.height == h;
    for (x, y, px) in dst.enumerate_pixels_mut() {
        let (sx, sy) = if same {
            (x, y)
        } else {
            (
                u32::try_from(u64::from(x) * u64::from(src.width) / u64::from(w.max(1))).unwrap_or(0),
                u32::try_from(u64::from(y) * u64::from(src.height) / u64::from(h.max(1))).unwrap_or(0),
            )
        };
        let at = (sy as usize * src.width as usize + sx as usize) * 4;
        let Some(s) = src.pixels.get(at..at + 4) else { continue };
        let keep = 255 - u32::from(s[3]);
        for (d, s) in px.0[..3].iter_mut().zip(s) {
            let sum = u32::from(*s) + u32::from(*d) * keep / 255;
            *d = u8::try_from(sum.min(255)).unwrap_or(u8::MAX);
        }
    }
}
