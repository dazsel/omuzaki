//! Thin-stroke ink against sub-pixel position on the GPU path (#888 option (a), A4): the sweep of
//! `omuzaki-face/tests/raster_subpixel.rs` (horizontal and 45 degree strokes, widths 0.5, 1, 1.5 and 2 px, 17 positions a
//! sixteenth of a pixel apart) through `VelloRasterizer`. The ink is the covered area: the sum of alpha over 255. For widths
//! of 1 px and up the swing, `max / min - 1`, must stay within 1 %; narrower widths are reported. Without an adapter the
//! test prints why and passes.

// Test code: a panic is a clear failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::f32::consts::FRAC_1_SQRT_2;
use std::fmt::Write as _;

use common::{Gpu, device, gpu_pixels};
use omuzaki_core::glam::Vec2;
use omuzaki_face::raster::gpu::VelloRasterizer;
use omuzaki_face_api::{Colour, DrawList, FaceLayerId, FaceLayerSpec, FaceRect, Paint};

const PX_PER_M: f32 = 500.0;
/// One pixel in metres at [`PX_PER_M`].
const PX: f32 = 1.0 / PX_PER_M;
const POSITIONS: usize = 17;
const WIDTHS: [f32; 4] = [0.5, 1.0, 1.5, 2.0];
/// The most a stroke of 1 px or more may swing, in percent (#888).
const MAX_SWING_PCT: f64 = 1.0;

#[derive(Clone, Copy, Debug)]
enum Direction {
    Horizontal,
    Diagonal,
}

/// The layer: 0.2 m x 0.1 m at 500 px/m, so 100 x 50 px with its centre at (0, 0).
fn spec() -> FaceLayerSpec {
    FaceLayerSpec::for_rect(FaceRect { x0: -0.1, x1: 0.1, y0: -0.05, y1: 0.05 }, FaceLayerId::Base, PX_PER_M)
}

/// The stroke, shifted by `d_px` pixels in y: a 60 px horizontal line, or a 40 px one along (1, 1).
fn stroke_list(dir: Direction, width_px: f32, d_px: f32) -> DrawList {
    let dy = d_px * PX;
    let (a, b) = match dir {
        Direction::Horizontal => (Vec2::new(-30.0 * PX, dy), Vec2::new(30.0 * PX, dy)),
        Direction::Diagonal => {
            let half = 20.0 * PX * FRAC_1_SQRT_2;
            (Vec2::new(-half, -half + dy), Vec2::new(half, half + dy))
        }
    };
    let mut list = DrawList::new();
    let path = list.path().move_to(a).line_to(b).finish();
    list.stroke(path, Paint::Solid(Colour::rgb(0, 0, 0)), width_px * PX);
    list
}

/// The covered area of the stroke in pixels, on the GPU.
fn ink(gpu: &Gpu, rasterizer: &mut VelloRasterizer, dir: Direction, width_px: f32, d_px: f32) -> f64 {
    let pixels = gpu_pixels(gpu, rasterizer, &spec(), &stroke_list(dir, width_px, d_px));
    pixels.as_chunks::<4>().0.iter().map(|p| f64::from(p[3])).sum::<f64>() / 255.0
}

#[test]
#[allow(clippy::cast_precision_loss)] // 17 positions
fn thin_strokes_do_not_swing_with_sub_pixel_position() {
    let Some(gpu) = device() else { return };
    let mut rasterizer = VelloRasterizer::new();
    let mut table = String::from("| direction | width px | min ink | max ink | swing |\n|---|---:|---:|---:|---:|\n");
    let mut failures = String::new();
    for dir in [Direction::Horizontal, Direction::Diagonal] {
        for width in WIDTHS {
            let inks: Vec<f64> =
                (0..POSITIONS).map(|i| ink(&gpu, &mut rasterizer, dir, width, i as f32 / 16.0)).collect();
            let (min, max) = inks.iter().fold((f64::MAX, f64::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
            assert!(min > 0.0, "{dir:?} {width} px: the stroke drew nothing");
            let swing = (max / min - 1.0) * 100.0;
            let _ = writeln!(table, "| {dir:?} | {width} | {min:.2} | {max:.2} | {swing:.2} % |");
            if width >= 1.0 && swing > MAX_SWING_PCT {
                let _ = writeln!(failures, "{dir:?} {width} px swings {swing:.2} %");
            }
        }
    }
    eprintln!("{table}");
    assert!(failures.is_empty(), "swing above {MAX_SWING_PCT} %:\n{failures}");
}
