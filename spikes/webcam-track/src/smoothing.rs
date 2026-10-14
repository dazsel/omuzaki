//! Landmark smoothing: MediaPipe's One-Euro filter as `FaceLandmarker` configures it for video (min cutoff 0.05,
//! beta 80, derivative cutoff 1; `LandmarksSmoothingCalculator` with `one_euro_filter`).
//!
//! Each landmark axis is filtered on its own, in pixels. The cutoff rises with speed, so a still face is held
//! almost perfectly steady while fast motion passes through with little lag.

use crate::landmarks::Landmark;

/// Cutoff (Hz) when nothing moves: the smaller, the steadier a still face.
const MIN_CUTOFF: f64 = 0.05;
/// How fast the cutoff rises with speed.
const BETA: f64 = 80.0;
/// Cutoff (Hz) of the speed estimate's own low-pass.
const DERIVATIVE_CUTOFF: f64 = 1.0;
/// Frame rate assumed until two timestamps give a real one.
const DEFAULT_FREQUENCY: f64 = 30.0;
/// Landmark sets smaller than this (pixels) pass through unfiltered, as in MediaPipe.
const MIN_OBJECT_SCALE: f32 = 1e-6;

/// Exponential low-pass with a per-sample smoothing factor.
#[derive(Debug, Clone, Copy, Default)]
struct LowPass {
    stored: Option<f64>,
}

impl LowPass {
    fn apply(&mut self, value: f64, alpha: f64) -> f64 {
        let out = self.stored.map_or(value, |s| alpha * value + (1.0 - alpha) * s);
        self.stored = Some(out);
        out
    }
}

/// One-Euro filter for a single scalar.
#[derive(Debug, Clone, Copy)]
struct OneEuro {
    value: LowPass,
    speed: LowPass,
    last_raw: Option<f64>,
}

fn alpha(cutoff: f64, frequency: f64) -> f64 {
    let (te, tau) = (1.0 / frequency, 1.0 / (2.0 * std::f64::consts::PI * cutoff));
    1.0 / (1.0 + tau / te)
}

impl OneEuro {
    fn new() -> Self {
        Self { value: LowPass::default(), speed: LowPass::default(), last_raw: None }
    }

    /// `frequency` is the current sample rate in Hz; `value_scale` converts the value to object-relative units so the
    /// cutoff does not depend on how big the face is in the frame.
    fn apply(&mut self, value: f64, frequency: f64, value_scale: f64) -> f64 {
        let speed = self.last_raw.map_or(0.0, |prev| (value - prev) * value_scale * frequency);
        self.last_raw = Some(value);
        let smoothed_speed = self.speed.apply(speed, alpha(DERIVATIVE_CUTOFF, frequency));
        let cutoff = MIN_CUTOFF + BETA * smoothed_speed.abs();
        self.value.apply(value, alpha(cutoff, frequency))
    }
}

/// Filters a stream of landmark sets.
#[derive(Debug, Clone, Default)]
pub struct LandmarkSmoother {
    filters: Vec<[OneEuro; 3]>,
    frequency: f64,
    last_time_ns: Option<i64>,
}

impl LandmarkSmoother {
    /// A smoother that has seen no frames.
    pub fn new() -> Self {
        Self { filters: Vec::new(), frequency: DEFAULT_FREQUENCY, last_time_ns: None }
    }

    /// Forgets all history, e.g. after the face was lost.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Smooths `points` (normalised frame coordinates) in place. `timestamp_ms` must increase from frame to frame.
    ///
    /// Reproduces MediaPipe's behaviour, including that a frame timestamp of exactly 0 never updates the measured
    /// frame rate (the first two frames then run at the default 30 Hz).
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // pixel-scale values are far below f32 limits
    pub fn apply(&mut self, points: &mut [Landmark], frame_w: usize, frame_h: usize, timestamp_ms: i64) {
        let now_ns = timestamp_ms.saturating_mul(1_000_000);
        match self.last_time_ns {
            Some(last) if now_ns <= last => return, // out-of-order timestamp: leave the points as measured
            Some(last) if last != 0 && now_ns != 0 => self.frequency = 1e9 / (now_ns - last) as f64,
            _ => {}
        }
        self.last_time_ns = Some(now_ns);
        if self.filters.len() != points.len() {
            self.filters = vec![[OneEuro::new(); 3]; points.len()];
        }
        let (fw, fh) = (frame_w as f32, frame_h as f32);
        let object_scale = object_scale(points, fw, fh);
        if object_scale < MIN_OBJECT_SCALE {
            return;
        }
        let value_scale = f64::from(1.0 / object_scale);
        for (p, f) in points.iter_mut().zip(&mut self.filters) {
            // Pixels for x and y; z is scaled like x.
            let px = [p[0] * fw, p[1] * fh, p[2] * fw];
            let out = [0, 1, 2].map(|a| f[a].apply(f64::from(px[a]), self.frequency, value_scale) as f32);
            *p = [out[0] / fw, out[1] / fh, out[2] / fw];
        }
    }
}

/// Mean of the landmark box's width and height in pixels: MediaPipe's measure of how big the face is.
fn object_scale(points: &[Landmark], fw: f32, fh: f32) -> f32 {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
    for p in points {
        x0 = x0.min(p[0] * fw);
        x1 = x1.max(p[0] * fw);
        y0 = y0.min(p[1] * fh);
        y1 = y1.max(p[1] * fh);
    }
    (x1 - x0).midpoint(y1 - y0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(offset: f32) -> Vec<Landmark> {
        vec![[0.3 + offset, 0.3, 0.0], [0.7 + offset, 0.7, 0.0]]
    }

    #[test]
    fn first_frame_passes_through_and_a_still_face_stays_put() {
        let mut s = LandmarkSmoother::new();
        let mut a = face(0.0);
        s.apply(&mut a, 100, 100, 0);
        assert_eq!(a, face(0.0));
        let mut b = face(0.0);
        s.apply(&mut b, 100, 100, 33);
        assert_eq!(b, face(0.0));
    }

    #[test]
    fn jitter_is_damped_but_a_large_move_follows_closely() {
        let mut s = LandmarkSmoother::new();
        let mut a = face(0.0);
        s.apply(&mut a, 100, 100, 0);
        // A 0.1 px wobble barely moves the output...
        let mut wobble = face(0.001);
        s.apply(&mut wobble, 100, 100, 33);
        assert!((wobble[0][0] - 0.3).abs() < 0.0004, "got {}", wobble[0][0]);
        // ...but after several frames of a fast sweep the output has caught up almost completely.
        let mut last = 0.0;
        for i in 1..=6 {
            let mut p = face(0.2);
            s.apply(&mut p, 100, 100, 33 + 33 * i);
            last = p[0][0];
        }
        assert!(last > 0.45, "sweep reached {last}");
    }

    #[test]
    fn out_of_order_timestamps_and_resets_are_handled() {
        let mut s = LandmarkSmoother::new();
        let mut a = face(0.0);
        s.apply(&mut a, 100, 100, 100);
        let mut b = face(0.05);
        s.apply(&mut b, 100, 100, 100);
        assert_eq!(b, face(0.05), "same timestamp: untouched");
        s.reset();
        let mut c = face(0.05);
        s.apply(&mut c, 100, 100, 50);
        assert_eq!(c, face(0.05), "after reset the next frame is the new start");
    }
}
