//! The mouth shape through the real painter: the closed line, the kiss and the open shape, each parameter's
//! effect, the viseme order, the continuity of a jaw sweep and the zero-allocation guarantee.
//!
//! Only the shape piece is painted ([`RealPainter::paint_mouth_piece`]), so these tests keep their meaning when the
//! interior, rim and tongue arrive. The pictures and the match with the prototype are in
//! `crates/omuzaki-test-face/tests/face_parity_mouth_shape.rs`.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default. Test
// helpers may panic: a panic is a clear test failure.
#![allow(unsafe_code, clippy::unwrap_used, clippy::expect_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;

use omuzaki_anim_api::{Channel, ChannelBuf};
use omuzaki_core::glam::Vec2;
use omuzaki_core::time::Seconds;
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face::{MouthPiece, RealPainter};
use omuzaki_face_api::{
    Cmd, DEFAULT_PX_PER_M, DrawList, ExpressionSet, FaceDrawing, FaceInput, FaceLayerId, FaceLayerSpec, FaceMapping,
    FaceProfile, FaceProjection, FaceRasterizer, FaceRect, FaceView, MouthParams, Paint, PaintParams, PathEl, Raster,
    narrow,
};

struct Counting;

thread_local! {
    // `const`-initialised cells without a destructor: touching them never allocates, so it is safe inside the allocator.
    static MEASURING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

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

const MOUTH_Y: f32 = -0.15;

fn projection() -> FaceProjection {
    FaceProjection { rect: FaceRect { x0: -0.1, x1: 0.1, y0: -0.2, y1: -0.1 }, front_fade: [0.0, 0.05] }
}

fn painter() -> RealPainter {
    RealPainter::new(projection(), DEFAULT_PX_PER_M)
}

/// Rest paint parameters with `edit` applied to the mouth.
fn params(edit: impl FnOnce(&mut MouthParams)) -> PaintParams {
    let mut p = FaceMapping::default().rest_params();
    p.mouth = MouthParams { smile_left: 0.0, smile_right: 0.0, ..p.mouth };
    edit(&mut p.mouth);
    p
}

fn paint_into(profile: &FaceProfile, p: &PaintParams, out: &mut FaceDrawing) {
    let input = FaceInput { profile, params: p, time: Seconds(0.0), view: FaceView::default() };
    painter().paint_mouth_piece(MouthPiece::Shape, &input, out).unwrap();
}

fn paint(profile: &FaceProfile, p: &PaintParams) -> DrawList {
    let mut out = FaceDrawing::new();
    paint_into(profile, p, &mut out);
    out[FaceLayerId::Base].clone()
}

fn flat() -> FaceProfile {
    FaceProfile::default()
}

fn glossy() -> FaceProfile {
    let mut p = FaceProfile::default();
    p.mouth.lips.strength = 1.0;
    p
}

/// The points of a path made of one move and lines (the closed line, the kiss slit, the open outline).
fn points(list: &DrawList, path: omuzaki_face_api::PathRef) -> Vec<Vec2> {
    list.elements(path)
        .iter()
        .filter_map(|e| match e {
            PathEl::Move(p) | PathEl::Line(p) => Some(*p),
            _ => None,
        })
        .collect()
}

fn first_path(list: &DrawList) -> omuzaki_face_api::PathRef {
    match list.cmds().first() {
        Some(Cmd::Stroke { path, .. } | Cmd::Fill { path, .. }) => *path,
        other => panic!("expected a stroke or fill first, got {other:?}"),
    }
}

fn stroke_widths(list: &DrawList) -> Vec<f32> {
    list.cmds()
        .iter()
        .filter_map(|c| match c {
            Cmd::Stroke { width, .. } => Some(*width),
            _ => None,
        })
        .collect()
}

fn closed(list: &DrawList) -> (Vec<Vec2>, f32) {
    let Some(Cmd::Stroke { path, width, .. }) = list.cmds().first() else {
        panic!("no closed line: {:?}", list.cmds())
    };
    (points(list, *path), *width)
}

fn assert_close(got: f32, want: f32, what: &str) {
    assert!((got - want).abs() < 1e-6, "{what}: got {got}, want {want}");
}

/// Rows of the Base layer holding ink at least half opaque, at the default density: the height the viewer sees.
fn ink_rows(list: &DrawList) -> usize {
    let spec = FaceLayerSpec::for_layer(FaceLayerId::Base, &projection(), DEFAULT_PX_PER_M);
    let mut raster = Raster::new(1, 1);
    SkiaRasterizer::new().rasterize(&spec, list, DEFAULT_PX_PER_M, &mut raster).unwrap();
    let width = raster.width as usize;
    let rows: Vec<usize> = raster
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .enumerate()
        .filter(|(_, px)| px[3] >= 128)
        .map(|(i, _)| i / width)
        .collect();
    match (rows.iter().min(), rows.iter().max()) {
        (Some(lo), Some(hi)) => hi - lo + 1,
        _ => 0,
    }
}

// ------------------------------------------------------------------------------------------------ closed line

#[test]
fn the_closed_mouth_is_one_line_of_seventeen_samples_from_corner_to_corner() {
    let list = paint(&glossy(), &params(|m| m.wide = 1.0));
    let (pts, width) = closed(&list);
    assert_eq!(pts.len(), 17);
    assert_eq!(list.cmds().len(), 1, "a lip-rim face has no under line");
    assert_close(pts[0].x, -0.0105, "left end");
    assert_close(pts[16].x, 0.0105, "right end");
    assert_close(pts[8].x, 0.0, "centre");
    assert!(pts.iter().all(|p| (p.y - MOUTH_Y).abs() < 1e-6), "no smile, so a straight line");
    assert_close(width, 0.0016, "the line width at grow 1");
}

#[test]
fn the_flat_look_adds_the_faint_line_under_the_mouth() {
    let list = paint(&flat(), &params(|_| {}));
    assert_eq!(list.cmds().len(), 2);
    assert_eq!(stroke_widths(&list)[1], 0.0012);
    let Some(Cmd::Stroke { path, .. }) = list.cmds().get(1) else { panic!() };
    let under = points(&list, *path);
    assert_eq!(under.len(), 2);
    assert_close(under[0].y, MOUTH_Y - 0.0038, "under line height");
    assert_close(under[1].x - under[0].x, 2.0 * 0.0105 * 0.35, "under line length");
}

#[test]
fn each_corner_follows_its_own_smile_and_the_centre_stays_on_the_line() {
    let list = paint(&flat(), &params(|m| (m.smile_left, m.smile_right) = (1.0, -1.0)));
    let (pts, _) = closed(&list);
    assert_close(pts[8].y, MOUTH_Y, "centre");
    assert_close(pts[16].y, MOUTH_Y + 0.0045, "her left corner (+x) lifts by the corner curve");
    assert_close(pts[0].y, MOUTH_Y - 0.0045, "her right corner droops");
    // Quadratic in the distance from the centre.
    assert_close(pts[12].y - MOUTH_Y, 0.0045 * 0.25, "halfway out");
}

#[test]
fn a_symmetric_smile_lifts_both_corners_and_a_frown_drops_them() {
    let (smile, _) = closed(&paint(&flat(), &params(|m| (m.smile_left, m.smile_right) = (0.5, 0.5))));
    let (frown, _) = closed(&paint(&flat(), &params(|m| (m.smile_left, m.smile_right) = (-0.5, -0.5))));
    assert!(smile[0].y > MOUTH_Y && smile[16].y > MOUTH_Y);
    assert!(frown[0].y < MOUTH_Y && frown[16].y < MOUTH_Y);
    assert_close(smile[16].y - MOUTH_Y, MOUTH_Y - frown[16].y, "mirror");
}

#[test]
fn the_mouth_shifts_sideways_by_six_millimetres_per_unit() {
    let (pts, _) = closed(&paint(&flat(), &params(|m| m.shift = 1.0)));
    assert_close(pts[8].x, 0.006, "shifted centre");
    let (pts, _) = closed(&paint(&flat(), &params(|m| m.shift = -0.5)));
    assert_close(pts[8].x, -0.003, "shifted the other way");
}

#[test]
fn a_press_shortens_and_thins_the_line_and_closes_some_of_the_opening() {
    let (plain, plain_width) = closed(&paint(&flat(), &params(|_| {})));
    let (pressed, pressed_width) = closed(&paint(&flat(), &params(|m| m.press = 1.0)));
    assert_close(pressed[16].x, plain[16].x * 0.75, "a full press shortens by a quarter");
    assert_close(pressed_width, plain_width * 0.7, "and thins by 30 %");
    // An opening of 0.4 is an open shape; a press of 0.8 takes 0.4 of it away and shuts it.
    let open = paint(&flat(), &params(|m| m.open = 0.4));
    assert!(matches!(open.cmds().first(), Some(Cmd::Fill { .. })));
    let shut = paint(&flat(), &params(|m| (m.open, m.press) = (0.4, 0.8)));
    assert!(matches!(shut.cmds().first(), Some(Cmd::Stroke { .. })));
}

#[test]
fn the_performance_scale_grows_the_line_with_the_square_root_and_the_width_with_the_scale() {
    let mut profile = flat();
    profile.mouth.closed_scale = 1.7;
    let (pts, width) = closed(&paint(&profile, &params(|_| {})));
    assert_close(pts[16].x, 0.0105 * 1.7, "closed scale widens");
    assert_close(width, 0.0016 * 1.7_f32.sqrt(), "and thickens by its root");
    // The line's corners scale with grow too.
    let smiling = closed(&paint(&profile, &params(|m| (m.smile_left, m.smile_right) = (1.0, 1.0)))).0;
    assert_close(smiling[16].y - MOUTH_Y, 0.0045 * 1.7, "corner curve times grow");
}

#[test]
fn the_width_channel_scales_the_mouth() {
    let (narrow, _) = closed(&paint(&flat(), &params(|m| m.wide = 0.5)));
    let (wide, _) = closed(&paint(&flat(), &params(|m| m.wide = 1.5)));
    assert_close(narrow[16].x, 0.0105 * 0.5, "narrow");
    assert_close(wide[16].x, 0.0105 * 1.5, "wide");
}

#[test]
fn the_style_sets_the_closed_line() {
    let mut profile = flat();
    profile.mouth.half_width = 0.02;
    profile.mouth.line_width = 0.004;
    profile.mouth.corner_curve = 0.01;
    profile.mouth.line_colour = omuzaki_face_api::ColourRef::rgb(1, 2, 3);
    let list = paint(&profile, &params(|m| (m.smile_left, m.smile_right) = (1.0, 1.0)));
    let Some(Cmd::Stroke { path, width, paint, .. }) = list.cmds().first() else { panic!() };
    let pts = points(&list, *path);
    assert_close(pts[16].x, 0.02, "half width");
    assert_close(pts[16].y - MOUTH_Y, 0.01, "corner curve");
    assert_close(*width, 0.004, "line width");
    assert_eq!(*paint, omuzaki_face_api::Paint::Solid(omuzaki_face_api::Colour::rgb(1, 2, 3)));
}

// ------------------------------------------------------------------------------------------------ kiss

#[test]
fn a_pucker_with_a_nearly_shut_jaw_is_a_kiss_of_four_strokes() {
    let list = paint(&flat(), &params(|m| m.pucker = 0.8));
    assert_eq!(list.cmds().len(), 4);
    assert_eq!(stroke_widths(&list), [0.0040, 0.0026, 0.0011, 0.0012]);
    let slit = points(&list, first_path(&list));
    assert_eq!(slit.len(), 19);
    // The slit is 0.85 of the (narrowed) mouth width, and wavy: it rises at the centre.
    let half = 0.0105 * (1.0 - 0.18 * 0.8) * 0.85;
    assert_close(slit[0].x, -half, "left end");
    assert_close(slit[18].x, half, "right end");
    assert!(slit[9].y > slit[0].y && slit[9].y > slit[18].y);
    assert_close(slit[9].y, MOUTH_Y + 0.0009, "centre rise");
}

#[test]
fn the_kiss_starts_above_a_pucker_of_one_third_and_ends_at_an_opening_of_a_quarter() {
    let is_kiss =
        |pucker: f32, open: f32| paint(&flat(), &params(|m| (m.pucker, m.open) = (pucker, open))).cmds().len() == 4;
    assert!(!is_kiss(0.35, 0.0), "the threshold itself is not a kiss");
    assert!(is_kiss(0.36, 0.0));
    assert!(is_kiss(1.0, 0.24));
    assert!(!is_kiss(1.0, 0.25), "an opening of a quarter is an open shape");
    // The same opening with a pucker below the threshold is the open shape too.
    assert!(!is_kiss(0.3, 0.2));
}

#[test]
fn the_kiss_pairs_the_slit_with_its_dark_core_on_top() {
    let list = paint(&flat(), &params(|m| m.pucker = 1.0));
    let colours: Vec<_> = list
        .cmds()
        .iter()
        .filter_map(|c| match c {
            Cmd::Stroke { paint: omuzaki_face_api::Paint::Solid(c), .. } => Some((c.r, c.g, c.b, c.a)),
            _ => None,
        })
        .collect();
    assert_eq!(colours[0], (150, 70, 80, 153), "shade, 60 %");
    assert_eq!(colours[1], (255, 244, 244, 242), "rim, 95 %");
    assert_eq!(colours[2], (0x7a, 0x2a, 0x35, 255), "core");
    assert_eq!(colours[3], (220, 130, 135, 115), "upper lip, 45 %");
}

// ------------------------------------------------------------------------------------------------ open shape

fn outline(list: &DrawList) -> Vec<Vec2> {
    assert_eq!(list.cmds().len(), 1, "an open mouth is one fill");
    let Some(Cmd::Fill { path, .. }) = list.cmds().first() else { panic!() };
    points(list, *path)
}

fn extent(pts: &[Vec2]) -> (Vec2, Vec2) {
    let lo = pts.iter().fold(Vec2::splat(f32::MAX), |a, p| a.min(*p));
    let hi = pts.iter().fold(Vec2::splat(f32::MIN), |a, p| a.max(*p));
    (lo, hi)
}

#[test]
fn the_open_shape_is_a_fifty_eight_point_lens_filled_with_the_interior_gradient() {
    let profile = flat();
    let list = paint(&profile, &params(|m| m.open = 0.5));
    let pts = outline(&list);
    assert_eq!(pts.len(), 2 * 29);
    let Some(Cmd::Fill { paint: omuzaki_face_api::Paint::Linear { from, to, stops }, .. }) = list.cmds().first() else {
        panic!("{:?}", list.cmds());
    };
    let h = 0.016 * 0.5;
    assert_close(from.y, MOUTH_Y, "gradient starts on the line");
    assert_close(to.y, MOUTH_Y - h, "and ends at the depth");
    let colours: Vec<_> = stops.as_slice().iter().map(|s| (s.colour.r, s.colour.g, s.colour.b)).collect();
    assert_eq!(colours, [(0x4a, 0x16, 0x22), (0x6e, 0x24, 0x33)]);
    let (lo, hi) = extent(&pts);
    let wd = 0.0105 * (1.0 + 0.25 * 0.5);
    assert_close(lo.x, -wd, "left end");
    assert_close(hi.x, wd, "right end");
    assert_close(lo.y, MOUTH_Y - h, "depth at the middle");
    assert_close(hi.y, MOUTH_Y + h * 0.125, "the upper edge rises an eighth of the depth");
}

#[test]
fn a_round_mouth_is_narrower_with_a_flatter_top_and_a_lower_edge_that_falls_away_faster() {
    let plain = outline(&paint(&flat(), &params(|m| m.open = 0.5)));
    let round = outline(&paint(&flat(), &params(|m| (m.open, m.round) = (0.5, 1.0))));
    let (p, r) = (extent(&plain), extent(&round));
    assert_close(r.1.x, p.1.x * 0.65, "a round mouth is 35 % narrower");
    assert!(r.1.y < p.1.y, "flatter top");
    // Halfway out, the lower edge's depth as a fraction of the middle's: (1 - 0.25)^exponent, with the exponent 0.5
    // for a plain mouth and 0.8 for a fully round one, which falls away faster toward the corners.
    let halfway = |pts: &[Vec2], deepest: f32| (MOUTH_Y - pts[29 + 7].y) / (MOUTH_Y - deepest);
    assert_close(halfway(&plain, p.0.y), 0.75_f32.powf(0.5), "plain lower edge");
    assert_close(halfway(&round, r.0.y), 0.75_f32.powf(0.8), "round lower edge");
}

#[test]
fn the_corners_of_an_open_mouth_follow_the_smile_and_the_centre_drops_with_the_average() {
    let pts = outline(&paint(&flat(), &params(|m| (m.open, m.smile_left, m.smile_right) = (0.5, 1.0, -1.0))));
    let (top_right, top_left) = (pts[0], pts[28]);
    let k = 0.004;
    assert_close(top_left.y, MOUTH_Y + 0.0 + k, "her left corner lifts by 0.004");
    assert_close(top_right.y, MOUTH_Y - k, "her right corner droops by 0.004");
    // The average smile is zero here; with an average of 1 the centre sinks by a millimetre.
    let low = outline(&paint(&flat(), &params(|m| (m.open, m.smile_left, m.smile_right) = (0.5, 1.0, 1.0))));
    let mid = outline(&paint(&flat(), &params(|m| m.open = 0.5)));
    assert_close(mid[14].y - low[14].y, 0.001, "centre follows the average smile");
}

#[test]
fn the_open_shape_grows_with_the_performance_scale() {
    let mut profile = flat();
    profile.mouth.closed_scale = 1.0;
    profile.mouth.open_scale = 2.0;
    // At open 0.4 the scale is fully in force (0.4 x 3 > 1).
    let (lo, _) = extent(&outline(&paint(&profile, &params(|m| m.open = 0.4))));
    assert_close(lo.y, MOUTH_Y - 0.016 * 0.4 * 2.0, "depth scales");
    // At open 0.25 three quarters of the way: grow = 1 + 0.75 (and the lens has finished easing in).
    let (lo, _) = extent(&outline(&paint(&profile, &params(|m| m.open = 0.25))));
    assert_close(lo.y, MOUTH_Y - 0.016 * 0.25 * 1.75, "partial scale");
}

#[test]
fn a_pucker_narrows_an_open_mouth_further() {
    let plain = extent(&outline(&paint(&flat(), &params(|m| m.open = 0.5)))).1.x;
    let puckered = extent(&outline(&paint(&flat(), &params(|m| (m.open, m.pucker) = (0.5, 0.6))))).1.x;
    assert_close(puckered, plain * (1.0 - 0.18 * 0.6) * (1.0 - 0.3 * 0.6), "pucker narrows twice");
}

// ------------------------------------------------------------------------------------------------ the sections around

#[test]
fn only_the_open_shape_leaves_an_outline_for_the_interior_and_the_section_paints_the_whole_mouth() {
    // The in-crate unit tests read `PaintCtx::open_mouth`; here, the interior piece alone records the outline to clip against and then draws into it.
    let mut out = FaceDrawing::new();
    let p = params(|m| m.open = 0.5);
    let input = FaceInput { profile: &flat(), params: &p, time: Seconds(0.0), view: FaceView::default() };
    let mut painter = painter();
    painter.paint_mouth_piece(MouthPiece::Shape, &input, &mut out).unwrap();
    let shape_only = out[FaceLayerId::Base].fingerprint();
    painter.paint_mouth_piece(MouthPiece::Interior, &input, &mut out).unwrap();
    assert!(out[FaceLayerId::Base].is_empty() || out[FaceLayerId::Base].fingerprint() != shape_only);
    painter.paint_section(omuzaki_face::Section::Mouth, &input, &mut out).unwrap();
    assert!(!out[FaceLayerId::Base].is_empty());
    assert!(out[FaceLayerId::Brows].is_empty() && out[FaceLayerId::Overlay].is_empty(), "the mouth is Base only");
}

// ------------------------------------------------------------------------------------------------ visemes and sweeps

fn through_the_mapping(name: &str) -> PaintParams {
    let set = ExpressionSet::starter();
    let id = omuzaki_face_api::ExpressionId::new(name).unwrap();
    FaceMapping::default().apply(&set.blend(&ChannelBuf::NEUTRAL, &[(&id, 1.0)]).unwrap())
}

#[test]
fn the_five_visemes_open_the_mouth_by_the_prototypes_amounts_and_paint_in_that_order() {
    let wanted = [("aa", 0.85), ("oh", 0.62), ("ou", 0.42), ("ee", 0.32), ("ih", 0.28)];
    let mut previous = usize::MAX;
    for (name, open) in wanted {
        let p = through_the_mapping(name);
        assert!((p.mouth.open - open).abs() < 0.03, "{name} opens {}", p.mouth.open);
        let rows = ink_rows(&paint(&flat(), &p));
        assert!(rows < previous, "{name} should paint shorter than the one before ({rows} >= {previous})");
        previous = rows;
    }
}

/// The vertical extent of everything the shape and the rim draw, in pixels at the default density (strokes count their
/// width): what the viewer sees of the mouth's outline.
fn height_px(profile: &FaceProfile, p: &PaintParams) -> f32 {
    let mut out = FaceDrawing::new();
    let input = FaceInput { profile, params: p, time: Seconds(0.0), view: FaceView::default() };
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for piece in [MouthPiece::Shape, MouthPiece::Rim] {
        painter().paint_mouth_piece(piece, &input, &mut out).unwrap();
        let list = &out[FaceLayerId::Base];
        for cmd in list.cmds() {
            let (path, half) = match cmd {
                Cmd::Stroke { path, width, opacity, .. } => (*path, width / 2.0 * opacity.signum()),
                Cmd::Fill { path, .. } => (*path, 0.0),
                _ => continue,
            };
            for p in points(list, path) {
                lo = lo.min(p.y - half);
                hi = hi.max(p.y + half);
            }
        }
    }
    (hi - lo) * DEFAULT_PX_PER_M
}

/// The mouth's height for `jaw` open (on the lip-rim look: the flat look's faint under line is not part of the mouth), and the opening the default mapping made of it.
fn sweep_height(jaw: f32) -> (f32, f32) {
    let mut bus = ChannelBuf::NEUTRAL;
    bus.set(Channel::JAW_OPEN, jaw);
    let p = FaceMapping::default().apply(&bus);
    (height_px(&glossy(), &p), p.mouth.open)
}

/// The smoothness bar: no step of a sweep differs from its neighbour by more than this, at 3600 px per metre (it was
/// 2.5 px, the prototype's jump, for the steps across the switch).
const SWITCH_JUMP_PX: f32 = 1.5;
/// The smallest bound a 20-step sweep (about 0.06 of opening a step) reaches while the rim fades in, 0.06 to 0.2, with
/// the line thinning, the rim fading and the lens deepening together: the mouth goes from a 5.8 px line to a lens with an
/// 8.6 px rim in two or three steps, and the line before the switch can only thin, so no easing of the three gets
/// the coarse steps under 5.5 px. The switch itself is continuous (see below), and a fine sweep meets the bar.
const EASING_STEP_PX: f32 = 5.6;

#[test]
fn a_jaw_sweep_in_twenty_steps_grows_the_mouth_smoothly_across_the_switch() {
    let heights: Vec<(f32, f32)> = (0..=20_u8).map(|i| sweep_height(f32::from(i) / 20.0)).collect();
    // The line thins as the jaw nears the switch (by design: the lens that replaces it is shallower than the line is
    // thick), so only from the switch on does the mouth only grow.
    for pair in heights.windows(2).filter(|pair| pair[0].1 >= 0.06) {
        let ((a, oa), (b, ob)) = (pair[0], pair[1]);
        assert!(b >= a - 1e-3, "the mouth shrank from {a} px (open {oa}) to {b} px (open {ob})");
    }
    // Smooth means each step is the same size as the one before it to within the bar (the curve is gentle: a step is
    // about 3.7 px), except where the rim eases in.
    println!("sweep heights (px, open): {heights:.2?}");
    let mut largest = 0.0_f32;
    for window in heights.windows(3) {
        let (first, second) = (window[1].0 - window[0].0, window[2].0 - window[1].0);
        largest = largest.max((second - first).abs());
        assert!(
            (second - first).abs() <= if window[0].1 < 0.2 { EASING_STEP_PX } else { SWITCH_JUMP_PX },
            "steps {first} then {second} px at open {:?}",
            window.iter().map(|w| w.1).collect::<Vec<_>>()
        );
    }
    println!("largest step-to-step change in the 20-step jaw sweep: {largest:.3} px");
}

#[test]
fn a_fine_sweep_of_the_opening_has_no_step_that_differs_from_its_neighbour_by_more_than_the_bar() {
    let profile = glossy();
    let heights: Vec<f32> =
        (0..=80_u8).map(|i| height_px(&profile, &params(|m| m.open = f32::from(i) * 0.005))).collect();
    let mut largest = 0.0_f32;
    for (i, w) in heights.windows(3).enumerate() {
        let change = ((w[2] - w[1]) - (w[1] - w[0])).abs();
        largest = largest.max(change);
        assert!(change <= SWITCH_JUMP_PX, "steps differ by {change} px, {} steps in", i + 1);
    }
    println!("largest step-to-step change in a sweep of the opening in steps of 0.005: {largest:.3} px");
}

/// Valid profiles away from the default: thin and thick closed lines on narrow and wide mouths.
fn odd_profiles() -> Vec<FaceProfile> {
    [(0.05, 0.0003), (0.002, 0.01), (0.05, 0.01), (0.002, 0.0003), (0.0105, 0.0003), (0.0105, 0.01)]
        .into_iter()
        .map(|(half_width, line_width)| {
            let mut p = glossy();
            (p.mouth.half_width, p.mouth.line_width) = (half_width, line_width);
            p
        })
        .collect()
}

#[test]
fn the_switch_is_continuous_for_thin_and_thick_lines_on_narrow_and_wide_mouths() {
    for profile in odd_profiles() {
        let height_at = |open: f32| height_px(&profile, &params(|m| m.open = open));
        let jump = (height_at(0.0601) - height_at(0.0599)).abs();
        let what = (profile.mouth.half_width, profile.mouth.line_width);
        assert!(jump < 0.2, "{what:?}: the switch changes the height by {jump} px");
        // The mouth only grows from the switch on, and the rim's eased-in weight stays within the coarse sweep's bound.
        let heights: Vec<f32> = (0..=20_u8)
            .map(|i| {
                let mut bus = ChannelBuf::NEUTRAL;
                bus.set(Channel::JAW_OPEN, f32::from(i) / 20.0);
                height_px(&profile, &FaceMapping::default().apply(&bus))
            })
            .collect();
        let steps: Vec<f32> = heights.windows(2).map(|w| w[1] - w[0]).collect();
        let largest = steps.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0_f32, f32::max);
        // The bound scales with the profile: a step of the lens' own growth (3.73 px at the default mouth, in proportion
        // to its half-width) and the heavier of the line and the rim, which the eased-in weight cannot exceed.
        let natural = 3.726 * narrow(profile.mouth.half_width) / 0.0105;
        let line_px = narrow(profile.mouth.line_width) * DEFAULT_PX_PER_M;
        let weight = line_px.max(8.64);
        assert!(largest <= 1.5 * natural + weight, "{what:?}: largest coarse step change {largest} px");
        // A line thicker than the open mouth's rim gives way to the thinner rim, so only the lighter lines never shrink.
        if line_px <= 8.64 {
            assert!(steps[1..].iter().all(|s| *s >= -1e-3), "{what:?}: the mouth shrank after the switch: {steps:?}");
        }
    }
}

