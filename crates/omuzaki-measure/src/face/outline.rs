//! The outline as a radial profile (design D5, D11 and 4.2): 85 rays from the eye midpoint, every 2.5° from 15° above
//! the eye line on the character's left, down through the chin, to 15° above it on the right. Each ray stops where it
//! leaves the face region and measures the outline stroke there; the radius is the stroke's centre, in IOD.
//!
//! Each sample gets a state: `excluded` (in a sector the caller left out), `cut` (the ray left the region of interest),
//! `leak` (the fill escaped through a gap: any leak refuses the measurement), `occluded` (a lock of hair in front of one
//! cheek) or `ok`.

use serde::Serialize;

use super::frame::{FaceFrame, Roi};
use super::segment::{RAY_START, STEP, Segmentation, delta_e, median};

/// The number of outline samples.
pub const ANGLES: usize = 85;

/// The first sample's angle, degrees.
pub const FROM_DEG: f64 = -15.0;

/// The angle between samples, degrees.
pub const STEP_DEG: f64 = 2.5;

/// The sample at 90°, straight down the midline: the chin.
pub const CHIN: usize = 42;

/// A stroke is a run at least this much darker than the lit skin, `L*`.
pub const STROKE_DARK: f64 = 15.0;

/// A stroke's outer edge counts when the lightness beyond it rises at least this much above the stroke's minimum, `L*`.
pub const STROKE_RISE: f64 = 15.0;

/// A dark run wider than this, IOD, is not a line (dark hair against the face).
pub const MAX_LINE: f64 = 0.10;

/// Neighbouring radii that differ by more than this (IOD) are a jump; samples between an upward and a downward jump
/// are a leak.
pub const JUMP: f64 = 0.08;

/// How many samples beside an unpartnered jump decide whether it is a leak or a lock of hair (20°).
const BESIDE_JUMP: usize = 8;

/// A hair sample this much (IOD) inside its mirror is a lock of hair in front of the cheek.
pub const OCCLUSION: f64 = 0.04;

/// A beyond colour within this CIE76 distance of the hair colour is hair.
pub const HAIR_DELTA_E: f64 = 20.0;

/// Asymmetry is measured over the mirror pairs `k` from 18 to 41 (30° to 87.5°).
pub const ASYMMETRY_PAIRS: std::ops::RangeInclusive<usize> = 18..=41;

/// Fewer pairs than this with both rays `ok` leave the yaw unchecked.
pub const MIN_ASYMMETRY_PAIRS: usize = 8;

/// The angle of sample `k`, degrees.
#[must_use]
pub fn angle(k: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)] // k < 85
    let k = k as f64;
    FROM_DEG + STEP_DEG * k
}

/// The mirror of sample `k` about the midline (`φ` to `180° - φ`).
#[must_use]
pub const fn mirror(k: usize) -> usize {
    ANGLES - 1 - k
}

/// A sample's weight in the outline term and the coverage: 2 in the jaw and chin sector (30° to 150°), 1 elsewhere.
#[must_use]
pub fn weight(k: usize) -> f64 {
    if (18..=66).contains(&k) { 2.0 } else { 1.0 }
}

/// A sample's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SampleState {
    /// Measured and usable.
    Ok,
    /// A lock of hair in front of the cheek: not compared.
    Occluded,
    /// Inside a sector the caller left out.
    Excluded,
    /// The ray left the region of interest before it left the face.
    Cut,
    /// The fill escaped through a gap here; a measurement with a leak is refused.
    Leak,
}

/// What lies just beyond the outline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Beyond {
    /// Transparent, or the background colour.
    Background,
    /// Skin-coloured: the neck, an ear.
    Skin,
    /// The hair colour.
    Hair,
    /// Anything else (clothes).
    Other,
}

/// A range of outline angles to leave out, degrees in the face frame, `from <= to`.
#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, Serialize, schemars::JsonSchema)]
pub struct Sector {
    /// The first angle left out, degrees.
    pub from_deg: f64,
    /// The last angle left out, degrees.
    pub to_deg: f64,
}

impl Sector {
    /// Whether `phi` lies in the sector.
    #[must_use]
    pub fn contains(&self, phi: f64) -> bool {
        (self.from_deg..=self.to_deg).contains(&phi)
    }
}

