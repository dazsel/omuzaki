//! What the visual evidence sees of the face: the brows over the skin (neutral, raised and lowered, and the Brows layer
//! alone, which hair composites over the bangs), and the mouth (the switch from the closed line to the open shape, and
//! the mouth cases that need a picture). Each case of `evidence.toml` runs one test here with
//! `OMUZAKI_FACE_EVIDENCE_DIR` set; in ordinary test runs they return at once and write nothing.

// The helpers are not `#[test]` bodies, so the test-only allowance does not reach them; a panic is a clear failure here.
// The mouth crop is a few hundred pixels, far inside what the casts hold.
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

/// Paints every layer for `params` with the mock profile, rasterises `layer` at the base density and composites it over
/// `SKIN`.
fn picture(params: &PaintParams, layer: FaceLayerId) -> image::RgbaImage {
    picture_of(&mock_profile(), params, layer)
}

/// [`picture`] for a profile of the caller's.
fn picture_of(profile: &FaceProfile, params: &PaintParams, layer: FaceLayerId) -> image::RgbaImage {
    let mut painter = RealPainter::new(mock_projection(), DEFAULT_PX_PER_M);
    let input = FaceInput { profile, params, time: Seconds(0.0), view: FaceView::default() };
    let mut drawing = FaceDrawing::new();
    painter.paint(&input, &mut drawing).unwrap();
    let spec = *painter.layers().iter().find(|s| s.id == layer).unwrap();
    let mut raster = Raster::new(1, 1);
    SkiaRasterizer::new().rasterize(&spec, &drawing[layer], spec.px_per_m, &mut raster).unwrap();
    over_skin(&raster)
}

/// The Base layer over the skin with the Overlay layer (in front of the hair) over that: the face as it reads.
fn picture_front(profile: &FaceProfile, params: &PaintParams) -> image::RgbaImage {
    let mut painter = RealPainter::new(mock_projection(), DEFAULT_PX_PER_M);
    let input = FaceInput { profile, params, time: Seconds(0.0), view: FaceView::default() };
    let mut drawing = FaceDrawing::new();
    painter.paint(&input, &mut drawing).unwrap();
    let mut out: Option<image::RgbaImage> = None;
    for layer in [FaceLayerId::Base, FaceLayerId::Overlay] {
        let spec = *painter.layers().iter().find(|s| s.id == layer).unwrap();
        let mut raster = Raster::new(1, 1);
        SkiaRasterizer::new().rasterize(&spec, &drawing[layer], spec.px_per_m, &mut raster).unwrap();
        out = Some(match out {
            None => over_skin(&raster),
            Some(mut below) => {
                // Premultiplied source over what is there: `src + dst * (1 - alpha)`.
                for (dst, src) in below.pixels_mut().zip(raster.pixels.as_chunks::<4>().0) {
                    let keep = 255 - u32::from(src[3]);
                    for (d, s) in dst.0[..3].iter_mut().zip(src) {
                        let sum = u32::from(*s) + u32::from(*d) * keep / 255;
                        *d = u8::try_from(sum.min(255)).unwrap_or(u8::MAX);
                    }
                }
                below
            }
        });
    }
    out.unwrap()
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
    let picture = picture_of(profile, &params, FaceLayerId::Base);
    let rect = mock_projection().rect;
    let at = |metres: f32| (metres * DEFAULT_PX_PER_M).round().max(0.0) as u32;
    let (x, y) = (at(-MOUTH_HALF[0] - rect.x0), at(rect.y1 - narrow(profile.layout.mouth_y) - MOUTH_HALF[1]));
    image::imageops::crop_imm(&picture, x, y, at(2.0 * MOUTH_HALF[0]), at(2.0 * MOUTH_HALF[1])).to_image()
}

/// Half the width and height of an eye picture of both eyes, and of one eye, in metres.
const EYES_HALF: [f32; 2] = [0.17, 0.06];
const EYE_HALF: [f32; 2] = [0.06, 0.06];

