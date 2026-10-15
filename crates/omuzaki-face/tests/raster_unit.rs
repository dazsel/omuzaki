//! The CPU rasteriser's own tests: what the mock rasteriser's suite asserts, restated for anti-aliased pixels, the
//! canvas semantics that differ from the mock (non-zero winding, straight-colour gradients, the miter limit, edges snapped
//! to a quarter row, hairlines), reuse of the output and scratch buffers, determinism (ADR 0009), a fuzz for panics, and the
//! speed and allocation budget of the neutral face.
//!
//! Parity with Chrome's canvas is `crates/omuzaki-test-face/tests/raster_probes.rs`: an implementation crate may not depend on an App crate such as `omuzaki-test-face`,
//! so everything here is an assertion of its own.
//!
//! Where the mock counts pixels whose centre is covered, an anti-aliased raster has partial pixels, so "area" here is the
//! sum of alpha over 255 (the covered area in pixels), which is what the mock's pixel count approximates.

// A counting allocator is the only way to *prove* an allocation budget; the workspace denies unsafe code by default.
// Tests may unwrap: a panic is a clear failure.
#![allow(unsafe_code, clippy::unwrap_used, clippy::expect_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::Instant;

use omuzaki_anim_api::{Channel, ChannelBuf};
use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_core::time::Seconds;
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face_api::{
    Cap, Cmd, Colour, DEFAULT_PX_PER_M, DrawList, FaceDrawing, FaceError, FaceInput, FaceLayerId, FaceLayerSpec,
    FaceMapping, FacePainter, FaceRasterizer, FaceRect, FaceView, Join, Paint, Raster, Stops,
};
use omuzaki_face_mock::{MockPainter, mock_profile};

// ------------------------------------------------------------------------------------------------ allocation counting

struct Counting;

thread_local! {
    // `const`-initialised without destructors, so touching them never allocates (this runs inside the allocator). Counting is
    // per thread, so tests running side by side cannot see each other's allocations.
    static MEASURING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    static LARGEST: Cell<usize> = const { Cell::new(0) };
}

/// Runs `f`; returns how many allocations this thread made during it and the size of the largest.
fn count_allocations(f: impl FnOnce()) -> (usize, usize) {
    let before = ALLOCATIONS.with(Cell::get);
    LARGEST.with(|l| l.set(0));
    MEASURING.with(|m| m.set(true));
    f();
    MEASURING.with(|m| m.set(false));
    (ALLOCATIONS.with(Cell::get) - before, LARGEST.with(Cell::get))
}

// SAFETY: every method forwards to `System` unchanged; the counters are the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
            let _ = LARGEST.try_with(|l| l.set(l.get().max(layout.size())));
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

/// 0.2 m x 0.1 m at 500 px/m: a 100 x 50 image, plane origin at its centre (pixel 50, 25).
fn spec() -> FaceLayerSpec {
    FaceLayerSpec::for_rect(FaceRect { x0: -0.1, x1: 0.1, y0: -0.05, y1: 0.05 }, FaceLayerId::Base, 500.0)
}

fn raster(list: &DrawList) -> Raster {
    let mut out = Raster::new(1, 1);
    SkiaRasterizer::new().rasterize(&spec(), list, 500.0, &mut out).unwrap();
    out
}

fn px(r: &Raster, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * r.width + x) * 4) as usize;
    [r.pixels[i], r.pixels[i + 1], r.pixels[i + 2], r.pixels[i + 3]]
}

/// The covered area in pixels: the sum of alpha over 255.
fn area(r: &Raster) -> f32 {
    r.pixels.chunks(4).map(|p| f32::from(p[3])).sum::<f32>() / 255.0
}

/// How many pixels have any coverage at all.
fn covered(r: &Raster) -> usize {
    r.pixels.chunks(4).filter(|p| p[3] > 0).count()
}

fn square(l: &mut DrawList, half_w: f32, half_h: f32) -> omuzaki_face_api::PathRef {
    l.path()
        .polyline(&[
            Vec2::new(-half_w, -half_h),
            Vec2::new(half_w, -half_h),
            Vec2::new(half_w, half_h),
            Vec2::new(-half_w, half_h),
        ])
        .close()
        .finish()
}

fn solid(r: u8, g: u8, b: u8) -> Paint {
    Paint::Solid(Colour::rgb(r, g, b))
}

// ------------------------------------------------------------------------------------------------ the mock suite, restated

#[test]
fn an_ellipse_covers_pi_a_b_pixels_and_y_is_up() {
    let mut l = DrawList::new();
    let p = l.path().ellipse(Vec2::new(0.0, 0.02), 0.04, 0.02).finish();
    l.fill(p, solid(255, 0, 0));
    let r = raster(&l);
    assert_eq!((r.width, r.height), (100, 50));
    let expected = std::f32::consts::PI * 20.0 * 10.0;
    assert!((area(&r) - expected).abs() < 0.01 * expected, "{} vs {expected}", area(&r));
    assert_eq!(px(&r, 50, 15), [255, 0, 0, 255], "centre of the ellipse, 0.02 m above the middle row");
    assert_eq!(px(&r, 50, 40), [0, 0, 0, 0], "below it is empty");
}

#[test]
fn alpha_is_premultiplied_and_opacity_multiplies() {
    let mut l = DrawList::new();
    let p = square(&mut l, 0.1, 0.05);
    l.push(Cmd::Fill { path: p, paint: Paint::Solid(Colour::rgba(200, 100, 0, 128)), opacity: 0.5 });
    let [red, green, blue, alpha] = px(&raster(&l), 10, 10);
    assert!((i32::from(alpha) - 64).abs() <= 1, "alpha 0.25: {alpha}");
    assert!(
        (i32::from(red) - 50).abs() <= 1 && (i32::from(green) - 25).abs() <= 1 && blue == 0,
        "{red} {green} {blue}"
    );
}

