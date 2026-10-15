//! The lash line with the cat-eye flick, the lower lid, the smiling lids, the crease, the inner tick and the closed eyes
//! through the public painter: what the eyes section puts in the base layer's draw list, and that painting it
//! allocates nothing once the lists are warm.
//!
//! The comparison with the prototype's pixels is `omuzaki-test-face`'s `face_parity_eye_lash.rs` (local lane).

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;

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

// ------------------------------------------------------------------------------------------------ helpers

/// The lash colour of the tests, so its fill and strokes are told apart from the other pieces.
const LASH: Colour = Colour::rgb(0x3a, 0x24, 0x20);
/// The lower lid and crease colours of the tests.
const LID: Colour = Colour::rgb(120, 70, 66);
const CREASE: Colour = Colour::rgb(170, 104, 92);

fn profile() -> FaceProfile {
    let mut profile = FaceProfile::default();
    profile.eyes.lash.colour = ColourRef::Fixed(LASH);
    profile.eyes.lash.thickness = 0.0076;
    profile.eyes.lash.flick = 1.0;
    profile.eyes.lash.lower = 1.0;
    profile.eyes.lash.inner_tick = true;
    profile.eyes.lower_lid.colour = ColourRef::Fixed(LID);
    profile.eyes.lower_lid.line = 0.65;
    profile.eyes.crease.colour = ColourRef::Fixed(CREASE);
    profile.eyes.crease.strength = 0.55;
    profile.eyes.crease.lift = 0.30;
    profile
}

fn eye(open: f32, close: f32, arc: f32, lower_lid: f32) -> EyeParams {
    EyeParams { open, close, arc, lower_lid }
}

fn params(left: EyeParams, right: EyeParams) -> PaintParams {
    PaintParams { left_eye: left, right_eye: right, ..FaceMapping::default().rest_params() }
}

fn paint_into(profile: &FaceProfile, p: &PaintParams, out: &mut FaceDrawing) {
    let input = FaceInput { profile, params: p, time: Seconds(0.0), view: FaceView::default() };
    RealPainter::new(mock_projection(), DEFAULT_PX_PER_M).paint_section(Section::Eyes, &input, out).unwrap();
}

/// The Base layer of the eyes section for an open pair of eyes (both eyes the same) or the given params.
fn paint(profile: &FaceProfile, p: &PaintParams) -> DrawList {
    let mut out = FaceDrawing::new();
    paint_into(profile, p, &mut out);
    out[FaceLayerId::Base].clone()
}

fn rest() -> PaintParams {
    params(eye(1.0, 0.0, 0.0, 0.0), eye(1.0, 0.0, 0.0, 0.0))
}

/// The points of a path made of moves and lines, with quadratics' control and end points too.
fn points(list: &DrawList, path: omuzaki_face_api::PathRef) -> Vec<Vec2> {
    let mut out = Vec::new();
    for el in list.elements(path) {
        match *el {
            PathEl::Move(p) | PathEl::Line(p) => out.push(p),
            PathEl::Quad(c, p) => out.extend([c, p]),
            PathEl::Cubic(c1, c2, p) => out.extend([c1, c2, p]),
            PathEl::Close => {}
        }
    }
    out
}

/// A solid colour of a paint, if it is one.
fn solid(paint: &Paint) -> Option<Colour> {
    match paint {
        Paint::Solid(c) => Some(*c),
        _ => None,
    }
}

/// The strokes of a colour (ignoring its alpha), as `(path, width, alpha)`.
fn strokes(list: &DrawList, colour: Colour) -> Vec<(omuzaki_face_api::PathRef, f32, u8)> {
    list.cmds()
        .iter()
        .filter_map(|c| match c {
            Cmd::Stroke { path, paint, width, .. } => {
                let c = solid(paint)?;
                ((c.r, c.g, c.b) == (colour.r, colour.g, colour.b)).then_some((*path, *width, c.a))
            }
            _ => None,
        })
        .collect()
}

/// The fills of exactly this colour.
fn fills(list: &DrawList, colour: Colour) -> Vec<omuzaki_face_api::PathRef> {
    list.cmds()
        .iter()
        .filter_map(|c| match c {
            Cmd::Fill { path, paint, .. } if solid(paint) == Some(colour) => Some(*path),
            _ => None,
        })
        .collect()
}

