//! The silhouette fit solver: deterministic coordinate descent with a golden-section line search per parameter.
//!
//! Each parameter in turn is searched over a bracket for the value that lowers the cost; the golden steps end with one
//! parabolic step through the lowest probe and its neighbours, which the smooth cost (the coverage `IoU`, see the
//! [module](super) docs) rewards with a far closer value than another golden step would. A sweep goes through every
//! parameter once; its brackets are a window round the current value, `WINDOW` of the parameter's range wide in the
//! first sweep and `SHRINK` times the last one's after each sweep that gained. A sweep that gains nothing is run once
//! more at the wide window. A last move along the sweep's net displacement follows each sweep. Every sweep searches
//! every parameter: whether one shows depends on the others, which keep moving. The result depends only on the inputs
//! and [`PARAM_ORDER`]: no threads, no randomness, and a tie keeps the lower parameter value within a line search and
//! the earlier line's value across them, so a parameter the views cannot see stays where the fit started.
//! [`fit`] starts from the default body; [`fit_from`] starts from a given one (an op's stored body), and since only a
//! strictly cheaper candidate replaces the start, its result never costs more than the body it started from.
//!
//! **What a silhouette identifies.** Widths, arm length, foot size and the shape axes show in the outline. Sole and
//! crown are aligned, so `leg_length`, `torso_length` and the `height` axis trade off: the share of the leg against
//! the torso is identified, each one alone is not. `neck_length` is not visible (the neck lies inside the torso loft)
//! and stays at its default. `hand_size` is searched (the hands are meshed), though the hands hang over the hip outline
//! and a silhouette reads them less well than the rest. The search stops at [`MAX_EVALUATIONS`] or after two sweeps in a row that
//! each lower the cost by less than [`MIN_GAIN`].

use std::cmp::Ordering;

use omuzaki_char_api::BodyParams;
use omuzaki_core::mesh::MeshData;

use super::{FitError, FitParams, FitView, Follow, Objective, Score, Target};

/// The most evaluations (builds and scores of a candidate) one fit makes.
pub const MAX_EVALUATIONS: usize = 600;
/// A sweep that lowers the cost by less than this has stalled; two stalled sweeps in a row end the fit.
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
/// A parabolic step closer than this fraction of its three probes' span to one of them is not worth an evaluation.
const PARABOLA_GAP: f64 = 0.02;
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
    /// `Proportions::hand_size`.
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

    /// The parameter's name in the body parameters (`leg_length`, `build`, ...).
    pub fn name(self) -> &'static str {
        match self {
            Self::NeckLength => "neck_length",
            Self::TorsoLength => "torso_length",
            Self::ShoulderWidth => "shoulder_width",
            Self::HipWidth => "hip_width",
            Self::ArmLength => "arm_length",
            Self::LegLength => "leg_length",
            Self::HandSize => "hand_size",
            Self::FootSize => "foot_size",
            Self::Height => "height",
            Self::Build => "build",
            Self::Curves => "curves",
            Self::Musculature => "musculature",
            Self::ShoulderHip => "shoulder_hip",
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
    /// `IoU` with the target at the parameters the fit started from (the default ones for [`fit`]).
    pub iou_before: f64,
    /// `IoU` with the target at the fitted parameters.
    pub iou_after: f64,
}

/// The outcome of a fit.
#[derive(Clone, Debug, PartialEq)]
pub struct FitResult {
    /// The fitted parameters.
    pub params: FitParams,
    /// Mean `IoU` over the views at the parameters the fit started from (the default ones for [`fit`]).
    pub iou_before: f64,
    /// Mean `IoU` over the views at the fitted parameters.
    pub iou_after: f64,
    /// What the search minimises (`1 -` the coverage `IoU`, see the [module](super) docs) at the starting parameters.
    pub cost_before: f64,
    /// The same at the fitted parameters: never more than [`cost_before`](Self::cost_before). The `IoU`s are
    /// centre-sampled, so they can move the other way by a little when the outline moves by less than a pixel.
    pub cost_after: f64,
    /// Each view's `IoU` before and after, in the order of the targets.
    pub per_view: Vec<ViewFit>,
    /// Candidates built and scored, at most the fit's budget ([`MAX_EVALUATIONS`] unless asked for fewer).
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
    descend(&objective, FitParams::default(), order, MAX_EVALUATIONS, &mut |_| true)
}