#[test]
fn clips_restrict_and_nest() {
    let mut l = DrawList::new();
    let big = l.path().ellipse(Vec2::ZERO, 0.09, 0.04).finish();
    let small = l.path().ellipse(Vec2::ZERO, 0.02, 0.02).finish();
    l.push(Cmd::PushClip(small));
    l.fill(big, solid(0, 255, 0));
    l.push(Cmd::PopClip);
    let mut alone = DrawList::new();
    let small_alone = alone.path().ellipse(Vec2::ZERO, 0.02, 0.02).finish();
    alone.fill(small_alone, solid(0, 255, 0));
    assert_eq!(raster(&l), raster(&alone), "a fill clipped to the small ellipse is exactly the small ellipse");

    // Clips intersect: the right half of the disc is the disc clipped by a half-plane, and nothing outside either survives.
    let mut nested = DrawList::new();
    let disc = nested.path().ellipse(Vec2::ZERO, 0.03, 0.03).finish();
    let right = nested
        .path()
        .polyline(&[Vec2::new(0.0, -0.05), Vec2::new(0.1, -0.05), Vec2::new(0.1, 0.05), Vec2::new(0.0, 0.05)])
        .close()
        .finish();
    nested.push(Cmd::PushClip(disc));
    nested.push(Cmd::PushClip(right));
    let sheet = square(&mut nested, 0.1, 0.05);
    nested.fill(sheet, solid(0, 0, 255));
    nested.push(Cmd::PopClip);
    nested.push(Cmd::PopClip);
    let r = raster(&nested);
    assert_eq!(px(&r, 60, 25), [0, 0, 255, 255], "inside both");
    assert_eq!(px(&r, 40, 25), [0, 0, 0, 0], "inside the disc, outside the half-plane");
    assert_eq!(px(&r, 90, 25), [0, 0, 0, 0], "inside the half-plane, outside the disc");
    let expected = std::f32::consts::PI * 15.0 * 15.0 / 2.0;
    assert!((area(&r) - expected).abs() < 0.02 * expected, "{} vs {expected}", area(&r));
}

#[test]
fn transforms_move_and_scale_everything_inside() {
    let mut l = DrawList::new();
    let p = l.path().ellipse(Vec2::ZERO, 0.02, 0.02).finish();
    l.push(Cmd::PushTransform(
        Affine2::from_translation(Vec2::new(0.05, 0.0)) * Affine2::from_scale(Vec2::new(1.0, 2.0)),
    ));
    l.fill(p, solid(0, 0, 255));
    l.push(Cmd::PopTransform);
    let r = raster(&l);
    assert_eq!(px(&r, 75, 25), [0, 0, 255, 255]);
    assert_eq!(px(&r, 75, 8), [0, 0, 255, 255], "stretched to 0.04 m tall: 20 px above the middle");
    assert_eq!(px(&r, 25, 25), [0, 0, 0, 0]);
}

#[test]
fn gradients_interpolate_between_stops() {
    let mut l = DrawList::new();
    let p = square(&mut l, 0.1, 0.05);
    let stops = Stops::new(&[(0.0, Colour::rgb(0, 0, 0)), (1.0, Colour::rgb(255, 255, 255))]);
    l.fill(p, Paint::Linear { from: Vec2::new(-0.1, 0.0), to: Vec2::new(0.1, 0.0), stops });
    let r = raster(&l);
    let (left, mid, right) = (px(&r, 1, 25)[0], px(&r, 50, 25)[0], px(&r, 98, 25)[0]);
    assert!(left < 10 && (i32::from(mid) - 128).abs() < 8 && right > 245, "{left} {mid} {right}");
}

#[test]
fn strokes_have_the_requested_width() {
    let mut l = DrawList::new();
    let p = l.path().polyline(&[Vec2::new(-0.05, 0.0), Vec2::new(0.05, 0.0)]).finish();
    l.push(Cmd::Stroke {
        path: p,
        paint: solid(9, 9, 9),
        width: 0.01,
        cap: Cap::Butt,
        join: Join::Miter,
        opacity: 1.0,
    });
    let r = raster(&l);
    assert!((area(&r) - 250.0).abs() < 2.5, "0.1 m x 0.01 m at 500 px/m is 50 x 5 px, got {}", area(&r));
    assert_eq!(px(&r, 50, 24)[3], 255, "rows 23 to 26 are full, rows 22 and 27 half");
    assert_eq!(px(&r, 50, 20), [0, 0, 0, 0]);
}

#[test]
fn unbalanced_clips_and_transforms_are_errors() {
    for cmd in [Cmd::PopClip, Cmd::PopTransform] {
        let mut l = DrawList::new();
        l.push(cmd);
        let mut out = Raster::new(1, 1);
        assert!(matches!(SkiaRasterizer::new().rasterize(&spec(), &l, 500.0, &mut out), Err(FaceError::Raster(_))));
    }
    let mut out = Raster::new(1, 1);
    let mut r = SkiaRasterizer::new();
    assert!(r.rasterize(&spec(), &DrawList::new(), 0.0, &mut out).is_err(), "no density");
    assert!(r.rasterize(&spec(), &DrawList::new(), f32::NAN, &mut out).is_err(), "a density that is not a number");
    assert!(r.rasterize(&spec(), &DrawList::new(), 1.0e9, &mut out).is_err(), "a raster no GPU texture could hold");
    let flat = FaceLayerSpec::for_rect(FaceRect { x0: 0.0, x1: 0.1, y0: 0.0, y1: 0.0 }, FaceLayerId::Base, 500.0);
    assert!(r.rasterize(&flat, &DrawList::new(), 500.0, &mut out).is_err(), "a rect with no height");
}

// ------------------------------------------------------------------------------------------------ canvas semantics

#[test]
fn fills_use_the_non_zero_rule() {
    let mut l = DrawList::new();
    // A pentagram drawn in one go: its middle is wound twice, so non-zero fills it (even-odd would leave a hole).
    let star: Vec<Vec2> = [0_u8, 2, 4, 1, 3]
        .iter()
        .map(|&k| {
            let a = std::f32::consts::FRAC_PI_2 + f32::from(k) * 2.0 * std::f32::consts::PI / 5.0;
            Vec2::new(0.04 * a.cos(), 0.04 * a.sin())
        })
        .collect();
    let p = l.path().polyline(&star).close().finish();
    l.fill(p, solid(255, 0, 0));
    assert_eq!(px(&raster(&l), 50, 25), [255, 0, 0, 255], "the pentagram's middle");

    // A ring wound the opposite way inside cancels to winding 0: a hole.
    let mut ring = DrawList::new();
    let p = ring
        .path()
        .polyline(&[Vec2::new(-0.04, -0.04), Vec2::new(0.04, -0.04), Vec2::new(0.04, 0.04), Vec2::new(-0.04, 0.04)])
        .close()
        .polyline(&[Vec2::new(-0.02, -0.02), Vec2::new(-0.02, 0.02), Vec2::new(0.02, 0.02), Vec2::new(0.02, -0.02)])
        .close()
        .finish();
    ring.fill(p, solid(0, 255, 0));
    let r = raster(&ring);
    assert_eq!(px(&r, 50, 25), [0, 0, 0, 0], "the hole");
    assert_eq!(px(&r, 50, 8), [0, 255, 0, 255], "the band");
}