/// The face with `edit` applied to its paint parameters, cropped to `half` round `centre` (face-plane metres).
fn crop_picture(
    profile: &FaceProfile,
    edit: impl FnOnce(&mut PaintParams),
    centre: [f32; 2],
    half: [f32; 2],
) -> image::RgbaImage {
    let mut params = profile.mapping.apply(&ChannelBuf::NEUTRAL);
    edit(&mut params);
    let picture = picture_front(profile, &params);
    let rect = mock_projection().rect;
    let at = |metres: f32| (metres * DEFAULT_PX_PER_M).round().max(0.0) as u32;
    let (x, y) = (at(centre[0] - half[0] - rect.x0), at(rect.y1 - centre[1] - half[1]));
    image::imageops::crop_imm(&picture, x, y, at(2.0 * half[0]), at(2.0 * half[1])).to_image()
}

/// Both eyes, from the face with `edit` applied.
fn eyes_picture(profile: &FaceProfile, edit: impl FnOnce(&mut PaintParams)) -> image::RgbaImage {
    crop_picture(profile, edit, [0.0, narrow(profile.layout.eye_y)], EYES_HALF)
}

/// The character's left eye alone, for strips.
fn eye_picture(profile: &FaceProfile, edit: impl FnOnce(&mut PaintParams)) -> image::RgbaImage {
    let centre = [narrow(profile.layout.eye_spacing), narrow(profile.layout.eye_y)];
    crop_picture(profile, edit, centre, EYE_HALF)
}

/// `images` side by side, left to right.
fn strip(images: &[image::RgbaImage]) -> image::RgbaImage {
    let (width, height) = (images.iter().map(image::RgbaImage::width).sum(), images[0].height());
    let mut out = image::RgbaImage::new(width, height);
    let mut x = 0;
    for image in images {
        image::imageops::replace(&mut out, image, i64::from(x), 0);
        x += image.width();
    }
    out
}

/// The mock profile in the prototype's eye: the full cat-eye flick, so a lash reads as designed.
fn eyes_profile() -> FaceProfile {
    let mut profile = mock_profile();
    profile.eyes.lash.flick = 1.0;
    profile.eyes.lash.thickness = 0.0076;
    profile
}

#[test]
fn render_the_eyes_when_asked() {
    let Some(dir) = evidence_dir() else {
        return;
    };
    let profile = eyes_profile();
    let save = |name: &str, image: image::RgbaImage| image.save(dir.join(name)).unwrap();
    // The lash line with its flick, the lower lid, the crease and the inner tick, on an open eye.
    save("eyes-open.png", eyes_picture(&profile, |_| {}));
    // Angry, sad and wide: the lash and crease follow the lid.
    save("eyes-angry.png", eyes_picture(&profile, |p| p.eye_tilt = 1.0));
    save("eyes-sad.png", eyes_picture(&profile, |p| p.eye_tilt = -1.0));
    save("eyes-wide.png", eyes_picture(&profile, |p| (p.left_eye.open, p.right_eye.open) = (1.9, 1.9)));
    // A blink: `close` from 0 to 1 in 8 steps, the character's left eye (the last frames are the relaxed lash curve).
    let blink: Vec<_> = (0..8_u8)
        .map(|i| {
            let close = f32::from(i) / 7.0;
            eye_picture(&profile, |p| p.left_eye.close = close)
        })
        .collect();
    save("eyes-blink-sweep.png", strip(&blink));
}