#[test]
fn the_closed_smiles_fang_hands_over_to_the_open_ones_as_the_rim_fades_in() {
    let profile = glossy();
    let fang_opacity = |open: f32| {
        let p = params(|m| (m.open, m.fangs, m.smile_left) = (open, 1.0, 0.8));
        let mut out = FaceDrawing::new();
        let input = FaceInput { profile: &profile, params: &p, time: Seconds(0.0), view: FaceView::default() };
        painter().paint_mouth_piece(MouthPiece::Interior, &input, &mut out).unwrap();
        // The closed fang is the last fill, outside the lens' clip.
        out[FaceLayerId::Base].cmds().iter().rev().find_map(|c| match c {
            Cmd::Fill { opacity, .. } => Some(*opacity),
            _ => None,
        })
    };
    let at_switch = fang_opacity(0.0601).unwrap();
    let halfway = fang_opacity(0.13).unwrap();
    assert!(at_switch > 0.99, "the closed fang is still fully there at the switch: {at_switch}");
    assert!((halfway - 0.5).abs() < 0.01, "and half there halfway: {halfway}");
    // From 0.2 the last fill is an open fang, at the channel's strength: the closed one is gone.
    let list = {
        let p = params(|m| (m.open, m.fangs, m.smile_left) = (0.2, 1.0, 0.8));
        let mut out = FaceDrawing::new();
        let input = FaceInput { profile: &profile, params: &p, time: Seconds(0.0), view: FaceView::default() };
        painter().paint_mouth_piece(MouthPiece::Interior, &input, &mut out).unwrap();
        out[FaceLayerId::Base].cmds().iter().filter(|c| matches!(c, Cmd::Stroke { .. })).count()
    };
    assert_eq!(list, 0, "no outlined closed fang once the mouth is open");
}