/// One ray's raw reading, ROI pixels from the origin.
#[derive(Clone, Copy, Debug)]
struct Ray {
    /// Where the stroke's inner edge is (or the region's edge, when there is no stroke).
    inner: f64,
    /// The stroke's width where its outer edge shows.
    width: Option<f64>,
    /// Whether a stroke was found.
    stroke: bool,
}

/// The outline of one face.
#[derive(Clone, Debug, PartialEq)]
pub struct OutlineProfile {
    /// Each sample's radius, IOD; `None` where the ray was cut or excluded.
    pub r: [Option<f64>; ANGLES],
    /// Each sample's state.
    pub state: [SampleState; ANGLES],
    /// What lies beyond each sample, where measured.
    pub beyond: [Option<Beyond>; ANGLES],
    /// The median stroke width, IOD (0 when no stroke's outer edge showed).
    pub line_width: f64,
    /// The hair colour, CIELAB, when the region above the eyes had one.
    pub hair: Option<[f64; 3]>,
}

impl OutlineProfile {
    /// The radius of sample `k` when it is `ok`.
    #[must_use]
    pub fn ok(&self, k: usize) -> Option<f64> {
        (self.state[k] == SampleState::Ok).then_some(self.r[k]).flatten()
    }

    /// The weighted fraction of samples that are `ok`.
    #[must_use]
    pub fn coverage(&self) -> f64 {
        let total: f64 = (0..ANGLES).map(weight).sum();
        let ok: f64 = (0..ANGLES).filter(|&k| self.state[k] == SampleState::Ok).map(weight).sum();
        ok / total
    }

    /// The mean absolute difference between mirror samples over [`ASYMMETRY_PAIRS`], divided by those pairs' mean
    /// radius; `None` when fewer than [`MIN_ASYMMETRY_PAIRS`] pairs have both rays `ok`.
    #[must_use]
    pub fn asymmetry(&self) -> Option<f64> {
        let pairs: Vec<(f64, f64)> = ASYMMETRY_PAIRS.filter_map(|k| Some((self.ok(k)?, self.ok(mirror(k))?))).collect();
        if pairs.len() < MIN_ASYMMETRY_PAIRS {
            return None;
        }
        #[allow(clippy::cast_precision_loss)] // at most 24 pairs
        let n = pairs.len() as f64;
        let diff = pairs.iter().map(|(a, b)| (a - b).abs()).sum::<f64>() / n;
        let mean = pairs.iter().map(|(a, b)| (a + b) / 2.0).sum::<f64>() / n;
        Some(diff / mean)
    }

    /// The contiguous runs of samples in `state`, as `(from_deg, to_deg)`.
    #[must_use]
    pub fn sectors(&self, state: SampleState) -> Vec<(f64, f64)> {
        runs(|k| self.state[k] == state)
    }
}

