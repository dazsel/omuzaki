//! The mouth interior through the real painter: the clip, the teeth within the mouth, the tongue below the line, the
//! lower teeth on a smile only, the rim and gloss by lip strength, and the zero-allocation guarantee.
//!
//! The match with the prototype is in `crates/omuzaki-test-face/tests/face_parity_mouth_interior.rs`.

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
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face::{MouthPiece, RealPainter, Section};
use omuzaki_face_api::{
    Cmd, DEFAULT_PX_PER_M, DrawList, FaceDrawing, FaceInput, FaceLayerId, FaceLayerSpec, FaceMapping, FaceProfile,
    FaceProjection, FaceRasterizer, FaceRect, FaceView, MouthParams, PaintParams, PathEl, PathRef, Raster,
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

fn projection() -> FaceProjection {
    FaceProjection { rect: FaceRect { x0: -0.1, x1: 0.1, y0: -0.2, y1: -0.1 }, front_fade: [0.0, 0.05] }
}

fn painter() -> RealPainter {
    RealPainter::new(projection(), DEFAULT_PX_PER_M)
}

fn params(edit: impl FnOnce(&mut MouthParams)) -> PaintParams {
    let mut p = FaceMapping::default().rest_params();
    p.mouth = MouthParams { smile_left: 0.0, smile_right: 0.0, ..p.mouth };
    edit(&mut p.mouth);
    p
}

fn glossy() -> FaceProfile {
    let mut p = FaceProfile::default();
    p.mouth.lips.strength = 1.0;
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

/// The open shape's outline points (upper edge left to right, then the lower edge back).
fn outline(profile: &FaceProfile, p: &PaintParams) -> Vec<Vec2> {
    let list = piece(MouthPiece::Shape, profile, p);
    let Some(Cmd::Fill { path, .. }) = list.cmds().first() else { panic!("no open shape") };
    points(&list, *path)
}

fn raster(list: &DrawList) -> Raster {
    let spec = FaceLayerSpec::for_layer(FaceLayerId::Base, &projection(), DEFAULT_PX_PER_M);
    let mut raster = Raster::new(1, 1);
    SkiaRasterizer::new().rasterize(&spec, list, DEFAULT_PX_PER_M, &mut raster).unwrap();
    raster
}

/// Even-odd point-in-polygon.
fn inside(poly: &[Vec2], p: Vec2) -> bool {
    let mut hit = false;
    for (a, b) in poly.iter().zip(poly.iter().cycle().skip(1)) {
        if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x) {
            hit = !hit;
        }
    }
    hit
}

/// A busy open mouth: every interior element on.
fn full(m: &mut MouthParams) {
    (m.open, m.teeth_upper, m.teeth_lower, m.fangs, m.tongue, m.smile_left, m.smile_right) =
        (0.8, 0.9, 0.9, 1.0, 0.8, 1.0, 0.6);
}

/// A few mouths that exercise the interior: a grin with fangs, a round `bleh`-like open mouth with the tongue out, a
/// wide `ee` with clenched teeth, and a frown.
fn poses() -> [PaintParams; 4] {
    [
        params(|m| (m.open, m.smile_left, m.smile_right, m.fangs, m.teeth_upper) = (0.7, 1.2, 1.2, 1.0, 0.8)),
        params(|m| (m.open, m.round, m.tongue, m.teeth_upper) = (0.5, 0.6, 1.0, 0.4)),
        params(|m| (m.open, m.wide, m.teeth_upper, m.teeth_lower) = (0.3, 1.3, 1.0, 1.0)),
        params(|m| (m.open, m.smile_left, m.smile_right, m.teeth_lower) = (0.6, -1.0, -1.0, 0.8)),
    ]
}

// ------------------------------------------------------------------------------------------------ properties

#[test]
fn no_interior_pixel_lands_outside_the_open_mouth() {
    let profile = glossy();
    for p in poses().iter().chain([&params(full)]) {
        let poly = outline(&profile, p);
        let list = piece(MouthPiece::Interior, &profile, p);
        assert!(matches!(list.cmds().first(), Some(Cmd::PushClip(_))), "the interior starts with its clip");
        let r = raster(&list);
        let spec = FaceLayerSpec::for_layer(FaceLayerId::Base, &projection(), DEFAULT_PX_PER_M);
        let mut inked = 0;
        for (i, px) in r.pixels.as_chunks::<4>().0.iter().enumerate() {
            if px[3] == 0 {
                continue;
            }
            inked += 1;
            // Pixel centre in metres; a one-pixel margin allows for the anti-aliased edge.
            let (x, y) = (i % r.width as usize, i / r.width as usize);
            let m = 1.0 / spec.px_per_m;
            let at =
                Vec2::new(projection().rect.x0 + (x as f32 + 0.5) * m, projection().rect.y1 - (y as f32 + 0.5) * m);
            let near = [
                Vec2::X,
                -Vec2::X,
                Vec2::Y,
                -Vec2::Y,
                Vec2::ONE,
                -Vec2::ONE,
                Vec2::new(1.0, -1.0),
                Vec2::new(-1.0, 1.0),
            ]
            .iter()
            .any(|d| inside(&poly, at + *d * m));
            assert!(inside(&poly, at) || near, "pixel ({x}, {y}) is outside the mouth");
        }
        assert!(inked > 0, "the interior drew something");
    }
}

#[test]
fn teeth_and_fangs_never_exceed_the_mouth_width() {
    let profile = glossy();
    for p in poses().iter().chain([&params(full)]) {
        let poly = outline(&profile, p);
        let (lo, hi) =
            (poly.iter().map(|v| v.x).fold(f32::MAX, f32::min), poly.iter().map(|v| v.x).fold(f32::MIN, f32::max));
        let list = piece(MouthPiece::Interior, &profile, p);
        // Skip the tongue (the first two fills): everything after it is teeth, split line or fangs.
        let paths: Vec<PathRef> = list
            .cmds()
            .iter()
            .filter_map(|c| match c {
                Cmd::Fill { path, .. } | Cmd::Stroke { path, .. } => Some(*path),
                _ => None,
            })
            .skip(2)
            .collect();
        for path in paths {
            for v in points(&list, path) {
                assert!(v.x >= lo - 1e-6 && v.x <= hi + 1e-6, "x {} outside {lo}..{hi}", v.x);
            }
        }
    }
}

#[test]
fn the_tongue_tab_hangs_below_the_lower_edge_and_never_rises_above_its_start() {
    let profile = glossy();
    let p = params(|m| (m.open, m.tongue) = (0.6, 1.0));
    let poly = outline(&profile, &p);
    // The lower edge in the middle: the 28-sample upper edge has 29 points, the lower edge comes back through u = 0.
    let bottom_mid = poly[29 + 14].y;
    let list = piece(MouthPiece::Tongue, &profile, &p);
    let Some(Cmd::Fill { path, .. }) = list.cmds().first() else { panic!("no tab") };
    let tab = points(&list, *path);
    assert!(tab.len() > 3, "the tab has its curve and its end points");
    assert!(tab.iter().all(|v| v.y <= bottom_mid + 0.0012 + 1e-6), "the tab never rises above where it starts");
    assert!(tab.iter().any(|v| v.y < bottom_mid), "and it hangs below the lower edge");
}

#[test]
fn the_gloss_hangs_from_the_lower_edge_as_drawn_at_every_smile() {
    let profile = glossy();
    for smile in [-1.5, 0.0, 1.5] {
        let p = params(|m| (m.open, m.smile_left, m.smile_right) = (0.6, smile, smile));
        let bottom_mid = outline(&profile, &p)[29 + 14].y;
        let list = piece(MouthPiece::Gloss, &profile, &p);
        let centre = list
            .cmds()
            .iter()
            .find_map(|c| match c {
                Cmd::Fill { paint: omuzaki_face_api::Paint::Radial { centre, .. }, .. } => Some(*centre),
                _ => None,
            })
            .unwrap();
        assert!(
            (centre.y - (bottom_mid - 0.0042)).abs() < 1e-6,
            "smile {smile}: gloss at {}, edge {bottom_mid}",
            centre.y
        );
    }
}

#[test]
fn the_tongue_tab_fades_in_from_nothing_instead_of_popping() {
    let profile = glossy();
    let opacity = |tongue: f32| {
        let p = params(|m| (m.open, m.tongue) = (0.6, tongue));
        piece(MouthPiece::Tongue, &profile, &p).cmds().iter().find_map(|c| match c {
            Cmd::Fill { opacity, .. } => Some(*opacity),
            _ => None,
        })
    };
    assert_eq!(opacity(0.005), None, "below the threshold there is no tab");
    assert!(opacity(0.0101).unwrap() < 0.01, "just above it the tab is nearly invisible");
    let mut last = 0.0;
    for i in 1..=12 {
        let now = opacity(0.01 + 0.01 * i as f32).unwrap();
        assert!(now >= last && now - last < 0.11, "a steady ramp");
        last = now;
    }
    assert_eq!(opacity(0.5), Some(1.0), "full from a tenth above the threshold");
}

#[test]
fn the_split_line_fades_in_with_the_lower_teeth() {
    let profile = glossy();
    let opacity = |lower: f32| {
        let p = params(|m| {
            (m.open, m.smile_left, m.smile_right, m.teeth_upper, m.teeth_lower) = (0.8, 1.5, 1.5, 1.0, lower);
        });
        piece(MouthPiece::Interior, &profile, &p).cmds().iter().find_map(|c| match c {
            Cmd::Stroke { opacity, .. } => Some(*opacity),
            _ => None,
        })
    };
    assert_eq!(opacity(0.005), None, "below the threshold there is no row, and no line");
    assert!(opacity(0.0101).unwrap() < 0.02, "just above it the line is nearly invisible");
    let mut last = opacity(0.02).unwrap();
    for i in 1..=98 {
        let now = opacity(0.02 + 0.01 * i as f32).unwrap();
        assert!(now >= last && now - last <= 0.011, "a steady ramp: {last} then {now}");
        last = now;
    }
    assert_eq!(opacity(1.0), Some(1.0), "full with the row");
}

#[test]
fn lower_teeth_show_on_a_real_smile_and_not_on_a_flat_mouth() {
    let profile = glossy();
    let ink = |smile: f32, lower: f32| {
        let p = params(|m| (m.open, m.smile_left, m.smile_right, m.teeth_lower) = (0.8, smile, smile, lower));
        raster(&piece(MouthPiece::Interior, &profile, &p)).pixels
    };
    assert_eq!(ink(0.0, 1.0), ink(0.0, 0.0), "the row sits below a flat mouth and is clipped away");
    assert_ne!(ink(1.5, 1.0), ink(1.5, 0.0), "a smile lowers the mouth into the row");
}

#[test]
fn the_default_fang_is_six_millimetres_across_its_base() {
    let profile = FaceProfile::default();
    let p = params(|m| (m.open, m.fangs) = (0.8, 1.0));
    let list = piece(MouthPiece::Interior, &profile, &p);
    let triangles: Vec<Vec<Vec2>> = list
        .cmds()
        .iter()
        .filter_map(|c| match c {
            Cmd::Fill { path, .. } if points(&list, *path).len() == 3 => Some(points(&list, *path)),
            _ => None,
        })
        .collect();
    assert_eq!(triangles.len(), 2);
    let grow = FaceProfile::default().mouth.open_scale as f32;
    for t in &triangles {
        assert!(((t[1].x - t[0].x) - 0.006 * grow).abs() < 1e-6, "base width {}", t[1].x - t[0].x);
    }
    assert!((triangles[0][0].x + triangles[0][1].x + triangles[1][0].x + triangles[1][1].x).abs() < 1e-5, "mirrored");
}

/// The two fangs' triangles, left one first. Each triangle is its two base corners, then the tip.
fn fang_triangles(list: &DrawList) -> Vec<Vec<Vec2>> {
    let mut triangles: Vec<Vec<Vec2>> = list
        .cmds()
        .iter()
        .filter_map(|c| match c {
            Cmd::Fill { path, .. } if points(list, *path).len() == 3 => Some(points(list, *path)),
            _ => None,
        })
        .collect();
    triangles.sort_by(|a, b| (a[0].x + a[1].x).total_cmp(&(b[0].x + b[1].x)));
    triangles
}

#[test]
fn fang_bases_stay_inside_a_narrow_mouth() {
    for position in [0.72, 0.85, 1.0] {
        let mut profile = FaceProfile::default();
        profile.mouth.fang.position = position;
        for wide in [0.3, 0.5, 1.0] {
            for open in [0.3, 0.8, 1.5] {
                let p = params(|m| (m.fangs, m.wide, m.open) = (1.0, wide, open));
                let poly = outline(&profile, &p);
                let (lo, hi) = (
                    poly.iter().map(|v| v.x).fold(f32::MAX, f32::min),
                    poly.iter().map(|v| v.x).fold(f32::MIN, f32::max),
                );
                let triangles = fang_triangles(&piece(MouthPiece::Interior, &profile, &p));
                assert_eq!(triangles.len(), 2, "position {position}, wide {wide}, open {open}");
                for t in &triangles {
                    for corner in &t[..2] {
                        assert!(
                            corner.x >= lo - 1e-6 && corner.x <= hi + 1e-6,
                            "corner x {} outside {lo}..{hi} (position {position}, wide {wide}, open {open})",
                            corner.x,
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn a_fang_narrows_from_its_outer_side_only() {
    let mut profile = FaceProfile::default();
    profile.mouth.fang.position = 0.72;
    let p = params(|m| (m.fangs, m.wide, m.open) = (1.0, 0.3, 0.8));
    let poly = outline(&profile, &p);
    let (lo, hi) =
        (poly.iter().map(|v| v.x).fold(f32::MAX, f32::min), poly.iter().map(|v| v.x).fold(f32::MIN, f32::max));
    let (centre, hw) = (lo.midpoint(hi), (hi - lo) / 2.0);
    let grow = FaceProfile::default().mouth.open_scale as f32;
    let half = 0.003 * grow;
    let triangles = fang_triangles(&piece(MouthPiece::Interior, &profile, &p));
    assert_eq!(triangles.len(), 2);
    let (left, right) = (&triangles[0], &triangles[1]);
    // The inner corner keeps its full half-width; the outer corner stops at the mouth's corner.
    assert!((right[0].x - (centre + 0.72 * hw - half)).abs() < 1e-6, "right inner {}", right[0].x);
    assert!((right[1].x - hi).abs() < 1e-6, "right outer {} is not the mouth's corner {hi}", right[1].x);
    assert!((left[1].x - (centre - 0.72 * hw + half)).abs() < 1e-6, "left inner {}", left[1].x);
    assert!((left[0].x - lo).abs() < 1e-6, "left outer {} is not the mouth's corner {lo}", left[0].x);
}

#[test]
fn the_rim_and_the_gloss_follow_the_lip_strength() {
    let p = params(|m| m.open = 0.6);
    let strokes = |list: &DrawList| list.cmds().iter().filter(|c| matches!(c, Cmd::Stroke { .. })).count();
    let flat = FaceProfile::default();
    assert_eq!(strokes(&piece(MouthPiece::Rim, &flat, &p)), 1, "a flat face gets the plain line");
    assert!(piece(MouthPiece::Gloss, &flat, &p).is_empty(), "and no gloss");
    assert_eq!(strokes(&piece(MouthPiece::Rim, &glossy(), &p)), 2, "a lip face gets dark and light");
    assert!(!piece(MouthPiece::Gloss, &glossy(), &p).is_empty());
    let shut = params(|_| {});
    assert!(piece(MouthPiece::Rim, &glossy(), &shut).is_empty(), "a closed mouth has no rim");
}

#[test]
fn the_whole_mouth_paints_the_pieces_in_the_prototypes_order() {
    let profile = glossy();
    let p = params(full);
    let mut out = FaceDrawing::new();
    let input = FaceInput { profile: &profile, params: &p, time: Seconds(0.0), view: FaceView::default() };
    painter().paint_section(Section::Mouth, &input, &mut out).unwrap();
    let cmds = out[FaceLayerId::Base].cmds();
    let at = |f: fn(&Cmd) -> bool| cmds.iter().position(f).unwrap();
    let transform = at(|c| matches!(c, Cmd::PushTransform(_)));
    let clip = at(|c| matches!(c, Cmd::PushClip(_)));
    assert!(transform < clip, "the gloss comes before the shape and its interior");
    assert!(
        cmds.iter().position(|c| matches!(c, Cmd::Fill { paint: omuzaki_face_api::Paint::Linear { .. }, .. })).unwrap()
            > transform
    );
    assert!(clip < at(|c| matches!(c, Cmd::PopClip)));
    assert!(matches!(cmds.last(), Some(Cmd::Fill { .. })), "the tongue tab comes last");
}

// ------------------------------------------------------------------------------------------------ allocation

#[test]
fn painting_the_mouth_with_its_interior_allocates_nothing_once_the_list_is_warm() {
    let profile = glossy();
    let mut all = poses().to_vec();
    all.push(params(full));
    all.push(params(|_| {}));
    let mut painter = painter();
    let mut out = FaceDrawing::new();
    for p in &all {
        let input = FaceInput { profile: &profile, params: p, time: Seconds(0.0), view: FaceView::default() };
        painter.paint_section(Section::Mouth, &input, &mut out).unwrap();
    }
    let allocations = count_allocations(|| {
        for i in 0..1000 {
            let input = FaceInput {
                profile: &profile,
                params: &all[i % all.len()],
                time: Seconds(0.0),
                view: FaceView::default(),
            };
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
