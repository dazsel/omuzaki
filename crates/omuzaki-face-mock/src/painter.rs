//! The mock painter: a flat geometric face driven by the paint parameters.

use omuzaki_anim_api::Channel;
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec2;
use omuzaki_face_api::{
    Cap, Cmd, ColourExt, ColourRef, DrawList, FaceDrawing, FaceError, FaceInput, FaceLayerId, FaceLayerSpec,
    FacePainter, FaceProfile, FaceProjection, FaceRect, Join, Paint, PaintParams, Stops, narrow,
};

/// Raster density the mock suggests for the base layer: 2 000 px/m puts a 0.4 m wide face at 800 px.
const PX_PER_M: f32 = 2000.0;

/// A face rectangle that fits the mock profile's layout.
pub fn mock_projection() -> FaceProjection {
    FaceProjection { rect: FaceRect { x0: -0.2, x1: 0.2, y0: -0.26, y1: 0.03 }, front_fade: [0.0, 0.07] }
}

/// A profile with distinct, easy-to-spot colours so test images show which part drew what.
pub fn mock_profile() -> FaceProfile {
    let mut p = FaceProfile::default();
    p.eyes.iris.top = ColourRef::rgb(0x23, 0x55, 0x8c);
    p.eyes.iris.low = ColourRef::rgb(0x8f, 0xc4, 0xe8);
    p.mouth.interior.top = ColourRef::rgb(0x70, 0x20, 0x30);
    // The mock's mouth is drawn at close-up size so it reads in a contact sheet.
    p.mouth.closed_scale = 1.7;
    p.mouth.open_scale = 2.6;
    p
}

/// A painter that draws the face with a few ellipses and lines. Pure and deterministic.
#[derive(Debug)]
pub struct MockPainter {
    layers: [FaceLayerSpec; 2],
}

impl MockPainter {
    /// The channels the mock reacts to (through the default `FaceMapping`); the rest are accepted and ignored.
    pub const SUPPORTED: [Channel; 24] = [
        Channel::EYE_BLINK_LEFT,
        Channel::EYE_BLINK_RIGHT,
        Channel::EYE_ARC_LEFT,
        Channel::EYE_ARC_RIGHT,
        Channel::EYE_WIDE_LEFT,
        Channel::EYE_WIDE_RIGHT,
        Channel::EYE_SQUINT_LEFT,
        Channel::EYE_SQUINT_RIGHT,
        Channel::EYE_LOOK_OUT_LEFT,
        Channel::EYE_LOOK_UP_RIGHT,
        Channel::BROW_INNER_UP,
        Channel::BROW_OUTER_UP_LEFT,
        Channel::BROW_DOWN_RIGHT,
        Channel::JAW_OPEN,
        Channel::JAW_LEFT,
        Channel::MOUTH_STRETCH_LEFT,
        Channel::MOUTH_SMILE_LEFT,
        Channel::MOUTH_SMILE_RIGHT,
        Channel::MOUTH_FROWN_LEFT,
        Channel::MOUTH_LEFT,
        Channel::MOUTH_UPPER_UP_RIGHT,
        Channel::BLUSH,
        Channel::CHEEK_PUFF,
        Channel::TEARS,
    ];

    /// A painter for a face projected onto `projection`.
    pub fn new(projection: FaceProjection) -> Self {
        let spec = |id| FaceLayerSpec::for_layer(id, &projection, PX_PER_M);
        Self { layers: [spec(FaceLayerId::Base), spec(FaceLayerId::Brows)] }
    }
}

impl Default for MockPainter {
    fn default() -> Self {
        Self::new(mock_projection())
    }
}

fn solid(c: Colour) -> Paint {
    Paint::Solid(c)
}

/// Painters are handed slot-free profiles (`FaceProfile::resolve_colours`), so a slot here is a stage bug. The mock
/// paints one mid-grey, so the bug shows in the image.
fn fixed(c: &ColourRef) -> Colour {
    match c {
        ColourRef::Fixed(c) => *c,
        ColourRef::Slot(_) => Colour::rgb(128, 128, 128),
    }
}

fn finite(p: &PaintParams) -> bool {
    let eyes =
        [p.left_eye, p.right_eye].iter().all(|e| [e.open, e.close, e.arc, e.lower_lid].iter().all(|v| v.is_finite()));
    let m = &p.mouth;
    eyes && p.gaze.is_finite()
        && [m.open, m.wide, m.smile_left, m.smile_right, m.shift, p.blush, p.left_brow.height, p.right_brow.height]
            .iter()
            .all(|v| v.is_finite())
}

impl FacePainter for MockPainter {
    fn layers(&self) -> &[FaceLayerSpec] {
        &self.layers
    }

