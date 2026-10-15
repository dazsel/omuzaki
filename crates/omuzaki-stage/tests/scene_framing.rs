//! The framing presets on the four template characters: how much of the frame each span fills at 16:9 and 9:16,
//! where each preset aims, and what yaw, pitch, zoom and shift do to the camera.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "scene/templates.rs"]
mod templates;

use omuzaki_core::glam::{Vec3, Vec4};
use omuzaki_render_api::{Camera, Projection};
use omuzaki_stage::StageError;
use omuzaki_stage::scene::frame::{
    BUST_CROWN_FROM_TOP, BUST_FILL, FACE_EYES_FROM_TOP, FACE_FILL, FULL_FILL, HANDS_FILL, frame,
};
use omuzaki_stage::scene::landmarks::{Bounds, Landmarks};
use omuzaki_stage_api::{CameraDef, Framing};

const ASPECTS: [f32; 2] = [16.0 / 9.0, 9.0 / 16.0];
const PRESETS: [Framing; 4] = [Framing::Face, Framing::Bust, Framing::Full, Framing::Hands];

fn landmarks(id: &str) -> Landmarks {
    Landmarks::measure(&templates::build(id)).unwrap()
}

/// Normalised device coordinates of `p`.
fn project(camera: &Camera, aspect: f32, p: Vec3) -> (f32, f32) {
    let clip = camera.projection_matrix(aspect) * camera.view() * Vec4::from((p, 1.0));
    (clip.x / clip.w, clip.y / clip.w)
}

/// How far down from the top edge `y` (at the box's centre column) sits, as a share of the frame height.
fn from_top(camera: &Camera, aspect: f32, bounds: &Bounds, y: f32) -> f32 {
    let c = bounds.centre();
    (1.0 - project(camera, aspect, Vec3::new(c.x, y, c.z)).1) * 0.5
}

/// The preset's span, box and fill.
fn span(framing: Framing, l: &Landmarks) -> (f32, f32, Bounds, f32) {
    match framing {
        Framing::Face => (l.chin().unwrap(), l.crown().unwrap(), l.head.unwrap(), FACE_FILL),
        Framing::Bust => (l.chest.unwrap(), l.crown().unwrap(), l.bust.unwrap(), BUST_FILL),
        Framing::Full => (l.bottom(), l.top(), l.body, FULL_FILL),
        Framing::Hands => (l.hands.unwrap().min.y, l.hands.unwrap().max.y, l.hands.unwrap(), HANDS_FILL),
        Framing::Free => unreachable!("free has no span"),
    }
}

#[test]
fn presets_fill_their_span() {
    let def = |f| CameraDef::preset(f);
    for id in templates::TEMPLATES {
        let l = landmarks(id);
        for aspect in ASPECTS {
            for framing in PRESETS {
                let camera = frame(framing, &l, &def(framing), aspect).unwrap();
                let (lo, hi, bounds, fill) = span(framing, &l);
                let c = bounds.centre();
                let t = (30.0_f32.to_radians() * 0.5).tan();
                let d_v = (hi - lo) / (2.0 * fill * t);
                let d_w = bounds.width_at(0.0) / (2.0 * fill * aspect * t);
                let (share, what) = if d_v >= d_w {
                    (
                        (project(&camera, aspect, Vec3::new(c.x, hi, c.z)).1
                            - project(&camera, aspect, Vec3::new(c.x, lo, c.z)).1)
                            * 0.5,
                        "height",
                    )
                } else {
                    (
                        (project(&camera, aspect, Vec3::new(bounds.max.x, c.y, c.z)).0
                            - project(&camera, aspect, Vec3::new(bounds.min.x, c.y, c.z)).0)
                            * 0.5,
                        "width",
                    )
                };
                eprintln!("{id} {aspect:.2} {framing:?}: {what} share {share:.3} (fill {fill})");
                assert!((share - fill).abs() <= 0.03 * fill, "{id} {aspect} {framing:?}: {share} vs {fill}");
            }
        }
    }
}

