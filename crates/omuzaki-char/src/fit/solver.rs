//! The silhouette fit solver: deterministic coordinate descent with a golden-section line search per parameter.
//!
//! Each parameter in turn is searched over a bracket for the value that lowers the cost. A sweep goes through every
//! parameter once; its brackets are a window round the current value, [`WINDOW`] of the parameter's range wide in the
//! first sweep and [`SHRINK`] times the last one's after each sweep that gained. A sweep that gains nothing is run once
//! more at the wide window. A last move along the sweep's net displacement follows each sweep. The result depends only on the
//! inputs and [`PARAM_ORDER`]: no threads, no randomness, and ties keep the earlier value, so a parameter the views
//! cannot see stays at its default.
//!
//! **What a silhouette identifies.** Widths, arm length, foot size and the shape axes show in the outline. Sole and
//! crown are aligned, so `leg_length`, `torso_length` and the `height` axis trade off: the share of the leg against
//! the torso is identified, each one alone is not. `neck_length` is not visible (the neck lies inside the torso loft)
//! and `hand_size` is not either until `hands::build` makes a mesh; both stay at their defaults. The search stops at
//! [`MAX_EVALUATIONS`] or after two sweeps in a row that each gain under [`MIN_GAIN`] mean `IoU`.

use omuzaki_core::mesh::MeshData;

use super::{FitError, FitParams, FitView, Objective, Score, Target};

/// The most evaluations (builds and scores of a candidate) one fit makes.
pub const MAX_EVALUATIONS: usize = 600;
/// A sweep that gains less mean `IoU` than this has stalled; two stalled sweeps in a row end the fit.
pub const MIN_GAIN: f64 = 1e-4;
/// A line search ends when its bracket is this fraction of the width it started with (three golden steps), or when the
/// evaluation budget is spent; nothing else ends it, in particular not equal costs.
const BRACKET_TOLERANCE: f64 = 0.25;
/// Width of the first sweep's window (and of the restart after a stalled sweep), as a fraction of a parameter's range.
const WINDOW: f64 = 0.5;
/// How much narrower each sweep's window is than the last, while the sweeps keep gaining.
const SHRINK: f64 = 0.85;
/// Two costs closer than this are the same sample.
const FLAT: f64 = 1e-12;
/// How far past its end a sweep's net displacement is searched, in displacements.
const PATTERN_REACH: f64 = 3.0;
/// The golden ratio's reciprocal.
const INV_PHI: f64 = 0.618_033_988_749_894_9;

/// A parameter the fit moves: the 8 proportions, then the 5 body-shape axes.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Param {
    /// `Proportions::neck_length`.
    NeckLength,
    /// `Proportions::torso_length`.
    TorsoLength,
    /// `Proportions::shoulder_width`.
    ShoulderWidth,
    /// `Proportions::hip_width`.
    HipWidth,
    /// `Proportions::arm_length`.
    ArmLength,
    /// `Proportions::leg_length`.
    LegLength,
    /// `Proportions::hand_size` (no mesh yet, so not visible).
    HandSize,
    /// `Proportions::foot_size`.
    FootSize,
    /// `BodyShape::height`.
    Height,
    /// `BodyShape::build`.
    Build,
    /// `BodyShape::curves`.
    Curves,
    /// `BodyShape::musculature`.
    Musculature,
    /// `BodyShape::shoulder_hip`.
    ShoulderHip,
}

/// The order the parameters are searched in: the ones that move most of the outline first (the lengths, then the
/// widths, then the shape axes), the subtle ones last. It is part of the result: a different order is a different fit.
pub const PARAM_ORDER: [Param; 13] = [
    Param::LegLength,
    Param::TorsoLength,
    Param::NeckLength,
    Param::ArmLength,
    Param::ShoulderWidth,
    Param::HipWidth,
    Param::HandSize,
    Param::FootSize,
    Param::Height,
    Param::Build,
    Param::ShoulderHip,
    Param::Curves,
    Param::Musculature,
];

impl Param {
    /// The valid range of the parameter.
    pub fn range(self) -> (f64, f64) {
        if matches!(self, Self::Height | Self::Build | Self::Curves | Self::Musculature | Self::ShoulderHip) {
            (-1.0, 1.0)
        } else {
            (0.5, 2.0)
        }
    }