#[test]
fn right_at_the_switch_the_mouth_changes_height_by_less_than_a_fifth_of_a_pixel() {
    let height_at = |open: f32| height_px(&glossy(), &params(|m| m.open = open));
    let jump = (height_at(0.0601) - height_at(0.0599)).abs();
    println!(
        "height at 0.0599 {:.3} px, at 0.0601 {:.3} px, change {jump:.3} px",
        height_at(0.0599),
        height_at(0.0601)
    );
    assert!(jump < 0.2, "the switch changes the height by {jump} px");
    // Away from the switch the height is continuous: 1 / 1000 of the opening moves it by well under a pixel.
    for open in [0.01, 0.03, 0.1, 0.15, 0.2, 0.5, 0.9, 1.4] {
        assert!((height_at(open + 0.001) - height_at(open)).abs() < 0.2, "open {open}");
    }
}

#[test]
fn the_closed_line_thins_towards_the_switch_and_is_untouched_at_rest() {
    let width_at = |open: f32| closed(&paint(&glossy(), &params(|m| m.open = open))).1;
    let rest = width_at(0.0);
    assert!(width_at(0.03) < rest && width_at(0.0599) < width_at(0.03), "the line thins as the jaw opens");
    assert!((width_at(0.0599) - rest * 0.8).abs() < rest * 0.01, "by a fifth at the switch");
}

