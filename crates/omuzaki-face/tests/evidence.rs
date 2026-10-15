//! What the visual evidence sees of the face: the brows over the skin (neutral, raised and lowered, and the Brows layer
//! alone, which hair composites over the bangs), and the mouth (the switch from the closed line to the open shape, and
//! the mouth cases that need a picture). Each case of `evidence.toml` runs one test here with
//! `OMUZAKI_FACE_EVIDENCE_DIR` set; in ordinary test runs they return at once and write nothing.

// The helpers are not `#[test]` bodies, so the test-only allowance does not reach them; a panic is a clear failure here.
// The crop is a few hundred pixels, far inside what the casts hold.
#![allow(clippy::unwrap_used, clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]

use omuzaki_anim_api::ChannelBuf;
use omuzaki_core::time::Seconds;
use omuzaki_face::RealPainter;
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face_api::{
    DEFAULT_PX_PER_M, FaceDrawing, FaceInput, FaceLayerId, FacePainter, FaceProfile, FaceRasterizer, FaceView,
    MouthParams, PaintParams, Raster, narrow,
};
use omuzaki_face_mock::{mock_profile, mock_projection};

/// The fixed ground the pictures are composited over: an opaque skin tone, so the brows read as they do on a face.
const SKIN: [u8; 4] = [236, 200, 180, 255];

/// Half the width and height of a mouth picture, in metres: the widest mouth of these cases with room round it.
const MOUTH_HALF: [f32; 2] = [0.05, 0.03];

/// Paints every layer for `params`, rasterises `layer` at the base density and composites it over `SKIN`.
fn picture(profile: &FaceProfile, params: &PaintParams, layer: FaceLayerId) -> image::RgbaImage {
    let mut painter = RealPainter::new(mock_projection(), DEFAULT_PX_PER_M);
    let input = FaceInput { profile, params, time: Seconds(0.0), view: FaceView::default() };
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

/// The mouth over the skin: the neutral face with `edit` applied to the mouth (a flat mouth, no smile), cropped round
/// the mouth.
fn mouth_picture(profile: &FaceProfile, edit: impl FnOnce(&mut MouthParams)) -> image::RgbaImage {
    let mut params = profile.mapping.apply(&ChannelBuf::NEUTRAL);
    params.mouth = MouthParams { smile_left: 0.0, smile_right: 0.0, ..params.mouth };
    edit(&mut params.mouth);
    let picture = picture(profile, &params, FaceLayerId::Base);
    let rect = mock_projection().rect;
    let at = |metres: f32| (metres * DEFAULT_PX_PER_M).round().max(0.0) as u32;
    let (x, y) = (at(-MOUTH_HALF[0] - rect.x0), at(rect.y1 - narrow(profile.layout.mouth_y) - MOUTH_HALF[1]));
    image::imageops::crop_imm(&picture, x, y, at(2.0 * MOUTH_HALF[0]), at(2.0 * MOUTH_HALF[1])).to_image()
}

/// The directory a case writes its pictures to, if it was asked for.
fn evidence_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("OMUZAKI_FACE_EVIDENCE_DIR").map(std::path::PathBuf::from)
}

#[test]
fn render_the_mouth_switch_when_asked() {
    let Some(dir) = evidence_dir() else {
        return;
    };
    // Either side of the switch at an opening of 0.06, and the openings the rim fades in over (0.06 to 0.2).
    let mut profile = mock_profile();
    profile.mouth.lips.strength = 1.0;
    for (name, open) in [("0.04", 0.04), ("0.06", 0.06), ("0.08", 0.08), ("0.12", 0.12), ("0.22", 0.22)] {
        mouth_picture(&profile, |m| m.open = open).save(dir.join(format!("mouth-switch-{name}.png"))).unwrap();
    }
}

#[test]
fn render_the_mouth_cases_when_asked() {
    let Some(dir) = evidence_dir() else {
        return;
    };
    let mut profile = mock_profile();
    profile.mouth.lips.strength = 1.0;
    let save = |name: &str, image: image::RgbaImage| image.save(dir.join(name)).unwrap();
    save("mouth-closed-smile-fang.png", mouth_picture(&profile, |m| (m.smile_left, m.fangs) = (0.8, 1.0)));
    save("mouth-kiss.png", mouth_picture(&profile, |m| (m.pucker, m.open) = (0.9, 0.05)));
    // Clenched teeth: the split line, with the lower teeth either side of the strength at which the band deepens.
    let clenched = |lower: f32| {
        mouth_picture(&profile, move |m| {
            (m.open, m.smile_left, m.smile_right, m.teeth_upper, m.teeth_lower) = (0.5, 0.6, 0.6, 1.0, lower);
        })
    };
    save("mouth-split-line-at-threshold.png", clenched(0.5));
    save("mouth-split-line-above-threshold.png", clenched(0.51));
    // A narrow mouth with the fangs at its very corners: they narrow instead of hanging past the corner.
    let mut narrow_mouth = profile.clone();
    narrow_mouth.mouth.fang.position = 1.0;
    save("mouth-narrow-fangs.png", mouth_picture(&narrow_mouth, |m| (m.wide, m.open, m.fangs) = (0.3, 0.5, 1.0)));
}

#[test]
fn render_the_brows_when_asked() {
    let Some(dir) = evidence_dir() else {
        return;
    };

    let neutral = mock_profile().mapping.apply(&ChannelBuf::NEUTRAL);
    let mut raised = neutral;
    raised.left_brow.height = 1.0;
    raised.right_brow.height = 1.0;
    let mut lowered = neutral;
    lowered.left_brow.height = -1.0;
    lowered.right_brow.height = -1.0;

    let profile = mock_profile();
    picture(&profile, &neutral, FaceLayerId::Base).save(dir.join("brows-neutral.png")).unwrap();
    picture(&profile, &raised, FaceLayerId::Base).save(dir.join("brows-raised.png")).unwrap();
    picture(&profile, &lowered, FaceLayerId::Base).save(dir.join("brows-lowered.png")).unwrap();
    picture(&profile, &neutral, FaceLayerId::Brows).save(dir.join("brows-through-bangs.png")).unwrap();
}