#[test]
fn render_the_closed_and_smiling_eyes_when_asked() {
    let Some(dir) = evidence_dir() else {
        return;
    };
    let mut profile = eyes_profile();
    profile.eyes.lower_lid.cheek_line = true;
    let save = |name: &str, image: image::RgbaImage| image.save(dir.join(name)).unwrap();
    // A wink (the left eye closes as the happy arc, the right stays open) and the happy `^^`.
    save("eyes-wink.png", eyes_picture(&profile, |p| p.left_eye.arc = 1.0));
    save("eyes-happy.png", eyes_picture(&profile, |p| (p.left_eye.arc, p.right_eye.arc) = (1.0, 1.0)));
    // The arc part-way (0.5 to 0.9): the eye is still there, closing into the `^`.
    save("eyes-arc-sweep.png", strip(&[0.5, 0.7, 0.9].map(|arc| eye_picture(&profile, |p| p.left_eye.arc = arc))));
    save("eyes-blink-closed.png", eyes_picture(&profile, |p| (p.left_eye.close, p.right_eye.close) = (1.0, 1.0)));
    // Smiling lids: the cheek raises the lower lid, 0 to 0.6 in four steps; the line and its rim fade in from 0.15.
    let smile: Vec<_> = [0.0, 0.2, 0.35, 0.6].map(|lift| eye_picture(&profile, |p| p.left_eye.lower_lid = lift)).into();
    save("eyes-smiling-lids.png", strip(&smile));
}

/// The directory a mouth case writes its pictures to, if it was asked for.
fn evidence_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("OMUZAKI_FACE_EVIDENCE_DIR").map(std::path::PathBuf::from)
}

/// The mock profile with the lips on, so the rim shows.
fn lips_profile() -> FaceProfile {
    let mut profile = mock_profile();
    profile.mouth.lips.strength = 1.0;
    profile
}

#[test]
fn render_the_mouth_switch_when_asked() {
    let Some(dir) = evidence_dir() else {
        return;
    };
    // Either side of the switch at an opening of 0.06, and the openings the rim fades in over (0.06 to 0.2).
    let profile = lips_profile();
    for (name, open) in [("0.04", 0.04), ("0.06", 0.06), ("0.08", 0.08), ("0.12", 0.12), ("0.22", 0.22)] {
        mouth_picture(&profile, |m| m.open = open).save(dir.join(format!("mouth-switch-{name}.png"))).unwrap();
    }
}

#[test]
fn render_the_smiling_mouth_switch_when_asked() {
    let Some(dir) = evidence_dir() else {
        return;
    };
    let profile = lips_profile();
    // Both corners lifted, either side of the switch: the closed line and the open lens that replaces it.
    for open in ["0.0599", "0.0601"] {
        let value: f32 = open.parse().unwrap();
        mouth_picture(&profile, |m| (m.open, m.smile_left, m.smile_right) = (value, 1.0, 1.0))
            .save(dir.join(format!("mouth-switch-smiling-{open}.png")))
            .unwrap();
    }
}

#[test]
fn render_the_mouth_cases_when_asked() {
    let Some(dir) = evidence_dir() else {
        return;
    };
    let profile = lips_profile();
    let save = |name: &str, image: image::RgbaImage| image.save(dir.join(name)).unwrap();
    save("mouth-closed-smile-fang.png", mouth_picture(&profile, |m| (m.smile_left, m.fangs) = (0.8, 1.0)));
    save("mouth-kiss.png", mouth_picture(&profile, |m| (m.pucker, m.open) = (0.9, 0.05)));
    // Clenched teeth (the split line), with the lower teeth either side of two thresholds: `LOWER_MIN` (0.01), where
    // the lower row and its split line first show, and `CLENCHED_ABOVE` (0.5), where the upper band deepens.
    let clenched = |lower: f32| {
        mouth_picture(&profile, move |m| {
            (m.open, m.smile_left, m.smile_right, m.teeth_upper, m.teeth_lower) = (0.5, 0.6, 0.6, 1.0, lower);
        })
    };
    save("mouth-split-line-0.0100.png", clenched(0.01));
    save("mouth-split-line-0.0101.png", clenched(0.0101));
    save("mouth-split-line-at-threshold.png", clenched(0.5));
    save("mouth-split-line-above-threshold.png", clenched(0.51));
    // A narrow mouth with the fangs at its very corners: they narrow instead of hanging past the corner.
    let mut narrow_mouth = profile.clone();
    narrow_mouth.mouth.fang.position = 1.0;
    save("mouth-narrow-fangs.png", mouth_picture(&narrow_mouth, |m| (m.wide, m.open, m.fangs) = (0.3, 0.5, 1.0)));
}

