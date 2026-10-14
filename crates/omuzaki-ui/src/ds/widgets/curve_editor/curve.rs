//! The maths of a response curve (`design-system.md` 7.5), free of egui so it can be tested exactly and lifted into the
//! crates that apply a curve to tracking, exaggeration and expression intensity.
//!
//! A [`Curve`] is a few control points joined by a **monotone cubic** (Fritsch and Butland's PCHIP): it passes
//! through every point, never overshoots between two points, and is monotone wherever the points are. That is why a
//! person can drag a point anywhere and the curve still behaves like a response curve. A point may carry an explicit
//! **tangent** ("Bezier tangents on request"), which replaces the automatic slope there and is then the person's
//! responsibility: it can overshoot, and the table clamps what it returns to the curve's range.
//!
//! What the runtime uses is the **256-entry lookup table**, rebuilt only when the curve changes: every method that
//! edits a curve rebuilds it, and nothing else does, so reading a curve costs nothing.

use std::ops::RangeInclusive;

/// Most points a curve holds.
pub const MAX_POINTS: usize = 16;
/// Fewest: the two ends.
pub const MIN_POINTS: usize = 2;
/// Entries in the lookup table.
pub const LUT_SIZE: usize = 256;
/// The least distance between two neighbouring points along x, as a fraction of the domain.
const MIN_GAP: f32 = 0.001;

/// One control point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurvePoint {
    /// Position along the input axis.
    pub x: f32,
    /// Position along the output axis.
    pub y: f32,
    /// A slope (output per input) that replaces the automatic one at this point; `None` for automatic.
    pub tangent: Option<f32>,
}

impl CurvePoint {
    /// A point with an automatic tangent.
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y, tangent: None }
    }
}

/// Why a list of points is not a curve.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CurveError {
    /// Fewer than two points.
    TooFewPoints,
    /// More than [`MAX_POINTS`].
    TooManyPoints,
    /// A coordinate is not a finite number, or the domain or range is empty.
    NotFinite,
    /// Two points are not in increasing order along x (or are closer than a thousandth of the domain).
    NotIncreasing,
}

impl std::fmt::Display for CurveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TooFewPoints => "a curve needs at least two points",
            Self::TooManyPoints => "a curve holds at most sixteen points",
            Self::NotFinite => "curve coordinates must be finite and its axes must have a width",
            Self::NotIncreasing => "curve points must increase along x",
        })
    }
}

impl std::error::Error for CurveError {}

/// The shapes a person starts from. Each is drawn on the unit square and scaled to the curve's axes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Preset {
    /// A straight line from corner to corner.
    Linear,
    /// Slow at first, then fast.
    EaseIn,
    /// Fast at first, then slow.
    EaseOut,
    /// Slow at both ends.
    SCurve,
    /// Nothing happens for the first fifth of the input.
    DeadZone,
    /// A straight line from top-left to bottom-right.
    Inverted,
    /// Jumps from nothing to everything halfway.
    Step,
}

impl Preset {
    /// Every preset, in the order the dropdown lists them.
    pub const ALL: [Self; 7] =
        [Self::Linear, Self::EaseIn, Self::EaseOut, Self::SCurve, Self::DeadZone, Self::Inverted, Self::Step];

    /// The preset's points on the unit square.
    fn unit_points(self) -> &'static [(f32, f32)] {
        match self {
            Self::Linear => &[(0.0, 0.0), (1.0, 1.0)],
            Self::EaseIn => &[(0.0, 0.0), (0.5, 0.25), (1.0, 1.0)],
            Self::EaseOut => &[(0.0, 0.0), (0.5, 0.75), (1.0, 1.0)],
            Self::SCurve => &[(0.0, 0.0), (0.3, 0.2), (0.7, 0.8), (1.0, 1.0)],
            Self::DeadZone => &[(0.0, 0.0), (0.2, 0.0), (1.0, 1.0)],
            Self::Inverted => &[(0.0, 1.0), (1.0, 0.0)],
            // A thousandth of width keeps the points in increasing order; the cubic through them is a step.
            Self::Step => &[(0.0, 0.0), (0.5, 0.0), (0.501, 1.0), (1.0, 1.0)],
        }
    }
}

