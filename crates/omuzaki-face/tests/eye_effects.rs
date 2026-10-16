//! The eye effects and the Overlay layer through the public painter: star eyes, gloom, shock, spiral, squeeze, tears and
//! the sweat drop (the heart catchlight and the sparkle dot are covered in `eyes_iris.rs`).
//!
//! Every effect scales smoothly from 0 to 1, draws nothing at 0, composes with the others, stays inside the face rect,
//! changes the picture visibly (more than 200 pixels at full strength), and allocates nothing once the lists are warm.
//! The tear is the prototype's (`face.js` 257-263) and has a parity check in `omuzaki-test-face`; the rest are new
//! designs, signed on a contact sheet (the `evidence.toml` `eye-effects` case).

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec2;
use omuzaki_core::time::Seconds;
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face::{RealPainter, Section};
use omuzaki_face_api::{
    Cmd, DEFAULT_PX_PER_M, DrawList, EyeEffects, FaceDrawing, FaceInput, FaceLayerId, FaceLayerSpec, FaceMapping,
    FacePainter, FaceProfile, FaceProjection, FaceRasterizer, FaceRect, FaceView, PaintParams, PathEl, Raster,
};

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

fn projection() -> FaceProjection {
    FaceProjection { rect: FaceRect { x0: -0.2, x1: 0.2, y0: -0.26, y1: 0.03 }, front_fade: [0.0, 0.07] }
}

fn painter() -> RealPainter {
    RealPainter::new(projection(), DEFAULT_PX_PER_M)
}

/// Rest parameters with `edit` applied to the effects and the sweat.
fn params(edit: impl FnOnce(&mut EyeEffects, &mut f32)) -> PaintParams {
    let mut p = FaceMapping::default().rest_params();
    let mut sweat = p.sweat;
    edit(&mut p.effects, &mut sweat);
    p.sweat = sweat;
    p
}

fn profile() -> FaceProfile {
    let mut profile = FaceProfile::default();
    // Face-plane numbers of the mock face, so the temple and forehead sit inside the rect.
    profile.layout.eye_y = -0.09;
    profile
}

fn paint(profile: &FaceProfile, p: &PaintParams) -> FaceDrawing {
    let input = FaceInput { profile, params: p, time: Seconds(0.0), view: FaceView::default() };
    let mut out = FaceDrawing::new();
    for section in [Section::Eyes, Section::Overlay] {
        let mut part = FaceDrawing::new();
        painter().paint_section(section, &input, &mut part).unwrap();
        for layer in FaceLayerId::ALL {
            for cmd in part[layer].cmds() {
                // Paths are per list: copy each command's path across.
                match *cmd {
                    Cmd::Fill { path, paint, opacity } => {
                        let copied = copy_path(&part[layer], path, &mut out[layer]);
                        out[layer].push(Cmd::Fill { path: copied, paint, opacity });
                    }
                    Cmd::Stroke { path, paint, width, cap, join, opacity } => {
                        let copied = copy_path(&part[layer], path, &mut out[layer]);
                        out[layer].push(Cmd::Stroke { path: copied, paint, width, cap, join, opacity });
                    }
                    // A clip names a path of its own list too, so it moves with the copy.
                    Cmd::PushClip(path) => {
                        let copied = copy_path(&part[layer], path, &mut out[layer]);
                        out[layer].push(Cmd::PushClip(copied));
                    }
                    other => out[layer].push(other),
                }
            }
        }
    }
    out
}