/// How one effect's channel is written into the paint parameters.
type Effect = fn(&mut PaintParams, f32);

#[test]
fn render_the_eye_effects_when_asked() {
    let Some(dir) = evidence_dir() else {
        return;
    };
    let profile = eyes_profile();
    let save = |name: &str, image: image::RgbaImage| image.save(dir.join(name)).unwrap();
    // Each effect at 0, 0.5 and 1, on the character's left eye: a strip of three.
    let effects: [(&str, Effect); 8] = [
        ("heart", |p, v| p.effects.heart = v),
        ("star", |p, v| p.effects.star = v),
        ("gloom", |p, v| p.effects.gloom = v),
        ("shock", |p, v| p.effects.shock = v),
        ("squeeze", |p, v| p.effects.squeeze = v),
        ("spiral", |p, v| p.effects.spiral = v),
        ("sparkle", |p, v| p.effects.sparkle = v),
        ("tears", |p, v| p.effects.tears = v),
    ];
    for (name, apply) in effects {
        let frames: Vec<_> = [0.0, 0.5, 1.0].map(|v| eye_picture(&profile, |p| apply(p, v))).into();
        save(&format!("eye-effect-{name}.png"), strip(&frames));
    }
    // The Overlay layer: the sweat drop at the temple and the gloom's band and hatch over the forehead, 0, 0.5 and 1.
    let upper = |edit: Effect| {
        let frames: Vec<_> = [0.0, 0.5, 1.0]
            .map(|v| crop_picture(&profile, |p| edit(p, v), [0.06, narrow(profile.layout.eye_y) + 0.05], [0.14, 0.08]))
            .into();
        strip(&frames)
    };
    save("overlay-sweat.png", upper(|p, v| p.sweat = v));
    save("overlay-gloom.png", upper(|p, v| p.effects.gloom = v));
}

#[test]
fn render_the_kiss_to_open_sweep_when_asked() {
    let Some(dir) = evidence_dir() else {
        return;
    };
    let mut profile = mock_profile();
    profile.mouth.lips.strength = 1.0;
    // Pucker 1, the opening from 0.20 (a plain kiss) to 0.26 (an open mouth), one step of 0.01 at a time: the slit fades
    // out and the lens fades in over 0.21 to 0.25.
    let frames: Vec<_> = (20..=26_u8)
        .map(|i| {
            let open = f32::from(i) / 100.0;
            mouth_picture(&profile, |m| (m.pucker, m.open) = (1.0, open))
        })
        .collect();
    strip(&frames).save(dir.join("mouth-kiss-to-open.png")).unwrap();
}

#[test]
fn render_the_pout_and_the_brows_when_asked() {
    let Some(dir) = evidence_dir() else {
        return;
    };
    let profile = eyes_profile();
    let save = |name: &str, image: image::RgbaImage| image.save(dir.join(name)).unwrap();
    // A pout: the cheeks puffing (`cheek_puff` 0, 0.5, 1) with the lips pushed forward and the brows coming down.
    let face = |puff: f32| {
        crop_picture(
            &profile,
            |p| {
                p.cheek_puff = puff;
                p.blush = 0.35 * puff;
                p.mouth.pucker = 0.5 * puff;
                p.left_brow.height = -0.25 * puff;
                p.right_brow.height = -0.25 * puff;
            },
            [0.0, narrow(profile.layout.blush_y)],
            [0.17, 0.09],
        )
    };
    save("pout-cheek-puff.png", strip(&[face(0.0), face(0.5), face(1.0)]));
    // The brows at height -1, 0 and +1 (both brows): the inner end leads and the arch flattens as the brow comes down.
    let brows = |height: f32| {
        let centre = [0.0, narrow(profile.layout.eye_y + profile.layout.brow_gap)];
        crop_picture(
            &profile,
            |p| {
                p.left_brow.height = height;
                p.right_brow.height = height;
            },
            centre,
            [0.15, 0.06],
        )
    };
    save("brows-height.png", strip(&[brows(-1.0), brows(0.0), brows(1.0)]));
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