/// A response curve over a domain (input) and a range (output).
#[derive(Clone, Debug, PartialEq)]
pub struct Curve {
    points: Vec<CurvePoint>,
    domain: (f32, f32),
    range: (f32, f32),
    lut: Box<[f32; LUT_SIZE]>,
    preset: Option<Preset>,
}

impl Curve {
    /// A curve through `points` over `domain` and `range`. The first and last points are moved onto the ends of the
    /// domain, and every y is clamped into the range.
    ///
    /// # Errors
    /// [`CurveError`] when there are too few or too many points, a value is not finite, or x does not increase.
    pub fn new(
        domain: RangeInclusive<f32>,
        range: RangeInclusive<f32>,
        mut points: Vec<CurvePoint>,
    ) -> Result<Self, CurveError> {
        let (domain, range) = ((*domain.start(), *domain.end()), (*range.start(), *range.end()));
        if domain.0.partial_cmp(&domain.1) != Some(std::cmp::Ordering::Less)
            || range.0.partial_cmp(&range.1) != Some(std::cmp::Ordering::Less)
            || !domain.0.is_finite()
            || !domain.1.is_finite()
            || !range.0.is_finite()
            || !range.1.is_finite()
            || points.iter().any(|p| !p.x.is_finite() || !p.y.is_finite() || p.tangent.is_some_and(|t| !t.is_finite()))
        {
            return Err(CurveError::NotFinite);
        }
        if points.len() < MIN_POINTS {
            return Err(CurveError::TooFewPoints);
        }
        if points.len() > MAX_POINTS {
            return Err(CurveError::TooManyPoints);
        }
        let gap = MIN_GAP * (domain.1 - domain.0);
        let last = points.len() - 1;
        points[0].x = domain.0;
        points[last].x = domain.1;
        if points.windows(2).any(|w| w[1].x - w[0].x < gap * 0.999) {
            return Err(CurveError::NotIncreasing);
        }
        for p in &mut points {
            p.y = p.y.clamp(range.0, range.1);
        }
        let mut curve = Self { points, domain, range, lut: Box::new([0.0; LUT_SIZE]), preset: None };
        curve.rebuild();
        Ok(curve)
    }

    /// The straight line over the unit square.
    pub fn linear() -> Self {
        Self::unit(Preset::Linear)
    }

    /// `preset` over the unit square.
    pub fn unit(preset: Preset) -> Self {
        let mut curve = Self {
            points: Vec::new(),
            domain: (0.0, 1.0),
            range: (0.0, 1.0),
            lut: Box::new([0.0; LUT_SIZE]),
            preset: None,
        };
        curve.apply_preset(preset);
        curve
    }

    /// Replaces every point with `preset`, scaled to this curve's axes, and drops explicit tangents.
    pub fn apply_preset(&mut self, preset: Preset) {
        let ((x0, x1), (y0, y1)) = (self.domain, self.range);
        self.points = preset
            .unit_points()
            .iter()
            .map(|&(x, y)| CurvePoint::new(x0 + x * (x1 - x0), y0 + y * (y1 - y0)))
            .collect();
        self.rebuild();
        self.preset = Some(preset);
    }

    /// The control points, in increasing order of x.
    pub fn points(&self) -> &[CurvePoint] {
        &self.points
    }

    /// The input axis: where the first and last points are.
    pub const fn domain(&self) -> (f32, f32) {
        self.domain
    }

    /// The output axis.
    pub const fn range(&self) -> (f32, f32) {
        self.range
    }

    /// The preset this curve is exactly, until it is edited.
    pub const fn preset(&self) -> Option<Preset> {
        self.preset
    }

    /// The 256-entry table: entry `i` is the output at input `domain.0 + i / 255` of the domain.
    pub fn lut(&self) -> &[f32; LUT_SIZE] {
        &self.lut
    }