#[test]
fn gradients_pad_and_interpolate_the_straight_colour_as_the_canvas_does() {
    let mut l = DrawList::new();
    let p = square(&mut l, 0.1, 0.05);
    // Opaque red at 40 % of the way along x (pixel 40), fully transparent blue at 60 % (pixel 60).
    let stops = Stops::new(&[(0.4, Colour::rgb(255, 0, 0)), (0.6, Colour::rgba(0, 0, 255, 0))]);
    l.fill(p, Paint::Linear { from: Vec2::new(-0.1, 0.0), to: Vec2::new(0.1, 0.0), stops });
    let r = raster(&l);
    assert_eq!(px(&r, 5, 25), [255, 0, 0, 255], "before the first stop: its colour");
    assert_eq!(px(&r, 95, 25), [0, 0, 0, 0], "after the last stop: its colour, here transparent");
    // Pixel 50's centre is 0.525 of the way from the first stop to the last. The straight colour there is (255 (1-t), 0, 255 t) at
    // alpha 1-t, which premultiplies to red 255 (1-t)^2 = 57 and blue 255 t (1-t) = 64. Interpolating premultiplied values
    // instead would give red 255 (1-t) = 121 and no blue at all: the blue would vanish with its alpha.
    let t = 0.525_f32;
    let expect = |v: f32| 255.0 * v;
    let [red, green, blue, alpha] = px(&r, 50, 25);
    assert!((f32::from(alpha) - expect(1.0 - t)).abs() <= 2.0, "alpha {alpha}");
    assert!((f32::from(red) - expect((1.0 - t) * (1.0 - t))).abs() <= 2.0, "red {red}");
    assert!((f32::from(blue) - expect(t * (1.0 - t))).abs() <= 2.0 && green == 0, "blue {blue}, green {green}");
}

#[test]
fn a_radial_gradient_centres_on_its_point_and_pads_beyond_its_radius() {
    let mut l = DrawList::new();
    let p = square(&mut l, 0.1, 0.05);
    let stops = Stops::new(&[(0.0, Colour::rgb(255, 255, 255)), (1.0, Colour::rgb(0, 0, 0))]);
    l.fill(p, Paint::Radial { centre: Vec2::new(0.02, 0.0), radius: 0.04, stops });
    let r = raster(&l);
    assert!(px(&r, 60, 25)[0] > 245, "the centre is the first stop");
    assert!((i32::from(px(&r, 70, 25)[0]) - 128).abs() < 8, "half the radius (0.02 m = 10 px) out");
    assert!(px(&r, 90, 25)[0] < 5 && px(&r, 5, 5)[0] < 5, "beyond the radius: the last stop");
}

#[test]
fn a_sharp_miter_is_mitred_up_to_the_canvas_limit_of_ten() {
    // The tip of a miter reaches `width / 2 / sin(half the corner angle)` past the vertex, at pixel x = 80 here.
    let corner = |half_angle_deg: f32| {
        let arm = 0.15 * half_angle_deg.to_radians().tan();
        let mut l = DrawList::new();
        let p = l.path().polyline(&[Vec2::new(-0.09, -arm), Vec2::new(0.06, 0.0), Vec2::new(-0.09, arm)]).finish();
        l.push(Cmd::Stroke {
            path: p,
            paint: solid(0, 0, 0),
            width: 0.008,
            cap: Cap::Butt,
            join: Join::Miter,
            opacity: 1.0,
        });
        raster(&l)
    };
    let rightmost = |r: &Raster| {
        (0..r.width).rev().find(|&x| (0..r.height).any(|y| px(r, x, y)[3] > 40)).expect("the corner draws something")
    };
    // A 25 degree corner has a miter ratio of 4.6: Skia's default limit (4) would bevel it (tip at pixel ~82), the canvas's
    // limit of 10 draws the point, 0.004 / sin(12.5 deg) = 9.2 px past the vertex.
    assert!((88..=90).contains(&rightmost(&corner(12.5))), "a mitred tip, got {}", rightmost(&corner(12.5)));
    // A 10 degree corner has a ratio of 11.5, past the limit: a bevel, whose outer edge stays within a pixel or so of the vertex.
    assert!(rightmost(&corner(5.0)) <= 82, "a bevel, got {}", rightmost(&corner(5.0)));
}

#[test]
fn round_caps_extend_a_stroke_and_butt_caps_do_not() {
    let line = |cap| {
        let mut l = DrawList::new();
        let p = l.path().polyline(&[Vec2::new(-0.02, 0.0), Vec2::new(0.02, 0.0)]).finish();
        l.push(Cmd::Stroke { path: p, paint: solid(0, 0, 0), width: 0.01, cap, join: Join::Round, opacity: 1.0 });
        raster(&l)
    };
    let (butt, round) = (line(Cap::Butt), line(Cap::Round));
    assert_eq!(px(&butt, 38, 25)[3], 0, "a butt cap stops at the end point (pixel 40)");
    assert!(px(&round, 38, 25)[3] > 200, "a round cap reaches half the width (2.5 px) beyond it");
    assert!(
        (area(&round) - area(&butt) - std::f32::consts::PI * 2.5 * 2.5).abs() < 1.0,
        "two half discs of radius 2.5 px"
    );
}