/// The rim and the other pieces of the open mouth at `open`.
fn rim_at(open: f32) -> DrawList {
    let mut out = FaceDrawing::new();
    let profile = glossy();
    let p = params(|m| m.open = open);
    let input = FaceInput { profile: &profile, params: &p, time: Seconds(0.0), view: FaceView::default() };
    painter().paint_mouth_piece(MouthPiece::Rim, &input, &mut out).unwrap();
    out[FaceLayerId::Base].clone()
}

#[test]
fn just_above_the_switch_the_rim_is_the_closed_line_and_from_an_opening_of_a_fifth_it_is_the_open_mouths() {
    let closed_colour = flat().mouth.line_colour;
    let list = rim_at(0.0601);
    let strokes: Vec<(&Paint, f32)> = list
        .cmds()
        .iter()
        .filter_map(|c| match c {
            Cmd::Stroke { paint, width, opacity, .. } if *opacity > 0.01 => Some((paint, *width)),
            _ => None,
        })
        .collect();
    assert_eq!(strokes.len(), 1, "only the outer edge shows at the switch, not the light rim: {strokes:?}");
    let omuzaki_face_api::ColourRef::Fixed(line) = closed_colour else { panic!("the default line colour is fixed") };
    assert_eq!(strokes[0].0, &Paint::Solid(line), "the rim starts as the closed line's colour");
    // From 0.2 up, the rim is the full two-tone rim: 2.4 mm dark, 1.2 mm light.
    let widths: Vec<f32> = stroke_widths(&rim_at(0.2));
    assert_eq!(widths.len(), 2);
    assert_close(widths[0], 0.0024, "dark rim");
    assert_close(widths[1], 0.0012, "light rim");
    // In between, both are partway.
    let mid = stroke_widths(&rim_at(0.13));
    assert!(mid[1] > 0.0 && mid[1] < 0.0012, "the light rim fades in: {mid:?}");
}

