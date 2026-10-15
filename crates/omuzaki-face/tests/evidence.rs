//! The brows as the visual evidence sees them: the neutral, raised and lowered brows over the skin, and the Brows layer
//! alone, which hair composites over the bangs. `evidence.toml`'s `brows` case runs this test with
//! `OMUZAKI_FACE_EVIDENCE_DIR` set; in ordinary test runs it returns at once and writes nothing.

// The helpers are not `#[test]` bodies, so the test-only allowance does not reach them; a panic is a clear failure here.
#![allow(clippy::unwrap_used)]

use omuzaki_anim_api::ChannelBuf;
use omuzaki_core::time::Seconds;
use omuzaki_face::RealPainter;
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face_api::{
    DEFAULT_PX_PER_M, FaceDrawing, FaceInput, FaceLayerId, FacePainter, FaceRasterizer, FaceView, PaintParams, Raster,
};
use omuzaki_face_mock::{mock_profile, mock_projection};

/// The fixed ground the pictures are composited over: an opaque skin tone, so the brows read as they do on a face.
const SKIN: [u8; 4] = [236, 200, 180, 255];

/// Paints every layer for `params`, rasterises `layer` at the base density and composites it over `SKIN`.
fn picture(params: &PaintParams, layer: FaceLayerId) -> image::RgbaImage {
    let profile = mock_profile();
    let mut painter = RealPainter::new(mock_projection(), DEFAULT_PX_PER_M);
    let input = FaceInput { profile: &profile, params, time: Seconds(0.0), view: FaceView::default() };
    let mut drawing = FaceDrawing::new();
    painter.paint(&input, &mut drawing).unwrap();
    let spec = *painter.layers().iter().find(|s| s.id == layer).unwrap();
    let mut raster = Raster::new(1, 1);
    SkiaRasterizer::new().rasterize(&spec, &drawing[layer], spec.px_per_m, &mut raster).unwrap();
    over_skin(&raster)
}

/// Premultiplied source over the opaque `SKIN`: `src + dst * (1 - alpha)`, channel by channel.
fn over_skin(raster: &Raster) -> image::RgbaImage {
    let mut out = image::RgbaImage::from_pixel(raster.width, raster.height, image::Rgba(SKIN));
    for (dst, src) in out.pixels_mut().zip(raster.pixels.as_chunks::<4>().0) {
        let keep = 255 - u32::from(src[3]);
        for (d, s) in dst.0[..3].iter_mut().zip(src) {
            let sum = u32::from(*s) + u32::from(*d) * keep / 255;
            *d = u8::try_from(sum.min(255)).unwrap_or(u8::MAX);
        }
    }
    out
}

#[test]
fn render_the_brows_when_asked() {
    let Some(dir) = std::env::var_os("OMUZAKI_FACE_EVIDENCE_DIR") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);

    let neutral = mock_profile().mapping.apply(&ChannelBuf::NEUTRAL);
    let mut raised = neutral;
    raised.left_brow.height = 1.0;
    raised.right_brow.height = 1.0;
    let mut lowered = neutral;
    lowered.left_brow.height = -1.0;
    lowered.right_brow.height = -1.0;

    picture(&neutral, FaceLayerId::Base).save(dir.join("brows-neutral.png")).unwrap();
    picture(&raised, FaceLayerId::Base).save(dir.join("brows-raised.png")).unwrap();
    picture(&lowered, FaceLayerId::Base).save(dir.join("brows-lowered.png")).unwrap();
    picture(&neutral, FaceLayerId::Brows).save(dir.join("brows-through-bangs.png")).unwrap();
}