#[test]
fn strokes_scale_with_the_transform() {
    let mut l = DrawList::new();
    let p = l.path().polyline(&[Vec2::new(-0.03, 0.0), Vec2::new(0.03, 0.0)]).finish();
    l.push(Cmd::PushTransform(Affine2::from_scale(Vec2::new(1.0, 2.0))));
    l.push(Cmd::Stroke {
        path: p,
        paint: solid(0, 0, 0),
        width: 0.004,
        cap: Cap::Butt,
        join: Join::Miter,
        opacity: 1.0,
    });
    l.push(Cmd::PopTransform);
    // 0.004 m is 2 px; the y stretch makes it 4 px tall and leaves the length alone.
    assert!((area(&raster(&l)) - 30.0 * 4.0).abs() < 2.0);
}

// ------------------------------------------------------------------------------------------------ the mock painter, through this rasteriser

mod with_the_mock_painter {
    use super::*;

    const PX_PER_M: f32 = 1000.0;

    fn render(buf: &ChannelBuf, layer: FaceLayerId) -> Raster {
        let profile = mock_profile();
        let params = FaceMapping::default().apply(buf);
        let mut painter = MockPainter::default();
        let input = FaceInput { profile: &profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
        let mut drawing = FaceDrawing::new();
        painter.paint(&input, &mut drawing).unwrap();
        let spec = *painter.layers().iter().find(|l| l.id == layer).unwrap();
        let mut out = Raster::new(1, 1);
        SkiaRasterizer::new().rasterize(&spec, &drawing[layer], PX_PER_M, &mut out).unwrap();
        out
    }

    fn with(pairs: &[(Channel, f32)]) -> ChannelBuf {
        let mut b = ChannelBuf::NEUTRAL;
        for (c, v) in pairs {
            b.set(*c, *v);
        }
        b
    }

    #[test]
    fn an_open_mouth_draws_more_pixels_than_a_closed_one() {
        let closed = render(&ChannelBuf::NEUTRAL, FaceLayerId::Base);
        let open = render(&with(&[(Channel::JAW_OPEN, 0.7)]), FaceLayerId::Base);
        assert!(covered(&open) > covered(&closed) + 50, "{} vs {}", covered(&open), covered(&closed));
    }

    #[test]
    fn a_blink_removes_the_eye_whites() {
        let open = render(&ChannelBuf::NEUTRAL, FaceLayerId::Base);
        let shut = render(&with(&[(Channel::EYE_BLINK_LEFT, 1.0), (Channel::EYE_BLINK_RIGHT, 1.0)]), FaceLayerId::Base);
        assert!(covered(&shut) + 1000 < covered(&open), "{} vs {}", covered(&shut), covered(&open));
    }

    #[test]
    fn brows_alone_make_a_sparse_layer_that_follows_the_brow_channels() {
        let neutral = render(&ChannelBuf::NEUTRAL, FaceLayerId::Brows);
        let raised = render(
            &with(&[(Channel::BROW_OUTER_UP_LEFT, 1.0), (Channel::BROW_OUTER_UP_RIGHT, 1.0)]),
            FaceLayerId::Brows,
        );
        let total = neutral.pixels.len() / 4;
        assert!(covered(&neutral) > 100 && covered(&neutral) * 20 < total, "thin strokes only");
        assert_ne!(neutral, raised, "raising moves the strokes");
    }

    #[test]
    fn the_same_channels_rasterise_to_the_same_bytes() {
        let buf = with(&[(Channel::MOUTH_SMILE_LEFT, 0.7), (Channel::EYE_LOOK_OUT_LEFT, 0.5)]);
        assert_eq!(render(&buf, FaceLayerId::Base), render(&buf, FaceLayerId::Base));
    }
}

// ------------------------------------------------------------------------------------------------ reuse, determinism, budget

/// A list that uses every command: fills (solid and both gradients), strokes (both caps and joins), nested clips and transforms.
fn busy_list() -> DrawList {
    let mut l = DrawList::new();
    let disc = l.path().ellipse(Vec2::ZERO, 0.04, 0.03).finish();
    l.push(Cmd::PushClip(disc));
    let inner = l.path().ellipse(Vec2::new(0.01, 0.0), 0.03, 0.02).finish();
    l.push(Cmd::PushClip(inner));
    let sheet = square(&mut l, 0.1, 0.05);
    let stops = Stops::new(&[(0.0, Colour::rgb(255, 240, 200)), (1.0, Colour::rgba(200, 60, 40, 0))]);
    l.fill(sheet, Paint::Radial { centre: Vec2::new(0.0, 0.01), radius: 0.05, stops });
    l.push(Cmd::PopClip);
    l.push(Cmd::PopClip);
    l.push(Cmd::PushTransform(Affine2::from_angle(0.4) * Affine2::from_translation(Vec2::new(0.01, 0.0))));
    let bar = square(&mut l, 0.03, 0.004);
    l.fill(
        bar,
        Paint::Linear {
            from: Vec2::new(-0.03, 0.0),
            to: Vec2::new(0.03, 0.0),
            stops: Stops::new(&[(0.0, Colour::rgb(0, 0, 0)), (1.0, Colour::rgb(255, 255, 255))]),
        },
    );
    let zig = l.path().polyline(&[Vec2::new(-0.08, -0.03), Vec2::new(0.0, 0.03), Vec2::new(0.08, -0.03)]).finish();
    l.push(Cmd::Stroke {
        path: zig,
        paint: solid(20, 30, 200),
        width: 0.006,
        cap: Cap::Round,
        join: Join::Miter,
        opacity: 0.8,
    });
    l.push(Cmd::PopTransform);
    l
}

#[test]
fn the_same_list_gives_the_same_bytes_fresh_or_reused() {
    let list = busy_list();
    let mut reused = SkiaRasterizer::new();
    let (mut first, mut second, mut fresh) = (Raster::new(1, 1), Raster::new(1, 1), Raster::new(1, 1));
    reused.rasterize(&spec(), &list, 500.0, &mut first).unwrap();
    // Something else in between, so any state left behind by one call would show in the next.
    let mut other = DrawList::new();
    let p = square(&mut other, 0.05, 0.02);
    other.push(Cmd::PushClip(p));
    other.push(Cmd::PushTransform(Affine2::from_scale(Vec2::splat(2.0))));
    reused.rasterize(&spec(), &other, 500.0, &mut second).unwrap();
    reused.rasterize(&spec(), &list, 500.0, &mut second).unwrap();
    SkiaRasterizer::new().rasterize(&spec(), &list, 500.0, &mut fresh).unwrap();
    assert!(covered(&first) > 500, "the list draws something");
    assert_eq!(first, second, "a reused rasteriser");
    assert_eq!(first, fresh, "a fresh rasteriser");
}

#[test]
fn the_output_is_cleared_and_resized_only_when_the_size_changes() {
    let list = busy_list();
    let mut r = SkiaRasterizer::new();
    let mut out = Raster::new(1, 1);
    r.rasterize(&spec(), &list, 500.0, &mut out).unwrap();
    let (ptr, capacity) = (out.pixels.as_ptr(), out.pixels.capacity());
    // Dirty the pixels: a rerun must not draw over what was there.
    out.pixels.fill(77);
    r.rasterize(&spec(), &list, 500.0, &mut out).unwrap();
    assert_eq!((out.pixels.as_ptr(), out.pixels.capacity()), (ptr, capacity), "same size: same buffer");
    assert_eq!(out, raster(&list), "and nothing left over from the dirt");

    r.rasterize(&spec(), &list, 250.0, &mut out).unwrap();
    assert_eq!((out.width, out.height, out.pixels.len()), (50, 25, 50 * 25 * 4), "half the density, half the size");
    r.rasterize(&spec(), &list, 500.0, &mut out).unwrap();
    assert_eq!(out, raster(&list), "back to full size, with the clip masks re-sized to match");
}

/// The mock painter's neutral face, base layer, over the prototype's 0.43 m window at the default density: 1548 x 1107.
fn neutral_face() -> (FaceLayerSpec, DrawList) {
    let profile = mock_profile();
    let params = FaceMapping::default().apply(&ChannelBuf::NEUTRAL);
    let mut painter = MockPainter::default();
    let input = FaceInput { profile: &profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
    let mut drawing = FaceDrawing::new();
    painter.paint(&input, &mut drawing).unwrap();
    let rect = FaceRect { x0: -0.215, x1: 0.215, y0: -0.2825, y1: 0.025 };
    (FaceLayerSpec::for_rect(rect, FaceLayerId::Base, DEFAULT_PX_PER_M), drawing[FaceLayerId::Base].clone())
}

#[test]
fn the_neutral_face_rasterises_in_the_budget() {
    let (spec, list) = neutral_face();
    assert_eq!(spec.size_px(), (1548, 1107));
    let mut r = SkiaRasterizer::new();
    let mut out = Raster::new(1, 1);
    for _ in 0..3 {
        r.rasterize(&spec, &list, spec.px_per_m, &mut out).unwrap();
    }
    // The best of ten, so a busy machine (other agents' builds) shows a slow run, not a slow rasteriser.
    let best = (0..10)
        .map(|_| {
            let start = Instant::now();
            r.rasterize(&spec, &list, spec.px_per_m, &mut out).unwrap();
            start.elapsed().as_secs_f64() * 1000.0
        })
        .fold(f64::MAX, f64::min);
    // The budget is for an optimised build (spec §10): 20 ms on one core. An unoptimised test build runs this crate's own loops
    // (clip masks) without the optimiser, so it only has to be within an order of magnitude.
    let limit_ms = if cfg!(debug_assertions) { 250.0 } else { 20.0 };
    println!("neutral face {}x{}: best of 10 {best:.2} ms (limit {limit_ms} ms)", out.width, out.height);
    assert!(best <= limit_ms, "the neutral face took {best:.2} ms, over {limit_ms} ms");
}

#[test]
fn after_warm_up_nothing_is_allocated() {
    let (spec, list) = neutral_face();
    let mut r = SkiaRasterizer::new();
    let mut out = Raster::new(1, 1);
    for _ in 0..3 {
        r.rasterize(&spec, &list, spec.px_per_m, &mut out).unwrap();
    }
    let (first, _) = count_allocations(|| r.rasterize(&spec, &list, spec.px_per_m, &mut out).unwrap());
    let (second, _) = count_allocations(|| r.rasterize(&spec, &list, spec.px_per_m, &mut out).unwrap());
    assert_eq!((first, second), (0, 0), "a warm rasteriser allocated while drawing the neutral face");
}

#[test]
fn a_busy_list_allocates_nothing_either_and_the_counter_sees_a_real_allocation() {
    // Every command kind: gradients, strokes with both caps and joins, nested clips and transforms, a hairline.
    let mut list = busy_list();
    let hair = list.path().polyline(&[Vec2::new(-0.09, 0.04), Vec2::new(0.09, 0.03)]).finish();
    list.stroke(hair, solid(10, 10, 10), 0.0004);
    let mut r = SkiaRasterizer::new();
    let mut out = Raster::new(1, 1);
    for _ in 0..3 {
        r.rasterize(&spec(), &list, 500.0, &mut out).unwrap();
    }
    let (allocations, _) = count_allocations(|| r.rasterize(&spec(), &list, 500.0, &mut out).unwrap());
    assert_eq!(allocations, 0);
    let (seen, _) = count_allocations(|| {
        std::hint::black_box(Vec::<u8>::with_capacity(64));
    });
    assert_eq!(seen, 1, "without this the zero-allocation tests prove nothing");
}

#[test]
fn a_bigger_raster_allocates_once_and_then_stays_quiet() {
    let (spec, list) = neutral_face();
    let mut r = SkiaRasterizer::new();
    let mut out = Raster::new(1, 1);
    r.rasterize(&spec, &list, spec.px_per_m, &mut out).unwrap();
    let (grown, _) = count_allocations(|| {
        let big = FaceLayerSpec::for_rect(spec.rect, FaceLayerId::Base, spec.px_per_m * 1.1);
        r.rasterize(&big, &list, big.px_per_m, &mut out).unwrap();
    });
    assert!(grown > 0, "a larger raster needs larger buffers");
    let big = FaceLayerSpec::for_rect(spec.rect, FaceLayerId::Base, spec.px_per_m * 1.1);
    let (again, _) = count_allocations(|| r.rasterize(&big, &list, big.px_per_m, &mut out).unwrap());
    assert_eq!(again, 0);
}

// ------------------------------------------------------------------------------------------------ edges, hairlines, robustness

/// The plane coordinate of pixel position (`px`, `py`) in the 100 x 50 test raster.
fn at(px: f32, py: f32) -> Vec2 {
    Vec2::new((px - 50.0) / 500.0, (25.0 - py) / 500.0)
}

fn pixel_rect(l: &mut DrawList, x0: f32, y0: f32, x1: f32, y1: f32) -> omuzaki_face_api::PathRef {
    l.path().polyline(&[at(x0, y0), at(x1, y0), at(x1, y1), at(x0, y1)]).close().finish()
}

#[test]
fn vertical_edges_are_exact_and_horizontal_ones_snap_to_a_quarter_pixel() {
    // This is Skia's analytic coverage: x is exact (to 1/256 px), y is snapped to the nearest quarter at every edge end.
    let mut l = DrawList::new();
    let p = pixel_rect(&mut l, 10.25, 5.2, 20.5, 15.9);
    l.fill(p, solid(0, 0, 0));
    let r = raster(&l);
    assert_eq!(px(&r, 10, 10)[3], 192, "x = 10.25: three quarters of the column");
    assert_eq!(px(&r, 20, 10)[3], 128, "x = 20.5: half of the column");
    assert_eq!(px(&r, 15, 5)[3], 192, "y = 5.2 snaps to 5.25: three quarters of the row (an exact area would be 0.8)");
    assert_eq!(px(&r, 15, 15)[3], 255, "y = 15.9 snaps to 16: the whole row");
    assert_eq!(px(&r, 15, 4)[3], 0);
    assert_eq!(px(&r, 15, 16)[3], 0);
}

#[test]
fn a_shape_larger_than_the_raster_fills_all_of_it() {
    let mut l = DrawList::new();
    let p = square(&mut l, 5.0, 5.0);
    l.fill(p, solid(7, 8, 9));
    let r = raster(&l);
    assert!(r.pixels.chunks(4).all(|p| p == [7, 8, 9, 255]));
}

#[test]
fn a_cubic_circle_covers_its_area() {
    let mut l = DrawList::new();
    let p = l.path().ellipse(Vec2::new(0.013, 0.007), 0.04, 0.04).finish();
    l.fill(p, solid(0, 0, 0));
    let expected = std::f32::consts::PI * 20.0 * 20.0;
    assert!((area(&raster(&l)) - expected).abs() < 0.005 * expected, "{} vs {expected}", area(&raster(&l)));
}

#[test]
fn quads_fill_too() {
    let mut l = DrawList::new();
    // The region under a parabola from (-0.05, -0.02) to (0.05, -0.02) with its control at (0, 0.06): area 2/3 of the
    // triangle of control points above the chord, plus the rectangle below it down to the bottom edge.
    let p = l
        .path()
        .move_to(Vec2::new(-0.05, -0.02))
        .quad_to(Vec2::new(0.0, 0.06), Vec2::new(0.05, -0.02))
        .line_to(Vec2::new(0.05, -0.05))
        .line_to(Vec2::new(-0.05, -0.05))
        .close()
        .finish();
    l.fill(p, solid(0, 0, 0));
    // In pixels: the chord is 50 px wide and 15 px above the bottom edge; the curve rises 20 px at its middle (a quad's
    // apex is half-way to the control point): the area is 50 * 15 + 2/3 * 50 * 20 = 750 + 666.7.
    let expected = 50.0 * 15.0 + 2.0 / 3.0 * 50.0 * 20.0;
    assert!((area(&raster(&l)) - expected).abs() < 0.01 * expected, "{} vs {expected}", area(&raster(&l)));
}

#[test]
fn a_zero_length_stroke_is_a_dot_with_round_caps_and_nothing_with_butt_caps() {
    let dot = |cap| {
        let mut l = DrawList::new();
        let p = l.path().polyline(&[Vec2::ZERO, Vec2::ZERO]).finish();
        l.push(Cmd::Stroke { path: p, paint: solid(0, 0, 0), width: 0.02, cap, join: Join::Round, opacity: 1.0 });
        raster(&l)
    };
    let round = area(&dot(Cap::Round));
    let expected = std::f32::consts::PI * 5.0 * 5.0;
    assert!((round - expected).abs() < 0.05 * expected, "{round} vs {expected}");
    assert_eq!(area(&dot(Cap::Butt)), 0.0);
}

#[test]
fn a_stroke_under_a_pixel_is_a_hairline_whose_weight_is_its_width() {
    let line = |width| {
        let mut l = DrawList::new();
        let p = l.path().polyline(&[at(10.0, 12.3), at(90.0, 12.3)]).finish();
        l.push(Cmd::Stroke { path: p, paint: solid(0, 0, 0), width, cap: Cap::Butt, join: Join::Miter, opacity: 1.0 });
        raster(&l)
    };
    // 0.0004 m is 0.2 px: 80 px of line at 0.2 px is 16 px of ink, however the line falls between rows.
    for width in [0.0004, 0.0009] {
        let expected = width * 500.0 * 80.0;
        let got = area(&line(width));
        assert!((got - expected).abs() < 0.04 * expected, "width {width}: {got} vs {expected}");
    }
    // Skia scales the paint's 8-bit alpha by `width * 256` in whole steps, so a very thin line is a little lighter than
    // its width says: 0.05 px is 12/256, which takes 255 to 11, so 80 px of line is 80 * 11 / 255 px of ink.
    let thin = area(&line(0.0001));
    assert!((thin - 80.0 * 11.0 / 255.0).abs() < 0.05, "{thin}");
    // A hairline spreads over two rows, never more.
    let r = line(0.0004);
    let rows = (0..r.height).filter(|&y| (0..r.width).any(|x| px(&r, x, y)[3] > 0)).count();
    assert_eq!(rows, 2);
}

#[test]
fn hairlines_blend_with_what_is_under_them_and_obey_clips() {
    let mut l = DrawList::new();
    let sheet = square(&mut l, 0.1, 0.05);
    l.fill(sheet, solid(255, 255, 255));
    let clip = l.path().ellipse(Vec2::ZERO, 0.03, 0.03).finish();
    l.push(Cmd::PushClip(clip));
    let p = l.path().polyline(&[at(0.0, 25.0), at(100.0, 25.0)]).finish();
    l.push(Cmd::Stroke {
        path: p,
        paint: solid(0, 0, 0),
        width: 0.0006,
        cap: Cap::Butt,
        join: Join::Miter,
        opacity: 1.0,
    });
    l.push(Cmd::PopClip);
    let r = raster(&l);
    let darkest = |x: u32| (24..=25).map(|y| px(&r, x, y)[0]).min().unwrap();
    assert!(darkest(50) < 240, "the line darkens the white sheet inside the clip");
    assert_eq!(darkest(5), 255, "and is clipped away outside it");
}

#[test]
fn finite_but_degenerate_inputs_draw_nothing_or_something_but_never_panic() {
    let mut l = DrawList::new();
    for pts in [
        [Vec2::new(1e30, -1e30), Vec2::new(-1e30, 1e30), Vec2::new(0.0, 0.0)],
        [Vec2::new(0.0, 1e20), Vec2::new(0.1, -1e20), Vec2::new(0.0, 0.0)],
        [Vec2::ZERO, Vec2::ZERO, Vec2::ZERO],
    ] {
        let p = l.path().polyline(&pts).close().finish();
        l.fill(p, solid(1, 2, 3));
        l.stroke(p, solid(1, 2, 3), 0.01);
        let q = l.path().move_to(pts[0]).quad_to(pts[1], pts[2]).cubic_to(pts[2], pts[1], pts[0]).finish();
        l.fill(q, solid(1, 2, 3));
        l.stroke(q, solid(1, 2, 3), 0.01);
        l.stroke(q, solid(1, 2, 3), 1e20);
        l.stroke(q, solid(1, 2, 3), 0.0);
        l.stroke(q, solid(1, 2, 3), -1.0);
    }
    // A gradient with no span, no radius, no stops, or under a matrix that cannot be inverted paints nothing, as on the canvas.
    let bad = Stops::new(&[(0.0, Colour::rgb(0, 0, 0)), (0.0, Colour::rgb(255, 255, 255))]);
    let p = square(&mut l, 0.05, 0.02);
    l.fill(p, Paint::Linear { from: Vec2::ZERO, to: Vec2::ZERO, stops: bad });
    l.fill(p, Paint::Radial { centre: Vec2::ZERO, radius: 0.0, stops: bad });
    l.fill(p, Paint::Linear { from: Vec2::ZERO, to: Vec2::X, stops: Stops::new(&[]) });
    l.push(Cmd::PushTransform(Affine2::from_scale(Vec2::ZERO)));
    l.fill(p, Paint::Linear { from: Vec2::ZERO, to: Vec2::X, stops: bad });
    l.push(Cmd::PopTransform);
    let first = raster(&l);
    assert_eq!(first, raster(&l), "and the answer does not change");
}

/// The error text of the first command in a list that is not finite.
fn rejected(build: impl FnOnce(&mut DrawList)) -> String {
    let mut l = DrawList::new();
    let p = square(&mut l, 0.05, 0.02);
    l.fill(p, solid(9, 9, 9)); // command 0 is fine
    build(&mut l);
    let mut out = Raster::new(1, 1);
    match SkiaRasterizer::new().rasterize(&spec(), &l, 500.0, &mut out) {
        Err(FaceError::Raster(message)) => message,
        other => panic!("expected an error, got {other:?}"),
    }
}

#[test]
fn numbers_that_are_not_finite_are_errors_that_name_the_command_and_the_field() {
    let nan = f32::NAN;
    let line = |l: &mut DrawList| l.path().polyline(&[Vec2::ZERO, Vec2::X * 0.01]).finish();
    assert_eq!(
        rejected(|l| {
            let p = line(l);
            l.push(Cmd::Fill { path: p, paint: solid(1, 2, 3), opacity: nan });
        }),
        "command 1 (Fill): opacity is not a finite number"
    );
    assert_eq!(
        rejected(|l| {
            let p = l.path().polyline(&[Vec2::ZERO, Vec2::new(f32::INFINITY, 0.0)]).finish();
            l.stroke(p, solid(1, 2, 3), 0.01);
        }),
        "command 1 (Stroke): a path coordinate is not a finite number"
    );
    assert_eq!(
        rejected(|l| {
            let p = line(l);
            l.stroke(p, solid(1, 2, 3), nan);
        }),
        "command 1 (Stroke): width is not a finite number"
    );
    assert_eq!(
        rejected(|l| {
            let p = line(l);
            l.push(Cmd::Stroke {
                path: p,
                paint: solid(1, 2, 3),
                width: 0.01,
                cap: Cap::Butt,
                join: Join::Miter,
                opacity: nan,
            });
        }),
        "command 1 (Stroke): opacity is not a finite number"
    );
    assert_eq!(
        rejected(|l| {
            let p = line(l);
            let paint =
                Paint::Linear { from: Vec2::new(nan, 0.0), to: Vec2::X, stops: Stops::new(&[(0.0, Colour::BLACK)]) };
            l.fill(p, paint);
        }),
        "command 1 (Fill): a gradient point is not a finite number"
    );
    assert_eq!(
        rejected(|l| {
            let p = line(l);
            let paint = Paint::Radial { centre: Vec2::ZERO, radius: nan, stops: Stops::new(&[(0.0, Colour::BLACK)]) };
            l.fill(p, paint);
        }),
        "command 1 (Fill): the gradient centre or radius is not a finite number"
    );
    assert_eq!(
        rejected(|l| {
            let p = line(l);
            let paint = Paint::Radial { centre: Vec2::ZERO, radius: 1.0, stops: Stops::new(&[(nan, Colour::BLACK)]) };
            l.fill(p, paint);
        }),
        "command 1 (Fill): a gradient stop offset is not a finite number"
    );
    assert_eq!(
        rejected(|l| {
            let p = line(l);
            l.push(Cmd::PushClip(p));
            l.push(Cmd::PushTransform(Affine2::from_scale(Vec2::new(nan, 1.0))));
        }),
        "command 2 (PushTransform): the matrix is not a finite number"
    );
    assert_eq!(
        rejected(|l| {
            l.push(Cmd::PushTransform(Affine2::from_scale(Vec2::splat(1e30))));
            l.push(Cmd::PushTransform(Affine2::from_scale(Vec2::splat(1e30))));
        }),
        "PushTransform: the composed transform overflows"
    );
}

mod fuzz {
    use proptest::prelude::*;

    use super::*;

    /// A coordinate: mostly inside the raster, sometimes far outside, once in a while not a number.
    fn coord() -> impl Strategy<Value = f32> {
        prop_oneof![
            8 => -0.15_f32..0.15,
            1 => -50.0_f32..50.0,
            1 => prop_oneof![Just(0.0_f32), Just(f32::NAN), Just(f32::INFINITY), Just(-1e30_f32)],
        ]
    }

    fn point() -> impl Strategy<Value = Vec2> {
        (coord(), coord()).prop_map(|(x, y)| Vec2::new(x, y))
    }

    fn element() -> BoxedStrategy<omuzaki_face_api::PathEl> {
        use omuzaki_face_api::PathEl;
        prop_oneof![
            point().prop_map(PathEl::Move),
            point().prop_map(PathEl::Line),
            (point(), point()).prop_map(|(c, p)| PathEl::Quad(c, p)),
            (point(), point(), point()).prop_map(|(a, b, p)| PathEl::Cubic(a, b, p)),
            Just(PathEl::Close),
        ]
        .boxed()
    }

    fn path() -> BoxedStrategy<Vec<omuzaki_face_api::PathEl>> {
        prop::collection::vec(element(), 0..7).boxed()
    }

    fn colour() -> BoxedStrategy<Colour> {
        (any::<u8>(), any::<u8>(), any::<u8>(), any::<u8>()).prop_map(|(r, g, b, a)| Colour::rgba(r, g, b, a)).boxed()
    }

    fn paint() -> BoxedStrategy<Paint> {
        let stops = prop::collection::vec((-0.5_f32..1.5, colour()), 0..5).prop_map(|v| Stops::new(&v));
        prop_oneof![
            colour().prop_map(Paint::Solid),
            (point(), point(), stops.clone()).prop_map(|(from, to, stops)| Paint::Linear { from, to, stops }),
            (point(), coord(), stops).prop_map(|(centre, radius, stops)| Paint::Radial { centre, radius, stops }),
        ]
        .boxed()
    }

    #[derive(Clone, Debug)]
    enum Op {
        Fill(Vec<omuzaki_face_api::PathEl>, Paint, f32),
        Stroke(Vec<omuzaki_face_api::PathEl>, Paint, f32, bool, bool, f32),
        Clip(Vec<omuzaki_face_api::PathEl>, Vec<Op>),
        Transform([f32; 6], Vec<Op>),
    }

    fn ops(depth: u32) -> BoxedStrategy<Vec<Op>> {
        let leaf = prop_oneof![
            (path(), paint(), 0.0_f32..1.2).prop_map(|(p, c, o)| Op::Fill(p, c, o)),
            (
                path(),
                paint(),
                prop_oneof![-0.01_f32..0.06, Just(0.0004_f32), coord()],
                any::<bool>(),
                any::<bool>(),
                0.0_f32..1.0
            )
                .prop_map(|(p, c, w, cap, join, o)| Op::Stroke(p, c, w, cap, join, o)),
        ];
        if depth == 0 {
            return prop::collection::vec(leaf, 0..4).boxed();
        }
        let inner = ops(depth - 1);
        let nested = prop_oneof![
            (path(), inner.clone()).prop_map(|(p, body)| Op::Clip(p, body)),
            (prop::array::uniform6(-3.0_f32..3.0), inner).prop_map(|(m, body)| Op::Transform(m, body)),
        ];
        prop::collection::vec(prop_oneof![3 => leaf, 1 => nested], 0..4).boxed()
    }

    fn push(list: &mut DrawList, op: &Op) {
        let path = |list: &mut DrawList, els: &[omuzaki_face_api::PathEl]| {
            let mut b = list.path();
            for el in els {
                b = match *el {
                    omuzaki_face_api::PathEl::Move(p) => b.move_to(p),
                    omuzaki_face_api::PathEl::Line(p) => b.line_to(p),
                    omuzaki_face_api::PathEl::Quad(c, p) => b.quad_to(c, p),
                    omuzaki_face_api::PathEl::Cubic(a, c, p) => b.cubic_to(a, c, p),
                    omuzaki_face_api::PathEl::Close => b.close(),
                };
            }
            b.finish()
        };
        match op {
            Op::Fill(els, paint, opacity) => {
                let p = path(list, els);
                list.push(Cmd::Fill { path: p, paint: *paint, opacity: *opacity });
            }
            Op::Stroke(els, paint, width, round_cap, round_join, opacity) => {
                let p = path(list, els);
                let (cap, join) = (
                    if *round_cap { Cap::Round } else { Cap::Butt },
                    if *round_join { Join::Round } else { Join::Miter },
                );
                list.push(Cmd::Stroke { path: p, paint: *paint, width: *width, cap, join, opacity: *opacity });
            }
            Op::Clip(els, body) => {
                let p = path(list, els);
                list.push(Cmd::PushClip(p));
                for inner in body {
                    push(list, inner);
                }
                list.push(Cmd::PopClip);
            }
            Op::Transform(m, body) => {
                list.push(Cmd::PushTransform(Affine2::from_cols_array(m)));
                for inner in body {
                    push(list, inner);
                }
                list.push(Cmd::PopTransform);
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 400, failure_persistence: None, ..ProptestConfig::default() })]

        /// Whatever a painter (or a broken one) hands over, the rasteriser returns pixels or an error, the same pixels every time.
        #[test]
        fn any_draw_list_rasterises_without_panicking_and_deterministically(body in ops(2)) {
            let mut list = DrawList::new();
            for op in &body {
                push(&mut list, op);
            }
            let mut a = Raster::new(1, 1);
            let mut b = Raster::new(1, 1);
            let mut reused = SkiaRasterizer::new();
            let first = reused.rasterize(&spec(), &list, 500.0, &mut a);
            // Rasterising something else in between must not leave anything behind.
            let _ = reused.rasterize(&spec(), &busy_list(), 500.0, &mut b);
            let second = reused.rasterize(&spec(), &list, 500.0, &mut b);
            prop_assert_eq!(first, second);
            prop_assert_eq!(a, b);
        }
    }
}