fn copy_path(src: &DrawList, path: omuzaki_face_api::PathRef, out: &mut DrawList) -> omuzaki_face_api::PathRef {
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

/// Pixels (all four channels) where two rasterisations of `layer` differ.
fn changed_pixels(layer: FaceLayerId, a: &DrawList, b: &DrawList) -> usize {
    let spec = FaceLayerSpec::for_layer(layer, &projection(), DEFAULT_PX_PER_M);
    let raster = |list: &DrawList| {
        let mut r = Raster::new(1, 1);
        SkiaRasterizer::new().rasterize(&spec, list, spec.px_per_m, &mut r).unwrap();
        r
    };
    let (ra, rb) = (raster(a), raster(b));
    assert_eq!((ra.width, ra.height), (rb.width, rb.height));
    ra.pixels.as_chunks::<4>().0.iter().zip(rb.pixels.as_chunks::<4>().0).filter(|(x, y)| x != y).count()
}

/// All the points of a list's paths.
fn points(list: &DrawList) -> Vec<Vec2> {
    list.cmds()
        .iter()
        .filter_map(|c| match c {
            Cmd::Fill { path, .. } | Cmd::Stroke { path, .. } | Cmd::PushClip(path) => Some(*path),
            _ => None,
        })
        .flat_map(|path| list.elements(path).to_vec())
        .flat_map(|el| match el {
            PathEl::Move(p) | PathEl::Line(p) => vec![p],
            PathEl::Quad(c, p) => vec![c, p],
            PathEl::Cubic(a, b, p) => vec![a, b, p],
            PathEl::Close => vec![],
        })
        .collect()
}

/// The effects, one at a time at full strength, by name.
fn each() -> [(&'static str, PaintParams); 7] {
    [
        ("star", params(|e, _| e.star = 1.0)),
        ("gloom", params(|e, _| e.gloom = 1.0)),
        ("shock", params(|e, _| e.shock = 1.0)),
        ("spiral", params(|e, _| e.spiral = 1.0)),
        ("squeeze", params(|e, _| e.squeeze = 1.0)),
        ("tears", params(|e, _| e.tears = 1.0)),
        ("sweat", params(|_, s| *s = 1.0)),
    ]
}

// ------------------------------------------------------------------------------------------------ the effects

#[test]
fn effects_at_zero_add_nothing() {
    let profile = profile();
    let rest = paint(&profile, &params(|_, _| {}));
    let zero = paint(
        &profile,
        &params(|e, s| {
            (e.star, e.gloom, e.shock, e.spiral, e.squeeze, e.tears, *s) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        }),
    );
    for layer in FaceLayerId::ALL {
        assert_eq!(rest[layer].cmds(), zero[layer].cmds());
    }
    assert_eq!(rest[FaceLayerId::Overlay].cmds().len(), 0, "no sweat and no gloom, no overlay");
}

#[test]
fn every_effect_changes_the_picture_and_stays_within_its_command_budget() {
    let profile = profile();
    let rest = paint(&profile, &params(|_, _| {}));
    for (name, p) in each() {
        let on = paint(&profile, &p);
        let changed: usize = [FaceLayerId::Base, FaceLayerId::Overlay]
            .into_iter()
            .map(|layer| changed_pixels(layer, &rest[layer], &on[layer]))
            .sum();
        assert!(changed > 200, "{name} changed only {changed} pixels");
        let added: usize = [FaceLayerId::Base, FaceLayerId::Overlay]
            .into_iter()
            .map(|layer| on[layer].cmds().len().saturating_sub(rest[layer].cmds().len()))
            .sum();
        assert!(added <= 300, "{name} adds {added} commands");
    }
    // The heart and the sparkle are `highlight`'s: they change the eye too.
    for (name, p) in [("heart", params(|e, _| e.heart = 1.0)), ("sparkle", params(|e, _| e.sparkle = 1.0))] {
        let on = paint(&profile, &p);
        let changed = changed_pixels(FaceLayerId::Base, &rest[FaceLayerId::Base], &on[FaceLayerId::Base]);
        assert!(changed > 50, "{name} changed only {changed} pixels");
    }
}

/// The sum of the opacities of a list's fills and strokes.
fn opacity_sum(list: &DrawList) -> f32 {
    list.cmds()
        .iter()
        .map(|c| match c {
            Cmd::Fill { opacity, .. } | Cmd::Stroke { opacity, .. } => *opacity,
            _ => 0.0,
        })
        .sum()
}

#[test]
fn every_effect_scales_with_its_strength() {
    let profile = profile();
    // The opacity of each effect's own commands grows with the channel: half the strength paints less than the full.
    let rest = paint(&profile, &params(|_, _| {}));
    for (name, edit) in [
        ("star", (|e: &mut EyeEffects, v: f32, _: &mut f32| e.star = v) as fn(&mut EyeEffects, f32, &mut f32)),
        ("gloom", |e, v, _| e.gloom = v),
        ("shock", |e, v, _| e.shock = v),
        ("spiral", |e, v, _| e.spiral = v),
        ("squeeze", |e, v, _| e.squeeze = v),
        ("tears", |e, v, _| e.tears = v),
        ("sweat", |_, v, s| *s = v),
    ] {
        let pixels = |v: f32| {
            let on = paint(&profile, &params(|e, s| edit(e, v, s)));
            [FaceLayerId::Base, FaceLayerId::Overlay]
                .into_iter()
                .map(|layer| changed_pixels(layer, &rest[layer], &on[layer]))
                .sum::<usize>()
        };
        let (quarter, half, full) = (pixels(0.25), pixels(0.5), pixels(1.0));
        assert!(quarter > 0 && quarter <= half + half / 4, "{name}: {quarter} then {half}");
        assert!(half > 0 && full > 0, "{name}");
        // The footprint of an opacity-only effect hardly changes, so also measure the ink: how far the summed command
        // opacities move from the rest, which must grow with the strength.
        let ink = |v: f32| {
            let on = paint(&profile, &params(|e, s| edit(e, v, s)));
            [FaceLayerId::Base, FaceLayerId::Overlay]
                .into_iter()
                .map(|layer| (opacity_sum(&on[layer]) - opacity_sum(&rest[layer])).abs())
                .sum::<f32>()
        };
        let (quarter, half, full) = (ink(0.25), ink(0.5), ink(1.0));
        assert!(quarter > 0.0 && quarter < half && half < full, "{name}: ink {quarter}, {half}, {full}");
    }
}

#[test]
fn effects_compose_and_stay_inside_the_face_rect() {
    let profile = profile();
    let all = params(|e, s| {
        (e.heart, e.star, e.gloom, e.shock, e.spiral, e.squeeze, e.sparkle, e.tears, *s) =
            (1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0);
    });
    let drawing = paint(&profile, &all);
    let rect = projection().rect;
    for layer in [FaceLayerId::Base, FaceLayerId::Overlay] {
        for p in points(&drawing[layer]) {
            assert!(
                p.x >= rect.x0 && p.x <= rect.x1 && p.y >= rect.y0 && p.y <= rect.y1,
                "{layer:?}: a point at {p:?} is outside the face rect"
            );
        }
    }
    // A heart with tears and blush together draws all three.
    let some = params(|e, _| (e.heart, e.tears) = (1.0, 1.0));
    let mut with_blush = some;
    with_blush.blush = 1.0;
    // Through the cheeks section as well as the eyes: the blush adds its own commands and leaves the eyes' alone.
    let with_cheeks = |p: &PaintParams| {
        let mut out = paint(&profile, p);
        let input = FaceInput { profile: &profile, params: p, time: Seconds(0.0), view: FaceView::default() };
        let mut cheeks = FaceDrawing::new();
        painter().paint_section(Section::Cheeks, &input, &mut cheeks).unwrap();
        (out[FaceLayerId::Base].cmds().len(), cheeks[FaceLayerId::Base].cmds().len(), std::mem::take(&mut out))
    };
    let (eyes_a, cheeks_a, _) = with_cheeks(&some);
    let (eyes_b, cheeks_b, _) = with_cheeks(&with_blush);
    assert_eq!(eyes_a, eyes_b, "blush leaves the eyes' commands alone");
    assert!(cheeks_b > cheeks_a, "blush draws cheek commands ({cheeks_a} then {cheeks_b})");
}

// ------------------------------------------------------------------------------------------------ each design

#[test]
fn a_tear_hangs_below_the_eye_toward_the_nose_and_only_fades() {
    let profile = profile();
    let tear = |v: f32| {
        let list = paint(&profile, &params(|e, _| e.tears = v)).into_base();
        // The tear is the fill in the style's tear colour (the default `rgba(190,225,255,0.85)`).
        list.cmds()
            .iter()
            .filter_map(|c| match c {
                Cmd::Fill { path, paint: omuzaki_face_api::Paint::Solid(c), opacity }
                    if (c.r, c.g, c.b) == (190, 225, 255) =>
                {
                    Some((points(&one(&list, *path)), *opacity))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let (half, full) = (tear(0.5), tear(1.0));
    assert_eq!(full.len(), 2, "one drop per eye");
    assert!((half[0].1 - 0.5).abs() < 1e-6 && (full[0].1 - 1.0).abs() < 1e-6, "only the opacity animates");
    // Same drop at any strength: the size is fixed.
    assert_eq!(half[0].0, full[0].0);
    let (lo, hi) = full[0].0.iter().fold((Vec2::MAX, Vec2::MIN), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
    assert!((hi.y - lo.y - 0.013).abs() < 1e-6, "13 mm tall");
    assert!(hi.x - lo.x < 0.0085 && hi.x - lo.x > 0.003, "about 4 to 8 mm wide");
    // The left eye's drop (the first) sits toward the nose of that eye: left of its centre at +0.09, and below it.
    assert!(lo.x < 0.09 && hi.y < -0.09 - 0.062 * 0.051 + 1e-3);
}

#[test]
fn gloom_darkens_the_iris_and_takes_the_light_out_of_the_highlights() {
    let profile = profile();
    let light = |gloom: f32| {
        let list = paint(&profile, &params(|e, _| e.gloom = gloom)).into_base();
        // The catchlight is the white fill at 0.95 of its strength.
        list.cmds()
            .iter()
            .filter_map(|c| match c {
                Cmd::Fill { paint: omuzaki_face_api::Paint::Solid(c), opacity, .. }
                    if (c.r, c.g, c.b) == (255, 255, 255) =>
                {
                    Some(*opacity)
                }
                _ => None,
            })
            .fold(0.0_f32, f32::max)
    };
    assert!((light(0.0) - 0.95).abs() < 1e-5);
    assert!((light(0.5) - 0.475).abs() < 1e-5, "half the light at half the gloom");
    assert!(light(1.0) < 1e-5, "no white highlight left");
}

#[test]
fn a_squeezed_eye_closes_into_a_chevron_pointing_at_the_nose() {
    let profile = profile();
    let list = paint(&profile, &params(|e, _| e.squeeze = 1.0)).into_base();
    // Fully squeezed there is no aperture (no clip): the lids are shut and the chevron stands in for the relaxed curve.
    assert_eq!(list.cmds().iter().filter(|c| matches!(c, Cmd::PushClip(_))).count(), 0);
    let chevrons: Vec<Vec<Vec2>> = list
        .cmds()
        .iter()
        .filter_map(|c| match c {
            Cmd::Stroke { path, width, .. } if (*width - 0.0048).abs() < 1e-7 => Some(points(&one(&list, *path))),
            _ => None,
        })
        .collect();
    assert_eq!(chevrons.len(), 2);
    for (i, chevron) in chevrons.iter().enumerate() {
        assert_eq!(chevron.len(), 3, "two arms and a vertex");
        // The vertex is the innermost point: toward the midline of the face.
        let vertex = chevron[1];
        let side = if i == 0 { 1.0 } else { -1.0 };
        assert!(side * vertex.x < side * chevron[0].x && side * vertex.x < side * chevron[2].x, "{chevron:?}");
    }
}

#[test]
fn the_sweat_drop_sits_at_the_left_temple_in_front_of_the_hair() {
    let profile = profile();
    let drawing = paint(&profile, &params(|_, s| *s = 1.0));
    assert_eq!(
        drawing[FaceLayerId::Base].cmds().len(),
        paint(&profile, &params(|_, _| {}))[FaceLayerId::Base].cmds().len()
    );
    let pts = points(&drawing[FaceLayerId::Overlay]);
    assert_ne!(pts.len(), 0);
    assert!(pts.iter().all(|p| p.x > 0.09), "the character's left (+x) temple");
    assert!(pts.iter().all(|p| p.y > -0.09), "above the eye line");
}

#[test]
fn gloom_draws_its_band_and_hatch_on_the_overlay() {
    let drawing = paint(&profile(), &params(|e, _| e.gloom = 1.0));
    let overlay = &drawing[FaceLayerId::Overlay];
    // The band, then nine hatch lines.
    assert_eq!(overlay.cmds().len(), 1 + 9);
}

#[test]
fn the_sweat_drop_fits_a_small_rect_instead_of_panicking() {
    let profile = profile();
    let p = params(|_, s| *s = 1.0);
    // From a normal rect down to ones narrower than 13 mm or shorter than 24 mm, and a degenerate one.
    for (w, h) in [(0.4, 0.29), (0.02, 0.2), (0.2, 0.02), (0.005, 0.005), (0.0, 0.0)] {
        let rect = FaceRect { x0: -w / 2.0, x1: w / 2.0, y0: -h / 2.0, y1: h / 2.0 };
        let proj = FaceProjection { rect, front_fade: [0.0, 0.07] };
        let input = FaceInput { profile: &profile, params: &p, time: Seconds(0.0), view: FaceView::default() };
        let mut out = FaceDrawing::new();
        RealPainter::new(proj, DEFAULT_PX_PER_M).paint_section(Section::Overlay, &input, &mut out).unwrap();
        for pt in points(&out[FaceLayerId::Overlay]) {
            assert!(
                pt.x >= rect.x0 - 1e-6 && pt.x <= rect.x1 + 1e-6 && pt.y >= rect.y0 - 1e-6 && pt.y <= rect.y1 + 1e-6,
                "{w} x {h}: a point at {pt:?} is outside the rect"
            );
        }
    }
}

#[test]
fn the_gloom_band_stays_inside_a_short_or_offset_rect() {
    let profile = profile();
    let p = params(|e, _| e.gloom = 1.0);
    // Rects that end below, around and above the band's start.
    for (y0, y1) in [(-0.3, -0.25), (-0.05, 0.0), (0.3, 0.4), (-0.26, 0.03)] {
        let rect = FaceRect { x0: -0.2, x1: 0.2, y0, y1 };
        let proj = FaceProjection { rect, front_fade: [0.0, 0.07] };
        let input = FaceInput { profile: &profile, params: &p, time: Seconds(0.0), view: FaceView::default() };
        let mut out = FaceDrawing::new();
        RealPainter::new(proj, DEFAULT_PX_PER_M).paint_section(Section::Overlay, &input, &mut out).unwrap();
        for pt in points(&out[FaceLayerId::Overlay]) {
            assert!(pt.y >= y0 - 1e-6 && pt.y <= y1 + 1e-6, "rect {y0}..{y1}: a point at {pt:?} is outside");
        }
    }
}

#[test]
fn the_gloom_band_is_cut_to_a_rect_that_does_not_straddle_the_midline() {
    let profile = profile();
    let p = params(|e, _| e.gloom = 1.0);
    // Left and right of the midline, and a rect the band misses on one side.
    for (x0, x1) in [(0.02, 0.06), (0.1, 0.2), (0.15, 0.3), (-0.3, -0.15), (-0.06, -0.02)] {
        let rect = FaceRect { x0, x1, y0: -0.5, y1: 0.5 };
        let proj = FaceProjection { rect, front_fade: [0.0, 0.07] };
        let input = FaceInput { profile: &profile, params: &p, time: Seconds(0.0), view: FaceView::default() };
        let mut out = FaceDrawing::new();
        RealPainter::new(proj, DEFAULT_PX_PER_M).paint_section(Section::Overlay, &input, &mut out).unwrap();
        let pts = points(&out[FaceLayerId::Overlay]);
        for pt in &pts {
            assert!(pt.x >= x0 - 1e-6 && pt.x <= x1 + 1e-6, "rect {x0}..{x1}: a point at {pt:?} is outside");
        }
        // The band reaches 0.12 m each side of the midline: a rect inside that gets a band, one beyond it does not.
        let reaches = x0 < 0.12 && x1 > -0.12;
        assert_eq!(!pts.is_empty(), reaches, "rect {x0}..{x1}: {} points", pts.len());
    }
}

/// The x of every hatch line (a two-point stroke) on the overlay, left to right.
fn hatch_xs(list: &DrawList) -> Vec<f32> {
    let mut xs: Vec<f32> = list
        .cmds()
        .iter()
        .filter_map(|c| match c {
            Cmd::Stroke { path, .. } => points(&one(list, *path)).first().map(|p| p.x),
            _ => None,
        })
        .collect();
    xs.sort_by(f32::total_cmp);
    xs
}

#[test]
fn a_clipped_rect_keeps_the_hatch_at_its_full_width_spacing() {
    let profile = profile();
    let p = params(|e, _| e.gloom = 1.0);
    let hatch = |x0: f32, x1: f32| {
        let proj = FaceProjection { rect: FaceRect { x0, x1, y0: -0.5, y1: 0.5 }, front_fade: [0.0, 0.07] };
        let input = FaceInput { profile: &profile, params: &p, time: Seconds(0.0), view: FaceView::default() };
        let mut out = FaceDrawing::new();
        RealPainter::new(proj, DEFAULT_PX_PER_M).paint_section(Section::Overlay, &input, &mut out).unwrap();
        hatch_xs(&out[FaceLayerId::Overlay])
    };
    let full = hatch(-0.3, 0.3);
    assert_eq!(full.len(), 9);
    let spacing = full[1] - full[0];
    // A rect from 0.05 to 0.2 keeps the lines that fall in it, at the same places and the same spacing.
    let clipped = hatch(0.05, 0.2);
    assert_eq!(clipped.len(), 3, "{clipped:?}");
    for x in &clipped {
        assert!(full.iter().any(|f| (f - x).abs() < 1e-6), "a line at {x} is not one of the full hatch's {full:?}");
    }
    assert!(clipped.windows(2).all(|w| (w[1] - w[0] - spacing).abs() < 1e-6), "{clipped:?}");
    // 0.10..0.20 keeps just the outermost line, still where it was.
    let edge = hatch(0.10, 0.20);
    assert_eq!(edge.len(), 1);
    assert!((edge[0] - full[8]).abs() < 1e-6);
}

#[test]
fn the_star_is_as_wide_as_it_is_tall_in_iris_half_widths() {
    // Radii are measured from the iris half-width `rx`: the star's physical extent is the same either way round
    // (a stretched star would be `rx^2 / ry` wide against `rx` tall).
    let profile = profile();
    let list = paint(&profile, &params(|e, _| e.star = 1.0)).into_base();
    let star = list
        .cmds()
        .iter()
        .find_map(|c| match c {
            Cmd::Fill { path, .. } if list.elements(*path).len() == 11 => Some(points(&one(&list, *path))),
            _ => None,
        })
        .expect("a ten-point star outline");
    let (min, max) = star.iter().fold((Vec2::MAX, Vec2::MIN), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
    let (w, h) = (max.x - min.x, max.y - min.y);
    // Five points: the height is the outer radius plus the lower points' reach, the width spans the side points.
    assert!(w / h > 0.9 && w / h < 1.3, "star {w} x {h} m");
}

/// The opacity of the closed eye's curve and lashes: the strokes of the lash colour and widths that are not the
/// chevron (three points, the same width as the curve).
fn closed_opacity(list: &DrawList) -> f32 {
    list.cmds()
        .iter()
        .filter_map(|c| match c {
            Cmd::Stroke { path, width, opacity, .. }
                if ((width - 0.0048).abs() < 1e-7 || (width - 0.0018).abs() < 1e-7)
                    && list.elements(*path).len() != 3 =>
            {
                Some(*opacity)
            }
            _ => None,
        })
        .fold(0.0, f32::max)
}

#[test]
fn a_squeeze_crossfades_the_closed_eye_into_the_chevron_with_no_jump() {
    let profile = profile();
    for arc in [0.0, 1.0] {
        let mut last = 1.0;
        for step in 0..=100_u8 {
            let squeeze = f32::from(step) / 100.0;
            let mut p = params(|e, _| e.squeeze = squeeze);
            p.left_eye = omuzaki_face_api::EyeParams { open: 1.0, close: 1.0, arc, lower_lid: 0.0 };
            p.right_eye = p.left_eye;
            let drawn = paint(&profile, &p).into_base();
            let closed = closed_opacity(&drawn);
            assert!((closed - (1.0 - squeeze)).abs() < 1e-5, "arc {arc} squeeze {squeeze}: closed eye at {closed}");
            assert!(last - closed < 0.011, "arc {arc}: the closed eye jumped from {last} to {closed} at {squeeze}");
            last = closed;
        }
    }
}

#[test]
fn the_spiral_keeps_the_alpha_of_the_iris_ring_colour() {
    let mut profile = profile();
    profile.eyes.iris.ring = omuzaki_face_api::ColourRef::Fixed(Colour::rgba(10, 20, 30, 0));
    let p = params(|e, _| e.spiral = 1.0);
    let drawn = paint(&profile, &p).into_base();
    let ring: Vec<u8> = drawn
        .cmds()
        .iter()
        .filter_map(|c| match c {
            Cmd::Stroke { paint: omuzaki_face_api::Paint::Solid(c), .. } if (c.r, c.g, c.b) == (10, 20, 30) => {
                Some(c.a)
            }
            _ => None,
        })
        .collect();
    assert!(!ring.is_empty(), "the spiral was not drawn in the ring colour");
    assert!(ring.iter().all(|&a| a == 0), "a transparent ring drew an opaque spiral: {ring:?}");
}

#[test]
fn squeezing_an_open_eye_hands_over_to_the_closed_curve_without_a_jump() {
    let profile = profile();
    let mut open_seen = false;
    let mut last_shown: Option<f32> = None;
    for step in 0..=200_u16 {
        let squeeze = f32::from(step) / 200.0;
        let mut p = params(|e, _| e.squeeze = squeeze);
        p.left_eye = omuzaki_face_api::EyeParams { open: 1.0, close: 0.0, arc: 0.0, lower_lid: 0.0 };
        p.right_eye = p.left_eye;
        let drawn = paint(&profile, &p).into_base();
        let closed = closed_opacity(&drawn);
        // An open eye is drawn with a clip round its iris; a shut one has none.
        let open = drawn.cmds().iter().any(|c| matches!(c, Cmd::PushClip(_)));
        if open {
            open_seen = true;
            assert!(closed == 0.0, "squeeze {squeeze}: an open eye and a closed curve together");
        } else {
            // The closed curve is there from the first frame the open eye is gone, and strong where it takes over.
            assert!(closed > 0.0 || squeeze >= 1.0, "squeeze {squeeze}: neither eye is drawn");
            if closed > 0.0 {
                match last_shown {
                    None => assert!(closed >= 0.95, "squeeze {squeeze}: the closed curve takes over at only {closed}"),
                    // The curve fades about 1 / 0.08 per unit of squeeze, so a step of 0.005 may lose 0.0625 of it.
                    Some(last) => assert!(
                        last - closed < 0.07,
                        "squeeze {squeeze}: the closed curve jumped from {last} to {closed}"
                    ),
                }
                last_shown = Some(closed);
            }
        }
    }
    assert!(open_seen, "the sweep never drew the open eye");
}

// ------------------------------------------------------------------------------------------------ allocation

/// One command's path copied into a fresh list, so its elements can be read without the rest.
fn one(list: &DrawList, path: omuzaki_face_api::PathRef) -> DrawList {
    let mut out = DrawList::new();
    let copied = copy_path(list, path, &mut out);
    out.push(Cmd::Fill {
        path: copied,
        paint: omuzaki_face_api::Paint::Solid(omuzaki_core::colour::Colour::rgb(0, 0, 0)),
        opacity: 1.0,
    });
    out
}

trait IntoBase {
    fn into_base(self) -> DrawList;
}

impl IntoBase for FaceDrawing {
    fn into_base(self) -> DrawList {
        self[FaceLayerId::Base].clone()
    }
}

fn count_allocations(f: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.with(Cell::get);
    MEASURING.with(|m| m.set(true));
    f();
    MEASURING.with(|m| m.set(false));
    ALLOCATIONS.with(Cell::get) - before
}

#[test]
fn painting_the_effects_allocates_nothing_once_the_list_is_warm() {
    let profile = profile();
    let mut poses: Vec<PaintParams> = each().into_iter().map(|(_, p)| p).collect();
    poses.push(params(|e, s| (e.heart, e.sparkle, e.tears, *s) = (1.0, 1.0, 0.5, 0.5)));
    let mut out = FaceDrawing::new();
    let mut painter = painter();
    for pose in &poses {
        let input = FaceInput { profile: &profile, params: pose, time: Seconds(0.0), view: FaceView::default() };
        painter.paint(&input, &mut out).unwrap();
    }
    let allocations = count_allocations(|| {
        for i in 0..1000 {
            let input = FaceInput {
                profile: &profile,
                params: &poses[i % poses.len()],
                time: Seconds(0.0),
                view: FaceView::default(),
            };
            painter.paint(&input, &mut out).unwrap();
            black_box(&out);
        }
    });
    assert_eq!(allocations, 0, "1000 paints made {allocations} allocations");
}

#[test]
fn the_counter_sees_an_allocation() {
    let allocations = count_allocations(|| {
        black_box(Vec::<u8>::with_capacity(64));
    });
    assert_eq!(allocations, 1, "without this the zero-allocation test proves nothing");
}
