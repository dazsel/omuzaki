//! The mouth's finishing pieces through the real painter: the lower-lip gloss and the closed-smile fang on a closed
//! mouth and a kiss, the clenched split line fading in with the lower teeth, the fangs' bases inside a narrow mouth, and
//! the zero-allocation guarantee for all of them.
//!
//! Run with `--nocapture` to print the numbers the pull request quotes.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default. Test
// helpers may panic: a panic is a clear test failure.
#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;

use omuzaki_core::glam::Vec2;
use omuzaki_core::time::Seconds;
use omuzaki_face::{MouthPiece, RealPainter, Section};
use omuzaki_face_api::{
    Cmd, DEFAULT_PX_PER_M, DrawList, FaceDrawing, FaceInput, FaceLayerId, FaceMapping, FaceProfile, FaceProjection,
    FaceRect, FaceView, MouthParams, Paint, PaintParams, PathEl, PathRef,
};

struct Counting;

thread_local! {
    // `const`-initialised cells without a destructor: touching them never allocates, so it is safe inside the allocator.
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

fn count_allocations(f: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.with(Cell::get);
    MEASURING.with(|m| m.set(true));
    f();
    MEASURING.with(|m| m.set(false));
    ALLOCATIONS.with(Cell::get) - before
}

// ------------------------------------------------------------------------------------------------ helpers

fn painter() -> RealPainter {
    RealPainter::new(
        FaceProjection { rect: FaceRect { x0: -0.1, x1: 0.1, y0: -0.2, y1: -0.1 }, front_fade: [0.0, 0.05] },
        DEFAULT_PX_PER_M,
    )
}

fn params(edit: impl FnOnce(&mut MouthParams)) -> PaintParams {
    let mut p = FaceMapping::default().rest_params();
    p.mouth = MouthParams { smile_left: 0.0, smile_right: 0.0, ..p.mouth };
    edit(&mut p.mouth);
    p
}

fn lips(strength: f64) -> FaceProfile {
    let mut p = FaceProfile::default();
    p.mouth.lips.strength = strength;
    p
}

fn piece(piece: MouthPiece, profile: &FaceProfile, p: &PaintParams) -> DrawList {
    let mut out = FaceDrawing::new();
    let input = FaceInput { profile, params: p, time: Seconds(0.0), view: FaceView::default() };
    painter().paint_mouth_piece(piece, &input, &mut out).unwrap();
    out[FaceLayerId::Base].clone()
}

fn points(list: &DrawList, path: PathRef) -> Vec<Vec2> {
    list.elements(path)
        .iter()
        .flat_map(|e| match e {
            PathEl::Move(p) | PathEl::Line(p) => vec![*p],
            PathEl::Cubic(a, b, p) => vec![*a, *b, *p],
            PathEl::Quad(a, p) => vec![*a, *p],
            PathEl::Close => vec![],
        })
        .collect()
}

/// A closed smile with the fang channel on.
fn closed_smile(m: &mut MouthParams) {
    (m.open, m.smile_left, m.smile_right, m.fangs) = (0.0, 1.0, 1.0, 1.0);
}

/// A kiss: pushed-forward lips, barely open.
fn kiss(m: &mut MouthParams) {
    (m.open, m.pucker) = (0.1, 0.9);
}

/// The gloss's radial swell: its centre and the alpha at its middle, if the piece painted one.
fn swell(list: &DrawList) -> Option<(Vec2, u8)> {
    list.cmds().iter().find_map(|c| match c {
        Cmd::Fill { paint: Paint::Radial { centre, stops, .. }, .. } => Some((*centre, stops.as_slice()[0].colour.a)),
        _ => None,
    })
}

/// The triangles filled by the closed fang (three points each).
fn triangles(list: &DrawList) -> Vec<Vec<Vec2>> {
    list.cmds()
        .iter()
        .filter_map(|c| match c {
            Cmd::Fill { path, .. } if points(list, *path).len() == 3 => Some(points(list, *path)),
            _ => None,
        })
        .collect()
}

// ------------------------------------------------------------------------------------------------ gloss

#[test]
fn a_closed_smile_and_a_kiss_have_the_lower_lip_gloss() {
    for (name, edit) in [("closed smile", closed_smile as fn(&mut MouthParams)), ("kiss", kiss)] {
        let p = params(edit);
        let (centre, alpha) =
            swell(&piece(MouthPiece::Gloss, &lips(1.0), &p)).unwrap_or_else(|| panic!("{name}: no gloss"));
        assert!(centre.y < -0.15, "{name}: the gloss hangs under the mouth line, not {}", centre.y);
        assert!(alpha > 0, "{name}: the swell has colour");
        println!("gloss {name}: centre y {:.5} m (line -0.15), middle alpha {alpha}/255", centre.y);
    }
}

#[test]
fn a_flat_face_has_no_gloss_on_a_closed_mouth_but_a_kiss_keeps_one() {
    let flat = lips(0.0);
    assert!(swell(&piece(MouthPiece::Gloss, &flat, &params(closed_smile))).is_none());
    assert!(swell(&piece(MouthPiece::Gloss, &flat, &params(|_| {}))).is_none());
    let kissing = swell(&piece(MouthPiece::Gloss, &flat, &params(kiss)));
    assert!(kissing.is_some(), "pushed-forward lips always read as glossy");
}

// ------------------------------------------------------------------------------------------------ closed fang

#[test]
fn a_closed_smile_shows_one_fang_where_a_wide_mouth_with_no_smile_shows_none() {
    let profile = FaceProfile::default();
    let smiling = piece(MouthPiece::Interior, &profile, &params(closed_smile));
    assert_eq!(triangles(&smiling).len(), 1, "one fang, on the corner that smiles");
    for (name, edit) in [
        ("a straight mouth", (|m: &mut MouthParams| m.fangs = 1.0) as fn(&mut MouthParams)),
        ("a weak smile", |m| (m.fangs, m.smile_left) = (1.0, 0.25)),
        ("a weak fang channel", |m| (m.fangs, m.smile_left) = (0.4, 1.0)),
        ("a kiss", |m| (m.fangs, m.smile_left, m.open, m.pucker) = (1.0, 1.0, 0.1, 0.9)),
    ] {
        let list = piece(MouthPiece::Interior, &profile, &params(edit));
        assert!(triangles(&list).is_empty(), "{name} shows no fang");
    }
}

#[test]
fn the_closed_fang_stands_on_the_line_inside_its_extent_at_every_width() {
    let profile = FaceProfile::default();
    let grow = profile.mouth.closed_scale as f32;
    for position in [0.72, 1.0] {
        let mut profile = profile.clone();
        profile.mouth.fang.position = position;
        for wide in [0.3, 0.5, 1.0, 2.0] {
            let p = params(|m| {
                closed_smile(m);
                m.wide = wide;
            });
            let half_line = profile.mouth.half_width as f32 * wide * grow;
            let t = &triangles(&piece(MouthPiece::Interior, &profile, &p))[0];
            let (left, right) = (t[0].x, t[1].x);
            assert!(left < right && t[2].y > t[0].y - 1.0, "a triangle with its tip above its base");
            assert!(right <= half_line + 1e-6, "base corner {right} past the line's end {half_line} (wide {wide})");
        }
    }
}

// ------------------------------------------------------------------------------------------------ split line

/// The split line's effective alpha (colour alpha times command opacity) for `teeth_lower`, if there is a line.
fn split_alpha(teeth_lower: f32) -> Option<f32> {
    let p = params(|m| {
        (m.open, m.teeth_upper, m.teeth_lower, m.smile_left, m.smile_right) = (0.8, 1.0, teeth_lower, 1.5, 1.5);
    });
    piece(MouthPiece::Interior, &FaceProfile::default(), &p).cmds().iter().find_map(|c| match c {
        Cmd::Stroke { paint: Paint::Solid(colour), opacity, .. } => Some(f32::from(colour.a) / 255.0 * opacity),
        _ => None,
    })
}

#[test]
fn the_split_line_is_continuous_as_the_lower_teeth_cross_their_threshold() {
    assert_eq!(split_alpha(0.0), None);
    assert_eq!(split_alpha(0.01), None);
    let steps: Vec<f32> = (0..=40).map(|i| 0.01 + 0.0005 * i as f32).collect();
    let alphas: Vec<f32> = steps.iter().map(|&t| split_alpha(t.max(0.0101)).unwrap()).collect();
    assert!(alphas[0] < 0.01, "just above the threshold the line is nearly invisible, not {}", alphas[0]);
    for w in alphas.windows(2) {
        assert!(w[1] >= w[0] && w[1] - w[0] < 0.55 * 0.0006, "a jump from {} to {}", w[0], w[1]);
    }
    println!(
        "split line alpha: teeth_lower 0.0101 -> {:.5}, 0.02 -> {:.5}, 0.5 -> {:.5}, 1.0 -> {:.5}",
        split_alpha(0.0101).unwrap(),
        split_alpha(0.02).unwrap(),
        split_alpha(0.5).unwrap(),
        split_alpha(1.0).unwrap()
    );
}

// ------------------------------------------------------------------------------------------------ fangs, narrow mouth

#[test]
fn every_fang_base_lies_inside_the_openings_extent_for_narrow_mouths() {
    for position in [0.72, 0.85, 1.0] {
        let mut profile = FaceProfile::default();
        profile.mouth.fang.position = position;
        for wide in [0.3, 0.4, 0.5, 1.0] {
            let p = params(|m| (m.fangs, m.wide, m.open) = (1.0, wide, 0.8));
            let list = piece(MouthPiece::Shape, &profile, &p);
            let Some(Cmd::Fill { path, .. }) = list.cmds().first() else { panic!("no open shape") };
            let xs: Vec<f32> = points(&list, *path).iter().map(|v| v.x).collect();
            let (lo, hi) = (xs.iter().copied().fold(f32::MAX, f32::min), xs.iter().copied().fold(f32::MIN, f32::max));
            let fangs = triangles(&piece(MouthPiece::Interior, &profile, &p));
            assert_eq!(fangs.len(), 2);
            let widest = fangs.iter().map(|t| t[1].x - t[0].x).fold(0.0, f32::max);
            for t in &fangs {
                assert!(t[0].x >= lo - 1e-6 && t[1].x <= hi + 1e-6, "fang {:?} outside {lo}..{hi}", &t[..2]);
            }
            println!(
                "narrow mouth: position {position}, wide {wide}: extent {lo:.5}..{hi:.5}, widest base {widest:.5}"
            );
        }
    }
}

#[test]
fn the_default_pose_keeps_the_six_millimetre_fang() {
    let p = params(|m| (m.fangs, m.open) = (1.0, 0.8));
    let grow = FaceProfile::default().mouth.open_scale as f32;
    for t in triangles(&piece(MouthPiece::Interior, &FaceProfile::default(), &p)) {
        assert!(((t[1].x - t[0].x) - 0.006 * grow).abs() < 1e-6, "base {}", t[1].x - t[0].x);
    }
}

// ------------------------------------------------------------------------------------------------ allocation

#[test]
fn painting_closed_smiles_and_kisses_allocates_nothing_once_the_list_is_warm() {
    let profile = lips(1.0);
    let poses = [
        params(closed_smile),
        params(kiss),
        params(|m| (m.open, m.pucker, m.fangs, m.smile_left) = (0.0, 0.0, 1.0, 1.0)),
        params(|m| (m.open, m.teeth_upper, m.teeth_lower, m.fangs) = (0.8, 1.0, 0.5, 1.0)),
    ];
    let mut painter = painter();
    let mut out = FaceDrawing::new();
    for p in &poses {
        let input = FaceInput { profile: &profile, params: p, time: Seconds(0.0), view: FaceView::default() };
        painter.paint_section(Section::Mouth, &input, &mut out).unwrap();
    }
    let allocations = count_allocations(|| {
        for p in poses.iter().cycle().take(1000) {
            let input = FaceInput { profile: &profile, params: p, time: Seconds(0.0), view: FaceView::default() };
            painter.paint_section(Section::Mouth, &input, &mut out).unwrap();
            black_box(&out);
        }
    });
    assert_eq!(allocations, 0, "1000 mouth paints made {allocations} allocations");
}

#[test]
fn the_counter_sees_an_allocation() {
    let allocations = count_allocations(|| {
        black_box(Vec::<u8>::with_capacity(64));
    });
    assert_eq!(allocations, 1, "without this the zero-allocation test proves nothing");
}