/// Fits the body to `input` in [`PARAM_ORDER`], starting from `body`'s proportions and shape (each value clamped into
/// its parameter's range) and building every candidate with `body`'s hands, which the fit does not move. The baseline
/// `IoU`s are those of `body` itself (with `input`'s fixed parts and the parts `follow` builds), and the result never costs more than it: what an op
/// fitting a stored character needs, so a fit cannot make the body it claims to fit worse.
///
/// `follow` builds, for every candidate, the parts that move with the body (a tail or wings built on its joints): they
/// are drawn with the candidate's body in scoring, so the fit sees the silhouette the rebuilt character will have.
/// `input.fixed` stays where it is.
///
/// It makes at most `budget` evaluations (1 to [`MAX_EVALUATIONS`]; outside that, the nearest). `watch` is called before
/// every evaluation with the number made so far; returning `false` stops the fit with [`FitError::Stopped`]. A job
/// reports its progress and checks for a cancel there.
///
/// # Errors
/// [`FitError::Stopped`] when `watch` stops it; otherwise as [`fit`].
pub fn fit_from(
    input: &FitInput<'_>,
    body: &BodyParams,
    follow: Option<Follow<'_>>,
    budget: usize,
    watch: &mut dyn FnMut(usize) -> bool,
) -> Result<FitResult, FitError> {
    let objective = Objective::new(input.template, input.targets, input.fixed)?.with_body(body).with_follow(follow);
    let mut params = FitParams { proportions: body.proportions.clone(), shape: body.shape.clone() };
    for p in PARAM_ORDER {
        let value = p.get(&params).clamp(p.range().0, p.range().1);
        p.set(&mut params, value);
    }
    descend(&objective, params, &PARAM_ORDER, budget.clamp(1, MAX_EVALUATIONS), watch)
}

/// What a descent scores candidates with: the silhouette [`Objective`], or a test's own cost.
trait Cost {
    /// The score of `params`.
    fn score(&self, params: &FitParams) -> Result<Score, FitError>;
    /// The views the scores' `per_view` are in.
    fn view_list(&self) -> Vec<FitView>;
}

impl Cost for Objective<'_> {
    fn score(&self, params: &FitParams) -> Result<Score, FitError> {
        self.evaluate(params)
    }

    fn view_list(&self) -> Vec<FitView> {
        self.views().collect()
    }
}

/// The descent itself: from `start`, in `order`, at most `budget` evaluations, with `watch` asked before each one (see
/// [`fit_from`]).
fn descend(
    objective: &dyn Cost,
    start: FitParams,
    order: &[Param],
    budget: usize,
    watch: &mut dyn FnMut(usize) -> bool,
) -> Result<FitResult, FitError> {
    if !watch(0) {
        return Err(FitError::Stopped);
    }
    let first = objective.score(&start)?;
    let mut search = Search {
        objective,
        params: start,
        best: first.clone(),
        keep: Keep::new(first.cost),
        evaluations: 1,
        budget,
        watch,
        probes: Vec::new(),
    };
    let mut width = WINDOW;
    let mut stalled = false;
    loop {
        let before = search.best.cost;
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
        let gained = before - search.best.cost >= MIN_GAIN;
        // A stalled sweep gets one more at the wide window before the fit ends: a narrow window can sit in a dip.
        if search.evaluations >= search.budget || (!gained && stalled) {
            break;
        }
        (width, stalled) = if gained { (width * SHRINK, false) } else { (WINDOW, true) };
    }
    let per_view = objective
        .view_list()
        .into_iter()
        .zip(first.per_view.iter().zip(&search.best.per_view))
        .map(|(view, (&iou_before, &iou_after))| ViewFit { view, iou_before, iou_after })
        .collect();
    Ok(FitResult {
        params: search.params,
        iou_before: first.iou,
        iou_after: search.best.iou,
        cost_before: first.cost,
        cost_after: search.best.cost,
        per_view,
        evaluations: search.evaluations,
    })
}

/// The state of a descent: the best parameters so far and the cost they reach.
struct Search<'a, 'w> {
    objective: &'a dyn Cost,
    params: FitParams,
    best: Score,
    keep: Keep,
    evaluations: usize,
    /// The most evaluations it makes.
    budget: usize,
    /// Asked before each evaluation whether the fit goes on (see [`fit_from`]).
    watch: &'w mut dyn FnMut(usize) -> bool,
    /// The probes `(t, cost)` of the current line search, for its parabolic step.
    probes: Vec<(f64, f64)>,
}