/// The commands of the one-eyed list in drawing order, as short names, for the lines this task draws.
fn order(list: &DrawList) -> Vec<&'static str> {
    list.cmds()
        .iter()
        .filter_map(|c| match c {
            Cmd::Fill { paint, .. } if solid(paint) == Some(LASH) => Some("lash"),
            Cmd::Stroke { paint, width, .. } => {
                let c = solid(paint)?;
                match ((c.r, c.g, c.b), *width) {
                    ((120, 70, 66), w) if (w - 0.0011).abs() < 1e-7 => Some("lower lid"),
                    ((120, 70, 66), w) if (w - 0.0012).abs() < 1e-7 => Some("smile line"),
                    ((255, 236, 232), _) => Some("smile rim"),
                    ((170, 104, 92), _) => Some("crease"),
                    ((0x3a, 0x24, 0x20), w) if (w - 0.0020).abs() < 1e-7 => Some("lower lash"),
                    ((120, 60, 60), _) => Some("tick"),
                    ((0x3a, 0x24, 0x20), w) if (w - 0.0048).abs() < 1e-7 => Some("closed"),
                    ((0x3a, 0x24, 0x20), w) if (w - 0.0018).abs() < 1e-7 => Some("closed lash"),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect()
}

fn smile_lids(profile: &mut FaceProfile) {
    profile.eyes.lower_lid.cheek_line = true;
}

// ------------------------------------------------------------------------------------------------ an open eye

#[test]
fn an_open_eye_draws_its_pieces_in_the_prototypes_order() {
    let mut p = profile();
    smile_lids(&mut p);
    // One eye open with the cheek raised, the other shut (so only the left eye's pieces show).
    let list = paint(&p, &params(eye(1.0, 0.0, 0.0, 0.6), eye(1.0, 1.0, 0.0, 0.0)));
    let names = order(&list);
    let left: Vec<&str> = names.iter().copied().take_while(|n| *n != "closed" && *n != "closed lash").collect();
    assert_eq!(left, ["lower lid", "smile line", "smile rim", "crease", "lash", "lower lash", "tick"]);
}

#[test]
fn a_closed_eye_draws_only_the_closed_curve_and_never_the_lash_polygon_crease_or_tick() {
    let list = paint(&profile(), &params(eye(1.0, 1.0, 0.0, 0.0), eye(1.0, 1.0, 0.0, 0.0)));
    let names = order(&list);
    assert_ne!(names.len(), 0);
    assert!(names.iter().all(|n| matches!(*n, "closed" | "closed lash")), "{names:?}");
}

// ------------------------------------------------------------------------------------------------ the lash line

#[test]
fn the_lash_polygon_has_a_tip_past_the_outer_corner_and_its_thickness_grows_from_the_inner_corner() {
    let list = paint(&profile(), &rest());
    let polygons = fills(&list, LASH);
    assert_eq!(polygons.len(), 2, "one per eye");
    let pts = points(&list, polygons[0]);
    // 45 top points, the tip, 45 bottom points.
    assert_eq!(pts.len(), 45 + 1 + 45);
    // The tip is the farthest point out (the outer direction is +x for the left eye).
    let tip = pts[45];
    assert!(pts.iter().all(|p| p.x <= tip.x + 1e-7), "the tip is the outermost point");
    // Thickness: the top and bottom edge points at a station differ by the thickness (78 % + 22 % of it). Inner end
    // 0.0036 m, thick end 0.0076 m.
    let gap = |i: usize| pts[i].y - pts[90 - i].y;
    assert!((gap(1) - 0.0036).abs() < 3e-4, "the inner end is {} thick", gap(1));
    assert!((gap(36) - 0.0076).abs() < 1e-5, "the thick end is {} thick", gap(36));
    assert!(gap(1) < gap(10) && gap(10) < gap(25), "it thickens inward to outward");
}

#[test]
fn no_flick_ends_the_lash_at_the_outer_corner_and_a_full_flick_lifts_the_tip() {
    let tip_x = |flick: f64| {
        let mut p = profile();
        p.eyes.lash.flick = flick;
        let list = paint(&p, &rest());
        let pts = points(&list, fills(&list, LASH)[0]);
        pts.iter().map(|p| p.x).fold(f32::MIN, f32::max)
    };
    let (none, half, full) = (tip_x(0.0), tip_x(0.5), tip_x(1.0));
    assert!(none < half && half < full, "the flick reaches out with its length: {none} {half} {full}");
    let mut p = profile();
    p.eyes.lash.flick = 0.0;
    let list = paint(&p, &rest());
    assert_eq!(points(&list, fills(&list, LASH)[0]).len(), 45 + 45, "no tip without a flick");
}

#[test]
fn the_lash_follows_the_lid_it_moves_with_the_openness() {
    // The top edge of the polygon at the thick end is the lid's line plus 78 % of the thickness: so a half-shut eye's
    // lash sits lower than an open one's by about what the lid fell.
    let top = |open: f32| {
        let list = paint(&profile(), &params(eye(open, 0.0, 0.0, 0.0), eye(open, 0.0, 0.0, 0.0)));
        points(&list, fills(&list, LASH)[0])[22].y
    };
    assert!(top(1.0) > top(0.5) && top(0.5) > top(0.2));
}

// ------------------------------------------------------------------------------------------------ lid, crease, tick

#[test]
fn the_lower_lid_covers_only_the_outer_half_of_the_eye() {
    let list = paint(&profile(), &rest());
    let lid = strokes(&list, LID);
    assert_eq!(lid.len(), 2, "one line per eye, no smiling lid");
    let (path, width, alpha) = lid[0];
    assert!((width - 0.0011).abs() < 1e-7);
    assert_eq!(alpha, 166, "0.65 of the colour's own alpha");
    let pts = points(&list, path);
    assert_eq!(pts.len(), 25);
    // Left eye: the outer corner is +x. The line runs from just outside the middle to just past the outer corner.
    assert!(pts[0].x < pts[24].x);
}

#[test]
fn the_smiling_lid_fades_in_as_the_cheek_raises_the_lower_lid_and_needs_the_style() {
    let alpha_at = |lift: f32, cheek_line: bool| {
        let mut p = profile();
        p.eyes.lower_lid.cheek_line = cheek_line;
        let list = paint(&p, &params(eye(1.0, 0.0, 0.0, lift), eye(1.0, 1.0, 0.0, 0.0)));
        strokes(&list, LID).iter().find(|s| (s.1 - 0.0012).abs() < 1e-7).map(|s| s.2)
    };
    assert_eq!(alpha_at(0.6, false), None, "no smiling lid without `cheek_line`");
    assert_eq!(alpha_at(0.15, true), None, "none until the lower lid rises past 0.15");
    let (half, full) = (alpha_at(0.30, true).unwrap(), alpha_at(0.45, true).unwrap());
    assert!(half > 0 && half < full, "{half} {full}");
    assert_eq!(full, 153, "0.6 of the colour at full strength");
}

#[test]
fn the_crease_is_lifted_above_the_lid_and_follows_it() {
    let list = paint(&profile(), &rest());
    let crease = strokes(&list, CREASE);
    assert_eq!(crease.len(), 2);
    assert_eq!(points(&list, crease[0].0).len(), 21);
    // It moves with the lid: raising the brows' lid (wide eyes) lifts it.
    let wide = paint(&profile(), &params(eye(1.9, 0.0, 0.0, 0.0), eye(1.9, 0.0, 0.0, 0.0)));
    let (a, b) = (points(&list, crease[0].0)[10].y, points(&wide, strokes(&wide, CREASE)[0].0)[10].y);
    assert!(b > a, "a wide eye's crease sits higher: {a} {b}");
    let mut none = profile();
    none.eyes.crease.strength = 0.0;
    assert_eq!(strokes(&paint(&none, &rest()), CREASE).len(), 0);
}

#[test]
fn the_tick_and_the_lower_lash_follow_their_switches() {
    let mut p = profile();
    p.eyes.lash.inner_tick = false;
    p.eyes.lash.lower = 0.0;
    let names = order(&paint(&p, &rest()));
    assert!(!names.contains(&"tick") && !names.contains(&"lower lash"), "{names:?}");
    let on = order(&paint(&profile(), &rest()));
    assert!(on.contains(&"tick") && on.contains(&"lower lash"));
}

// ------------------------------------------------------------------------------------------------ closed eyes

#[test]
fn a_happy_arc_bows_up_and_a_relaxed_blink_bows_down_with_three_small_lashes() {
    let arc = paint(&profile(), &params(eye(1.0, 0.0, 1.0, 0.0), eye(1.0, 0.0, 1.0, 0.0)));
    let lines = strokes(&arc, LASH);
    assert_eq!(lines.len(), 2, "one arc per eye and no lashes");
    let pts = points(&arc, lines[0].0);
    assert_eq!(pts.len(), 31);
    assert!(pts[15].y > pts[0].y && pts[15].y > pts[30].y, "the ^ rises in the middle");
    let blink = paint(&profile(), &params(eye(1.0, 1.0, 0.0, 0.0), eye(1.0, 1.0, 0.0, 0.0)));
    let lines = strokes(&blink, LASH);
    assert_eq!(lines.iter().filter(|l| (l.1 - 0.0048).abs() < 1e-7).count(), 2);
    assert_eq!(lines.iter().filter(|l| (l.1 - 0.0018).abs() < 1e-7).count(), 6, "three lashes per eye");
    let pts = points(&blink, lines[0].0);
    assert!(pts[15].y < pts[0].y.max(pts[30].y), "the relaxed curve bows down in the middle");
}

#[test]
fn a_wink_closes_one_eye_as_the_arc_and_leaves_the_other_open() {
    let list = paint(&profile(), &params(eye(1.0, 0.0, 1.0, 0.0), eye(1.0, 0.0, 0.0, 0.0)));
    let names = order(&list);
    assert_eq!(names.iter().filter(|n| **n == "closed").count(), 1);
    assert_eq!(names.iter().filter(|n| **n == "lash").count(), 1, "the open eye keeps its lash line");
}

#[test]
fn sweeping_close_from_open_to_shut_keeps_the_lash_on_the_lid() {
    // At every step the lash polygon's centre line is the lid's: its top edge never leaves the aperture's top by more
    // than the lash is thick. Checked through the number of pieces: an open eye always has all of them.
    for step in 0..=7_u8 {
        let close = f32::from(step) / 7.0;
        let list = paint(&profile(), &params(eye(1.0, close, 0.0, 0.0), eye(1.0, close, 0.0, 0.0)));
        let names = order(&list);
        if close < 0.93 {
            assert!(names.contains(&"lash"), "close {close}: {names:?}");
        } else {
            assert!(names.contains(&"closed"), "close {close}: {names:?}");
        }
    }
}

// ------------------------------------------------------------------------------------------------ allocation

fn count_allocations(f: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.with(Cell::get);
    MEASURING.with(|m| m.set(true));
    f();
    MEASURING.with(|m| m.set(false));
    ALLOCATIONS.with(Cell::get) - before
}

#[test]
fn painting_the_lids_lashes_and_closed_eyes_allocates_nothing_once_the_list_is_warm() {
    let mut p = profile();
    smile_lids(&mut p);
    let poses = [
        rest(),
        params(eye(1.0, 0.0, 0.0, 0.6), eye(1.0, 0.0, 0.0, 0.6)),
        params(eye(1.0, 0.0, 1.0, 0.0), eye(1.0, 0.0, 0.0, 0.0)),
        params(eye(1.0, 1.0, 0.0, 0.0), eye(0.3, 0.0, 0.0, 0.0)),
    ];
    let mut out = FaceDrawing::new();
    for pose in &poses {
        paint_into(&p, pose, &mut out);
    }
    let mut painter = RealPainter::new(mock_projection(), DEFAULT_PX_PER_M);
    let allocations = count_allocations(|| {
        for i in 0..1000 {
            let input = FaceInput {
                profile: &p,
                params: &poses[i % poses.len()],
                time: Seconds(0.0),
                view: FaceView::default(),
            };
            painter.paint_section(Section::Eyes, &input, &mut out).unwrap();
            black_box(&out);
        }
    });
    assert_eq!(allocations, 0, "1000 eye paints made {allocations} allocations");
}

#[test]
fn the_counter_sees_an_allocation() {
    let allocations = count_allocations(|| {
        black_box(Vec::<u8>::with_capacity(64));
    });
    assert_eq!(allocations, 1, "without this the zero-allocation test proves nothing");
}