    fn paint(&mut self, input: &FaceInput<'_>, out: &mut FaceDrawing) -> Result<(), FaceError> {
        let (profile, params) = (input.profile, input.params);
        if !finite(params) {
            return Err(FaceError::Invalid("a paint parameter is not a finite number".to_owned()));
        }
        out.clear();
        // Cheeks first, so features draw over them.
        let base = &mut out[FaceLayerId::Base];
        let blush =
            fixed(&profile.cheeks.colour).faded((params.blush * narrow(profile.cheeks.strength)).clamp(0.0, 1.0));
        for side in [-1.0_f32, 1.0] {
            let centre = Vec2::new(side * narrow(profile.layout.blush_spacing), narrow(profile.layout.blush_y));
            let path = base
                .path()
                .ellipse(centre, narrow(profile.cheeks.radius[0]), narrow(profile.cheeks.radius[1]))
                .finish();
            base.fill(path, solid(blush));
        }
        paint_eye(base, profile, params, 1.0);
        paint_eye(base, profile, params, -1.0);
        paint_mouth(base, profile, params);
        // Brows go on both layers: the skin, and the layer hair composites to draw them through the bangs.
        let brow_colour = fixed(&profile.brows.colour);
        for layer in [FaceLayerId::Base, FaceLayerId::Brows] {
            for sign in [1.0_f32, -1.0] {
                let brow = if sign > 0.0 { params.left_brow } else { params.right_brow };
                let y = narrow(profile.layout.eye_y) + narrow(profile.layout.brow_gap) + 0.012 * brow.height;
                let list = &mut out[layer];
                let path = list
                    .path()
                    .polyline(&[
                        Vec2::new(sign * narrow(profile.brows.inner_x), y - 0.006 * brow.tilt),
                        Vec2::new(
                            sign * (narrow(profile.brows.inner_x) + narrow(profile.brows.outer_x)) * 0.5,
                            y + narrow(profile.brows.arch),
                        ),
                        Vec2::new(sign * narrow(profile.brows.outer_x), y + 0.006 * brow.tilt),
                    ])
                    .finish();
                list.stroke(path, solid(brow_colour), narrow(profile.brows.thickness) * 2.0);
            }
        }
        Ok(())
    }
}

/// One eye; `sign` is +1 for the character's left (+x).
fn paint_eye(list: &mut DrawList, profile: &FaceProfile, params: &PaintParams, sign: f32) {
    let eye = if sign > 0.0 { params.left_eye } else { params.right_eye };
    // An arc closes the lid too: the eye is shut when either a blink or a smile says so.
    let open = (eye.open * (1.0 - eye.close.max(eye.arc))).max(0.0);
    let half_w = narrow(profile.layout.eye_width) * narrow(profile.eyes.scale);
    let centre =
        Vec2::new(sign * narrow(profile.layout.eye_spacing), narrow(profile.layout.eye_y) - narrow(profile.eyes.drop));
    let lash = fixed(&profile.eyes.lash.colour);
    if open < 0.05 {
        // Shut: a lash line, bowed up into a ^ when the lid draws as an arc.
        let bow = if eye.arc >= 0.5 { 0.5 * half_w } else { 0.0 };
        let path = list
            .path()
            .move_to(centre + Vec2::new(-half_w, 0.0))
            .quad_to(centre + Vec2::new(0.0, 2.0 * bow), centre + Vec2::new(half_w, 0.0))
            .finish();
        list.stroke(path, solid(lash), narrow(profile.eyes.lash.thickness));
        return;
    }
    let half_h = half_w * narrow(profile.eyes.aperture.upper_height) * open;
    let opening = list.path().ellipse(centre, half_w, half_h).finish();
    list.fill(opening, solid(fixed(&profile.eyes.sclera.mid)));
    // The iris is clipped to the opening, like a lid closing over it.
    list.push(Cmd::PushClip(opening));
    let iris_style = &profile.eyes.iris;
    let travel =
        Vec2::new(params.gaze.x * narrow(iris_style.gaze_travel[0]), params.gaze.y * narrow(iris_style.gaze_travel[1]));
    let iris_centre = centre + travel;
    let (rx, ry) = (half_w * narrow(iris_style.width), half_w * narrow(iris_style.height));
    let iris = list.path().ellipse(iris_centre, rx, ry).finish();
    let stops = Stops::new(&[(0.0, fixed(&iris_style.top)), (1.0, fixed(&iris_style.low))]);
    list.fill(iris, Paint::Radial { centre: iris_centre, radius: ry, stops });
    let pupil = list.path().ellipse(iris_centre, rx * 0.35 * params.pupil, ry * 0.35 * params.pupil).finish();
    list.fill(pupil, solid(fixed(&iris_style.pupil.colour)));
    list.push(Cmd::PopClip);
    let lid = list.path().ellipse(centre, half_w, half_h).finish();
    list.push(Cmd::Stroke {
        path: lid,
        paint: solid(lash),
        width: narrow(profile.eyes.lash.thickness),
        cap: Cap::Round,
        join: Join::Round,
        opacity: 1.0,
    });
    let tears = params.effects.tears;
    if tears > 0.01 {
        let drop = list.path().ellipse(centre + Vec2::new(0.0, -half_h - 0.01), 0.004, 0.006).finish();
        list.fill(drop, solid(fixed(&profile.eyes.tear).faded(tears)));
    }
}