#[test]
fn presets_aim_where_the_table_says() {
    for id in templates::TEMPLATES {
        let l = landmarks(id);
        for aspect in ASPECTS {
            let at = |f: Framing| frame(f, &l, &CameraDef::preset(f), aspect).unwrap();
            let face = from_top(&at(Framing::Face), aspect, &l.head.unwrap(), l.eyes.unwrap());
            assert!((face - FACE_EYES_FROM_TOP).abs() < 0.02, "{id} {aspect}: eyes at {face}");
            let bust = from_top(&at(Framing::Bust), aspect, &l.bust.unwrap(), l.crown().unwrap());
            assert!((bust - BUST_CROWN_FROM_TOP).abs() < 0.02, "{id} {aspect}: crown at {bust}");
            let full = from_top(&at(Framing::Full), aspect, &l.body, f32::midpoint(l.bottom(), l.top()));
            assert!((full - 0.5).abs() < 0.02, "{id} {aspect}: full centre at {full}");
            let hands = l.hands.unwrap();
            let h = from_top(&at(Framing::Hands), aspect, &hands, f32::midpoint(hands.min.y, hands.max.y));
            assert!((h - 0.5).abs() < 0.02, "{id} {aspect}: hands centre at {h}");
        }
    }
}

#[test]
fn the_head_fills_the_same_share_on_every_template() {
    let aspect = 16.0 / 9.0;
    let share = |id: &str| {
        let l = landmarks(id);
        let camera = frame(Framing::Face, &l, &CameraDef::preset(Framing::Face), aspect).unwrap();
        let head = l.head.unwrap();
        let c = head.centre();
        (project(&camera, aspect, Vec3::new(c.x, head.max.y, c.z)).1
            - project(&camera, aspect, Vec3::new(c.x, head.min.y, c.z)).1)
            * 0.5
    };
    let (small, tall) = (share("soft-sd"), share("tall"));
    assert!((small - tall).abs() <= 0.03 * tall, "{small} vs {tall}");
}

#[test]
fn zoom_two_halves_the_distance() {
    let l = landmarks("standard");
    let distance = |zoom: f32| {
        let def = CameraDef { zoom, ..CameraDef::preset(Framing::Full) };
        let camera = frame(Framing::Full, &l, &def, 16.0 / 9.0).unwrap();
        (camera.position()
            - l.body.centre() * Vec3::new(1.0, 0.0, 1.0)
            - Vec3::new(0.0, f32::midpoint(l.bottom(), l.top()), 0.0))
        .length()
    };
    assert!((distance(2.0) * 2.0 - distance(1.0)).abs() < 1e-4);
}

#[test]
fn yaw_90_looks_from_the_side() {
    let l = landmarks("standard");
    let front = frame(Framing::Full, &l, &CameraDef::preset(Framing::Full), 1.0).unwrap();
    let def = CameraDef { yaw: 90.0, ..CameraDef::preset(Framing::Full) };
    let side = frame(Framing::Full, &l, &def, 1.0).unwrap();
    let target = Vec3::new(l.body.centre().x, f32::midpoint(l.bottom(), l.top()), l.body.centre().z);
    let offset = side.position() - target;
    assert!(offset.x > 0.0 && offset.y.abs() < 1e-4 && offset.z.abs() < 1e-4, "{offset}");
    // The height binds at yaw 0, and the body is narrower than tall, so the distance is the same from the side.
    assert!((offset.length() - (front.position() - target).length()).abs() < 1e-3);
}

#[test]
fn pitch_raises_the_camera() {
    let l = landmarks("standard");
    let level = frame(Framing::Full, &l, &CameraDef::preset(Framing::Full), 1.0).unwrap();
    let def = CameraDef { pitch: 30.0, ..CameraDef::preset(Framing::Full) };
    let above = frame(Framing::Full, &l, &def, 1.0).unwrap();
    assert!(above.position().y > level.position().y);
}

