//! The iris, pupil, ring, lid shadow, catchlights and shimmer through the public painter: what the eyes section
//! puts inside each eye's aperture clip, that it stays inside it at the extremes of the gaze, that the shimmer is a pure
//! function of the clock, and that painting allocates nothing once the lists are warm.
//!
//! The comparison with the prototype's pixels is `omuzaki-test-face`'s `face_parity_eye_iris.rs`.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used, clippy::cast_possible_truncation, clippy::cast_precision_loss)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec2;
use omuzaki_core::time::Seconds;
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face::{RealPainter, Section};
use omuzaki_face_api::{
    CatchlightShape, Cmd, ColourRef, DEFAULT_PX_PER_M, DrawList, EyeParams, FaceDrawing, FaceInput, FaceLayerId,
    FaceMapping, FacePainter, FaceProfile, FaceRasterizer, FaceView, Paint, PaintParams, PathEl, PathRef, PupilShape,
    Raster,
};
use omuzaki_face_mock::mock_projection;

struct Counting;

thread_local! {
    // `const`-initialised without a destructor, so touching them never allocates (see `hot_loop.rs`).
    static MEASURING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// The most commands one eye may take with everything on (the budget, kept).
const COMMANDS_PER_EYE: usize = 600;

fn bounds(els: &[PathEl]) -> (Vec2, Vec2) {
    let points = els.iter().flat_map(|el| match *el {
        PathEl::Move(p) | PathEl::Line(p) => vec![p],
        PathEl::Quad(c, p) => vec![c, p],
        PathEl::Cubic(a, b, p) => vec![a, b, p],
        PathEl::Close => vec![],
    });
    points.fold((Vec2::splat(f32::MAX), Vec2::splat(f32::MIN)), |(lo, hi), p| (lo.min(p), hi.max(p)))
}

const WHITE: Paint = Paint::Solid(Colour::rgb(255, 255, 255));
/// A pupil colour no other piece uses, so the pupil's fill can be found.
const PUPIL: Colour = Colour::rgba(1, 2, 3, 200);

fn open() -> EyeParams {
    EyeParams { open: 1.0, close: 0.0, arc: 0.0, lower_lid: 0.0 }
}

fn base() -> PaintParams {
    PaintParams { left_eye: open(), right_eye: open(), ..FaceMapping::default().rest_params() }
}

fn profile() -> FaceProfile {
    let mut profile = FaceProfile::default();
    profile.eyes.iris.pupil.colour = ColourRef::Fixed(PUPIL);
    profile
}

fn paint_with(profile: &FaceProfile, params: &PaintParams, time: f64, view: FaceView) -> FaceDrawing {
    let mut drawing = FaceDrawing::new();
    let mut painter = RealPainter::new(mock_projection(), DEFAULT_PX_PER_M);
    let input = FaceInput { profile, params, time: Seconds(time), view };
    painter.paint_section(Section::Eyes, &input, &mut drawing).unwrap();
    drawing
}

fn paint(profile: &FaceProfile, params: &PaintParams) -> FaceDrawing {
    paint_with(profile, params, 0.0, FaceView::default())
}

/// What the painter draws inside each open eye's aperture clip (outside any nested clip too), in order, as
/// `(command, the path's elements)`; one entry per open eye, the character's left first.
fn inside(list: &DrawList) -> Vec<Vec<(Cmd, Vec<PathEl>)>> {
    let (mut depth, mut eyes) = (0_usize, Vec::new());
    for cmd in list.cmds() {
        match *cmd {
            Cmd::PushClip(_) => {
                if depth == 0 {
                    eyes.push(Vec::new());
                } else {
                    eyes.last_mut().unwrap().push((*cmd, Vec::new()));
                }
                depth += 1;
            }
            Cmd::PopClip => {
                depth -= 1;
                if depth > 0 {
                    eyes.last_mut().unwrap().push((*cmd, Vec::new()));
                }
            }
            Cmd::Fill { path, .. } | Cmd::Stroke { path, .. } if depth > 0 => {
                eyes.last_mut().unwrap().push((*cmd, list.elements(path).to_vec()));
            }
            _ => {}
        }
    }
    eyes
}

fn fills(eye: &[(Cmd, Vec<PathEl>)]) -> impl Iterator<Item = (&[PathEl], Paint, f32)> {
    eye.iter().filter_map(|(cmd, els)| match *cmd {
        Cmd::Fill { paint, opacity, .. } => Some((els.as_slice(), paint, opacity)),
        _ => None,
    })
}

/// The fill painted with `paint` (and the opacity, when given): its elements.
fn find(eye: &[(Cmd, Vec<PathEl>)], paint: Paint, opacity: Option<f32>) -> Vec<&[PathEl]> {
    fills(eye).filter(|f| f.1 == paint && opacity.is_none_or(|o| (f.2 - o).abs() < 1e-5)).map(|f| f.0).collect()
}

fn centre(els: &[PathEl]) -> Vec2 {
    let (lo, hi) = bounds(els);
    (lo + hi) / 2.0
}

fn size(els: &[PathEl]) -> Vec2 {
    let (lo, hi) = bounds(els);
    hi - lo
}

/// The iris gradient's fill of the first eye: the one with four stops.
fn iris_of(eye: &[(Cmd, Vec<PathEl>)]) -> (&[PathEl], Paint) {
    let f = fills(eye).find(|f| matches!(f.1, Paint::Linear { stops, .. } if stops.as_slice().len() == 4)).unwrap();
    (f.0, f.1)
}

// ---- the iris

#[test]
fn the_iris_is_a_four_stop_gradient_ellipse_set_toward_the_nose_and_below_the_eye_centre() {
    let mut profile = profile();
    profile.eyes.iris.top = ColourRef::rgb(10, 20, 30);
    profile.eyes.iris.mid = ColourRef::rgb(40, 50, 60);
    profile.eyes.iris.low = ColourRef::rgb(200, 100, 0);
    let drawing = paint(&profile, &base());
    let eyes = inside(&drawing[FaceLayerId::Base]);
    assert_eq!(eyes.len(), 2);
    let (els, paint) = iris_of(&eyes[0]);
    let Paint::Linear { from, to, stops } = paint else { unreachable!() };
    let offsets: Vec<f32> = stops.as_slice().iter().map(|s| s.offset).collect();
    assert_eq!(offsets, [0.0, 0.5, 0.85, 1.0]);
    let colours: Vec<Colour> = stops.as_slice().iter().map(|s| s.colour).collect();
    assert_eq!(colours[..3], [Colour::rgb(10, 20, 30), Colour::rgb(40, 50, 60), Colour::rgb(200, 100, 0)]);
    assert!(colours[3].r > 200 && colours[3].g > 100 && colours[3].b > 0, "the last stop is lighter than `low`");
    // The gradient runs top to bottom over the iris' own height.
    let (lo, hi) = bounds(els);
    assert!((from.y - hi.y).abs() < 1e-6 && (to.y - lo.y).abs() < 1e-6 && (from.x - to.x).abs() < 1e-7);
    // Eye units: 0.05 m half-width; the iris is 0.64 x 0.84 of it, 0.1 toward the nose and 0.07 below the centre.
    let (w, layout) = (0.05_f32, FaceProfile::default().layout);
    let size = size(els);
    assert!((size.x - 2.0 * 0.64 * w).abs() < 2e-5 && (size.y - 2.0 * 0.84 * w).abs() < 2e-5, "{size}");
    let c = centre(els);
    assert!((c.x - (layout.eye_spacing as f32 - 0.1 * w)).abs() < 2e-5, "left eye: toward the nose is -x, {c}");
    assert!((c.y - (layout.eye_y as f32 - 0.07 * w)).abs() < 2e-5);
    // The right eye mirrors it.
    let (right, _) = iris_of(&eyes[1]);
    assert!((centre(right).x + c.x).abs() < 2e-5 && (centre(right).y - c.y).abs() < 1e-6);
}

#[test]
fn the_gaze_moves_the_iris_by_its_travel_times_the_eye_scale() {
    let mut profile = profile();
    profile.eyes.scale = 1.5;
    profile.eyes.iris.gaze_travel = [0.02, 0.01];
    let rest = centre(iris_of(&inside(&paint(&profile, &base())[FaceLayerId::Base])[0]).0);
    let mut params = base();
    params.gaze = Vec2::new(0.5, -1.0);
    let moved = centre(iris_of(&inside(&paint(&profile, &params)[FaceLayerId::Base])[0]).0);
    let d = moved - rest;
    assert!((d.x - 0.5 * 0.02 * 1.5).abs() < 2e-5 && (d.y + 0.01 * 1.5).abs() < 2e-5, "{d}");
}

#[test]
fn convergence_moves_each_iris_toward_its_own_nose_by_the_gaze_travel_times_the_eye_scale() {
    let mut profile = profile();
    profile.eyes.scale = 1.5;
    profile.eyes.iris.gaze_travel = [0.02, 0.01];
    let at = |vergence: f32| {
        let mut params = base();
        params.vergence = vergence;
        let eyes = inside(&paint(&profile, &params)[FaceLayerId::Base]);
        (centre(iris_of(&eyes[0]).0), centre(iris_of(&eyes[1]).0))
    };
    let (rest_left, rest_right) = at(0.0);
    // Positive converges: the left iris moves toward the nose (-x), the right one toward its own (+x).
    let (left, right) = at(0.5);
    let (dl, dr) = (left - rest_left, right - rest_right);
    assert!((dl.x + 0.5 * 0.02 * 1.5).abs() < 2e-5 && dl.y.abs() < 1e-6, "left: {dl}");
    assert!((dr.x - 0.5 * 0.02 * 1.5).abs() < 2e-5 && dr.y.abs() < 1e-6, "right: {dr}");
    // Negative turns them apart by the same amount.
    let (left, right) = at(-0.5);
    assert!(((left - rest_left).x - 0.5 * 0.02 * 1.5).abs() < 2e-5, "left apart: {}", left - rest_left);
    assert!(((right - rest_right).x + 0.5 * 0.02 * 1.5).abs() < 2e-5, "right apart: {}", right - rest_right);
}

/// `list` rasterised alone at 2000 px/m.
fn rasterise(list: &DrawList) -> Raster {
    let painter = RealPainter::new(mock_projection(), 2000.0);
    let spec = painter.layers().iter().find(|s| s.id == FaceLayerId::Base).unwrap();
    let mut raster = Raster::new(1, 1);
    SkiaRasterizer::new().rasterize(spec, list, spec.px_per_m, &mut raster).unwrap();
    raster
}

fn copy_path(src: &DrawList, path: PathRef, out: &mut DrawList) -> PathRef {
    let mut builder = out.path();
    for el in src.elements(path) {
        builder = match *el {
            PathEl::Move(p) => builder.move_to(p),
            PathEl::Line(p) => builder.line_to(p),
            PathEl::Quad(c, p) => builder.quad_to(c, p),
            PathEl::Cubic(a, b, p) => builder.cubic_to(a, b, p),
            PathEl::Close => builder.close(),
        };
    }
    builder.finish()
}

fn copy_cmd(src: &DrawList, cmd: Cmd, out: &mut DrawList) {
    let cmd = match cmd {
        Cmd::Fill { path, paint, opacity } => Cmd::Fill { path: copy_path(src, path, out), paint, opacity },
        Cmd::Stroke { path, paint, width, cap, join, opacity } => {
            Cmd::Stroke { path: copy_path(src, path, out), paint, width, cap, join, opacity }
        }
        Cmd::PushClip(path) => Cmd::PushClip(copy_path(src, path, out)),
        other => other,
    };
    out.push(cmd);
}

/// The eye white of the first open eye on its own: the first command (the white is drawn first).
fn white_only(list: &DrawList) -> DrawList {
    let mut out = DrawList::new();
    copy_cmd(list, list.cmds()[0], &mut out);
    out
}

/// The first open eye's white and everything inside its aperture clip (iris, ring, shadow, catchlights, shimmer),
/// without what is drawn after it (lid, lash, ...). Clips nest (the iris has its own), so the aperture clip closes at
/// the `PopClip` that brings the depth back to zero, not at the first one.
fn clipped_eye(list: &DrawList) -> DrawList {
    let mut out = DrawList::new();
    let mut depth = 0usize;
    for cmd in list.cmds() {
        copy_cmd(list, *cmd, &mut out);
        match cmd {
            Cmd::PushClip(_) => depth += 1,
            Cmd::PopClip => {
                let Some(shallower) = depth.checked_sub(1) else { unreachable!("a PopClip without its PushClip") };
                depth = shallower;
                if depth == 0 {
                    return out;
                }
            }
            _ => {}
        }
    }
    unreachable!("the first eye's aperture clip is never closed")
}

#[test]
fn an_extreme_gaze_keeps_the_iris_inside_the_aperture() {
    // The shimmer is on so the whole eye is checked: its band and stars are drawn inside the aperture too.
    let profile = shimmering(1.0);
    for gaze in [Vec2::new(1.0, 1.0), Vec2::new(-1.0, 1.0), Vec2::new(1.0, -1.0), Vec2::new(-1.0, -1.0)] {
        for (open, vergence) in [(0.4, -1.0), (1.0, 1.0), (1.9, 1.0), (1.9, -1.0)] {
            let mut params = base();
            params.gaze = gaze;
            params.vergence = vergence;
            params.left_eye.open = open;
            let drawing = paint(&profile, &params);
            let list = &drawing[FaceLayerId::Base];
            // The structure: iris, pupil, ring, shadow and catchlights all sit between the aperture's push and pop.
            let eyes = inside(list);
            assert!(fills(&eyes[0]).count() >= 6, "iris, crescent, pupil, shadow, catchlight, second spark");
            // And the picture: with the iris painted, no pixel is covered that is more than a pixel away from the
            // white (edge pixels gain coverage, since every layer anti-aliases the same edge).
            let (white, whole) = (rasterise(&white_only(list)), rasterise(&clipped_eye(list)));
            let (w, h) = (white.width as usize, white.height as usize);
            let alpha = |r: &Raster, x: usize, y: usize| r.pixels[(y * w + x) * 4 + 3];
            let mut outside = 0;
            for y in 0..h {
                for x in 0..w {
                    let near_white = (y.saturating_sub(1)..=(y + 1).min(h - 1))
                        .any(|ny| (x.saturating_sub(1)..=(x + 1).min(w - 1)).any(|nx| alpha(&white, nx, ny) > 0));
                    outside += usize::from(!near_white && alpha(&whole, x, y) > 0);
                }
            }
            assert_eq!(
                outside, 0,
                "gaze {gaze}, open {open}, vergence {vergence}: {outside} pixels outside the aperture"
            );
            // The iris really does reach the edge at the extremes (the test is not vacuous).
            let (iris, _) = iris_of(&eyes[0]);
            let (white_lo, white_hi) = bounds(&eyes_outline(list));
            let (lo, hi) = bounds(iris);
            assert!(lo.x < white_lo.x + 0.012 || hi.x > white_hi.x - 0.012 || hi.y > white_hi.y || lo.y < white_lo.y);
        }
    }
}

/// The outline of the first open eye (the white's path).
fn eyes_outline(list: &DrawList) -> Vec<PathEl> {
    let Cmd::Fill { path, .. } = list.cmds()[0] else { unreachable!("the white comes first") };
    list.elements(path).to_vec()
}

#[test]
fn a_shut_eye_draws_no_iris() {
    let shut = EyeParams { close: 1.0, ..open() };
    let drawing = paint(&profile(), &PaintParams { left_eye: shut, right_eye: shut, ..base() });
    assert!(drawing[FaceLayerId::Base].is_empty());
    let wink = paint(&profile(), &PaintParams { left_eye: shut, ..base() });
    assert_eq!(inside(&wink[FaceLayerId::Base]).len(), 1, "only the open eye has an iris");
}

// ---- the pupil, ring, crescent and lid shadow

#[test]
fn the_pupil_follows_its_shape_size_and_the_pupil_channel() {
    let pupil_of = |edit: &dyn Fn(&mut FaceProfile, &mut PaintParams)| {
        let (mut profile, mut params) = (profile(), base());
        edit(&mut profile, &mut params);
        let drawing = paint(&profile, &params);
        let eyes = inside(&drawing[FaceLayerId::Base]);
        let found = find(&eyes[0], Paint::Solid(PUPIL), None);
        let iris = size(iris_of(&eyes[0]).0);
        (found.first().map(|els| (els.to_vec(), size(els))), iris)
    };
    // Round: the prototype's slot, 0.24 x 0.34 of the iris radii.
    let (round, iris) = pupil_of(&|_, _| {});
    let round = round.unwrap().1;
    assert!((round.x / iris.x - 0.24).abs() < 1e-3 && (round.y / iris.y - 0.34).abs() < 1e-3);
    // The channel scales it, but never below 0.6; the profile's size multiplies.
    let big = pupil_of(&|_, p| p.pupil = 1.5).0.unwrap().1;
    assert!((big.x / round.x - 1.5).abs() < 1e-3);
    let floor = pupil_of(&|_, p| p.pupil = 0.4).0.unwrap().1;
    assert!((floor.x / round.x - 0.6).abs() < 1e-3, "the channel is floored at 0.6");
    let sized = pupil_of(&|f, p| {
        f.eyes.iris.pupil.size = 0.5;
        p.pupil = 0.4;
    })
    .0
    .unwrap()
    .1;
    assert!((sized.x / round.x - 0.3).abs() < 1e-3, "size 0.5 x the floored 0.6");
    // Slit: taller than wide. Heart: its own outline. None: no pupil at all.
    let slit = pupil_of(&|f, _| f.eyes.iris.pupil.shape = PupilShape::Slit).0.unwrap();
    assert!(slit.1.y > 3.0 * slit.1.x && slit.0.iter().any(|e| matches!(e, PathEl::Quad(..))));
    let heart = pupil_of(&|f, _| f.eyes.iris.pupil.shape = PupilShape::Heart).0.unwrap();
    assert!(heart.0.iter().filter(|e| matches!(e, PathEl::Cubic(..))).count() == 2 && heart.1.x > heart.1.y);
    assert!(pupil_of(&|f, _| f.eyes.iris.pupil.shape = PupilShape::None).0.is_none());
    let mut seen = vec![round, slit.1, heart.1];
    seen.dedup();
    assert_eq!(seen.len(), 3, "the shapes differ");
}

#[test]
fn the_ring_is_a_stroke_of_the_iris_outline_in_the_ring_colour() {
    let mut profile = profile();
    profile.eyes.iris.ring = ColourRef::rgba(9, 8, 7, 140);
    let drawing = paint(&profile, &base());
    let list = &drawing[FaceLayerId::Base];
    let strokes: Vec<_> = list
        .cmds()
        .iter()
        .filter_map(|c| match *c {
            Cmd::Stroke { path, paint, width, .. } => Some((list.elements(path).to_vec(), paint, width)),
            _ => None,
        })
        .collect();
    assert_eq!(strokes.len(), 2, "one ring per eye");
    let eyes = inside(list);
    assert_eq!(strokes[0].0, iris_of(&eyes[0]).0);
    assert_eq!(strokes[0].1, Paint::Solid(Colour::rgba(9, 8, 7, 140)));
    assert!((strokes[0].2 - 0.0007).abs() < 1e-9);
}

#[test]
fn the_crescent_takes_the_iris_hue_and_the_lid_shadow_its_opacity() {
    let mut profile = profile();
    profile.eyes.iris.low = ColourRef::rgb(100, 40, 160);
    profile.eyes.lid_shadow = 0.3;
    let drawing = paint(&profile, &base());
    let eyes = inside(&drawing[FaceLayerId::Base]);
    let (iris, _) = iris_of(&eyes[0]);
    let crescent =
        fills(&eyes[0]).find(|f| (f.2 - 0.35).abs() < 1e-6).map(|f| (f.0.to_vec(), f.1)).expect("a crescent at 0.35");
    let Paint::Solid(c) = crescent.1 else { panic!("solid") };
    assert!(c.r > 100 && c.b > 160 && c.g > 40 && c.b > c.g, "the low colour, lightened: {c:?}");
    // At the bottom of the iris, inside it.
    let (ci, ii) = (centre(&crescent.0), centre(iris));
    assert!(ci.y < ii.y - 0.3 * size(iris).y / 2.0);
    // The shadow: a vertical fade of one colour from opaque to clear, a little above the lid.
    let (shadow, paint, opacity) =
        fills(&eyes[0]).find(|f| matches!(f.1, Paint::Linear { stops, .. } if stops.as_slice().len() == 2)).unwrap();
    assert!((opacity - 0.3).abs() < 1e-6);
    let Paint::Linear { from, to, stops } = paint else { unreachable!() };
    assert_eq!(stops.as_slice()[0].colour.a, 255);
    assert_eq!(stops.as_slice()[1].colour.a, 0);
    assert!((from.x - to.x).abs() < 1e-7 && (from.y - to.y - 0.016).abs() < 1e-6);
    let band = size(shadow);
    assert!((band.x - 2.6 * 0.05).abs() < 1e-5 && (band.y - 0.02).abs() < 1e-6);
    let (lo, hi) = bounds(&eyes_outline(&drawing[FaceLayerId::Base]));
    assert!(from.y < hi.y && from.y > lo.y, "the gradient starts at the upper lid, inside the eye's height");
}

// ---- the catchlights

fn catchlights(eye: &[(Cmd, Vec<PathEl>)]) -> Vec<Vec<PathEl>> {
    find(eye, WHITE, Some(0.95)).into_iter().map(<[PathEl]>::to_vec).collect()
}

#[test]
fn the_catchlight_takes_the_profile_shape_and_the_effects_switch_it() {
    let shape_of = |edit: &dyn Fn(&mut FaceProfile, &mut PaintParams)| {
        let (mut profile, mut params) = (profile(), base());
        edit(&mut profile, &mut params);
        let eyes = inside(&paint(&profile, &params)[FaceLayerId::Base]);
        catchlights(&eyes[0])
    };
    let blob = shape_of(&|_, _| {});
    assert_eq!(blob.len(), 1);
    assert_eq!(blob[0].iter().filter(|e| matches!(e, PathEl::Cubic(..))).count(), 4, "an ellipse");
    let blob_size = size(&blob[0]);
    assert!(blob_size.x > 1.5 * blob_size.y, "a flat glossy blob");
    let star = shape_of(&|f, _| f.eyes.highlights.shape = CatchlightShape::Star);
    assert_eq!(star[0].iter().filter(|e| matches!(e, PathEl::Quad(..))).count(), 4);
    let heart = shape_of(&|f, _| f.eyes.highlights.shape = CatchlightShape::Heart);
    assert_eq!(heart[0].iter().filter(|e| matches!(e, PathEl::Cubic(..))).count(), 2);
    assert_eq!(shape_of(&|f, _| f.eyes.highlights.shape = CatchlightShape::None).len(), 0);
    // The heart and star channels switch the shape (above one half), the heart first.
    assert_eq!(shape_of(&|_, p| p.effects.heart = 0.7), heart);
    assert_eq!(shape_of(&|_, p| p.effects.star = 0.7), star);
    assert_eq!(shape_of(&|_, p| (p.effects.heart, p.effects.star) = (0.7, 0.9)), heart);
    assert_eq!(shape_of(&|_, p| p.effects.heart = 0.4), blob, "below one half the profile's shape stays");
}

#[test]
fn the_catchlight_is_placed_and_sized_in_iris_radii_and_slides_with_the_view() {
    let draw = |size: f64, offset: [f64; 2], motion: f64, shift: Vec2| {
        let mut profile = profile();
        profile.eyes.highlights.size = size;
        profile.eyes.highlights.offset = offset;
        profile.eyes.highlights.view_motion = motion;
        let drawing = paint_with(&profile, &base(), 0.0, FaceView { highlight_shift: shift });
        let eyes = inside(&drawing[FaceLayerId::Base]);
        let iris = iris_of(&eyes[0]).0.to_vec();
        (catchlights(&eyes[0]).remove(0), iris)
    };
    let (still, iris) = draw(0.3, [0.28, 0.22], 0.5, Vec2::ZERO);
    let (rx, ry) = (size(&iris).x / 2.0, size(&iris).y / 2.0);
    let (c, ic) = (centre(&still), centre(&iris));
    assert!((c.x - ic.x - 0.28 * rx).abs() < 2e-5 && (c.y - ic.y - 0.22 * ry).abs() < 2e-5);
    // Half the size, half the width.
    let (small, _) = draw(0.15, [0.28, 0.22], 0.5, Vec2::ZERO);
    assert!((size(&small).x / size(&still).x - 0.5).abs() < 0.02);
    // The view slides it by shift x view_motion iris radii; no motion, no slide.
    let (slid, _) = draw(0.3, [0.28, 0.22], 0.5, Vec2::new(0.4, -0.2));
    let d = centre(&slid) - c;
    assert!((d.x - 0.5 * 0.4 * rx).abs() < 2e-5 && (d.y + 0.5 * 0.2 * ry).abs() < 2e-5, "{d}");
    let (fixed, _) = draw(0.3, [0.28, 0.22], 0.0, Vec2::new(0.4, -0.2));
    assert!((centre(&fixed) - c).length() < 1e-7);
}

#[test]
fn the_second_spark_has_the_profiles_strength_and_none_when_zero() {
    let spark = |second: f64| {
        let mut profile = profile();
        profile.eyes.highlights.second = second;
        let eyes = inside(&paint(&profile, &base())[FaceLayerId::Base]);
        find(&eyes[0], WHITE, Some(second as f32)).len()
    };
    assert_eq!(spark(0.4), 1);
    let mut profile = profile();
    profile.eyes.highlights.second = 0.0;
    let eyes = inside(&paint(&profile, &base())[FaceLayerId::Base]);
    assert_eq!(fills(&eyes[0]).filter(|f| f.1 == WHITE).count(), 1, "only the main catchlight is left");
}

#[test]
fn the_sparkle_effect_adds_a_dot_in_the_upper_iris() {
    let mut params = base();
    params.effects.sparkle = 0.6;
    let eyes = inside(&paint(&profile(), &params)[FaceLayerId::Base]);
    let dot = find(&eyes[0], WHITE, Some(0.6));
    assert_eq!(dot.len(), 1);
    let (iris, c) = (iris_of(&eyes[0]).0, centre(dot[0]));
    assert!(c.y > centre(iris).y && (size(dot[0]).x - size(dot[0]).y).abs() < 1e-6, "a round dot above the centre");
}

// ---- the shimmer

fn shimmering(strength: f64) -> FaceProfile {
    let mut profile = profile();
    profile.eyes.highlights.shimmer = strength;
    profile
}

/// The shimmer's fills of the left eye: the radial band, then the two stars.
fn shimmer_of(drawing: &FaceDrawing) -> (Paint, Vec<PathEl>, Vec<Vec<PathEl>>) {
    let eyes = inside(&drawing[FaceLayerId::Base]);
    let band = fills(&eyes[0]).find(|f| matches!(f.1, Paint::Radial { .. })).map(|f| (f.1, f.0.to_vec())).unwrap();
    let stars = find(&eyes[0], WHITE, Some(0.9)).into_iter().map(<[PathEl]>::to_vec).collect();
    (band.0, band.1, stars)
}

#[test]
fn no_shimmer_means_no_band_and_no_stars() {
    let eyes = inside(&paint(&shimmering(0.0), &base())[FaceLayerId::Base]);
    assert!(fills(&eyes[0]).all(|f| !matches!(f.1, Paint::Radial { .. })));
    let eyes = inside(&paint(&shimmering(0.005), &base())[FaceLayerId::Base]);
    assert!(fills(&eyes[0]).all(|f| !matches!(f.1, Paint::Radial { .. })), "under 0.01 counts as off");
}

#[test]
fn the_shimmer_is_a_pure_function_of_the_clock() {
    let profile = shimmering(0.8);
    let at = |t: f64| paint_with(&profile, &base(), t, FaceView::default());
    for t in [0.0, 0.37, 12.5, 3600.25] {
        assert_eq!(at(t)[FaceLayerId::Base].cmds(), at(t)[FaceLayerId::Base].cmds(), "t = {t}");
        assert_eq!(at(t).fingerprints(), at(t).fingerprints(), "t = {t}");
    }
    assert_ne!(at(0.0).fingerprints(), at(0.3).fingerprints(), "it moves");
    // Nothing else moves: with the shimmer off the clock changes nothing.
    let off = shimmering(0.0);
    assert_eq!(
        paint_with(&off, &base(), 0.0, FaceView::default()).fingerprints(),
        paint_with(&off, &base(), 9.0, FaceView::default()).fingerprints()
    );
}

#[test]
fn the_band_glides_across_the_iris_and_the_eyes_are_out_of_step() {
    let profile = shimmering(1.0);
    let band_x = |t: f64, eye: usize| {
        let drawing = paint_with(&profile, &base(), t, FaceView::default());
        let eyes = inside(&drawing[FaceLayerId::Base]);
        let iris = iris_of(&eyes[eye]).0.to_vec();
        let Paint::Radial { centre: c, radius, .. } =
            fills(&eyes[eye]).find(|f| matches!(f.1, Paint::Radial { .. })).unwrap().1
        else {
            unreachable!()
        };
        let rx = size(&iris).x / 2.0;
        assert!((radius / rx - 0.55).abs() < 1e-3);
        (c.x - centre(&iris).x) / rx
    };
    // Left eye: phase = t * 0.23 + 0.17; x = -0.6 + 1.2 phase (iris radii).
    for t in [0.0, 0.5, 2.0] {
        let phase = (t * 0.23 + 0.17) % 1.0;
        assert!((f64::from(band_x(t, 0)) - (-0.6 + 1.2 * phase)).abs() < 2e-3, "t = {t}");
    }
    // The right eye leads by the other sign.
    let phase = (0.5_f64 * 0.23 - 0.17) % 1.0;
    assert!((f64::from(band_x(0.5, 1)) - (-0.6 + 1.2 * phase)).abs() < 2e-3);
    assert!((band_x(1.0, 0) - band_x(0.0, 0)).abs() > 0.1, "it glides");
    // One lap takes 1 / 0.23 seconds.
    let lap = 1.0 / 0.23;
    assert!((band_x(0.4 + lap, 0) - band_x(0.4, 0)).abs() < 2e-3);
}

#[test]
fn the_stars_twinkle_on_the_clock_and_scale_with_the_strength() {
    let reach = |t: f64, strength: f64| {
        let drawing = paint_with(&shimmering(strength), &base(), t, FaceView::default());
        let (_, _, stars) = shimmer_of(&drawing);
        assert_eq!(stars.len(), 2);
        stars.iter().map(|s| size(s).x).collect::<Vec<_>>()
    };
    // sin(7 t + phase + 1) for the left eye; the star's reach is iris half-width x size x twinkle x 1.6, both ways.
    let rx = 0.64 * 0.05_f32;
    for t in [0.0, 0.2, 0.9] {
        let r = reach(t, 1.0);
        for (i, (size, phase)) in [(0.11_f64, 0.0_f64), (0.07, 2.1)].into_iter().enumerate() {
            let twinkle = 0.55 + 0.45 * (t * 7.0 + phase + 1.0).sin();
            let want = 2.0 * f64::from(rx) * size * twinkle * 1.6;
            assert!((f64::from(r[i]) - want).abs() < 3e-5, "t = {t}, star {i}: {} vs {want}", r[i]);
        }
    }
    // Strength is opacity: the stars' and the band's opacities follow it.
    let drawing = paint_with(&shimmering(0.5), &base(), 0.0, FaceView::default());
    let eyes = inside(&drawing[FaceLayerId::Base]);
    assert_eq!(find(&eyes[0], WHITE, Some(0.45)).len(), 2, "0.9 x 0.5");
    let band = fills(&eyes[0]).find(|f| matches!(f.1, Paint::Radial { .. })).unwrap();
    assert!((band.2 - 0.15).abs() < 1e-6, "0.30 x 0.5");
}

#[test]
fn the_shimmer_slides_against_the_view_but_the_stars_less() {
    let profile = {
        let mut p = shimmering(1.0);
        p.eyes.highlights.view_motion = 1.0;
        p
    };
    let at = |shift: Vec2| paint_with(&profile, &base(), 0.0, FaceView { highlight_shift: shift });
    let (rest, slid) = (at(Vec2::ZERO), at(Vec2::new(0.5, 0.0)));
    let (band0, _, stars0) = shimmer_of(&rest);
    let (band1, _, stars1) = shimmer_of(&slid);
    let (Paint::Radial { centre: c0, .. }, Paint::Radial { centre: c1, .. }) = (band0, band1) else { unreachable!() };
    let rx = 0.64 * 0.05;
    assert!(((c1.x - c0.x) + 0.5 * rx).abs() < 2e-5, "the band moves against the shift by a radius fraction");
    for (a, b) in stars0.iter().zip(&stars1) {
        assert!(((centre(b).x - centre(a).x) + 0.6 * 0.5 * rx).abs() < 2e-5, "stars move 0.6 as far");
    }
}

// ---- the budget

#[test]
fn an_eye_with_everything_on_is_a_handful_of_commands() {
    let mut profile = shimmering(1.0);
    profile.eyes.highlights.shape = CatchlightShape::Heart;
    let mut params = base();
    params.effects.sparkle = 1.0;
    let drawing = paint(&profile, &params);
    assert!(drawing[FaceLayerId::Base].cmds().len() <= 2 * COMMANDS_PER_EYE);
}

#[test]
fn painting_the_same_face_twice_gives_the_same_list() {
    let (profile, mut params) = (shimmering(0.7), base());
    params.gaze = Vec2::new(0.3, -0.2);
    let view = FaceView { highlight_shift: Vec2::new(0.1, 0.05) };
    assert_eq!(
        paint_with(&profile, &params, 1.25, view).fingerprints(),
        paint_with(&profile, &params, 1.25, view).fingerprints()
    );
}

#[test]
fn painting_irises_pupils_catchlights_and_shimmer_allocates_nothing_once_the_lists_are_warm() {
    let mut faces = Vec::new();
    for (shape, pupil) in [
        (CatchlightShape::Blob, PupilShape::Round),
        (CatchlightShape::Star, PupilShape::Slit),
        (CatchlightShape::Heart, PupilShape::Heart),
        (CatchlightShape::None, PupilShape::None),
    ] {
        let mut profile = shimmering(0.9);
        profile.eyes.highlights.shape = shape;
        profile.eyes.iris.pupil.shape = pupil;
        let mut params = base();
        params.gaze = Vec2::new(0.7, -0.4);
        params.effects.sparkle = 0.5;
        faces.push((profile, params));
    }
    let mut painter = RealPainter::new(mock_projection(), DEFAULT_PX_PER_M);
    let mut drawing = FaceDrawing::new();
    let mut frame = |painter: &mut RealPainter, i: usize| {
        let (profile, params) = &faces[i % faces.len()];
        let view = FaceView { highlight_shift: Vec2::new(0.1, -0.1) };
        let input = FaceInput { profile, params, time: Seconds(i as f64 * 0.016), view };
        painter.paint_section(Section::Eyes, &input, &mut drawing).unwrap();
    };
    for i in 0..faces.len() {
        frame(&mut painter, i);
    }
    let before = ALLOCATIONS.with(Cell::get);
    MEASURING.with(|m| m.set(true));
    for i in 0..1000 {
        frame(&mut painter, i);
    }
    MEASURING.with(|m| m.set(false));
    let allocations = ALLOCATIONS.with(Cell::get) - before;
    assert_eq!(allocations, 0, "1000 frames of eyes made {allocations} allocations");
}