fn paint_mouth(list: &mut DrawList, profile: &FaceProfile, params: &PaintParams) {
    let style = &profile.mouth;
    let m = &params.mouth;
    let scale = if m.open > 0.06 { narrow(style.open_scale) } else { narrow(style.closed_scale) };
    let half_w = narrow(style.half_width) * m.wide * scale;
    let centre = Vec2::new(0.006 * m.shift, narrow(profile.layout.mouth_y));
    let (left, right) = (m.smile_left * narrow(style.corner_curve), m.smile_right * narrow(style.corner_curve));
    let line = fixed(&style.line_colour);
    if m.open < 0.06 {
        let path = list
            .path()
            .move_to(centre + Vec2::new(-half_w, right))
            .quad_to(centre + Vec2::new(0.0, -0.5 * (left + right)), centre + Vec2::new(half_w, left))
            .finish();
        list.stroke(path, solid(line), narrow(style.line_width));
        return;
    }
    let depth = 0.016 * m.open;
    let mouth = list
        .path()
        .move_to(centre + Vec2::new(-half_w, right))
        .quad_to(centre + Vec2::new(0.0, -0.5 * (left + right)), centre + Vec2::new(half_w, left))
        .quad_to(centre + Vec2::new(0.0, -depth * 2.0), centre + Vec2::new(-half_w, right))
        .close()
        .finish();
    list.fill(mouth, solid(fixed(&style.interior.top)));
    if m.teeth_upper >= 0.5 {
        let reach = half_w * 0.8;
        let corners =
            [Vec2::new(-reach, 0.0), Vec2::new(reach, 0.0), Vec2::new(reach, -0.003), Vec2::new(-reach, -0.003)];
        let teeth = list.path().polyline(&corners.map(|c| centre + c)).close().finish();
        list.fill(teeth, solid(fixed(&style.interior.teeth)));
    }
    list.stroke(mouth, solid(line), narrow(style.line_width));
}

#[cfg(test)]
mod tests {
    use super::*;
    use omuzaki_anim_api::{ChannelBuf, Region};
    use omuzaki_core::time::Seconds;
    use omuzaki_face_api::{FaceMapping, FaceView, Palette, PaletteSlot};
    use std::collections::BTreeMap;

    fn paint_profile(profile: &FaceProfile, p: &PaintParams) -> FaceDrawing {
        let input = FaceInput { profile, params: p, time: Seconds(0.0), view: FaceView::default() };
        let mut out = FaceDrawing::new();
        MockPainter::default().paint(&input, &mut out).unwrap();
        out
    }

    fn paint_params(p: &PaintParams) -> FaceDrawing {
        paint_profile(&mock_profile(), p)
    }

    fn paint(buf: &ChannelBuf) -> FaceDrawing {
        paint_params(&FaceMapping::default().apply(buf))
    }

    fn fingerprint(buf: &ChannelBuf) -> u64 {
        paint(buf)[FaceLayerId::Base].fingerprint()
    }

    /// The colours the base layer paints: solid fills and strokes, and every stop of a gradient.
    fn base_colours(drawing: &FaceDrawing) -> Vec<Colour> {
        let mut colours = Vec::new();
        for cmd in drawing[FaceLayerId::Base].cmds() {
            if let Cmd::Fill { paint, .. } | Cmd::Stroke { paint, .. } = cmd {
                match paint {
                    Paint::Solid(c) => colours.push(*c),
                    Paint::Linear { stops, .. } | Paint::Radial { stops, .. } => {
                        colours.extend(stops.as_slice().iter().map(|stop| stop.colour));
                    }
                }
            }
        }
        colours
    }

    #[test]
    fn a_resolved_palette_slot_paints_its_palette_colour() {
        let mut profile = mock_profile();
        profile.eyes.iris.top = ColourRef::parse("@eyes").unwrap();
        profile.cheeks.colour = ColourRef::parse("@blush").unwrap();
        let palette = Palette(BTreeMap::from([
            (PaletteSlot::new("eyes").unwrap(), Colour::rgb(0x12, 0x34, 0x56)),
            (PaletteSlot::new("blush").unwrap(), Colour::rgb(0xf0, 0x80, 0x90)),
        ]));
        let params = FaceMapping::default().apply(&ChannelBuf::NEUTRAL);
        let resolved = profile.resolve_colours(&palette).unwrap();
        let painted = base_colours(&paint_profile(&resolved, &params));
        assert!(painted.contains(&Colour::rgb(0x12, 0x34, 0x56)), "the iris top paints the eyes colour");
        assert!(painted.iter().any(|c| (c.r, c.g, c.b) == (0xf0, 0x80, 0x90)), "the blush paints the blush colour");
        // Unresolved, the slot paints mid-grey: the marker that a stage forgot to resolve.
        let unresolved = base_colours(&paint_profile(&profile, &params));
        assert!(unresolved.contains(&Colour::rgb(128, 128, 128)));
    }

