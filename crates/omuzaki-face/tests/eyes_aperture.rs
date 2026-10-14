//! The eye aperture and the eye white through the public painter: what the eyes section puts in the base
//! layer's draw list, and that painting it allocates nothing once the lists are warm.
//!
//! The curves themselves (the prototype's numbers, the shape properties) are tested beside the code, in
//! `painter/eyes/aperture.rs`; the comparison with the prototype's pixels is `omuzaki-test`'s
//! `face_parity_eye_aperture.rs`.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec2;
use omuzaki_core::time::Seconds;
use omuzaki_face::{RealPainter, Section};
use omuzaki_face_api::{
    Cmd, ColourRef, DEFAULT_PX_PER_M, DrawList, EyeParams, FaceDrawing, FaceInput, FaceLayerId, FaceMapping,
    FaceProfile, FaceView, Paint, PaintParams, PathEl,
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

/// The most commands one eye may take (the painter's budget).
const COMMANDS_PER_EYE: usize = 600;

fn params(left: EyeParams, right: EyeParams) -> PaintParams {
    PaintParams { left_eye: left, right_eye: right, ..FaceMapping::default().rest_params() }
}

fn lids(open: f32, close: f32, arc: f32) -> EyeParams {
    EyeParams { open, close, arc, lower_lid: 0.0 }
}

/// Paints the eyes section alone and returns the drawing.
fn paint_eyes(profile: &FaceProfile, params: &PaintParams) -> FaceDrawing {
    let mut drawing = FaceDrawing::new();
    paint_eyes_into(&mut RealPainter::new(mock_projection(), DEFAULT_PX_PER_M), profile, params, &mut drawing);
    drawing
}

fn paint_eyes_into(painter: &mut RealPainter, profile: &FaceProfile, params: &PaintParams, out: &mut FaceDrawing) {
    let input = FaceInput { profile, params, time: Seconds(0.0), view: FaceView::default() };
    painter.paint_section(Section::Eyes, &input, out).unwrap();
}

/// The `(Fill, PushClip, PopClip)` triples of the base layer, one per open eye, as the clip path's elements and the
/// fill's paint. Panics if the list is not made of such triples.
fn open_eyes(list: &DrawList) -> Vec<(Vec<PathEl>, Vec<PathEl>, Paint)> {
    assert_eq!(list.cmds().len() % 3, 0, "an open eye is a fill, a clip and the end of the clip: {:?}", list.cmds());
    list.cmds()
        .chunks(3)
        .map(|c| {
            let (Cmd::Fill { path, paint, opacity }, Cmd::PushClip(clip), Cmd::PopClip) = (c[0], c[1], c[2]) else {
                panic!("not a clipped fill: {c:?}")
            };
            assert!((opacity - 1.0).abs() < f32::EPSILON);
            (list.elements(clip).to_vec(), list.elements(path).to_vec(), paint)
        })
        .collect()
}

fn bounds(els: &[PathEl]) -> (Vec2, Vec2) {
    let points = els.iter().flat_map(|el| match *el {
        PathEl::Move(p) | PathEl::Line(p) => vec![p],
        PathEl::Quad(c, p) => vec![c, p],
        PathEl::Cubic(a, b, p) => vec![a, b, p],
        PathEl::Close => vec![],
    });
    points.fold((Vec2::splat(f32::MAX), Vec2::splat(f32::MIN)), |(lo, hi), p| (lo.min(p), hi.max(p)))
}

#[test]
fn two_open_eyes_are_a_white_and_a_clip_each_in_the_base_layer() {
    let drawing = paint_eyes(&FaceProfile::default(), &params(lids(1.0, 0.0, 0.0), lids(1.0, 0.0, 0.0)));
    assert!(drawing[FaceLayerId::Brows].is_empty() && drawing[FaceLayerId::Overlay].is_empty());
    let eyes = open_eyes(&drawing[FaceLayerId::Base]);
    assert_eq!(eyes.len(), 2);
    for (clip, fill, _) in &eyes {
        assert_eq!(clip, fill, "the white fills exactly the aperture it is clipped to");
        assert_eq!(clip.len(), 92, "move, 44 lines, the outer quadratic, 45 lines, close");
        assert!(matches!(clip.last(), Some(PathEl::Close)));
    }
    // The character's left eye (+x) is painted first.
    assert!(bounds(&eyes[0].0).0.x > 0.0 && bounds(&eyes[1].0).1.x < 0.0);
}

#[test]
fn the_white_is_a_three_stop_gradient_from_the_upper_to_the_lower_lid() {
    let mut profile = FaceProfile::default();
    profile.eyes.sclera.top = ColourRef::rgb(1, 2, 3);
    profile.eyes.sclera.mid = ColourRef::rgb(4, 5, 6);
    profile.eyes.sclera.bottom = ColourRef::rgb(7, 8, 9);
    let drawing = paint_eyes(&profile, &params(lids(1.0, 0.0, 0.0), lids(1.0, 0.0, 0.0)));
    let eyes = open_eyes(&drawing[FaceLayerId::Base]);
    let Paint::Linear { from, to, stops } = eyes[0].2 else { panic!("a linear gradient") };
    let stops: Vec<_> = stops.as_slice().iter().map(|s| (s.offset, s.colour)).collect();
    assert_eq!(stops, [(0.0, Colour::rgb(1, 2, 3)), (0.5, Colour::rgb(4, 5, 6)), (1.0, Colour::rgb(7, 8, 9))]);
    assert!((from.x - to.x).abs() < 1e-7, "vertical");
    let (lo, hi) = bounds(&eyes[0].0);
    assert!(from.y < hi.y && from.y > to.y && to.y > lo.y, "from the upper lid to the lower lid, inside the aperture");
}

#[test]
fn a_shut_eye_draws_no_aperture() {
    let open = lids(1.0, 0.0, 0.0);
    for shut in [lids(1.0, 1.0, 0.0), lids(1.0, 0.0, 1.0), lids(0.0, 0.0, 0.0), lids(1.0, 0.0, 0.5)] {
        let both = paint_eyes(&FaceProfile::default(), &params(shut, shut));
        assert!(both[FaceLayerId::Base].is_empty(), "{shut:?}");
        // A wink: the other eye still opens.
        let wink = paint_eyes(&FaceProfile::default(), &params(shut, open));
        let eyes = open_eyes(&wink[FaceLayerId::Base]);
        assert_eq!(eyes.len(), 1, "{shut:?}");
        assert!(bounds(&eyes[0].0).1.x < 0.0, "only the right eye is left open");
    }
}

#[test]
fn eye_scale_drop_and_layout_move_the_eyes() {
    let draw = |edit: &dyn Fn(&mut FaceProfile)| {
        let mut profile = FaceProfile::default();
        edit(&mut profile);
        let drawing = paint_eyes(&profile, &params(lids(1.0, 0.0, 0.0), lids(1.0, 0.0, 0.0)));
        bounds(&open_eyes(&drawing[FaceLayerId::Base])[0].0)
    };
    let (lo, hi) = draw(&|_| {});
    let (lo_big, hi_big) = draw(&|p| p.eyes.scale = 1.5);
    assert!(((hi_big.x - lo_big.x) / (hi.x - lo.x) - 1.5).abs() < 0.02, "the eye grows with its scale");
    // Spacing grows by a third of the scale change, so the centre moves out by 0.088 x 0.175 and the width by 0.025.
    assert!(((hi_big.x + lo_big.x) - (hi.x + lo.x) - 2.0 * 0.088 * 0.175).abs() < 0.004, "the centre moves outwards");
    let (_, hi_dropped) = draw(&|p| p.eyes.drop = 0.01);
    assert!((hi.y - hi_dropped.y - 0.01).abs() < 1e-6);
    let (_, hi_raised) = draw(&|p| p.layout.eye_y += 0.02);
    assert!((hi_raised.y - hi.y - 0.02).abs() < 1e-6);
}

#[test]
fn an_eye_is_a_handful_of_commands() {
    let drawing = paint_eyes(&FaceProfile::default(), &params(lids(1.9, 0.0, 0.0), lids(0.4, 0.0, 0.0)));
    assert!(drawing[FaceLayerId::Base].cmds().len() <= 2 * COMMANDS_PER_EYE);
}

#[test]
fn painting_the_same_face_twice_gives_the_same_list() {
    let (profile, p) = (FaceProfile::default(), params(lids(1.3, 0.0, 0.0), lids(0.7, 0.2, 0.0)));
    let (a, b) = (paint_eyes(&profile, &p), paint_eyes(&profile, &p));
    assert_eq!(a.fingerprints(), b.fingerprints());
}

#[test]
fn painting_the_eyes_allocates_nothing_once_the_lists_are_warm() {
    let profile = FaceProfile::default();
    let faces = [
        params(lids(1.0, 0.0, 0.0), lids(1.0, 0.0, 0.0)),
        params(lids(1.9, 0.0, 0.0), lids(0.5, 0.0, 0.0)),
        params(lids(1.0, 1.0, 0.0), lids(1.0, 0.0, 1.0)),
    ];
    let mut painter = RealPainter::new(mock_projection(), DEFAULT_PX_PER_M);
    let mut drawing = FaceDrawing::new();
    for face in &faces {
        paint_eyes_into(&mut painter, &profile, face, &mut drawing);
    }

    let before = ALLOCATIONS.with(Cell::get);
    MEASURING.with(|m| m.set(true));
    for i in 0..1000 {
        paint_eyes_into(&mut painter, &profile, &faces[i % faces.len()], &mut drawing);
    }
    MEASURING.with(|m| m.set(false));
    let allocations = ALLOCATIONS.with(Cell::get) - before;
    assert_eq!(allocations, 0, "1000 frames of eyes made {allocations} allocations");
}
