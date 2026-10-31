//! The cheeks, nose and marks sections: where the prototype's numbers land in the face plane, the blush kinds,
//! the nose styles, marks that are deterministic and stay on the face, legible blush on every skin tone, and painting
//! that allocates nothing once warm.
//!
//! The allocator counts only allocations made on the measuring thread, as in `hot_loop.rs`: the harness's own threads
//! allocate at unpredictable times, which would make a process-wide count flaky under load.
//!
//! `blush_over_skin_tones_contact_sheet` writes the picture for review and is run by hand (`--ignored`).

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
// Tests may unwrap: a panic is a clear failure.
#![allow(unsafe_code, clippy::unwrap_used, clippy::expect_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use omuzaki_anim_api::ChannelBuf;
use omuzaki_core::colour::{Colour, to_byte};
use omuzaki_core::glam::Vec2;
use omuzaki_core::time::Seconds;
use omuzaki_face::{RealPainter, Section};
use omuzaki_face_api::{
    BlushKind, Cmd, ColourRef, DEFAULT_PX_PER_M, DrawList, FaceDrawing, FaceInput, FaceLayerId, FaceMark, FacePainter,
    FaceProfile, FaceProjection, FaceRect, FaceView, MarkKind, NoseKind, Paint, PaintParams, PathEl, narrow,
};
use omuzaki_face_mock::{mock_profile, mock_projection};

// ------------------------------------------------------------------------------------------------ allocation counting

struct Counting;