#[test]
fn shift_moves_the_subject_right() {
    let l = landmarks("standard");
    let c = l.body.centre();
    let x = |shift: [f32; 2]| {
        let def = CameraDef { shift, ..CameraDef::preset(Framing::Full) };
        let camera = frame(Framing::Full, &l, &def, 16.0 / 9.0).unwrap();
        project(&camera, 16.0 / 9.0, c)
    };
    let (plain, right, up) = (x([0.0, 0.0]), x([0.1, 0.0]), x([0.0, 0.1]));
    assert!((right.0 - plain.0 - 0.2).abs() < 1e-3, "a shift of 0.1 of the width is 0.2 in NDC");
    assert!((up.1 - plain.1 - 0.2).abs() < 1e-3);
}

#[test]
fn the_camera_looks_through_a_positive_depth_range() {
    let l = landmarks("tall");
    let camera = frame(Framing::Face, &l, &CameraDef::preset(Framing::Face), 1.0).unwrap();
    let Projection::Perspective { near, far, .. } = camera.projection else { panic!("perspective") };
    assert!(near > 0.0 && far > near);
}

#[test]
fn free_and_missing_landmarks_are_framing_errors() {
    let l = landmarks("standard");
    let is_framing = |r: Result<Camera, StageError>| matches!(r, Err(StageError::Framing { .. }));
    let def = CameraDef::preset(Framing::Full);
    assert!(is_framing(frame(Framing::Free, &l, &def, 1.0)));

    // `Landmarks` is `non_exhaustive` but its fields are public: take landmarks away from a measured character.
    let without = |edit: fn(&mut Landmarks)| {
        let mut l = l;
        edit(&mut l);
        l
    };
    let headless = without(|l| l.head = None);
    assert!(is_framing(frame(Framing::Face, &headless, &def, 1.0)), "no head");
    assert!(is_framing(frame(Framing::Bust, &headless, &def, 1.0)), "no head to give the crown");
    assert!(is_framing(frame(Framing::Face, &without(|l| l.eyes = None), &def, 1.0)), "no eyes");
    assert!(is_framing(frame(Framing::Bust, &without(|l| l.chest = None), &def, 1.0)), "no chest");
    assert!(is_framing(frame(Framing::Bust, &without(|l| l.bust = None), &def, 1.0)), "no bust");
    assert!(is_framing(frame(Framing::Hands, &without(|l| l.hands = None), &def, 1.0)), "no hands");

    for bad in [
        CameraDef { fov: 0.0, ..def.clone() },
        CameraDef { fov: 180.0, ..def.clone() },
        CameraDef { zoom: 0.0, ..def.clone() },
        CameraDef { pitch: 90.0, ..def.clone() },
        CameraDef { yaw: f32::NAN, ..def.clone() },
    ] {
        assert!(is_framing(frame(Framing::Full, &l, &bad, 1.0)), "{bad:?}");
    }
    for aspect in [0.0, -1.0, f32::NAN] {
        assert!(is_framing(frame(Framing::Full, &l, &def, aspect)), "aspect {aspect}");
    }
}

#[test]
fn degenerate_landmarks_are_framing_errors() {
    let l = landmarks("standard");
    let is_framing =
        |l: &Landmarks, f: Framing| matches!(frame(f, l, &CameraDef::preset(f), 1.0), Err(StageError::Framing { .. }));
    let with = |edit: fn(&mut Landmarks)| {
        let mut l = l;
        edit(&mut l);
        l
    };
    assert!(is_framing(&with(|l| l.eyes = Some(f32::NAN)), Framing::Face), "a NaN eye line");
    assert!(is_framing(&with(|l| l.body.max.y = l.body.min.y - 1.0), Framing::Full), "an inverted box");
    assert!(is_framing(&with(|l| l.body.max.x = f32::INFINITY), Framing::Full), "an infinite box");
    assert!(is_framing(&with(|l| l.height_m = 0.0), Framing::Full), "no height for the far plane");
    assert!(is_framing(&with(|l| l.height_m = -1.0), Framing::Full), "a negative height");
}