// ------------------------------------------------------------------------------------------------ determinism, allocation

#[test]
fn the_same_face_paints_the_same_list_and_a_different_mouth_a_different_one() {
    let a = paint(&flat(), &params(|m| m.open = 0.5));
    let b = paint(&flat(), &params(|m| m.open = 0.5));
    let c = paint(&flat(), &params(|m| m.open = 0.51));
    assert_eq!(a.fingerprint(), b.fingerprint());
    assert_ne!(a.fingerprint(), c.fingerprint());
}

#[test]
fn painting_the_mouth_allocates_nothing_once_the_list_is_warm() {
    let profile = flat();
    // One of each shape, cycling: the closed line, the kiss and the open mouth (with and without a corner lift).
    let shapes = [
        params(|_| {}),
        params(|m| m.pucker = 0.9),
        params(|m| m.open = 0.7),
        params(|m| (m.open, m.round, m.smile_left) = (0.4, 1.0, 1.5)),
    ];
    let mut out = FaceDrawing::new();
    for p in &shapes {
        paint_into(&profile, p, &mut out);
    }
    let mut painter = painter();
    let allocations = count_allocations(|| {
        for i in 0..1000 {
            let p = &shapes[i % shapes.len()];
            let input = FaceInput { profile: &profile, params: p, time: Seconds(0.0), view: FaceView::default() };
            painter.paint_mouth_piece(MouthPiece::Shape, &input, &mut out).unwrap();
            black_box(&out);
        }
    });
    assert_eq!(allocations, 0, "1000 mouth paints made {allocations} allocations");
}

