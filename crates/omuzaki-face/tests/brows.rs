//! The brows section: the Base and Brows layers carry the same geometry, the brows stay on the face whatever the
//! style and the parameters, the outline sits where the prototype puts it, and painting allocates nothing once warm.
//!
//! The allocator counts only allocations made on the measuring thread, as in `hot_loop.rs`: the harness's own threads
//! allocate at unpredictable times, which would make a process-wide count flaky under load.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
// Tests may unwrap: a panic is a clear failure.
#![allow(unsafe_code, clippy::unwrap_used, clippy::expect_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use omuzaki_anim_api::ChannelBuf;
use omuzaki_core::glam::Vec2;
use omuzaki_core::time::Seconds;
use omuzaki_face::{RealPainter, Section};
use omuzaki_face_api::{
    BrowParams, Cmd, ColourRef, DEFAULT_PX_PER_M, DrawList, FaceDrawing, FaceInput, FaceLayerId, FaceProjection,
    FaceRect, FaceView, PaintParams, PathEl,
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

/// Paints the brows section alone for `profile` and `params`.
fn paint_brows(
    profile: &omuzaki_face_api::FaceProfile,
    params: &PaintParams,
    projection: FaceProjection,
) -> FaceDrawing {
    let mut painter = RealPainter::new(projection, DEFAULT_PX_PER_M);
    let input = FaceInput { profile, params, time: Seconds(0.0), view: FaceView::default() };
    let mut out = FaceDrawing::default();
    painter.paint_section(Section::Brows, &input, &mut out).unwrap();
    out
}

/// Every point a list's paths touch.
fn points(list: &DrawList) -> Vec<Vec2> {
    let mut out = Vec::new();
    for cmd in list.cmds() {
        if let Cmd::Fill { path, .. } = cmd {
            for el in list.elements(*path) {
                match *el {
                    PathEl::Move(p) | PathEl::Line(p) => out.push(p),
                    PathEl::Quad(c, p) => out.extend([c, p]),
                    PathEl::Cubic(c1, c2, p) => out.extend([c1, c2, p]),
                    PathEl::Close => {}
                }
            }
        }
    }
    out
}

fn neutral_params(profile: &omuzaki_face_api::FaceProfile) -> PaintParams {
    profile.mapping.apply(&ChannelBuf::NEUTRAL)
}

// ------------------------------------------------------------------------------------------------ the layers

#[test]
fn the_brows_layer_repeats_the_base_layers_brows_command_for_command() {
    let profile = mock_profile();
    let drawing = paint_brows(&profile, &neutral_params(&profile), mock_projection());
    let (base, brows) = (&drawing[FaceLayerId::Base], &drawing[FaceLayerId::Brows]);
    assert_eq!(base.cmds().len(), 2, "one filled outline per brow");
    assert_eq!(base.cmds(), brows.cmds(), "the commands match");
    assert_eq!(points(base), points(brows), "and so do the points they fill");
}

#[test]
fn the_brows_paint_no_other_layer() {
    let profile = mock_profile();
    let drawing = paint_brows(&profile, &neutral_params(&profile), mock_projection());
    assert!(drawing[FaceLayerId::Overlay].is_empty());
}

// ------------------------------------------------------------------------------------------------ the geometry

#[test]
fn a_neutral_brow_sits_at_the_prototype_height_above_the_eye_line() {
    // With no raise the top edge peaks mid-brow one gap above the eye line (0.082: the prototype's 0.762 over its
    // 0.680), plus the half-thickness there and the slight rise along the brow.
    let mut profile = mock_profile();
    profile.layout.eye_y = -0.098;
    profile.layout.brow_gap = 0.082;
    profile.brows.arch = 0.0;
    profile.brows.thickness = 0.0018;
    let mut params = neutral_params(&profile);
    params.left_brow = BrowParams { height: 0.0, tilt: 0.0 };
    params.brow_arch = 0.0;
    // Give the section a window that holds the brows whatever the mock window is.
    let window = FaceProjection { rect: FaceRect { x0: -0.2, x1: 0.2, y0: -0.3, y1: 1.0 }, front_fade: [0.0, 0.07] };
    let drawing = paint_brows(&profile, &params, window);
    let top_mid =
        points(&drawing[FaceLayerId::Base]).into_iter().filter(|p| p.x > 0.0).map(|p| p.y).fold(f32::MIN, f32::max);
    let expected = -0.098 + 0.082 + 0.0018 + 0.0005;
    assert!((top_mid - expected).abs() < 1e-4, "the top edge peaks at {top_mid}, expected {expected}");
}

/// The centre line of the first (character's left) brow at its 23 stations: the mean of its top and bottom edge points.
fn centre_line(profile: &omuzaki_face_api::FaceProfile, height: f32) -> Vec<f32> {
    let mut params = neutral_params(profile);
    params.left_brow = BrowParams { height, tilt: 0.0 };
    params.brow_arch = 0.0;
    let window = FaceProjection { rect: FaceRect { x0: -0.2, x1: 0.2, y0: -0.3, y1: 1.0 }, front_fade: [0.0, 0.07] };
    let drawing = paint_brows(profile, &params, window);
    let list = &drawing[FaceLayerId::Base];
    let Some(Cmd::Fill { path, .. }) = list.cmds().first() else { panic!("no brow") };
    let pts: Vec<Vec2> = list
        .elements(*path)
        .iter()
        .filter_map(|e| match e {
            PathEl::Move(p) | PathEl::Line(p) => Some(*p),
            _ => None,
        })
        .collect();
    let n = pts.len() / 2;
    (0..n).map(|i| f32::midpoint(pts[i].y, pts[pts.len() - 1 - i].y)).collect()
}

/// The profile of these tests: no arch of its own, so only the height moves the brow.
fn flat_brow_profile() -> omuzaki_face_api::FaceProfile {
    let mut profile = mock_profile();
    profile.brows.arch = 0.0;
    profile
}

#[test]
fn a_raised_brow_moves_most_at_its_inner_end_and_hardly_at_its_outer_end() {
    let profile = flat_brow_profile();
    let (rest, raised) = (centre_line(&profile, 0.0), centre_line(&profile, 1.0));
    let shift: Vec<f32> = raised.iter().zip(&rest).map(|(a, b)| a - b).collect();
    let mid = shift.len() / 2;
    // Per unit of height: 0.018 m at the inner end, 0.013 m in the middle, 0.004 m at the outer end.
    assert!((shift[0] - 0.018).abs() < 1e-5, "inner end moved {}", shift[0]);
    assert!((shift[mid] - 0.013).abs() < 1e-3, "middle moved {}", shift[mid]);
    assert!((shift[shift.len() - 1] - 0.004).abs() < 1e-5, "outer end moved {}", shift[shift.len() - 1]);
    assert!(
        shift.windows(2).all(|w| w[1] <= w[0] + 1e-7),
        "the travel falls from the inner end to the outer: {shift:?}"
    );
}

#[test]
fn a_lowered_brow_drops_far_more_at_its_inner_end_than_at_its_outer_end() {
    let profile = flat_brow_profile();
    let (rest, lowered) = (centre_line(&profile, 0.0), centre_line(&profile, -1.0));
    let shift: Vec<f32> = rest.iter().zip(&lowered).map(|(a, b)| a - b).collect();
    let mid = shift.len() / 2;
    assert!((shift[0] - 0.031).abs() < 1e-5, "inner end dropped {}", shift[0]);
    // The lowered brow is straight, so the middle drops by the mean of the two ends.
    assert!((shift[mid] - 0.0175).abs() < 1e-3, "middle dropped {}", shift[mid]);
    assert!((shift[shift.len() - 1] - 0.004).abs() < 1e-5, "outer end dropped {}", shift[shift.len() - 1]);
}

/// The largest distance of a centre line from the chord between its two ends.
#[allow(clippy::cast_precision_loss)] // a few dozen stations
fn bow(line: &[f32]) -> f32 {
    let (first, last) = (line[0], line[line.len() - 1]);
    let n = (line.len() - 1) as f32;
    line.iter().enumerate().map(|(i, y)| (y - (first + (last - first) * i as f32 / n)).abs()).fold(0.0, f32::max)
}

#[test]
fn a_brow_lowered_all_the_way_is_flat_against_its_chord() {
    // With the style's own arch and the live arch parameter on, so neither can bring a bow back.
    for arch in [0.0, 0.5, -0.5] {
        let profile = mock_profile();
        let mut params = neutral_params(&profile);
        params.left_brow = BrowParams { height: -1.0, tilt: 0.0 };
        params.brow_arch = arch;
        let window =
            FaceProjection { rect: FaceRect { x0: -0.2, x1: 0.2, y0: -0.3, y1: 1.0 }, front_fade: [0.0, 0.07] };
        let drawing = paint_brows(&profile, &params, window);
        let Some(Cmd::Fill { path, .. }) = drawing[FaceLayerId::Base].cmds().first() else { panic!("no brow") };
        let pts: Vec<Vec2> = drawing[FaceLayerId::Base]
            .elements(*path)
            .iter()
            .filter_map(|e| match e {
                PathEl::Move(p) | PathEl::Line(p) => Some(*p),
                _ => None,
            })
            .collect();
        let n = pts.len() / 2;
        let line: Vec<f32> = (0..n).map(|i| f32::midpoint(pts[i].y, pts[pts.len() - 1 - i].y)).collect();
        assert!(bow(&line) < 1e-5, "arch {arch}: bow {} m", bow(&line));
    }
}

#[test]
fn the_brow_at_height_zero_is_the_prototypes_whatever_the_weights() {
    let profile = flat_brow_profile();
    let rest = centre_line(&profile, 0.0);
    let again = centre_line(&profile, 1e-9);
    assert!(rest.iter().zip(&again).all(|(a, b)| (a - b).abs() < 1e-7));
}

/// The peak's height above the chord between the ends, for a brow of `height` with the style's own arch.
fn arch_height(height: f32) -> f32 {
    let mut profile = mock_profile();
    profile.brows.arch = 0.004;
    let flat = centre_line(&flat_brow_profile(), height);
    let arched = centre_line(&profile, height);
    // The arch is what the style adds over the same brow without one.
    arched.iter().zip(&flat).map(|(a, b)| a - b).fold(f32::MIN, f32::max)
}

#[test]
fn the_arch_grows_a_quarter_when_the_brow_rises_and_is_gone_when_it_is_lowered() {
    assert!((arch_height(0.0) - 0.004).abs() < 1e-5, "rest {}", arch_height(0.0));
    assert!((arch_height(1.0) - 0.005).abs() < 1e-5, "raised {}", arch_height(1.0));
    assert!(arch_height(-1.0).abs() < 1e-5, "lowered {}", arch_height(-1.0));
    // Half way down it is half there; past full it stays at its limits.
    assert!((arch_height(-0.5) - 0.002).abs() < 1e-5);
    assert!((arch_height(1.5) - 0.005).abs() < 1e-5);
    assert!(arch_height(-1.5).abs() < 1e-5);
}

#[test]
fn the_layout_gap_moves_both_layers_equally() {
    let window = FaceProjection { rect: FaceRect { x0: -0.2, x1: 0.2, y0: -0.3, y1: 1.0 }, front_fade: [0.0, 0.07] };
    let mut profile = mock_profile();
    profile.layout.brow_gap = 0.05;
    let near = paint_brows(&profile, &neutral_params(&profile), window);
    profile.layout.brow_gap = 0.09;
    let far = paint_brows(&profile, &neutral_params(&profile), window);
    for layer in [FaceLayerId::Base, FaceLayerId::Brows] {
        let (a, b) = (points(&near[layer]), points(&far[layer]));
        assert_eq!(a.len(), b.len());
        for (a, b) in a.iter().zip(&b) {
            assert!((b.y - a.y - 0.04).abs() < 1e-5 && (b.x - a.x).abs() < 1e-6, "{layer:?}: {a:?} -> {b:?}");
        }
    }
}

#[test]
fn a_brow_is_mirrored_across_the_midline() {
    let profile = mock_profile();
    let drawing = paint_brows(&profile, &neutral_params(&profile), mock_projection());
    let pts = points(&drawing[FaceLayerId::Base]);
    let (left, right): (Vec<Vec2>, Vec<Vec2>) = pts.iter().partition(|p| p.x > 0.0);
    assert_eq!(left.len(), right.len());
    let max_x = left.iter().map(|p| p.x).fold(f32::MIN, f32::max);
    let min_x = right.iter().map(|p| p.x).fold(f32::MAX, f32::min);
    assert!((max_x + min_x).abs() < 1e-6, "the outer ends sit at +-outer_x");
}

#[test]
fn brows_stay_on_the_face_for_extreme_styles_and_parameters() {
    let projection = mock_projection();
    let rect: FaceRect = projection.rect;
    let mut profile = mock_profile();
    profile.brows.inner_x = 0.0;
    profile.brows.outer_x = 0.25;
    profile.brows.thickness = 0.01;
    profile.brows.arch = 0.02;
    profile.brows.colour = ColourRef::rgb(0, 0, 0);
    profile.layout.eye_y = 0.4;
    let mut params = neutral_params(&profile);
    params.left_brow = BrowParams { height: 1.5, tilt: 2.5 };
    params.right_brow = BrowParams { height: -1.5, tilt: -2.5 };
    params.brow_arch = 1.0;
    let drawing = paint_brows(&profile, &params, projection);
    for layer in [FaceLayerId::Base, FaceLayerId::Brows] {
        for p in points(&drawing[layer]) {
            assert!(
                (rect.x0..=rect.x1).contains(&p.x) && (rect.y0..=rect.y1).contains(&p.y),
                "{layer:?} point {p:?} is off the face rect {rect:?}"
            );
        }
    }
}

#[test]
fn a_thin_style_still_fills_a_hairline_at_the_tips() {
    let mut profile = mock_profile();
    profile.brows.thickness = 0.0003;
    let drawing = paint_brows(&profile, &neutral_params(&profile), mock_projection());
    assert_eq!(drawing[FaceLayerId::Base].cmds().len(), 2);
    assert!(points(&drawing[FaceLayerId::Base]).iter().all(|p| p.is_finite()));
}

// ------------------------------------------------------------------------------------------------ the budget

#[test]
fn painting_the_brows_allocates_nothing_after_warm_up() {
    let profile = mock_profile();
    let params = neutral_params(&profile);
    let mut painter = RealPainter::new(mock_projection(), DEFAULT_PX_PER_M);
    let input = FaceInput { profile: &profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
    let mut out = FaceDrawing::default();
    // Warm-up: every list and path arena reaches its working size.
    painter.paint_section(Section::Brows, &input, &mut out).unwrap();
    painter.paint_section(Section::Brows, &input, &mut out).unwrap();

    let allocations = count_allocations(|| {
        for _ in 0..1000 {
            painter.paint_section(Section::Brows, &input, &mut out).unwrap();
        }
    });
    assert_eq!(allocations, 0, "1000 brow paints made {allocations} allocations");
}