    #[test]
    fn painting_is_deterministic_and_reuses_the_output() {
        let mut buf = ChannelBuf::NEUTRAL;
        buf.set(Channel::JAW_OPEN, 0.4);
        let (a, b) = (paint(&buf), paint(&buf));
        for id in FaceLayerId::ALL {
            assert_eq!(a[id].fingerprint(), b[id].fingerprint(), "{id:?}");
        }
        let mut again = paint(&buf);
        let (profile, params) = (mock_profile(), FaceMapping::default().apply(&buf));
        let input = FaceInput { profile: &profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
        MockPainter::default().paint(&input, &mut again).unwrap();
        assert_eq!(
            again[FaceLayerId::Base].fingerprint(),
            a[FaceLayerId::Base].fingerprint(),
            "refilling clears first"
        );
    }

    #[test]
    fn every_supported_channel_changes_the_picture() {
        // Baseline: mouth open (so teeth can show), a smile on one side, eyes open.
        let mut base = ChannelBuf::NEUTRAL;
        base.set(Channel::JAW_OPEN, 0.5);
        base.set(Channel::MOUTH_SMILE_RIGHT, 0.3);
        for c in MockPainter::SUPPORTED {
            let mut moved = base;
            moved.set(c, if base.get(c) > 0.5 { 0.0 } else { 1.0 });
            assert_ne!(fingerprint(&base), fingerprint(&moved), "{c} has no visible effect in the mock");
        }
    }

    #[test]
    fn a_smiling_blink_draws_a_bowed_lid_and_a_plain_blink_a_flat_one() {
        let mut flat = ChannelBuf::NEUTRAL;
        flat.set(Channel::EYE_BLINK_LEFT, 1.0);
        let mut arc = ChannelBuf::NEUTRAL;
        arc.set(Channel::EYE_BLINK_LEFT, 1.0);
        arc.set(Channel::EYE_ARC_LEFT, 1.0);
        assert_ne!(fingerprint(&flat), fingerprint(&arc));
        let mut happy = ChannelBuf::NEUTRAL;
        happy.set(Channel::EYE_ARC_LEFT, 1.0);
        assert_eq!(fingerprint(&arc), fingerprint(&happy), "an arc closes the lid by itself (^^ happy eyes)");
    }

    #[test]
    fn the_supported_list_names_only_face_channels() {
        for c in MockPainter::SUPPORTED {
            assert!(
                matches!(c.region(), Region::Brows | Region::Eyes | Region::Gaze | Region::Mouth | Region::Extras),
                "{c}"
            );
            assert!(!omuzaki_face_api::UNMAPPED.contains(&c), "{c}");
        }
    }

    #[test]
    fn brows_go_on_the_base_and_the_brows_layer_and_nothing_else_goes_on_the_brows_layer() {
        let d = paint(&ChannelBuf::NEUTRAL);
        assert_eq!(d[FaceLayerId::Brows].cmds().len(), 2);
        assert!(d[FaceLayerId::Brows].cmds().iter().all(|c| matches!(c, Cmd::Stroke { .. })));
        assert!(d[FaceLayerId::Overlay].is_empty());
        assert!(d[FaceLayerId::Base].cmds().len() > 10);
    }

    #[test]
    fn a_non_finite_parameter_is_an_error() {
        let mut p = FaceMapping::default().rest_params();
        p.gaze.x = f32::NAN;
        let profile = mock_profile();
        let input = FaceInput { profile: &profile, params: &p, time: Seconds(0.0), view: FaceView::default() };
        let e = MockPainter::default().paint(&input, &mut FaceDrawing::new()).unwrap_err();
        assert!(matches!(e, FaceError::Invalid(_)));
    }

    #[test]
    fn layers_cover_the_projection() {
        let p = MockPainter::default();
        let ids: Vec<_> = p.layers().iter().map(|l| l.id).collect();
        assert_eq!(ids, [FaceLayerId::Base, FaceLayerId::Brows]);
        assert!(p.layers().iter().all(|l| l.rect == mock_projection().rect));
        assert_eq!(p.layers()[1].px_per_m * 2.0, p.layers()[0].px_per_m, "brows use half the density");
    }
}