/// Which candidate a search stores: the first strictly cheaper one, and among equal costs found by one line search
/// the one at the lowest value `t` along it. A tie with a candidate from an earlier line search keeps that one.
#[derive(Clone, Copy, Debug)]
struct Keep {
    /// The cost of the stored candidate.
    cost: f64,
    /// Where along the current line search the stored candidate lies, if it came from this line search.
    t: Option<f64>,
}

impl Keep {
    fn new(cost: f64) -> Self {
        Self { cost, t: None }
    }

    /// A new line search starts: the stored candidate is no longer one to compare values with.
    fn new_line(&mut self) {
        self.t = None;
    }

    /// Whether the candidate at `t` with `cost` replaces the stored one (and then records it).
    fn replaces(&mut self, cost: f64, t: f64) -> bool {
        let wins = match cost.total_cmp(&self.cost) {
            Ordering::Less => true,
            Ordering::Equal => self.t.is_some_and(|held| t < held),
            Ordering::Greater => false,
        };
        if wins {
            (self.cost, self.t) = (cost, Some(t));
        }
        wins
    }
}

impl Search<'_, '_> {
    /// Scores `candidate` found at `t` along the line search, keeping it per [`Keep`]. `None` once the budget is spent.
    ///
    /// # Errors
    /// [`FitError::Stopped`] when the watch says stop; otherwise as [`Objective::evaluate`].
    fn probe(&mut self, t: f64, candidate: FitParams) -> Result<Option<f64>, FitError> {
        if self.evaluations >= self.budget {
            return Ok(None);
        }
        if !(self.watch)(self.evaluations) {
            return Err(FitError::Stopped);
        }
        self.evaluations += 1;
        let score = self.objective.score(&candidate)?;
        let cost = score.cost;
        self.probes.push((t, cost));
        if self.keep.replaces(cost, t) {
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

    /// Golden-section search of `at(t)` for `t` in `(a, b)`, then one parabolic step ([`parabola_step`]); `false` when
    /// the budget ran out.
    fn line(&mut self, at: &dyn Fn(f64) -> FitParams, bracket: (f64, f64)) -> Result<bool, FitError> {
        self.keep.new_line();
        self.probes.clear();
        if !golden(bracket, &mut |t| self.probe(t, at(t)))? {
            return Ok(false);
        }
        match parabola_step(&mut self.probes) {
            Some(t) => Ok(self.probe(t, at(t))?.is_some()),
            None => Ok(true),
        }
    }
}

/// The vertex of the parabola through the lowest of `probes` (`(t, cost)`, sorted here) and its nearest probes on
/// each side, when it lies strictly between them and not on top of a probe already made; `None` otherwise, or when
/// the three are collinear. Golden steps alone close in on a minimum linearly; on a smooth cost this one step lands
/// close to it.
fn parabola_step(probes: &mut [(f64, f64)]) -> Option<f64> {
    probes.sort_by(|a, b| a.0.total_cmp(&b.0));
    let i = (0..probes.len()).min_by(|&i, &j| probes[i].1.total_cmp(&probes[j].1).then(i.cmp(&j)))?;
    if i == 0 || i + 1 >= probes.len() {
        return None;
    }
    let ((x0, f0), (x1, f1), (x2, f2)) = (probes[i - 1], probes[i], probes[i + 1]);
    let den = (x1 - x0) * (f1 - f2) - (x1 - x2) * (f1 - f0);
    if den.abs() <= f64::EPSILON * (f0.abs() + f1.abs() + f2.abs()) * (x2 - x0) {
        return None;
    }
    let v = x1 - 0.5 * ((x1 - x0).powi(2) * (f1 - f2) - (x1 - x2).powi(2) * (f1 - f0)) / den;
    let gap = (x2 - x0) * PARABOLA_GAP;
    (v > x0 + gap && v < x2 - gap && (v - x1).abs() >= gap).then_some(v)
}

/// Golden-section search over `(a, b)` of `cost`, which gives `None` once the budget is spent (then this is `false`).
///
/// Equal costs never end the search and never pick a side. Two interior probes that cost the same prove nothing at low
/// resolution, where distinct values can rasterise alike, and the samples can straddle a dip between them. On a tie
/// the ends and the midpoints of the three intervals are probed too (seven samples, six intervals), and the golden
/// search runs in every interval that has a lowest sample at either end: when all seven tie, in all six, lowest first.
/// [`Search::probe`] stores a strictly cheaper result, and of equal results the one at the lowest value ([`Keep`]), so
/// the outcome is deterministic. Inside one interval a tie keeps the lower half. The tolerance is [`BRACKET_TOLERANCE`] of the whole
/// bracket, so an interval of a sixth is probed at its two interior points and no further. The search stops at the
/// tolerance or the budget.
fn golden((a, b): (f64, f64), cost: &mut dyn FnMut(f64) -> Result<Option<f64>, FitError>) -> Result<bool, FitError> {
    let (c, d) = (b - INV_PHI * (b - a), a + INV_PHI * (b - a));
    let Some(fc) = cost(c)? else { return Ok(false) };
    let Some(fd) = cost(d)? else { return Ok(false) };
    if (fc - fd).abs() >= FLAT {
        return steps((a, b), (c, d), (fc, fd), (b - a) * BRACKET_TOLERANCE, cost);
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
    let lowest = samples.iter().fold(f64::MAX, |m, (_, f)| m.min(*f));
    for pair in samples.windows(2) {
        if pair.iter().all(|(_, f)| *f > lowest + FLAT) {
            continue;
        }
        let (lo, hi) = (pair[0].0, pair[1].0);
        let (c, d) = (hi - INV_PHI * (hi - lo), lo + INV_PHI * (hi - lo));
        let Some(fc) = cost(c)? else { return Ok(false) };
        let Some(fd) = cost(d)? else { return Ok(false) };
        if !steps((lo, hi), (c, d), (fc, fd), (b - a) * BRACKET_TOLERANCE, cost)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The golden-section steps from the bracket `(a, b)` whose interior probes `(c, d)` cost `(fc, fd)`, until the
/// bracket is no wider than `stop`. A tie keeps the lower half.
fn steps(
    (mut a, mut b): (f64, f64),
    (mut c, mut d): (f64, f64),
    (mut fc, mut fd): (f64, f64),
    stop: f64,
    cost: &mut dyn FnMut(f64) -> Result<Option<f64>, FitError>,
) -> Result<bool, FitError> {
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

    /// All seven samples tie and the only improvement lies above 0.236, in the second interval: it must be found.
    #[test]
    fn a_dip_above_the_lower_interval_is_found_when_all_samples_tie() {
        let mut lowest = f64::MAX;
        let ok = golden((0.0, 1.0), &mut |t| {
            let f = if (t - 0.3).abs() < 0.01 { 0.0 } else { 1.0 };
            lowest = lowest.min(f);
            Ok(Some(f))
        });
        assert_eq!(ok, Ok(true));
        assert!(lowest == 0.0, "the dip near 0.3 was never probed");
    }

    /// Of equal results in different intervals the lowest value wins, as the first strictly better one is kept.
    #[test]
    fn equal_dips_are_visited_lowest_first() {
        let mut firsts = Vec::new();
        let _ = golden((0.0, 1.0), &mut |t| {
            let low = (t - 0.3).abs() < 0.01 || (t - 0.7).abs() < 0.01;
            if low {
                firsts.push(t);
            }
            Ok(Some(if low { 0.0 } else { 1.0 }))
        });
        assert!(firsts.first().is_some_and(|t| *t < 0.5), "{firsts:?}");
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

    /// The stored candidate of a staircase search is the lowest tied value, though the interior probe at 0.382 is the
    /// first to reach the tied cost and the end at 0.0 is probed later.
    #[test]
    fn the_stored_candidate_of_a_tie_is_the_lowest_value() {
        // Cost 0.5 up to 0.9 beats the starting 1.0; the tied probes then differ only in value.
        let mut keep = Keep::new(1.0);
        let mut stored = None;
        let ok = golden((0.0, 1.0), &mut |t| {
            let cost = if t <= 0.9 { 0.5 } else { 1.0 };
            if keep.replaces(cost, t) {
                stored = Some(t);
            }
            Ok(Some(cost))
        });
        assert_eq!(ok, Ok(true));
        assert_eq!(stored, Some(0.0), "the tied probes at 0.382 and 0.618 were kept over the end at 0");
    }

    #[test]
    fn a_tie_with_an_earlier_line_search_keeps_the_earlier_value() {
        let mut keep = Keep::new(0.5);
        keep.new_line();
        assert!(!keep.replaces(0.5, 0.1));
        assert!(keep.replaces(0.4, 0.8));
        assert!(!keep.replaces(0.4, 0.9));
        assert!(keep.replaces(0.4, 0.2));
        keep.new_line();
        assert!(!keep.replaces(0.4, 0.0));
    }

    #[test]
    fn the_parabolic_step_lands_on_a_parabolas_vertex() {
        let f = |t: f64| (t - 0.3) * (t - 0.3) + 2.0;
        let mut probes: Vec<(f64, f64)> = [1.0, 0.0, 0.5, 0.2].iter().map(|&t| (t, f(t))).collect();
        let v = parabola_step(&mut probes).unwrap();
        assert!((v - 0.3).abs() < 1e-12, "{v}");
    }

    #[test]
    fn the_parabolic_step_is_skipped_when_it_would_tell_nothing() {
        // The lowest probe at an end of the bracket: no neighbour on one side.
        let mut edge = vec![(0.0, 1.0), (0.5, 2.0), (1.0, 3.0)];
        assert_eq!(parabola_step(&mut edge), None);
        // Three equal costs: no curvature.
        let mut level = vec![(0.0, 1.0), (0.5, 1.0), (1.0, 1.0)];
        assert_eq!(parabola_step(&mut level), None);
        // A vertex on top of the lowest probe is not worth an evaluation.
        let f = |t: f64| (t - 0.5) * (t - 0.5);
        let mut centred: Vec<(f64, f64)> = [0.0, 0.5, 1.0].iter().map(|&t| (t, f(t))).collect();
        assert_eq!(parabola_step(&mut centred), None);
    }

    /// A cost in which `leg_length` shows only once `musculature` is past 0.3: musculature is searched last, so the
    /// legs are flat through the whole first sweep and show from the second. Every other parameter has a gentle bowl
    /// at its default, so their line searches cost what a visible parameter's does. It records the evaluation at which
    /// the legs first move off their start while they show.
    #[derive(Default)]
    struct Coupled {
        evaluations: std::cell::Cell<usize>,
        legs_moved: std::cell::Cell<Option<usize>>,
    }

    impl Cost for Coupled {
        fn score(&self, p: &FitParams) -> Result<Score, FitError> {
            let n = self.evaluations.get() + 1;
            self.evaluations.set(n);
            let (musculature, legs) = (Param::Musculature.get(p), Param::LegLength.get(p));
            if musculature > 0.3 && (legs - 1.0).abs() > 0.1 && self.legs_moved.get().is_none() {
                self.legs_moved.set(Some(n));
            }
            // Once they show, the legs lower the cost, most at 1.4, so moving musculature past 0.3 pays even with
            // the legs still at 1.
            let shown = if musculature > 0.3 { 0.2 - (legs - 1.4).powi(2) } else { 0.0 };
            let rest = FitParams::default();
            let bowls: f64 = PARAM_ORDER
                .iter()
                .filter(|q| !matches!(q, Param::LegLength | Param::Musculature))
                .map(|q| 0.01 * (q.get(p) - q.get(&rest)).powi(2))
                .sum();
            let cost = (musculature - 0.5).powi(2) - shown + bowls;
            Ok(Score { cost, iou: 1.0 - cost, per_view: vec![1.0 - cost] })
        }

        fn view_list(&self) -> Vec<FitView> {
            vec![FitView::Front]
        }
    }

    /// Whether a parameter shows can change as the others move, so every sweep searches every parameter: the legs here
    /// move in the second sweep, not only once a stalled descent widens its window again.
    #[test]
    fn a_parameter_that_shows_only_once_another_moves_is_searched_in_the_next_sweep() {
        let cost = Coupled::default();
        let r = descend(&cost, FitParams::default(), &PARAM_ORDER, MAX_EVALUATIONS, &mut |_| true).unwrap();
        let (legs, musculature) = (Param::LegLength.get(&r.params), Param::Musculature.get(&r.params));
        assert!((musculature - 0.5).abs() < 0.01, "musculature {musculature}");
        assert!((legs - 1.4).abs() < 0.01, "legs {legs}");
        // A sweep is a line search per parameter (5 to 8 evaluations each) and a pattern move; the legs come first in
        // the order, so the second sweep moves them within two sweeps' evaluations.
        let moved = cost.legs_moved.get().unwrap();
        let two_sweeps = 2 * (PARAM_ORDER.len() + 1) * 8;
        eprintln!("the legs moved at evaluation {moved} of {}", r.evaluations);
        assert!(moved <= two_sweeps, "the legs first moved at evaluation {moved}, after {two_sweeps}");
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
