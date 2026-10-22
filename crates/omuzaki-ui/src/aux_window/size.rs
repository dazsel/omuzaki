//! An exact client area in physical pixels.
//!
//! egui sizes a viewport in points and egui-winit asks the OS for `pixels_per_point × points` physical pixels, which
//! winit rounds to whole pixels. So the points for a physical size are `physical / pixels_per_point`, computed in the
//! same `f32` arithmetic, and [`points_for`] checks that the round trip lands on the exact size. The scale is the
//! monitor's (times egui's zoom), known only once the window is open, so the window re-plans when it reads a scale it
//! wasn't planned for ([`fit_commands`]).

use egui::{Vec2, ViewportCommand};
use serde::{Deserialize, Serialize};

/// A size in physical pixels.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PhysicalSize {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl PhysicalSize {
    /// 1920 × 1080, the output size the V0 slice ships with.
    pub const FULL_HD: Self = Self::new(1920, 1080);

    /// `width` × `height` pixels.
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// The physical size egui-winit asks the OS for when a viewport is `points` big at `pixels_per_point`: the same
    /// product, rounded as winit rounds a fractional physical size.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a window side is far below u32::MAX
    pub fn of_points(points: Vec2, pixels_per_point: f32) -> Self {
        let side = |p: f32| (pixels_per_point * p).round().max(0.0) as u32;
        Self { width: side(points.x), height: side(points.y) }
    }
}

/// The point size that gives exactly `size` at `pixels_per_point`, or `None` for a scale that is not a positive
/// finite number.
#[allow(clippy::cast_precision_loss)] // window sides are far below 2^24
pub fn points_for(size: PhysicalSize, pixels_per_point: f32) -> Option<Vec2> {
    if !(pixels_per_point.is_finite() && pixels_per_point > 0.0) {
        return None;
    }
    let points = Vec2::new(size.width as f32 / pixels_per_point, size.height as f32 / pixels_per_point);
    debug_assert_eq!(PhysicalSize::of_points(points, pixels_per_point), size, "the round trip is exact");
    Some(points)
}

/// The commands that fit a window to `wanted` physical pixels at `pixels_per_point`, the scale of the monitor it is on
/// now; `None` for a scale that is not a positive finite number. All three, always: the size, and the minimum and
/// maximum that pin a fixed-size window to it. Bounds left at the points of the monitor the window came from would
/// fight the new size (they are in points, which are a different number of pixels on each monitor). Send them from the
/// window's own frame, so the scale is its own.
pub fn fit_commands(wanted: PhysicalSize, pixels_per_point: f32) -> Option<[ViewportCommand; 3]> {
    let points = points_for(wanted, pixels_per_point)?;
    Some([
        ViewportCommand::InnerSize(points),
        ViewportCommand::MinInnerSize(points),
        ViewportCommand::MaxInnerSize(points),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_hd_is_exact_at_100_125_and_150_percent() {
        for scale in [1.0, 1.25, 1.5] {
            let points = points_for(PhysicalSize::FULL_HD, scale).expect("a usable scale");
            assert_eq!(PhysicalSize::of_points(points, scale), PhysicalSize::FULL_HD, "at {scale}");
        }
        assert_eq!(points_for(PhysicalSize::FULL_HD, 1.25), Some(Vec2::new(1536.0, 864.0)));
        assert_eq!(points_for(PhysicalSize::FULL_HD, 1.5), Some(Vec2::new(1280.0, 720.0)));
    }

    #[test]
    fn every_common_size_round_trips_at_every_quarter_scale_from_75_to_300_percent() {
        let sizes = [(1920, 1080), (1280, 720), (2560, 1440), (3840, 2160), (1080, 1920), (1366, 768), (1001, 997)];
        for (w, h) in sizes {
            for quarter in 3_u16..=12 {
                let scale = f32::from(quarter) / 4.0;
                let size = PhysicalSize::new(w, h);
                let points = points_for(size, scale).expect("usable");
                assert_eq!(PhysicalSize::of_points(points, scale), size, "{w}x{h} at {scale}");
            }
        }
    }

    #[test]
    fn an_unusable_scale_gives_no_size() {
        for scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(points_for(PhysicalSize::FULL_HD, scale), None);
        }
    }

    #[test]
    fn a_window_is_fitted_with_its_size_and_both_bounds_at_the_scale_it_is_on() {
        let wanted = PhysicalSize::FULL_HD;
        let at_150 = Vec2::new(1280.0, 720.0);
        assert_eq!(
            fit_commands(wanted, 1.5),
            Some([
                ViewportCommand::InnerSize(at_150),
                ViewportCommand::MinInnerSize(at_150),
                ViewportCommand::MaxInnerSize(at_150),
            ])
        );
        assert_eq!(fit_commands(wanted, 0.0), None, "an unusable scale fits nothing");
    }
}