    /// The output at `x` from the table, interpolated between its entries and clamped to the range. This is the call
    /// the runtime makes per frame.
    pub fn sample(&self, x: f32) -> f32 {
        let (x0, x1) = self.domain;
        let t = ((x - x0) / (x1 - x0)).clamp(0.0, 1.0) * (LUT_SIZE - 1) as f32;
        let i = (t.floor() as usize).min(LUT_SIZE - 2);
        let f = t - i as f32;
        self.lut[i] + (self.lut[i + 1] - self.lut[i]) * f
    }

    /// The output at `x` computed from the points, not the table. Used to fill the table and to place a new point on
    /// the curve.
    pub fn evaluate(&self, x: f32) -> f32 {
        let n = self.points.len();
        let (first, last) = (self.points[0], self.points[n - 1]);
        let y = if x <= first.x {
            first.y
        } else if x >= last.x {
            last.y
        } else {
            let slopes = self.slopes();
            let k = self.points.windows(2).position(|w| x < w[1].x).unwrap_or(n - 2);
            hermite(self.points[k], self.points[k + 1], slopes[k], slopes[k + 1], x)
        };
        y.clamp(self.range.0, self.range.1)
    }

    /// The slope at every point: the explicit tangent where there is one, the automatic monotone one elsewhere.
    pub fn slopes(&self) -> [f32; MAX_POINTS] {
        let mut m = [0.0; MAX_POINTS];
        automatic_slopes(&self.points, &mut m);
        for (slope, p) in m.iter_mut().zip(&self.points) {
            if let Some(t) = p.tangent {
                *slope = t;
            }
        }
        m
    }

    /// The automatic slope at point `i`, ignoring any explicit tangent on it: what "reset the tangent" returns to.
    pub fn automatic_slope(&self, i: usize) -> f32 {
        let mut m = [0.0; MAX_POINTS];
        automatic_slopes(&self.points, &mut m);
        m.get(i).copied().unwrap_or(0.0)
    }

    /// Moves point `i` to `(x, y)`. The ends keep their x; an inner point stays between its neighbours; y stays in the
    /// range. Returns whether anything moved.
    pub fn move_point(&mut self, i: usize, x: f32, y: f32) -> bool {
        let n = self.points.len();
        let Some(&old) = self.points.get(i) else { return false };
        let gap = MIN_GAP * (self.domain.1 - self.domain.0);
        let x =
            if i == 0 || i == n - 1 { old.x } else { x.clamp(self.points[i - 1].x + gap, self.points[i + 1].x - gap) };
        let y = y.clamp(self.range.0, self.range.1);
        if !x.is_finite() || !y.is_finite() || ((x - old.x).abs() < f32::EPSILON && (y - old.y).abs() < f32::EPSILON) {
            return false;
        }
        self.points[i].x = x;
        self.points[i].y = y;
        self.edited();
        true
    }

    /// Sets (or with `None` clears) the explicit tangent of point `i`. Returns whether it changed.
    pub fn set_tangent(&mut self, i: usize, tangent: Option<f32>) -> bool {
        let Some(point) = self.points.get_mut(i) else { return false };
        if tangent.is_some_and(|t| !t.is_finite()) || point.tangent == tangent {
            return false;
        }
        point.tangent = tangent;
        self.edited();
        true
    }

    /// Adds a point at `x`, on the curve as it is, and returns its index. Refused when the curve is full or `x` is
    /// closer than a thousandth of the domain to a neighbour.
    pub fn insert(&mut self, x: f32) -> Option<usize> {
        if self.points.len() >= MAX_POINTS || !x.is_finite() {
            return None;
        }
        let gap = MIN_GAP * (self.domain.1 - self.domain.0);
        let after = self.points.iter().position(|p| p.x > x)?;
        if after == 0 || x - self.points[after - 1].x < gap || self.points[after].x - x < gap {
            return None;
        }
        let y = self.evaluate(x);
        self.points.insert(after, CurvePoint::new(x, y));
        self.edited();
        Some(after)
    }

    /// Removes point `i`. The two ends stay, and so do the last two points. Returns whether it was removed.
    pub fn remove(&mut self, i: usize) -> bool {
        if i == 0 || i + 1 >= self.points.len() || self.points.len() <= MIN_POINTS {
            return false;
        }
        self.points.remove(i);
        self.edited();
        true
    }