    fn slot(self, p: &mut FitParams) -> &mut f64 {
        match self {
            Self::NeckLength => &mut p.proportions.neck_length,
            Self::TorsoLength => &mut p.proportions.torso_length,
            Self::ShoulderWidth => &mut p.proportions.shoulder_width,
            Self::HipWidth => &mut p.proportions.hip_width,
            Self::ArmLength => &mut p.proportions.arm_length,
            Self::LegLength => &mut p.proportions.leg_length,
            Self::HandSize => &mut p.proportions.hand_size,
            Self::FootSize => &mut p.proportions.foot_size,
            Self::Height => &mut p.shape.height,
            Self::Build => &mut p.shape.build,
            Self::Curves => &mut p.shape.curves,
            Self::Musculature => &mut p.shape.musculature,
            Self::ShoulderHip => &mut p.shape.shoulder_hip,
        }
    }

    /// Sets the parameter in `p`.
    pub fn set(self, p: &mut FitParams, value: f64) {
        *self.slot(p) = value;
    }

    /// The value of the parameter in `p`.
    pub fn get(self, p: &FitParams) -> f64 {
        *self.slot(&mut p.clone())
    }
}

/// What a fit is given.
#[derive(Clone, Copy, Debug)]
pub struct FitInput<'a> {
    /// The body template id (`soft-sd`, `mini`, ...).
    pub template: &'a str,
    /// The target masks, one per view (up to front, side and back).
    pub targets: &'a [Target],
    /// The other built parts that show in the silhouette (head, hair, outfit), in the same space as the body.
    pub fixed: &'a [MeshData],
}

/// How one view fits.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewFit {
    /// The view.
    pub view: FitView,
    /// `IoU` with the target at the default parameters.
    pub iou_before: f64,
    /// `IoU` with the target at the fitted parameters.
    pub iou_after: f64,
}

/// The outcome of a fit.
#[derive(Clone, Debug, PartialEq)]
pub struct FitResult {
    /// The fitted parameters.
    pub params: FitParams,
    /// Mean `IoU` over the views at the default parameters.
    pub iou_before: f64,
    /// Mean `IoU` over the views at the fitted parameters.
    pub iou_after: f64,
    /// Each view's `IoU` before and after, in the order of the targets.
    pub per_view: Vec<ViewFit>,
    /// Candidates built and scored, at most [`MAX_EVALUATIONS`].
    pub evaluations: usize,
}

/// The mean `IoU` over the views of the body of `params` against `input`'s targets: how well a given set of parameters
/// fits, for checking a fit against a cleaner target than it was made from.
///
/// # Errors
/// As [`fit`].
pub fn iou_of(input: &FitInput<'_>, params: &FitParams) -> Result<f64, FitError> {
    Objective::new(input.template, input.targets, input.fixed)?.evaluate(params).map(|s| s.iou)
}

/// Fits the body to `input` in [`PARAM_ORDER`], from the default parameters.
///
/// # Errors
/// [`FitError`] if the input is bad or a candidate body cannot be built.
pub fn fit(input: &FitInput<'_>) -> Result<FitResult, FitError> {
    fit_ordered(input, &PARAM_ORDER)
}

/// Like [`fit`] with the parameters searched in `order` (each at most once, at least one).
///
/// # Errors
/// [`FitError::BadOrder`] for an empty order or a repeat; otherwise as [`fit`].
pub fn fit_ordered(input: &FitInput<'_>, order: &[Param]) -> Result<FitResult, FitError> {
    if order.is_empty() || order.iter().enumerate().any(|(i, p)| order[..i].contains(p)) {
        return Err(FitError::BadOrder);
    }
    let objective = Objective::new(input.template, input.targets, input.fixed)?;
    let start = FitParams::default();
    let first = objective.evaluate(&start)?;
    let mut search = Search { objective: &objective, params: start, best: first.clone(), evaluations: 1 };
    let mut width = WINDOW;
    let mut stalled = false;
    loop {
        let before = search.best.iou;
        let sweep_start = search.params.clone();
        for &p in order {
            let (lo, hi) = p.range();
            let at = p.get(&search.params);
            let half = (width * (hi - lo)) / 2.0;
            let bracket = ((at - half).max(lo), (at + half).min(hi));
            let base = search.params.clone();
            let along = |v: f64| {
                let mut c = base.clone();
                p.set(&mut c, v);
                c
            };
            if !search.line(&along, bracket)? {
                break;
            }
        }
        search.pattern_move(&sweep_start)?;
        let gained = search.best.iou - before >= MIN_GAIN;
        // A stalled sweep gets one more at the wide window before the fit ends: a narrow window can sit in a dip.
        if search.evaluations >= MAX_EVALUATIONS || (!gained && stalled) {
            break;
        }
        (width, stalled) = if gained { (width * SHRINK, false) } else { (WINDOW, true) };
    }
    let per_view = objective
        .views()
        .zip(first.per_view.iter().zip(&search.best.per_view))
        .map(|(view, (&iou_before, &iou_after))| ViewFit { view, iou_before, iou_after })
        .collect();
    Ok(FitResult {
        params: search.params,
        iou_before: first.iou,
        iou_after: search.best.iou,
        per_view,
        evaluations: search.evaluations,
    })
}

