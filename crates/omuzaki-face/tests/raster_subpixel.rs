//! Thin-stroke ink against sub-pixel position (#1665, step 1 of #888).
//!
//! The CPU rasteriser snaps y to a quarter pixel and truncates x to 1/256 px, both on purpose (`src/raster/scan.rs`).
//! Those steps mean the ink a thin stroke deposits can change as it moves by a fraction of a pixel, which shows as
//! shimmer under motion. This file measures that swing and pins it; it changes nothing in the rasteriser.
//!
//! Each stroke is an opaque black line in a 100 x 50 px layer, shifted by `d` px in y for `d` = 0, 1/16, …, 1. The ink is
//! the covered area in pixels: the sum of alpha over 255.

// Tests may unwrap: a panic is a clear failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::f32::consts::FRAC_1_SQRT_2;

use omuzaki_core::glam::Vec2;
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face_api::{Colour, DrawList, FaceLayerId, FaceLayerSpec, FaceRasterizer, FaceRect, Paint, Raster};

const PX_PER_M: f32 = 500.0;
/// One pixel in metres at [`PX_PER_M`].
const PX: f32 = 1.0 / PX_PER_M;
/// The shifts `d` in pixels: 0, 1/16, …, 1 (17 positions, one sixteenth apart).
const POSITIONS: usize = 17;
/// The stroke widths in pixels.
const WIDTHS: [f32; 4] = [0.5, 1.0, 1.5, 2.0];

#[derive(Clone, Copy, Debug)]
enum Direction {
    Horizontal,
    Diagonal,
}

const DIRECTIONS: [Direction; 2] = [Direction::Horizontal, Direction::Diagonal];

/// Pinned swing limits in percent, indexed `[direction][width]` as in [`DIRECTIONS`] and [`WIDTHS`]: today's
/// `max / min - 1` measured by `report` plus 0.5 points. A swing above its limit is the regression #888 tracks.
const SWING_LIMIT_PCT: [[f64; 4]; 2] = [[0.51, 0.51, 0.76, 0.69], [0.55, 0.51, 12.89, 10.03]];

/// The layer: 0.2 m x 0.1 m at 500 px/m, so 100 x 50 px with its centre at (0, 0).
fn spec() -> FaceLayerSpec {
    FaceLayerSpec::for_rect(FaceRect { x0: -0.1, x1: 0.1, y0: -0.05, y1: 0.05 }, FaceLayerId::Base, PX_PER_M)
}

/// The stroke's endpoints in metres, shifted by `dy` metres in y. The horizontal stroke is 60 px long, the diagonal one
/// 40 px long along (1, 1).
fn endpoints(dir: Direction, dy: f32) -> (Vec2, Vec2) {
    match dir {
        Direction::Horizontal => {
            let half = 30.0 * PX;
            (Vec2::new(-half, dy), Vec2::new(half, dy))
        }
        Direction::Diagonal => {
            let half = 20.0 * PX * FRAC_1_SQRT_2;
            (Vec2::new(-half, -half + dy), Vec2::new(half, half + dy))
        }
    }
}

fn stroke_list(dir: Direction, width_px: f32, d_px: f32) -> DrawList {
    let (a, b) = endpoints(dir, d_px * PX);
    let mut list = DrawList::new();
    let path = list.path().move_to(a).line_to(b).finish();
    list.stroke(path, Paint::Solid(Colour::rgb(0, 0, 0)), width_px * PX);
    list
}

/// The covered area in pixels: the sum of alpha over 255.
fn ink(dir: Direction, width_px: f32, d_px: f32) -> f64 {
    let mut out = Raster::new(1, 1);
    SkiaRasterizer::new().rasterize(&spec(), &stroke_list(dir, width_px, d_px), PX_PER_M, &mut out).unwrap();
    out.pixels.chunks(4).map(|p| f64::from(p[3])).sum::<f64>() / 255.0
}

/// The ink at each of the 17 positions, in order. Sixteenths are exact in binary, so stepping `d` adds no error.
fn sweep(dir: Direction, width_px: f32) -> [f64; POSITIONS] {
    let mut d = 0.0;
    std::array::from_fn(|_| {
        let v = ink(dir, width_px, d);
        d += 1.0 / 16.0;
        v
    })
}

/// `max / min - 1` in percent.
fn swing_percent(ink: &[f64]) -> f64 {
    let max = ink.iter().copied().fold(f64::MIN, f64::max);
    let min = ink.iter().copied().fold(f64::MAX, f64::min);
    assert!(min > 0.0, "a thin stroke must leave some ink at every position, got {min}");
    (max / min - 1.0) * 100.0
}

#[test]
fn sweeps_are_deterministic() {
    for dir in DIRECTIONS {
        for width in WIDTHS {
            assert_eq!(sweep(dir, width), sweep(dir, width), "the raster is deterministic (ADR 0009)");
        }
    }
}

/// Prints the table for the PR body. Run with
/// `cargo test -p omuzaki-face --test raster_subpixel -- --ignored --nocapture report`.
#[test]
#[ignore = "prints the measurement table for #1665; run with --ignored --nocapture"]
fn report() {
    for dir in DIRECTIONS {
        let sweeps: Vec<[f64; POSITIONS]> = WIDTHS.iter().map(|&w| sweep(dir, w)).collect();
        println!("\n### {dir:?} stroke, ink in px\n");
        let header: Vec<String> = WIDTHS.iter().map(|w| format!("{w} px")).collect();
        println!("| d (px) | {} |", header.join(" | "));
        println!("|---|{}", "---|".repeat(WIDTHS.len()));
        for i in 0..POSITIONS {
            let cells: Vec<String> = sweeps.iter().map(|s| format!("{:.3}", s[i])).collect();
            println!("| {}/16 | {} |", i, cells.join(" | "));
        }
        let swings: Vec<String> = sweeps.iter().map(|s| format!("{:.2} %", swing_percent(s))).collect();
        println!("| swing (max/min - 1) | {} |", swings.join(" | "));
    }
}

#[test]
fn swing_does_not_grow() {
    for (di, &dir) in DIRECTIONS.iter().enumerate() {
        for (wi, &width) in WIDTHS.iter().enumerate() {
            let swing = swing_percent(&sweep(dir, width));
            let limit = SWING_LIMIT_PCT[di][wi];
            assert!(swing <= limit, "{dir:?} at {width} px swings {swing:.2} %, limit {limit:.2} % (#888)");
        }
    }
}