/// The contiguous runs of samples where `pick` holds, as `(from_deg, to_deg)`.
pub(crate) fn runs(pick: impl Fn(usize) -> bool) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut start = None;
    for k in 0..=ANGLES {
        let on = k < ANGLES && pick(k);
        match (on, start) {
            (true, None) => start = Some(k),
            (false, Some(s)) => {
                out.push((angle(s), angle(k - 1)));
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// Reads one ray at `phi` from the face region's edge: the stroke's inner edge and width (ROI pixels), or `None` when
/// the ray leaves the region of interest first.
fn read_ray(roi: &Roi, seg: &Segmentation, frame: &FaceFrame, phi: f64) -> Option<Ray> {
    let skin_l = seg.skin[0];
    // March to the first sample outside the region.
    let mut t = RAY_START * frame.iod_px;
    let exit = loop {
        let i = roi.index(frame.along(phi, t))?;
        if !seg.filled[i] {
            break t;
        }
        t += STEP;
    };
    // Sample the lightness from 3 px before the exit to 0.10 IOD + 3 px after it.
    let start = exit - 3.0;
    let end = exit + MAX_LINE * frame.iod_px + 3.0;
    let mut samples: Vec<(f64, f64)> = Vec::new();
    let mut s = start;
    while s <= end {
        match roi.lightness(frame.along(phi, s)) {
            Some(l) => samples.push((s, l)),
            None => break,
        }
        s += STEP;
    }
    let no_stroke = Ray { inner: exit - STEP / 2.0, width: None, stroke: false };
    // 1. The stroke: the first sample darker than the skin by STROKE_DARK at or after exit - 1 px, down to its minimum.
    let Some(first) = samples.iter().position(|&(t, l)| t >= exit - 1.0 && l < skin_l - STROKE_DARK) else {
        return Some(no_stroke);
    };
    let mut lowest = first;
    while lowest + 1 < samples.len() && samples[lowest + 1].1 <= samples[lowest].1 {
        lowest += 1;
    }
    let l_min = samples[lowest].1;
    if skin_l - l_min <= STROKE_DARK {
        return Some(no_stroke);
    }
    // 2. The inner edge: back from the minimum, where L* crosses halfway between the skin and the minimum.
    let half = skin_l - (skin_l - l_min) / 2.0;
    let inner = match (0..lowest).rev().find(|&i| samples[i].1 >= half) {
        Some(i) => crossing(samples[i], samples[i + 1], half),
        None => samples[0].0,
    };
    // 3. The outer edge, when the lightness beyond the minimum rises far enough.
    let (top, l_max) = samples[lowest..]
        .iter()
        .enumerate()
        .fold((lowest, l_min), |best, (offset, &(_, l))| if l > best.1 { (lowest + offset, l) } else { best });
    let mut width = None;
    if l_max - l_min >= STROKE_RISE {
        let mid = f64::midpoint(l_min, l_max);
        if let Some(i) = (lowest + 1..=top).find(|&i| samples[i].1 >= mid) {
            let outer = crossing(samples[i - 1], samples[i], mid);
            if outer - inner <= MAX_LINE * frame.iod_px {
                width = Some(outer - inner);
            }
        }
    }
    Some(Ray { inner, width, stroke: true })
}

/// Where the lightness crosses `level` between two samples `(t, L)`, by linear interpolation.
fn crossing(a: (f64, f64), b: (f64, f64), level: f64) -> f64 {
    if (b.1 - a.1).abs() < f64::EPSILON {
        return a.0;
    }
    a.0 + (b.0 - a.0) * (level - a.1) / (b.1 - a.1)
}

/// The hair colour: the median colour just outside the face region along the rays from -150° to -30° (above the eyes).
fn hair_colour(roi: &Roi, seg: &Segmentation, frame: &FaceFrame) -> Option<[f64; 3]> {
    let mut colours: Vec<[f64; 3]> = Vec::new();
    for step in 0..=48_u32 {
        let phi = -150.0 + STEP_DEG * f64::from(step);
        let mut t = RAY_START * frame.iod_px;
        while let Some(i) = roi.index(frame.along(phi, t)) {
            if !seg.filled[i] {
                if let Some(j) = roi.index(frame.along(phi, t + 1.0))
                    && roi.opaque(j)
                {
                    colours.push(roi.lab_at(j));
                }
                break;
            }
            t += STEP;
        }
    }
    if colours.is_empty() {
        return None;
    }
    let mut out = [0.0; 3];
    for (c, slot) in out.iter_mut().enumerate() {
        let mut channel: Vec<f64> = colours.iter().map(|lab| lab[c]).collect();
        *slot = median(&mut channel)?;
    }
    Some(out)
}

/// What lies beyond the outline at the ROI point `p`.
fn beyond(roi: &Roi, seg: &Segmentation, hair: Option<[f64; 3]>, p: [f64; 2]) -> Option<Beyond> {
    let i = roi.index(p)?;
    let lab = roi.lab_at(i);
    Some(if !roi.opaque(i) || seg.background.is_some_and(|bg| delta_e(lab, bg) <= super::segment::BACKGROUND_DELTA_E) {
        Beyond::Background
    } else if seg.is_skin_colour(lab) {
        Beyond::Skin
    } else if hair.is_some_and(|h| delta_e(lab, h) <= HAIR_DELTA_E) {
        Beyond::Hair
    } else {
        Beyond::Other
    })
}

/// Measures the outline of the face region `seg` in `roi`, about `frame` (ROI pixels), leaving out `exclude`.
#[must_use]
pub fn outline(roi: &Roi, seg: &Segmentation, frame: &FaceFrame, exclude: &[Sector]) -> OutlineProfile {
    let d = frame.iod_px;
    let mut state = [SampleState::Ok; ANGLES];
    let mut rays: [Option<Ray>; ANGLES] = [None; ANGLES];
    for k in 0..ANGLES {
        let phi = angle(k);
        if exclude.iter().any(|s| s.contains(phi)) {
            state[k] = SampleState::Excluded;
            continue;
        }
        rays[k] = read_ray(roi, seg, frame, phi);
        if rays[k].is_none() {
            state[k] = SampleState::Cut;
        }
    }
    let mut widths: Vec<f64> = rays.iter().flatten().filter_map(|ray| ray.width).collect();
    let line_px = median(&mut widths).unwrap_or(0.0);
    let mut r = [None; ANGLES];
    for k in 0..ANGLES {
        if let Some(ray) = rays[k] {
            let centre = if ray.stroke { ray.inner + line_px / 2.0 } else { ray.inner };
            r[k] = Some(centre / d);
        }
    }
    mark_leaks(&r, &mut state);
    let hair = hair_colour(roi, seg, frame);
    let mut beyond_of = [None; ANGLES];
    for k in 0..ANGLES {
        if let Some(ray) = rays[k] {
            let reach = ray.inner + line_px + 0.03 * d + 1.0;
            beyond_of[k] = beyond(roi, seg, hair, frame.along(angle(k), reach));
        }
    }
    // Occlusion: hair beyond a sample that is well inside its mirror, which is itself usable.
    let usable = state;
    for k in 0..ANGLES {
        let m = mirror(k);
        if usable[k] == SampleState::Ok
            && usable[m] == SampleState::Ok
            && beyond_of[k] == Some(Beyond::Hair)
            && let (Some(here), Some(there)) = (r[k], r[m])
            && here < there - OCCLUSION
        {
            state[k] = SampleState::Occluded;
        }
    }
    OutlineProfile { r, state, beyond: beyond_of, line_width: line_px / d, hair }
}

/// Marks the leaks: between two jumps of more than [`JUMP`] between neighbouring measured samples, the first upward.
///
/// A jump with no partner (a downward one with no upward one before it, or an upward one with nothing after it) leaks at
/// the profile's start or end unless the short side of the jump is shorter than its mirror by more than half a jump: a
/// fill that escapes makes one side longer than the face, while a lock of hair over one cheek that reaches the profile's
/// end makes that side shorter, which is an occlusion, not a leak.
fn mark_leaks(r: &[Option<f64>; ANGLES], state: &mut [SampleState; ANGLES]) {
    let measured: Vec<usize> = (0..ANGLES).filter(|&k| state[k] == SampleState::Ok && r[k].is_some()).collect();
    let at = |i: usize| r[measured[i]].unwrap_or(0.0);
    // Jumps between measured[i] and measured[i + 1]: (i, upward).
    let jumps: Vec<(usize, bool)> = (0..measured.len().saturating_sub(1))
        .filter_map(|i| {
            let step = at(i + 1) - at(i);
            (step.abs() > JUMP).then_some((i, step > 0.0))
        })
        .collect();
    // Whether the measured samples from position `from` to `to` are shorter than their mirrors (outside that run).
    // Only the samples next to the jump are judged, so a lock that thins out gradually still reads as a lock.
    let shorter_than_mirror = |from: usize, to: usize, next_to_end: bool| {
        let (from, to) = if next_to_end {
            (from.max((to + 1).saturating_sub(BESIDE_JUMP)), to)
        } else {
            (from, to.min(from + BESIDE_JUMP - 1))
        };
        let run = &measured[from..=to];
        let mut excess: Vec<f64> = run
            .iter()
            .filter(|&&k| !run.contains(&mirror(k)) && measured.contains(&mirror(k)))
            .filter_map(|&k| Some(r[k]? - r[mirror(k)]?))
            .collect();
        median(&mut excess).is_some_and(|m| m < -JUMP / 2.0)
    };
    let last = measured.len().saturating_sub(1);
    let mut open: Option<(usize, usize)> = None; // (the jump's index in `jumps`, the position after it)
    let mut first_down_seen = false;
    let mut leaks = Vec::new();
    for (j, &(i, up)) in jumps.iter().enumerate() {
        if up {
            if open.is_none() {
                open = Some((j, i + 1));
            }
            continue;
        }
        match open.take() {
            Some((_, from)) => leaks.push((from, i)),
            None if !first_down_seen => {
                let short_end = jumps.get(j + 1).map_or(last, |&(next, _)| next);
                if !shorter_than_mirror(i + 1, short_end, false) {
                    leaks.push((0, i));
                }
            }
            None => {}
        }
        first_down_seen = true;
    }
    if let Some((j, from)) = open {
        let short_start = j.checked_sub(1).map_or(0, |prev| jumps[prev].0 + 1);
        if !shorter_than_mirror(short_start, from - 1, true) {
            leaks.push((from, last));
        }
    }
    for (from, to) in leaks {
        mark(state, &measured, measured[from], measured[to]);
    }
}

/// Marks the measured samples from `from` to `to` (inclusive) as leaks.
fn mark(state: &mut [SampleState; ANGLES], measured: &[usize], from: usize, to: usize) {
    for &k in measured.iter().filter(|&&k| (from..=to).contains(&k)) {
        state[k] = SampleState::Leak;
    }
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss)] // sample indices
mod tests {
    use super::*;

    #[test]
    fn the_angle_table_is_symmetric_about_the_midline() {
        assert_eq!(angle(0), -15.0);
        assert_eq!(angle(CHIN), 90.0);
        assert_eq!(angle(84), 195.0);
        for k in 0..ANGLES {
            assert!((angle(k) + angle(mirror(k)) - 180.0).abs() < 1e-12);
        }
        assert_eq!((angle(18), angle(41)), (30.0, 87.5));
        assert_eq!(weight(17) + weight(18) + weight(66) + weight(67), 6.0);
    }

    fn profile(r: &[f64; ANGLES]) -> OutlineProfile {
        OutlineProfile {
            r: r.map(Some),
            state: [SampleState::Ok; ANGLES],
            beyond: [Some(Beyond::Background); ANGLES],
            line_width: 0.01,
            hair: None,
        }
    }

    #[test]
    fn a_rise_and_a_fall_between_neighbours_is_a_leak() {
        let mut r = [0.7; ANGLES];
        for v in &mut r[50..55] {
            *v = 1.2;
        }
        let mut state = [SampleState::Ok; ANGLES];
        mark_leaks(&r.map(Some), &mut state);
        assert_eq!(
            (0..ANGLES).filter(|&k| state[k] == SampleState::Leak).collect::<Vec<_>>(),
            (50..55).collect::<Vec<_>>()
        );
        // A smooth face has none.
        let smooth: [f64; ANGLES] = std::array::from_fn(|k| 0.7 + 0.001 * k as f64);
        let mut clean = [SampleState::Ok; ANGLES];
        mark_leaks(&smooth.map(Some), &mut clean);
        assert!(clean.iter().all(|s| *s == SampleState::Ok));
    }

    #[test]
    fn asymmetry_needs_eight_pairs_and_ignores_which_side_is_larger() {
        let mut p = profile(&[0.7; ANGLES]);
        for k in ASYMMETRY_PAIRS {
            p.r[k] = Some(0.75);
        }
        let a = p.asymmetry().unwrap();
        let mirrored = {
            let mut m = profile(&[0.7; ANGLES]);
            for k in ASYMMETRY_PAIRS {
                m.r[mirror(k)] = Some(0.75);
            }
            m
        };
        assert!((a - mirrored.asymmetry().unwrap()).abs() < 1e-12 && a > 0.06);
        // Exactly 7 usable pairs: unchecked; 8: a number.
        for (usable, expect) in [(0, false), (7, false), (8, true)] {
            let mut q = profile(&[0.7; ANGLES]);
            for (n, k) in ASYMMETRY_PAIRS.enumerate() {
                if n >= usable {
                    q.state[k] = SampleState::Excluded;
                }
            }
            assert_eq!(q.asymmetry().is_some(), expect, "{usable} pairs");
        }
    }

    #[test]
    fn coverage_weighs_the_jaw_twice_and_runs_name_sectors() {
        let mut p = profile(&[0.7; ANGLES]);
        for k in 0..18 {
            p.state[k] = SampleState::Excluded;
        }
        let total: f64 = (0..ANGLES).map(weight).sum();
        assert!((p.coverage() - (total - 18.0) / total).abs() < 1e-12);
        assert_eq!(p.sectors(SampleState::Excluded), vec![(-15.0, 27.5)]);
    }
}