/// The state of a descent: the best parameters so far and the cost they reach.
struct Search<'a> {
    objective: &'a Objective,
    params: FitParams,
    best: Score,
    evaluations: usize,
}

impl Search<'_> {
    /// Scores `candidate`, keeping it if it beats the best so far. `None` once the budget is spent.
    fn probe(&mut self, candidate: FitParams) -> Result<Option<f64>, FitError> {
        if self.evaluations >= MAX_EVALUATIONS {
            return Ok(None);
        }
        self.evaluations += 1;
        let score = self.objective.evaluate(&candidate)?;
        let cost = score.cost;
        if cost < self.best.cost {
            self.params = candidate;
            self.best = score;
        }
        Ok(Some(cost))
    }

    /// A move along the sweep's net displacement `start -> params`, which a ridge of coupled parameters needs and
    /// one-parameter steps crawl along.
    fn pattern_move(&mut self, start: &FitParams) -> Result<(), FitError> {
        let end = self.params.clone();
        let along = |t: f64| {
            let mut c = end.clone();
            for p in PARAM_ORDER {
                let (lo, hi) = p.range();
                let (a, b) = (p.get(start), p.get(&end));
                p.set(&mut c, (a + (b - a) * t).clamp(lo, hi));
            }
            c
        };
        self.line(&along, (0.0, PATTERN_REACH)).map(|_| ())
    }

    /// Golden-section search of `at(t)` for `t` in `(a, b)`; `false` when the budget ran out.
    fn line(&mut self, at: &dyn Fn(f64) -> FitParams, bracket: (f64, f64)) -> Result<bool, FitError> {
        golden(bracket, &mut |t| self.probe(at(t)))
    }
}

/// Golden-section search over `(a, b)` of `cost`, which gives `None` once the budget is spent (then this is `false`).
///
/// Equal costs never end the search. Two interior probes that cost the same prove nothing at low resolution, where
/// distinct values can rasterise alike, and the samples can straddle a dip between them. On a tie the ends and the
/// midpoints of the three intervals are probed too, and if one of the seven is lower the search goes on in the bracket
/// round it. If all seven agree it goes on in the whole bracket, and a tie always keeps the lower half, so the search
/// is deterministic and narrows towards the lower end; it stops at [`BRACKET_TOLERANCE`] or the budget.
fn golden((a, b): (f64, f64), cost: &mut dyn FnMut(f64) -> Result<Option<f64>, FitError>) -> Result<bool, FitError> {
    let (c, d) = (b - INV_PHI * (b - a), a + INV_PHI * (b - a));
    let Some(fc) = cost(c)? else { return Ok(false) };
    let Some(fd) = cost(d)? else { return Ok(false) };
    if (fc - fd).abs() >= FLAT {
        return steps((a, b), (c, d), (fc, fd), cost);
    }
    let mut samples = [
        (a, f64::NAN),
        (f64::midpoint(a, c), f64::NAN),
        (c, fc),
        (f64::midpoint(c, d), f64::NAN),
        (d, fd),
        (f64::midpoint(d, b), f64::NAN),
        (b, f64::NAN),
    ];
    for (t, f) in samples.iter_mut().filter(|(_, f)| f.is_nan()) {
        let Some(v) = cost(*t)? else { return Ok(false) };
        *f = v;
    }
    if samples.iter().all(|(_, f)| (f - fc).abs() < FLAT) {
        return steps((a, b), (c, d), (fc, fd), cost);
    }
    let low = samples.iter().enumerate().fold(0, |m, (i, (_, f))| if *f < samples[m].1 { i } else { m });
    let (lo, hi) = (samples[low.saturating_sub(1)].0, samples[(low + 1).min(samples.len() - 1)].0);
    let (c, d) = (hi - INV_PHI * (hi - lo), lo + INV_PHI * (hi - lo));
    let Some(fc) = cost(c)? else { return Ok(false) };
    let Some(fd) = cost(d)? else { return Ok(false) };
    steps((lo, hi), (c, d), (fc, fd), cost)
}