    /// An edit: the curve is no longer exactly a preset.
    fn edited(&mut self) {
        self.preset = None;
        self.rebuild();
    }

    /// Fills the table from the points.
    fn rebuild(&mut self) {
        let (x0, x1) = self.domain;
        let slopes = self.slopes();
        let n = self.points.len();
        let mut k = 0;
        for i in 0..LUT_SIZE {
            let x = x0 + (x1 - x0) * i as f32 / (LUT_SIZE - 1) as f32;
            while k + 2 < n && x >= self.points[k + 1].x {
                k += 1;
            }
            let y = if x <= self.points[0].x {
                self.points[0].y
            } else if x >= self.points[n - 1].x {
                self.points[n - 1].y
            } else {
                hermite(self.points[k], self.points[k + 1], slopes[k], slopes[k + 1], x)
            };
            self.lut[i] = y.clamp(self.range.0, self.range.1);
        }
    }
}

/// The cubic Hermite segment from `from` to `to` with the slopes `from_slope` and `to_slope`, at `x` between them.
fn hermite(from: CurvePoint, to: CurvePoint, from_slope: f32, to_slope: f32, x: f32) -> f32 {
    let width = to.x - from.x;
    let t = (x - from.x) / width;
    let (t2, t3) = (t * t, t * t * t);
    (2.0 * t3 - 3.0 * t2 + 1.0) * from.y
        + (t3 - 2.0 * t2 + t) * width * from_slope
        + (-2.0 * t3 + 3.0 * t2) * to.y
        + (t3 - t2) * width * to_slope
}

/// Fritsch and Butland's slopes: the weighted harmonic mean of the two neighbouring secants where they agree in sign,
/// zero where they do not (a local extreme stays flat), and a one-sided three-point estimate at the ends that is kept
/// from overshooting.
fn automatic_slopes(points: &[CurvePoint], out: &mut [f32; MAX_POINTS]) {
    let n = points.len();
    let mut h = [0.0; MAX_POINTS];
    let mut d = [0.0; MAX_POINTS];
    for k in 0..n - 1 {
        h[k] = points[k + 1].x - points[k].x;
        d[k] = (points[k + 1].y - points[k].y) / h[k];
    }
    if n == 2 {
        out[0] = d[0];
        out[1] = d[0];
        return;
    }
    for k in 1..n - 1 {
        out[k] = if d[k - 1] * d[k] <= 0.0 {
            0.0
        } else {
            3.0 * (h[k - 1] + h[k]) / ((2.0 * h[k] + h[k - 1]) / d[k - 1] + (h[k] + 2.0 * h[k - 1]) / d[k])
        };
    }
    out[0] = end_slope(h[0], h[1], d[0], d[1]);
    out[n - 1] = end_slope(h[n - 2], h[n - 3], d[n - 2], d[n - 3]);
}