#[test]
fn painting_the_mouth_across_the_switch_allocates_nothing_once_the_list_is_warm() {
    let profile = glossy();
    // The openings the rim and the closed fang ease over, every piece that reads the fade.
    let openings: Vec<PaintParams> = [0.05, 0.0601, 0.1, 0.15, 0.2]
        .into_iter()
        .map(|open| params(|m| (m.open, m.fangs, m.smile_left) = (open, 1.0, 0.8)))
        .collect();
    let pieces = [MouthPiece::Shape, MouthPiece::Interior, MouthPiece::Gloss, MouthPiece::Rim];
    let mut out = FaceDrawing::new();
    let mut painter = painter();
    let mut paint_all = |out: &mut FaceDrawing, i: usize| {
        let input = FaceInput {
            profile: &profile,
            params: &openings[i % openings.len()],
            time: Seconds(0.0),
            view: FaceView::default(),
        };
        for piece in pieces {
            painter.paint_mouth_piece(piece, &input, out).unwrap();
        }
    };
    for i in 0..openings.len() {
        paint_all(&mut out, i);
    }
    let allocations = count_allocations(|| {
        for i in 0..1000 {
            paint_all(&mut out, i);
            black_box(&out);
        }
    });
    assert_eq!(allocations, 0, "1000 paints across the switch made {allocations} allocations");
}

#[test]
fn the_counter_sees_an_allocation() {
    let allocations = count_allocations(|| {
        black_box(Vec::<u8>::with_capacity(64));
    });
    assert_eq!(allocations, 1, "without this the zero-allocation test proves nothing");
}