/// The golden-section steps from the bracket `(a, b)` whose interior probes `(c, d)` cost `(fc, fd)`, until the
/// bracket is [`BRACKET_TOLERANCE`] of its starting width. A tie keeps the lower half.
fn steps(
    (mut a, mut b): (f64, f64),
    (mut c, mut d): (f64, f64),
    (mut fc, mut fd): (f64, f64),
    cost: &mut dyn FnMut(f64) -> Result<Option<f64>, FitError>,
) -> Result<bool, FitError> {
    let stop = (b - a) * BRACKET_TOLERANCE;
    while b - a > stop {
        let probed = if fc <= fd {
            (b, d, fd) = (d, c, fc);
            c = b - INV_PHI * (b - a);
            cost(c)?.map(|f| fc = f)
        } else {
            (a, c, fc) = (c, d, fd);
            d = a + INV_PHI * (b - a);
            cost(d)?.map(|f| fd = f)
        };
        if probed.is_none() {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A staircase: the interior probes at 0.382 and 0.618 of `(0, 1)` tie, the end at 1 is lower. A pixel grid does
    /// this to a visible parameter.
    fn stairs(t: f64) -> f64 {
        if t > 0.9 { 0.0 } else { 1.0 }
    }

    #[test]
    fn tied_interior_probes_do_not_make_a_parameter_flat() {
        let mut probes = Vec::new();
        let ok = golden((0.0, 1.0), &mut |t| {
            probes.push(t);
            Ok(Some(stairs(t)))
        });
        assert_eq!(ok, Ok(true));
        assert!(probes.len() > 4, "the search stopped after {probes:?}");
        assert!(probes.contains(&1.0), "the end of the bracket was not probed");
    }

    /// A dip in the middle of the middle interval: the four samples at 0, 0.382, 0.618 and 1 all miss it.
    #[test]
    fn a_dip_between_the_four_samples_is_found() {
        let mut lowest = f64::MAX;
        let ok = golden((0.0, 1.0), &mut |t| {
            let f = if (t - 0.5).abs() < 0.02 { 0.0 } else { 1.0 };
            lowest = lowest.min(f);
            Ok(Some(f))
        });
        assert_eq!(ok, Ok(true));
        assert!(lowest == 0.0, "the dip near 0.5 was never probed");
    }

    /// Equal costs end nothing: a constant cost is searched to the tolerance, not given up on after the extra probes.
    #[test]
    fn a_constant_cost_is_searched_to_the_tolerance() {
        let mut probes = Vec::new();
        let ok = golden((0.0, 1.0), &mut |t| {
            probes.push(t);
            Ok(Some(0.5))
        });
        assert_eq!(ok, Ok(true));
        assert!(probes.len() > 7, "{probes:?}");
        let (lo, hi) =
            (probes.iter().copied().fold(f64::MAX, f64::min), probes.iter().copied().fold(f64::MIN, f64::max));
        assert!((lo, hi) == (0.0, 1.0), "{probes:?}");
    }

    /// A dip only near 0.1: no sample of the first seven lands in it, and the search still reaches it.
    #[test]
    fn a_dip_near_the_lower_end_is_reached() {
        let mut lowest = f64::MAX;
        let ok = golden((0.0, 1.0), &mut |t| {
            let f = if (t - 0.1).abs() < 0.02 { 0.0 } else { 1.0 };
            lowest = lowest.min(f);
            Ok(Some(f))
        });
        assert_eq!(ok, Ok(true));
        assert!(lowest == 0.0, "the dip near 0.1 was never probed");
    }

    #[test]
    fn the_budget_ends_a_search_early() {
        let mut left = 5;
        let ok = golden((0.0, 1.0), &mut |_| {
            left -= 1;
            Ok((left >= 0).then_some(0.5))
        });
        assert_eq!(ok, Ok(false));
    }
}