/// The slope at an end from its two nearest segments (`h0`, `d0` next to the end), kept monotone.
fn end_slope(h0: f32, h1: f32, d0: f32, d1: f32) -> f32 {
    let m = ((2.0 * h0 + h1) * d0 - h0 * d1) / (h0 + h1);
    if m * d0 <= 0.0 {
        0.0
    } else if d0 * d1 <= 0.0 && m.abs() > 3.0 * d0.abs() {
        3.0 * d0
    } else {
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve(points: &[(f32, f32)]) -> Curve {
        Curve::new(0.0..=1.0, 0.0..=1.0, points.iter().map(|&(x, y)| CurvePoint::new(x, y)).collect())
            .unwrap_or_else(|e| unreachable!("{e}"))
    }

    #[test]
    fn it_passes_through_every_point_and_the_table_agrees_with_the_maths() {
        let c = curve(&[(0.0, 0.1), (0.3, 0.2), (0.6, 0.9), (1.0, 0.8)]);
        for p in c.points() {
            assert!((c.evaluate(p.x) - p.y).abs() < 1e-5, "{p:?}");
        }
        assert!((c.lut()[0] - 0.1).abs() < 1e-6 && (c.lut()[LUT_SIZE - 1] - 0.8).abs() < 1e-6);
        for i in 0..LUT_SIZE {
            let x = i as f32 / (LUT_SIZE - 1) as f32;
            assert!((c.lut()[i] - c.evaluate(x)).abs() < 1e-6, "entry {i}");
        }
        assert!((c.sample(0.5) - c.evaluate(0.5)).abs() < 2e-3, "sampling between entries stays close");
    }

    #[test]
    fn a_curve_through_rising_points_never_goes_down_or_overshoots() {
        // A dead zone then a steep rise, a step, and an ease: the shapes where a plain cubic rings.
        for points in [
            vec![(0.0, 0.0), (0.2, 0.0), (1.0, 1.0)],
            vec![(0.0, 0.0), (0.5, 0.0), (0.501, 1.0), (1.0, 1.0)],
            vec![(0.0, 0.0), (0.05, 0.9), (0.1, 0.95), (1.0, 1.0)],
        ] {
            let c = curve(&points);
            for pair in c.lut().windows(2) {
                assert!(pair[1] >= pair[0] - 1e-6, "{points:?}: {pair:?}");
            }
            assert!(c.lut().iter().all(|y| (0.0..=1.0).contains(y)), "{points:?} overshoots");
        }
    }

    #[test]
    fn random_rising_point_sets_stay_monotone() {
        // A seeded generator, so the test is the same every run.
        let mut seed = 0x2545_F491_u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed % 10_000) as f32 / 10_000.0
        };
        for _ in 0..200 {
            let count = 3 + (next() * 8.0) as usize;
            let mut xs: Vec<f32> = (0..count - 2).map(|_| 0.02 + next() * 0.96).collect();
            let mut ys: Vec<f32> = (0..count - 2).map(|_| next()).collect();
            xs.sort_by(f32::total_cmp);
            ys.sort_by(f32::total_cmp);
            xs.dedup_by(|a, b| (*a - *b).abs() < 0.01);
            ys.truncate(xs.len());
            let mut points = vec![(0.0, 0.0)];
            points.extend(xs.iter().copied().zip(ys.iter().copied()));
            points.push((1.0, 1.0));
            let c = curve(&points);
            for pair in c.lut().windows(2) {
                assert!(pair[1] >= pair[0] - 1e-5, "{points:?}");
            }
        }
    }

    #[test]
    fn an_explicit_tangent_replaces_the_automatic_slope_and_none_brings_it_back() {
        let mut c = curve(&[(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)]);
        let auto = c.automatic_slope(1);
        assert!((auto - 1.0).abs() < 1e-5, "a straight line has slope 1: {auto}");
        assert!(c.set_tangent(1, Some(0.0)));
        assert!((c.slopes()[1]).abs() < 1e-6, "the explicit tangent is used");
        assert!((c.evaluate(0.5) - 0.5).abs() < 1e-5, "and the curve still passes through the point");
        assert!(c.set_tangent(1, None));
        assert!((c.slopes()[1] - auto).abs() < 1e-6);
        assert!(!c.set_tangent(1, None), "nothing changed");
        assert!(!c.set_tangent(1, Some(f32::NAN)), "a tangent must be a number");
    }

    #[test]
    fn a_wild_tangent_cannot_push_the_table_out_of_the_range() {
        let mut c = curve(&[(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)]);
        c.set_tangent(0, Some(40.0));
        assert!(c.lut().iter().all(|y| (0.0..=1.0).contains(y)));
    }

    #[test]
    fn moving_a_point_keeps_x_in_order_pins_the_ends_and_keeps_y_in_range() {
        let mut c = curve(&[(0.0, 0.0), (0.4, 0.4), (0.7, 0.7), (1.0, 1.0)]);
        assert!(c.move_point(1, 0.9, 0.2));
        assert!(c.points()[1].x < c.points()[2].x, "it stopped short of its neighbour: {:?}", c.points());
        assert!(c.move_point(0, 0.5, 0.3));
        assert!((c.points()[0].x - 0.0).abs() < f32::EPSILON, "the first point keeps its x");
        assert!(c.move_point(3, 1.0, 0.5));
        assert!(c.move_point(3, 1.0, 5.0));
        assert!((c.points()[3].y - 1.0).abs() < f32::EPSILON, "y is clamped into the range");
        assert!(!c.move_point(3, 1.0, 5.0), "no movement, no change");
        assert!(!c.move_point(9, 0.5, 0.5), "no such point");
    }

    #[test]
    fn points_are_added_on_the_curve_and_removed_but_never_the_ends_or_the_last_two() {
        let mut c = curve(&[(0.0, 0.0), (1.0, 1.0)]);
        let i = c.insert(0.25).unwrap_or_default();
        assert_eq!(i, 1);
        assert!((c.points()[1].y - 0.25).abs() < 1e-5, "a new point sits on the curve, so nothing jumps");
        assert_eq!(c.insert(0.2505), None, "too close to a neighbour");
        assert_eq!(c.insert(1.5), None, "outside the domain");
        assert!(!c.remove(0) && !c.remove(2), "the ends stay");
        assert!(c.remove(1));
        assert!(!c.remove(1), "and so do the last two");
        let mut full = Curve::linear();
        for k in 1..MAX_POINTS - 1 {
            assert!(full.insert(k as f32 / MAX_POINTS as f32).is_some());
        }
        assert_eq!(full.points().len(), MAX_POINTS);
        assert_eq!(full.insert(0.999), None, "a full curve takes no more");
    }

    #[test]
    fn a_preset_is_remembered_until_the_curve_is_edited_and_scales_to_the_axes() {
        let mut c = Curve::new(-1.0..=1.0, 0.0..=10.0, vec![CurvePoint::new(-1.0, 0.0), CurvePoint::new(1.0, 10.0)])
            .unwrap_or_else(|e| unreachable!("{e}"));
        c.apply_preset(Preset::Inverted);
        assert_eq!(c.preset(), Some(Preset::Inverted));
        assert!((c.points()[0].y - 10.0).abs() < 1e-6 && (c.points()[1].x - 1.0).abs() < 1e-6);
        c.apply_preset(Preset::SCurve);
        assert!(c.move_point(1, -0.3, 2.0));
        assert_eq!(c.preset(), None, "an edited preset is a custom curve");
        for preset in Preset::ALL {
            let unit = Curve::unit(preset);
            assert_eq!(unit.preset(), Some(preset));
            assert!(unit.lut().iter().all(|y| (0.0..=1.0).contains(y)), "{preset:?} stays in range");
        }
    }

    #[test]
    fn lists_that_are_not_curves_are_refused_with_the_reason() {
        let ok = |p: &[(f32, f32)]| {
            Curve::new(0.0..=1.0, 0.0..=1.0, p.iter().map(|&(x, y)| CurvePoint::new(x, y)).collect())
        };
        assert_eq!(ok(&[(0.0, 0.0)]).err(), Some(CurveError::TooFewPoints));
        assert_eq!(ok(&[(0.0, 0.0), (0.5, 0.5), (0.5, 0.6), (1.0, 1.0)]).err(), Some(CurveError::NotIncreasing));
        assert_eq!(ok(&[(0.0, 0.0), (f32::NAN, 0.5), (1.0, 1.0)]).err(), Some(CurveError::NotFinite));
        assert_eq!(
            Curve::new(1.0..=1.0, 0.0..=1.0, vec![CurvePoint::new(1.0, 0.0), CurvePoint::new(1.0, 1.0)]).err(),
            Some(CurveError::NotFinite)
        );
        let many: Vec<_> = (0..=MAX_POINTS).map(|i| CurvePoint::new(i as f32 / MAX_POINTS as f32, 0.5)).collect();
        assert_eq!(Curve::new(0.0..=1.0, 0.0..=1.0, many).err(), Some(CurveError::TooManyPoints));
        // The ends are moved onto the ends of the domain, and y into the range.
        let c = ok(&[(0.2, 7.0), (0.5, 0.5), (0.8, -1.0)]).unwrap_or_else(|e| unreachable!("{e}"));
        assert!((c.points()[0].x - 0.0).abs() < f32::EPSILON && (c.points()[0].y - 1.0).abs() < f32::EPSILON);
        assert!((c.points()[2].x - 1.0).abs() < f32::EPSILON && c.points()[2].y.abs() < f32::EPSILON);
    }
}