thread_local! {
    // `const`-initialised without destructors, so touching them never allocates (this runs inside the allocator).
    static MEASURING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

/// Runs `f` and returns how many allocations this thread made during it.
fn count_allocations(f: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.with(Cell::get);
    MEASURING.with(|m| m.set(true));
    f();
    MEASURING.with(|m| m.set(false));
    ALLOCATIONS.with(Cell::get) - before
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

/// The prototype's blush parameter at rest (`BASE.blush`).
const BLUSH: f32 = 0.75;

fn params(blush: f32) -> PaintParams {
    let mut p = mock_profile().mapping.apply(&ChannelBuf::NEUTRAL);
    p.blush = blush;
    p
}

fn paint_with(
    section: Section,
    profile: &FaceProfile,
    params: &PaintParams,
    projection: FaceProjection,
    out: &mut FaceDrawing,
) {
    let mut painter = RealPainter::new(projection, DEFAULT_PX_PER_M);
    let input = FaceInput { profile, params, time: Seconds(0.0), view: FaceView::default() };
    painter.paint_section(section, &input, out).unwrap();
}

/// The Base layer of `section` painted alone.
fn base(section: Section, profile: &FaceProfile, blush: f32) -> DrawList {
    let mut out = FaceDrawing::default();
    paint_with(section, profile, &params(blush), mock_projection(), &mut out);
    assert!(out[FaceLayerId::Brows].is_empty() && out[FaceLayerId::Overlay].is_empty(), "only the Base layer is used");
    out[FaceLayerId::Base].clone()
}

fn face_rect() -> FaceRect {
    RealPainter::new(mock_projection(), DEFAULT_PX_PER_M).layers()[FaceLayerId::Base as usize].rect
}

/// Every point a list's paths touch.
fn points(list: &DrawList) -> Vec<Vec2> {
    list.cmds()
        .iter()
        .filter_map(|cmd| match cmd {
            Cmd::Fill { path, .. } | Cmd::Stroke { path, .. } | Cmd::PushClip(path) => Some(points_of(list, *path)),
            Cmd::PopClip | Cmd::PushTransform(_) | Cmd::PopTransform => None,
        })
        .flatten()
        .collect()
}

/// The fills with a radial paint: the wash and the soft patches.
fn radials(list: &DrawList) -> Vec<(Vec2, f32, Vec<u8>)> {
    list.cmds()
        .iter()
        .filter_map(|c| match c {
            Cmd::Fill { paint: Paint::Radial { centre, radius, stops }, .. } => {
                Some((*centre, *radius, stops.as_slice().iter().map(|s| s.colour.a).collect()))
            }
            _ => None,
        })
        .collect()
}

fn count(list: &DrawList, pick: impl Fn(&Cmd) -> bool) -> usize {
    list.cmds().iter().filter(|c| pick(c)).count()
}

fn profile_with(f: impl FnOnce(&mut FaceProfile)) -> FaceProfile {
    let mut profile = mock_profile();
    f(&mut profile);
    profile
}

// ------------------------------------------------------------------------------------------------ blush, soft

#[test]
fn soft_blush_is_the_wash_then_two_squashed_radial_patches() {
    let profile = profile_with(|p| {
        p.layout.blush_y = -0.133;
        p.layout.blush_spacing = 0.1;
    });
    let list = base(Section::Cheeks, &profile, BLUSH);
    let radial = radials(&list);
    assert_eq!(radial.len(), 3, "the wash and one patch per cheek");

    let (centre, radius, alphas) = &radial[0];
    assert!((centre.y - (-0.133 - 0.005)).abs() < 1e-6 && centre.x == 0.0, "the wash sits just under the blush");
    assert_eq!(*radius, 0.17);
    assert_eq!(alphas, &[to_byte(0.3), 0]);

    for (patch, side) in radial[1..].iter().zip([1.0, -1.0]) {
        assert!((patch.0.x - side * 0.1).abs() < 1e-6 && (patch.0.y + 0.133).abs() < 1e-6, "{:?}", patch.0);
        assert!((patch.1 - 0.068).abs() < 1e-6, "the disc has the patch's half-width");
        // 0.95 x blush at the centre, 0.6 of it at the half way stop, none at the rim.
        let centre_alpha = to_byte(0.95 * BLUSH);
        assert_eq!(patch.2, [centre_alpha, to_byte(0.95 * BLUSH * 0.6), 0]);
    }
    assert_eq!(count(&list, |c| matches!(c, Cmd::PushTransform(_))), 2, "each disc is squashed to an ellipse");
    assert_eq!(count(&list, |c| matches!(c, Cmd::PushTransform(_))), count(&list, |c| matches!(c, Cmd::PopTransform)));
}

#[test]
fn the_squash_maps_the_disc_to_the_patch_ellipse() {
    let profile = mock_profile();
    let list = base(Section::Cheeks, &profile, BLUSH);
    let squash =
        list.cmds().iter().find_map(|c| if let Cmd::PushTransform(m) = c { Some(*m) } else { None }).expect("a squash");
    let centre = Vec2::new(narrow(profile.layout.blush_spacing), narrow(profile.layout.blush_y));
    let (rx, ry) = (narrow(profile.cheeks.radius[0]), narrow(profile.cheeks.radius[1]));
    let top = squash.transform_point2(centre + Vec2::new(0.0, rx));
    assert!((top - (centre + Vec2::new(0.0, ry))).length() < 1e-6, "the disc's top lands on the ellipse's");
    assert!((squash.transform_point2(centre + Vec2::new(rx, 0.0)) - (centre + Vec2::new(rx, 0.0))).length() < 1e-6);
}

#[test]
fn blush_strength_scales_with_the_blush_channel_and_a_strong_blush_saturates() {
    let profile = mock_profile();
    let top = |blush| radials(&base(Section::Cheeks, &profile, blush))[1].2[0];
    assert_eq!(top(0.5), to_byte(0.95 * 0.5));
    assert_eq!(top(1.6), 255, "the prototype's rgba() clamps alpha at 1");
}

#[test]
fn the_wash_follows_its_own_opacity_and_zero_draws_none() {
    let none = profile_with(|p| p.cheeks.wash = 0.0);
    let list = base(Section::Cheeks, &none, BLUSH);
    assert_eq!(radials(&list).len(), 2, "only the patches");
    let half = profile_with(|p| p.cheeks.wash = 0.5);
    assert_eq!(radials(&base(Section::Cheeks, &half, BLUSH))[0].2[0], to_byte(0.5));
}

#[test]
fn the_blush_colour_comes_from_the_style() {
    let profile = profile_with(|p| p.cheeks.colour = ColourRef::rgb(10, 20, 30));
    let list = base(Section::Cheeks, &profile, BLUSH);
    let patch = list
        .cmds()
        .iter()
        .filter_map(|c| match c {
            Cmd::Fill { paint: Paint::Radial { stops, .. }, .. } => Some(stops.as_slice()[0].colour),
            _ => None,
        })
        .nth(1)
        .unwrap();
    assert_eq!((patch.r, patch.g, patch.b), (10, 20, 30));
}

/// The strongest alpha byte of a blush patch: the soft patch's centre stop (the second radial fill, after the wash)
/// or a hatch line.
fn patch_peak(list: &DrawList) -> Option<u8> {
    let hatch = list.cmds().iter().find_map(|c| match c {
        Cmd::Stroke { paint: Paint::Solid(colour), .. } => Some(colour.a),
        _ => None,
    });
    hatch.or_else(|| radials(list).get(1).map(|patch| patch.2[0]))
}

#[test]
fn the_blush_colours_own_alpha_scales_both_kinds_after_coverage_saturates() {
    for kind in [BlushKind::Soft, BlushKind::Hatched] {
        let with_alpha = |a| {
            profile_with(|p| {
                p.cheeks.kind = kind;
                p.cheeks.colour = ColourRef::rgba(240, 140, 132, a);
            })
        };
        // Fully transparent: no patch at all, only the wash and the fringe shade remain.
        for blush in [BLUSH, 1.6] {
            let none = base(Section::Cheeks, &with_alpha(0), blush);
            assert_eq!(count(&none, |c| matches!(c, Cmd::Stroke { .. } | Cmd::PushClip(_) | Cmd::PushTransform(_))), 0);
            assert_eq!(radials(&none).len(), 1, "{kind:?}: only the wash");
        }
        // Translucent: coverage saturates at 1 first (blush 1.6 x 0.95 > 1), then the alpha scales it, so the peak
        // is the colour's alpha, never above it.
        for (blush, coverage) in [(BLUSH, 0.95 * BLUSH), (1.6, 1.0)] {
            for alpha in [128_u8, 255] {
                let list = base(Section::Cheeks, &with_alpha(alpha), blush);
                let peak = patch_peak(&list).unwrap();
                let expected = to_byte(coverage * f32::from(alpha) / 255.0);
                assert!(peak <= alpha, "{kind:?} blush {blush}: peak {peak} above alpha {alpha}");
                assert!(peak.abs_diff(expected) <= 1, "{kind:?} blush {blush} alpha {alpha}: {peak} vs {expected}");
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------ blush, hatched

fn hatched() -> FaceProfile {
    profile_with(|p| p.cheeks.kind = BlushKind::Hatched)
}

#[test]
fn hatched_blush_is_diagonal_lines_clipped_to_each_patch() {
    let list = base(Section::Cheeks, &hatched(), BLUSH);
    assert_eq!(count(&list, |c| matches!(c, Cmd::PushClip(_))), 2, "one clip per cheek");
    assert_eq!(count(&list, |c| matches!(c, Cmd::PopClip)), 2);
    assert_eq!(count(&list, |c| matches!(c, Cmd::Stroke { .. })), 2 * 13, "the same lines at any radius");
    assert_eq!(radials(&list).len(), 1, "no soft patches: only the wash");

    // Every line is a single segment, steeper than horizontal, and all of one slant.
    let mut slants = Vec::new();
    for cmd in list.cmds() {
        if let Cmd::Stroke { path, width, .. } = cmd {
            let els = list.elements(*path);
            let (PathEl::Move(a), PathEl::Line(b)) = (els[0], els[1]) else { panic!("a line: {els:?}") };
            let d = (b - a).normalize();
            assert!(d.y.abs() > d.x.abs(), "steeper than 45 degrees for a wide patch: {d:?}");
            slants.push(d);
            assert!(*width > 0.0);
        }
    }
    assert!(slants.windows(2).all(|w| (w[0] - w[1]).length() < 1e-5));
}

#[test]
fn hatched_lines_cover_the_patch_from_edge_to_edge() {
    let profile = hatched();
    let list = base(Section::Cheeks, &profile, BLUSH);
    let spacing = narrow(profile.layout.blush_spacing);
    let centre = Vec2::new(spacing, narrow(profile.layout.blush_y));
    let lines: Vec<(Vec2, Vec2)> = list
        .cmds()
        .iter()
        .filter_map(|c| match c {
            Cmd::Stroke { path, .. } => match list.elements(*path) {
                [PathEl::Move(a), PathEl::Line(b)] => Some((*a, *b)),
                _ => None,
            },
            _ => None,
        })
        .filter(|(a, b)| ((*a + *b) / 2.0 - centre).length() < 0.07)
        .collect();
    assert_eq!(lines.len(), 13);
    let (rx, ry) = (narrow(profile.cheeks.radius[0]), narrow(profile.cheeks.radius[1]));
    // The outermost lines touch the ellipse (a tangent), the middle one passes through its centre.
    let side = |l: &(Vec2, Vec2)| {
        let mid = (l.0 + l.1) / 2.0 - centre;
        mid.x.hypot(mid.y)
    };
    assert!(side(&lines[6]) < 1e-5, "the middle line goes through the centre");
    let tangent = side(&lines[0]);
    assert!(tangent <= rx.max(ry) && tangent > ry * 0.9, "the first line grazes the ellipse: {tangent}");
}

#[test]
fn hatched_blush_at_strength_zero_draws_no_patch() {
    for profile in [hatched(), profile_with(|p| p.cheeks.strength = 0.0)] {
        let list = base(Section::Cheeks, &profile, 0.0);
        assert_eq!(count(&list, |c| matches!(c, Cmd::Stroke { .. } | Cmd::PushClip(_))), 0);
        assert_eq!(radials(&list).len(), 1, "the wash and the fringe shade are not blush");
    }
    let weak = profile_with(|p| {
        p.cheeks.kind = BlushKind::Hatched;
        p.cheeks.strength = 0.0;
    });
    assert_eq!(count(&base(Section::Cheeks, &weak, 1.0), |c| matches!(c, Cmd::Stroke { .. })), 0);
}

// ------------------------------------------------------------------------------------------------ puffed cheeks

/// The cheeks section for `puff` (and the default blush), on the Base layer.
fn puffed(puff: f32) -> DrawList {
    let mut p = params(BLUSH);
    p.cheek_puff = puff;
    let mut out = FaceDrawing::default();
    paint_with(Section::Cheeks, &mock_profile(), &p, mock_projection(), &mut out);
    out[FaceLayerId::Base].clone()
}

/// The alpha of every solid stroke and fill, in order.
fn solid_alphas(list: &DrawList) -> Vec<u8> {
    list.cmds()
        .iter()
        .filter_map(|c| match c {
            Cmd::Stroke { paint: Paint::Solid(c), .. } | Cmd::Fill { paint: Paint::Solid(c), .. } => Some(c.a),
            _ => None,
        })
        .collect()
}

#[test]
fn the_puffed_rim_keeps_the_alpha_of_the_blush_colour() {
    for alpha in [0_u8, 100] {
        let mut profile = mock_profile();
        profile.cheeks.colour = ColourRef::Fixed(Colour::rgba(200, 100, 50, alpha));
        let mut p = params(BLUSH);
        p.cheek_puff = 1.0;
        let mut out = FaceDrawing::default();
        paint_with(Section::Cheeks, &profile, &p, mock_projection(), &mut out);
        // The rim line is the blush colour darkened: its alpha can never exceed the blush colour's own.
        let rim: Vec<u8> = out[FaceLayerId::Base]
            .cmds()
            .iter()
            .filter_map(|c| match c {
                Cmd::Stroke { paint: Paint::Solid(c), .. }
                    if c.r < 200 && c.g < 100 && c.b < 50 && (c.r, c.g, c.b) != (0, 0, 0) =>
                {
                    Some(c.a)
                }
                _ => None,
            })
            .collect();
        assert!(!rim.is_empty(), "alpha {alpha}: no rim line was drawn");
        assert!(rim.iter().all(|&a| a <= alpha), "alpha {alpha}: the rim is more opaque than the blush: {rim:?}");
    }
}

#[test]
fn no_puff_adds_nothing_and_a_puff_adds_a_rim_and_a_highlight_to_each_cheek() {
    let (rest, none) = (puffed(0.0), base(Section::Cheeks, &mock_profile(), BLUSH));
    assert_eq!(rest.cmds(), none.cmds(), "a puff of 0 paints the same list as before");
    let full = puffed(1.0);
    // Per cheek: the soft shade, the crisp line and the highlight.
    assert_eq!(full.cmds().len(), rest.cmds().len() + 6);
    assert_eq!(count(&full, |c| matches!(c, Cmd::Stroke { .. })), 4);
}

#[test]
fn the_puff_scales_smoothly_with_the_channel() {
    let (half, full) = (solid_alphas(&puffed(0.5)), solid_alphas(&puffed(1.0)));
    assert_eq!(half.len(), full.len());
    // The last three of each cheek's commands are the puff's: shade, line, highlight; they scale by the channel.
    let n = full.len();
    for i in [n - 1, n - 2, n - 3, n - 4, n - 5, n - 6] {
        let (h, f) = (f32::from(half[i]), f32::from(full[i]));
        assert!((h - f * 0.5).abs() <= 2.0, "command {i}: {h} against {f}");
    }
}

#[test]
fn the_two_puffed_cheeks_mirror_each_other_and_stay_on_the_face() {
    let list = puffed(1.0);
    let rect = face_rect();
    let pts = points(&list);
    assert!(pts.iter().all(|p| p.x >= rect.x0 && p.x <= rect.x1 && p.y >= rect.y0 && p.y <= rect.y1));
    // The rim strokes are the polylines among the strokes: the left cheek's mirrors the right's.
    let rims: Vec<Vec<Vec2>> = list
        .cmds()
        .iter()
        .filter_map(|c| match c {
            Cmd::Stroke { path, width, .. } if *width > 0.005 => Some(points_of(&list, *path)),
            _ => None,
        })
        .collect();
    assert_eq!(rims.len(), 2);
    for (a, b) in rims[0].iter().zip(&rims[1]) {
        assert!((a.x + b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6, "{a:?} against {b:?}");
    }
}

// ------------------------------------------------------------------------------------------------ the fringe shade

#[test]
fn the_fringe_shade_is_a_strip_under_the_top_of_the_face_in_the_face_plane() {
    let list = base(Section::Cheeks, &mock_profile(), BLUSH);
    let rect = face_rect();
    let shade = list.cmds().last().unwrap();
    let Cmd::Fill { path, paint: Paint::Linear { from, to, stops }, .. } = shade else { panic!("{shade:?}") };
    // The prototype's 0.86 and 0.80 in its head frame, whose origin is 0.778 above the face plane's.
    assert!((from.y - 0.082).abs() < 1e-6 && (to.y - 0.022).abs() < 1e-6, "{from} {to}");
    assert_eq!(stops.as_slice().iter().map(|s| s.colour.a).collect::<Vec<_>>(), [to_byte(0.3), 0]);
    let strip = &points_of(&list, *path);
    assert!(strip.iter().all(|p| p.x >= rect.x0 && p.x <= rect.x1 && p.y <= rect.y1 + 1e-6));
    let (lo, hi) = strip.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p.y), hi.max(p.y)));
    assert!((lo - 0.022).abs() < 1e-6 && (hi - rect.y1).abs() < 1e-6, "from the rect's top to where the fade ends");
}

fn points_of(list: &DrawList, path: omuzaki_face_api::PathRef) -> Vec<Vec2> {
    list.elements(path)
        .iter()
        .flat_map(|e| match *e {
            PathEl::Move(p) | PathEl::Line(p) => vec![p],
            PathEl::Quad(c, p) => vec![c, p],
            PathEl::Cubic(c1, c2, p) => vec![c1, c2, p],
            PathEl::Close => vec![],
        })
        .collect()
}

// ------------------------------------------------------------------------------------------------ the nose

fn nose(kind: NoseKind, size: f64) -> (FaceProfile, DrawList) {
    let profile = profile_with(|p| {
        p.nose.kind = kind;
        p.nose.size = size;
        p.layout.nose_y = -0.1185;
    });
    let list = base(Section::Nose, &profile, BLUSH);
    (profile, list)
}

#[test]
fn the_tick_is_a_warm_stroke_and_a_dot_of_light_at_the_prototype_offsets() {
    let (_, list) = nose(NoseKind::Tick, 1.0);
    let [
        Cmd::Stroke { path, paint, width, cap, opacity, .. },
        Cmd::Fill { path: dot, paint: light, opacity: light_opacity },
    ] = list.cmds()
    else {
        panic!("{:?}", list.cmds())
    };
    assert_eq!(*paint, Paint::Solid(Colour::rgb(0xbe, 0x78, 0x78)));
    assert_eq!((*width, *opacity, *cap), (0.0012, 0.8, omuzaki_face_api::Cap::Round));
    let [PathEl::Move(a), PathEl::Line(b)] = list.elements(*path) else { panic!() };
    assert!((*a - Vec2::new(0.0012, -0.1185 + 0.0017)).length() < 1e-6, "{a}");
    assert!((*b - Vec2::new(-0.0006, -0.1185 - 0.0017)).length() < 1e-6, "{b}");
    assert_eq!(*light, Paint::Solid(Colour::rgb(255, 250, 250)));
    assert_eq!(*light_opacity, 0.7);
    let dot_points = points_of(&list, *dot);
    // The middle of the disc's bounding box (a cubic circle's control points are symmetric about it).
    let lo = dot_points.iter().fold(Vec2::MAX, |m, p| m.min(*p));
    let hi = dot_points.iter().fold(Vec2::MIN, |m, p| m.max(*p));
    let centre = (lo + hi) / 2.0;
    assert!((centre - Vec2::new(0.0028, -0.1185 + 0.004)).length() < 1e-5, "{centre}");
}

#[test]
fn the_dot_is_one_warm_disc_on_the_nose_line() {
    let (_, list) = nose(NoseKind::Dot, 1.0);
    let [Cmd::Fill { paint, .. }] = list.cmds() else { panic!("{:?}", list.cmds()) };
    assert_eq!(*paint, Paint::Solid(Colour::rgb(0xbe, 0x78, 0x78)));
    let pts = points(&list);
    assert!(pts.iter().all(|p| (p.y + 0.1185).abs() <= 0.0011 + 1e-6 && p.x.abs() <= 0.0011 + 1e-6), "{pts:?}");
}

#[test]
fn no_nose_draws_nothing_and_size_scales_the_tick() {
    assert!(nose(NoseKind::None, 1.0).1.is_empty());
    let (_, big) = nose(NoseKind::Tick, 2.0);
    let Cmd::Stroke { path, width, .. } = big.cmds()[0] else { panic!() };
    assert_eq!(width, 0.0024);
    let [PathEl::Move(a), PathEl::Line(b)] = big.elements(path) else { panic!() };
    assert!(((*a - *b).length() - 2.0 * (Vec2::new(0.0018, 0.0034)).length()).abs() < 1e-6);
}

// ------------------------------------------------------------------------------------------------ marks

/// A circuit trace: across, a right-angle bend down, and on to the corner, with pads at its ends.
const TRACE: [[f64; 2]; 4] = [[-1.0, 0.0], [0.2, 0.0], [0.5, -0.3], [0.5, -0.8]];

fn mark(kind: MarkKind, at: [f64; 2], size: f64) -> FaceMark {
    let points = if kind == MarkKind::Line { TRACE.to_vec() } else { Vec::new() };
    FaceMark { kind, at, size, points, pads: 3.0, ..FaceMark::default() }
}

fn marks(marks: Vec<FaceMark>) -> DrawList {
    base(Section::Marks, &profile_with(|p| p.marks = marks), BLUSH)
}

#[test]
fn every_kind_of_mark_draws_and_a_profile_without_marks_draws_nothing() {
    assert!(marks(vec![]).is_empty());
    for kind in [MarkKind::Freckles, MarkKind::BeautyMark, MarkKind::Paint, MarkKind::Scar, MarkKind::Line] {
        assert!(!marks(vec![mark(kind, [0.05, -0.08], 0.01)]).is_empty(), "{kind:?}");
    }
}

#[test]
fn freckles_are_count_dots_placed_from_the_seed_alone() {
    let freckles = |count, seed| FaceMark { count, seed, ..mark(MarkKind::Freckles, [0.06, -0.1], 0.02) };
    let a = marks(vec![freckles(17, 7)]);
    assert_eq!(count(&a, |c| matches!(c, Cmd::Fill { .. })), 17);
    assert_eq!(points(&a), points(&marks(vec![freckles(17, 7)])), "the same seed gives the same dots");
    assert_ne!(points(&a), points(&marks(vec![freckles(17, 8)])), "another seed, another scatter");
    assert_ne!(points(&marks(vec![freckles(5, 0)])), points(&marks(vec![freckles(5, 1)])));
    // The dots lie in the patch.
    let centre = Vec2::new(0.06, -0.1);
    assert!(points(&a).iter().all(|p| (*p - centre).length() <= 0.02 * 1.3));
}

#[test]
fn marks_with_no_opacity_or_size_draw_nothing() {
    let none = FaceMark { opacity: 0.0, ..mark(MarkKind::Scar, [0.0, 0.0], 0.01) };
    assert!(marks(vec![none]).is_empty());
    let tiny = FaceMark { size: 0.0, ..FaceMark::default() };
    assert!(marks(vec![tiny]).is_empty());
}

#[test]
fn paint_shape_follows_the_seed() {
    let paint = |seed| FaceMark { seed, ..mark(MarkKind::Paint, [0.05, -0.1], 0.02) };
    let stripes = marks(vec![paint(0)]);
    let star = marks(vec![paint(1)]);
    assert_eq!(count(&stripes, |c| matches!(c, Cmd::Fill { .. })), 3, "three bars");
    assert_eq!(count(&star, |c| matches!(c, Cmd::Fill { .. })), 1, "one star");
    assert_eq!(points(&star).len(), 10, "five points, five notches");
    assert_eq!(points(&stripes), points(&marks(vec![paint(2)])), "the shape repeats every second seed");
}

#[test]
fn a_scar_is_a_line_with_cross_ticks() {
    let list = marks(vec![mark(MarkKind::Scar, [-0.05, -0.05], 0.03)]);
    assert_eq!(count(&list, |c| matches!(c, Cmd::Stroke { .. })), 5, "the line and four ticks");
}

#[test]
fn a_line_runs_through_its_points_with_a_pad_at_each_end() {
    let size = 0.02;
    let line = mark(MarkKind::Line, [0.06, -0.1], size);
    let list = marks(vec![line.clone()]);
    assert_eq!(count(&list, |c| matches!(c, Cmd::Stroke { .. })), 1, "one stroke through every point");
    assert_eq!(count(&list, |c| matches!(c, Cmd::Fill { .. })), 2, "a pad at each end");
    let centre = Vec2::new(0.06, -0.1);
    let drawn = points(&list);
    for p in TRACE {
        #[allow(clippy::cast_possible_truncation)]
        let want = centre + Vec2::new(p[0] as f32, p[1] as f32) * size as f32;
        assert!(drawn.iter().any(|q| (*q - want).length() < 1e-6), "{want} is on the line");
    }
    assert_eq!(points(&list), points(&marks(vec![line.clone()])), "a line is a pure function of its fields");
    let bare = marks(vec![FaceMark { pads: 0.0, ..line.clone() }]);
    assert_eq!(count(&bare, |c| matches!(c, Cmd::Fill { .. })), 0, "no pads asked, none drawn");
    let one_point = marks(vec![FaceMark { points: vec![[0.0, 0.0]], ..line }]);
    assert!(one_point.is_empty(), "a line needs two points");
}

#[test]
fn a_line_mark_is_checked() {
    use omuzaki_face_api::Validate;
    let bad = |m: FaceMark| {
        let mut c = omuzaki_face_api::Check::new();
        m.check(&mut c);
        c.finish().is_err()
    };
    let line = mark(MarkKind::Line, [0.0, 0.0], 0.02);
    assert!(!bad(line.clone()));
    assert!(bad(FaceMark { points: vec![[0.0, 0.0]], ..line.clone() }), "one point");
    assert!(bad(FaceMark { points: vec![[0.0, 0.0], [1.5, 0.0]], ..line.clone() }), "a point beyond 1");
    assert!(bad(FaceMark { points: vec![[0.0, 0.0]; 33], ..line.clone() }), "too many points");
    assert!(bad(FaceMark { width: 0.05, ..line }), "too wide");
}

#[test]
fn every_mark_stays_inside_the_face_rect_wherever_it_is_put() {
    let rect = face_rect();
    let spots = [[-0.3, -0.4], [0.3, 0.3], [0.3, -0.4], [-0.3, 0.3], [0.0, 0.0]];
    for kind in [MarkKind::Freckles, MarkKind::BeautyMark, MarkKind::Paint, MarkKind::Scar, MarkKind::Line] {
        for at in spots {
            for size in [0.0003, 0.01, 0.1] {
                for seed in [0, 1, 99] {
                    let list = marks(vec![FaceMark { seed, count: 40, ..mark(kind, at, size) }]);
                    for p in points(&list) {
                        assert!(
                            p.x >= rect.x0 && p.x <= rect.x1 && p.y >= rect.y0 && p.y <= rect.y1,
                            "{kind:?} at {at:?} size {size} seed {seed}: {p} outside {rect:?}"
                        );
                    }
                }
            }
        }
    }
}

/// Every mark kind, big and in the corners, in rects far narrower than the mark: its points and its strokes' half
/// widths stay inside, because a mark that cannot fit at its size is scaled down.
#[test]
fn marks_fit_narrow_rects_stroke_included() {
    let rects = [
        FaceRect { x0: -0.05, x1: 0.05, y0: -0.2, y1: 0.1 },
        FaceRect { x0: -0.2, x1: 0.2, y0: -0.01, y1: 0.01 },
        FaceRect { x0: 0.0, x1: 0.001, y0: 0.0, y1: 0.001 },
    ];
    for rect in rects {
        let mut projection = mock_projection();
        projection.rect = rect;
        for kind in [MarkKind::Freckles, MarkKind::BeautyMark, MarkKind::Paint, MarkKind::Scar, MarkKind::Line] {
            for seed in [0, 1] {
                let profile = profile_with(|p| {
                    p.marks = vec![FaceMark { seed, count: 30, ..mark(kind, [0.0, 0.0], 0.05) }];
                });
                let mut out = FaceDrawing::default();
                paint_with(Section::Marks, &profile, &params(BLUSH), projection, &mut out);
                let list = &out[FaceLayerId::Base];
                for cmd in list.cmds() {
                    let (path, half) = match cmd {
                        Cmd::Fill { path, .. } => (*path, 0.0),
                        Cmd::Stroke { path, width, .. } => (*path, width / 2.0),
                        _ => continue,
                    };
                    for p in points_of(list, path) {
                        let eps = 1e-6;
                        assert!(
                            p.x - half >= rect.x0 - eps
                                && p.x + half <= rect.x1 + eps
                                && p.y - half >= rect.y0 - eps
                                && p.y + half <= rect.y1 + eps,
                            "{kind:?} seed {seed} in {rect:?}: {p} with half width {half}"
                        );
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------ legibility

/// sRGB bytes to CIE L*a*b* (D65).
#[allow(clippy::many_single_char_names)] // the standard's own names
fn lab(rgb: [f32; 3]) -> [f32; 3] {
    let lin = |c: f32| {
        let c = c / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let (r, g, b) = (lin(rgb[0]), lin(rgb[1]), lin(rgb[2]));
    let (x, y, z) = (
        (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047,
        0.2126 * r + 0.7152 * g + 0.0722 * b,
        (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883,
    );
    let f = |t: f32| if t > 0.008_856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
    [116.0 * f(y) - 16.0, 500.0 * (f(x) - f(y)), 200.0 * (f(y) - f(z))]
}

/// The skin with the blush's centre laid over it: what is seen at the middle of a cheek.
fn blushed(skin: [u8; 3], style: &omuzaki_face_api::CheekStyle, blush: f32) -> [f32; 3] {
    let ColourRef::Fixed(c) = style.colour else { panic!("a fixed colour") };
    let alpha = (narrow(style.strength) * blush).min(1.0);
    let mix = |s: u8, b: u8| f32::from(s) * (1.0 - alpha) + f32::from(b) * alpha;
    [mix(skin[0], c.r), mix(skin[1], c.g), mix(skin[2], c.b)]
}

fn delta_e(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (a, b) = (lab(a), lab(b));
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Four skin tones from the lightest to the darkest the character creator offers; the contact sheet and this test use
/// them until the skin-tone op provides the twelve.
const SKIN_TONES: [[u8; 3]; 4] = [[255, 224, 205], [224, 172, 140], [160, 106, 78], [78, 50, 38]];

#[test]
fn the_default_blush_is_legible_on_the_lightest_and_the_darkest_skin() {
    let style = omuzaki_face_api::CheekStyle::default();
    for skin in SKIN_TONES {
        let seen = delta_e(skin.map(f32::from), blushed(skin, &style, BLUSH));
        assert!(seen >= 10.0, "skin {skin:?}: delta E {seen}");
    }
}

// ------------------------------------------------------------------------------------------------ no allocation

#[test]
fn painting_the_three_sections_allocates_nothing_once_warm() {
    let profile = profile_with(|p| {
        p.nose.kind = NoseKind::Tick;
        p.marks = vec![
            FaceMark { count: 30, ..mark(MarkKind::Freckles, [0.05, -0.09], 0.02) },
            mark(MarkKind::BeautyMark, [0.07, -0.13], 0.003),
            mark(MarkKind::Paint, [-0.08, -0.1], 0.02),
            FaceMark { seed: 1, ..mark(MarkKind::Paint, [-0.1, -0.1], 0.02) },
            mark(MarkKind::Scar, [-0.05, -0.05], 0.03),
        ];
    });
    let hatched_profile = profile_with(|p| p.cheeks.kind = BlushKind::Hatched);
    let projection = mock_projection();
    let p = params(BLUSH);
    let mut painter = RealPainter::new(projection, DEFAULT_PX_PER_M);
    let mut out = FaceDrawing::default();
    let paint_all = |painter: &mut RealPainter, out: &mut FaceDrawing| {
        for (section, profile) in [
            (Section::Cheeks, &profile),
            (Section::Cheeks, &hatched_profile),
            (Section::Nose, &profile),
            (Section::Marks, &profile),
        ] {
            let input = FaceInput { profile, params: &p, time: Seconds(0.0), view: FaceView::default() };
            painter.paint_section(section, &input, out).unwrap();
        }
    };
    // The largest of the lists decides the capacity: warm each once, then measure all.
    paint_all(&mut painter, &mut out);
    paint_all(&mut painter, &mut out);
    let allocations = count_allocations(|| {
        for _ in 0..10 {
            paint_all(&mut painter, &mut out);
        }
    });
    assert_eq!(allocations, 0, "a warm painter must not allocate");
}

// ------------------------------------------------------------------------------------------------ the contact sheet

/// `sections` of `profile` over a `skin` coloured face window, rasterised on the CPU at 1200 px/m.
fn render_cell(
    projection: FaceProjection,
    profile: &FaceProfile,
    blush: f32,
    skin: [u8; 3],
    sections: &[Section],
) -> image::RgbaImage {
    use omuzaki_face::raster::SkiaRasterizer;
    use omuzaki_face_api::{FaceRasterizer, Raster};

    let mut painter = RealPainter::new(projection, 1200.0);
    let spec = painter.layers()[FaceLayerId::Base as usize];
    let (w, h) = spec.size_px();
    let mut rasterizer = SkiaRasterizer::new();
    let mut cell = image::RgbaImage::from_pixel(w, h, image::Rgba([skin[0], skin[1], skin[2], 255]));
    let p = params(blush);
    let input = FaceInput { profile, params: &p, time: Seconds(0.0), view: FaceView::default() };
    for section in sections {
        let mut drawing = FaceDrawing::default();
        painter.paint_section(*section, &input, &mut drawing).unwrap();
        let mut raster = Raster::new(1, 1);
        rasterizer.rasterize(&spec, &drawing[FaceLayerId::Base], spec.px_per_m, &mut raster).unwrap();
        for (dst, src) in cell.pixels_mut().zip(raster.pixels.as_chunks::<4>().0.iter()) {
            // Premultiplied over opaque: src + dst * (1 - alpha).
            let keep = 255 - u32::from(src[3]);
            let under = dst.0;
            #[allow(clippy::needless_range_loop)] // three channels of two arrays
            for channel in 0..3 {
                let sum = u32::from(src[channel]) + u32::from(under[channel]) * keep / 255;
                dst.0[channel] = u8::try_from(sum.min(255)).unwrap_or(u8::MAX);
            }
        }
    }
    cell
}

/// Puts `cells` side by side, top aligned, on a dark ground.
fn strip(cells: &[image::RgbaImage]) -> image::RgbaImage {
    let width = cells.iter().map(image::RgbaImage::width).sum();
    let height = cells.iter().map(image::RgbaImage::height).max().unwrap_or(1);
    let mut sheet = image::RgbaImage::from_pixel(width, height, image::Rgba([40, 40, 40, 255]));
    let mut x = 0;
    for cell in cells {
        image::imageops::replace(&mut sheet, cell, i64::from(x), 0);
        x += cell.width();
    }
    sheet
}

/// Blush kinds (rows) over the skin tones (columns) with the nose, to the `.png` named by `OMUZAKI_CONTACT_SHEET_OUT`.
#[test]
#[ignore = "writes a picture to a local assets folder; run by hand with OMUZAKI_CONTACT_SHEET_OUT=<file>.png"]
fn blush_over_skin_tones_contact_sheet() {
    let out_path = std::env::var_os("OMUZAKI_CONTACT_SHEET_OUT").expect("OMUZAKI_CONTACT_SHEET_OUT");
    let rows: Vec<image::RgbaImage> = [BlushKind::Soft, BlushKind::Hatched]
        .into_iter()
        .map(|kind| {
            let profile = profile_with(|p| {
                p.cheeks.kind = kind;
                p.nose.kind = NoseKind::Tick;
            });
            let cells: Vec<_> = SKIN_TONES
                .iter()
                .map(|skin| render_cell(mock_projection(), &profile, 1.0, *skin, &[Section::Cheeks, Section::Nose]))
                .collect();
            strip(&cells)
        })
        .collect();
    let (w, h) = (rows[0].width(), rows[0].height());
    let mut sheet = image::RgbaImage::new(w, h * 2);
    for (i, row) in rows.iter().enumerate() {
        image::imageops::replace(&mut sheet, row, 0, i64::from(h) * i64::try_from(i).unwrap());
    }
    sheet.save(out_path).unwrap();
}

/// Transparent, half and opaque blush colours at the strongest blush, soft then hatched, then every mark kind in a
/// window much narrower than the marks, to the `.png` named by `OMUZAKI_ALPHA_STRIP_OUT`.
#[test]
#[ignore = "writes a picture to a local assets folder; run by hand with OMUZAKI_ALPHA_STRIP_OUT=<file>.png"]
fn blush_alpha_and_narrow_mark_strip() {
    let out_path = std::env::var_os("OMUZAKI_ALPHA_STRIP_OUT").expect("OMUZAKI_ALPHA_STRIP_OUT");
    let skin = SKIN_TONES[1];
    let mut cells = Vec::new();
    for kind in [BlushKind::Soft, BlushKind::Hatched] {
        for alpha in [0_u8, 128, 255] {
            let profile = profile_with(|p| {
                p.cheeks.kind = kind;
                p.cheeks.colour = ColourRef::rgba(240, 140, 132, alpha);
            });
            cells.push(render_cell(mock_projection(), &profile, 1.6, skin, &[Section::Cheeks]));
        }
    }
    let mut narrow = mock_projection();
    narrow.rect = FaceRect { x0: -0.05, x1: 0.05, y0: narrow.rect.y0, y1: narrow.rect.y1 };
    let profile = profile_with(|p| {
        p.marks = vec![
            mark(MarkKind::BeautyMark, [0.0, -0.02], 0.05),
            FaceMark { count: 30, ..mark(MarkKind::Freckles, [0.0, -0.08], 0.05) },
            mark(MarkKind::Scar, [0.0, -0.14], 0.05),
            mark(MarkKind::Paint, [0.0, 0.05], 0.05),
        ];
    });
    cells.push(render_cell(narrow, &profile, 0.0, skin, &[Section::Marks]));
    strip(&cells).save(out_path).unwrap();
}
