//! Lining a render up with its reference: the similarity transform that carries our pixels onto the reference's, from
//! matching points (least squares, Umeyama 1991), from the two silhouettes (centroid and area), refined by searching the
//! transform that minimises the L* and coverage difference, coarse to fine.
//!
//! The transform is a diagnosis, never a fix: the score that decides parity is taken on the render as it was rendered.
//! It answers "how far is the camera off", so the next render can move the camera instead of the pixels.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::raster::{LabImage, Plane, Similarity};
use super::regions::Points;

/// How to find the transform.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AlignMethod {
    /// From the points both images name (at least two), else the silhouettes, then refined on the pixels.
    #[default]
    Auto,
    /// From the points both images name (at least two) only.
    Points,
    /// From the silhouettes' centroids and areas only.
    Silhouette,
    /// Refined on the pixels from no change.
    Pixels,
}

/// Where the transform came from and how much it explains.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct Alignment {
    /// The method that found it: `points`, `silhouette`, `pixels` or `none`.
    pub method: &'static str,
    /// Carries our pixels onto the reference's.
    pub transform: Similarity,
    /// The mean absolute L* difference (with coverage) before the transform.
    pub residual_before: f64,
    /// The same after it.
    pub residual_after: f64,
    /// What the camera has to change, as seen in the frame.
    pub camera: CameraHint,
}

/// The camera change the transform asks for, as the subject's motion in the frame: scale and turn about the frame's
/// centre, then shift.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct CameraHint {
    /// How far the subject has to move, in pixels (right, down).
    pub subject_shift_px: [f64; 2],
    /// The same as a share of the frame's width and height.
    pub subject_shift_frame: [f64; 2],
    /// How much larger the subject has to be (above 1: zoom in, or move the camera closer).
    pub subject_scale: f64,
    /// How far the subject has to turn, degrees clockwise on screen (a camera roll the other way).
    pub subject_roll_deg: f64,
    /// The same in words, for a person.
    pub summary: String,
}

impl CameraHint {
    /// The hint for `t` in a `w x h` frame.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // frame sides
    pub fn of(t: &Similarity, w: usize, h: usize) -> Self {
        let c = [w as f64 / 2.0, h as f64 / 2.0];
        let moved = t.apply(c);
        let d = [moved[0] - c[0], moved[1] - c[1]];
        let round = |v: f64, k: f64| (v * k).round() / k;
        let mut words = Vec::new();
        if d[0].abs() >= 0.5 {
            words.push(format!("move the subject {:.1} px {}", d[0].abs(), if d[0] > 0.0 { "right" } else { "left" }));
        }
        if d[1].abs() >= 0.5 {
            words.push(format!("{:.1} px {}", d[1].abs(), if d[1] > 0.0 { "down" } else { "up" }));
        }
        if (t.scale - 1.0).abs() >= 0.002 {
            words.push(format!(
                "make it {:.1}% {}",
                (t.scale - 1.0).abs() * 100.0,
                if t.scale > 1.0 { "larger" } else { "smaller" }
            ));
        }
        if t.angle_deg.abs() >= 0.05 {
            words.push(format!(
                "turn it {:.2} degrees {}",
                t.angle_deg.abs(),
                if t.angle_deg > 0.0 { "clockwise" } else { "anticlockwise" }
            ));
        }
        let summary = if words.is_empty() { "the framing matches".to_owned() } else { words.join(", ") };
        Self {
            subject_shift_px: [round(d[0], 100.0), round(d[1], 100.0)],
            subject_shift_frame: [round(d[0] / w as f64, 1e5), round(d[1] / h as f64, 1e5)],
            subject_scale: round(t.scale, 1e5),
            subject_roll_deg: round(t.angle_deg, 1e3),
            summary,
        }
    }
}

/// The least-squares similarity that carries `ours` onto `reference` over the names both hold; `None` with fewer than
/// two shared names or with the shared points on one spot.
#[must_use]
#[allow(clippy::cast_precision_loss)] // a handful of points
pub fn from_points(ours: &Points, reference: &Points) -> Option<Similarity> {
    let pairs: Vec<([f64; 2], [f64; 2])> =
        ours.iter().filter_map(|(name, a)| reference.get(name).map(|b| (*a, *b))).collect();
    if pairs.len() < 2 {
        return None;
    }
    let n = pairs.len() as f64;
    let ma = [pairs.iter().map(|p| p.0[0]).sum::<f64>() / n, pairs.iter().map(|p| p.0[1]).sum::<f64>() / n];
    let mb = [pairs.iter().map(|p| p.1[0]).sum::<f64>() / n, pairs.iter().map(|p| p.1[1]).sum::<f64>() / n];
    let (mut sxx, mut sxy, mut var) = (0.0, 0.0, 0.0);
    for (a, b) in &pairs {
        let (ax, ay, bx, by) = (a[0] - ma[0], a[1] - ma[1], b[0] - mb[0], b[1] - mb[1]);
        sxx += ax * bx + ay * by;
        sxy += ax * by - ay * bx;
        var += ax * ax + ay * ay;
    }
    if var < 1e-9 {
        return None;
    }
    let angle = sxy.atan2(sxx);
    let scale = sxx.hypot(sxy) / var;
    let turn = Similarity { scale, angle_deg: angle.to_degrees(), shift: [0.0, 0.0] };
    let moved = turn.apply(ma);
    Some(Similarity { shift: [mb[0] - moved[0], mb[1] - moved[1]], ..turn })
}

/// The shift and scale that put our silhouette's centroid on the reference's and match their areas; `None` when either
/// is empty or touches the frame's edge (a silhouette cut by the frame has a moved centroid and a wrong area).
#[must_use]
#[allow(clippy::cast_precision_loss)] // pixel counts
pub fn from_silhouettes(ours: &[bool], reference: &[bool], w: usize) -> Option<Similarity> {
    let moments = |m: &[bool]| {
        let (mut n, mut sx, mut sy) = (0.0f64, 0.0f64, 0.0f64);
        for (i, _) in m.iter().enumerate().filter(|(_, s)| **s) {
            n += 1.0;
            sx += (i % w) as f64 + 0.5;
            sy += (i / w) as f64 + 0.5;
        }
        (n > 0.0).then(|| (n, [sx / n, sy / n]))
    };
    let h = ours.len() / w.max(1);
    let touches =
        |m: &[bool]| (0..w).any(|x| m[x] || m[(h - 1) * w + x]) || (0..h).any(|y| m[y * w] || m[y * w + w - 1]);
    if touches(ours) || touches(reference) {
        return None;
    }
    let (na, ca) = moments(ours)?;
    let (nb, cb) = moments(reference)?;
    let scale = (nb / na).sqrt();
    Some(Similarity { scale, angle_deg: 0.0, shift: [cb[0] - scale * ca[0], cb[1] - scale * ca[1]] })
}

/// One pyramid level of an image for the search: L* and alpha shrunk by `factor`.
struct Level {
    factor: usize,
    l: Plane,
    alpha: Plane,
}

impl Level {
    fn of(image: &LabImage, factor: usize) -> Self {
        Self { factor, l: image.l.shrink(factor), alpha: image.alpha.shrink(factor) }
    }
}

/// The mean absolute L* difference plus the coverage difference (scaled to L*'s range), of ours carried by `t`,
/// on the reference's grid at one level. Reference pixels that `t` maps outside our frame are left out (nothing is
/// known there), and a transform that leaves out more than half of the frame costs infinity.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // pixel grids
fn cost(ours: &Level, reference: &Level, t: &Similarity) -> f64 {
    let inv = t.inverse();
    let f = reference.factor as f64;
    let (w, h) = (reference.l.w, reference.l.h);
    let (mut sum, mut n) = (0.0f64, 0usize);
    for y in 0..h {
        for x in 0..w {
            let q = inv.apply([(x as f64 + 0.5) * f, (y as f64 + 0.5) * f]);
            let (qx, qy) = ((q[0] / ours.factor as f64) as f32, (q[1] / ours.factor as f64) as f32);
            if qx < 0.0 || qy < 0.0 || qx > ours.l.w as f32 || qy > ours.l.h as f32 {
                continue;
            }
            let i = y * w + x;
            let (l, a) = (ours.l.sample(qx, qy), ours.alpha.sample(qx, qy));
            sum += f64::from((l - reference.l.v[i]).abs()) + 100.0 * f64::from((a - reference.alpha.v[i]).abs());
            n += 1;
        }
    }
    if n * 2 < w * h { f64::INFINITY } else { sum / n as f64 }
}

/// The transform near `start` with the least [`cost`], found coarse to fine by coordinate descent.
#[must_use]
#[allow(clippy::cast_precision_loss)] // level factors
pub fn refine(ours: &LabImage, reference: &LabImage, start: Similarity) -> Similarity {
    let longest = reference.w.max(reference.h);
    let mut factors = Vec::new();
    let mut f = 1;
    while longest / f > 192 {
        f *= 2;
    }
    while f >= 1 {
        factors.push(f);
        f /= 2;
    }
    let mut t = start;
    let mut first = true;
    for factor in factors {
        // Below full size only the coarse levels carry the long moves; the full level only polishes.
        let (lo, lr) = (Level::of(ours, factor), Level::of(reference, factor));
        let mut best = cost(&lo, &lr, &t);
        if first {
            // The coarsest level tries every shift within a sixth of the frame, so a start far off is not a trap.
            first = false;
            let reach = (lr.l.w.max(lr.l.h) / 6).max(2) as isize;
            let base = t;
            for dy in -reach..=reach {
                for dx in -reach..=reach {
                    let mut c = base;
                    c.shift[0] += dx as f64 * factor as f64;
                    c.shift[1] += dy as f64 * factor as f64;
                    let v = cost(&lo, &lr, &c);
                    if v + 1e-9 < best {
                        best = v;
                        t = c;
                    }
                }
            }
        }
        let mut step = [factor as f64, factor as f64, 0.01, 0.5];
        for _ in 0..60 {
            let mut moved = false;
            for k in 0..4 {
                for sign in [1.0, -1.0] {
                    let mut c = t;
                    match k {
                        0 => c.shift[0] += sign * step[0],
                        1 => c.shift[1] += sign * step[1],
                        2 => {
                            // Scale about the reference's centre, so a scale step does not also shift the subject.
                            let centre = [reference.w as f64 / 2.0, reference.h as f64 / 2.0];
                            let k = 1.0 + sign * step[2];
                            c.scale *= k;
                            c.shift =
                                [centre[0] + (c.shift[0] - centre[0]) * k, centre[1] + (c.shift[1] - centre[1]) * k];
                        }
                        _ => {
                            let centre = [reference.w as f64 / 2.0, reference.h as f64 / 2.0];
                            let turn = Similarity { scale: 1.0, angle_deg: sign * step[3], shift: [0.0, 0.0] };
                            let about = turn.apply(centre);
                            let turned = turn.apply(c.shift);
                            c.angle_deg += sign * step[3];
                            c.shift = [turned[0] + centre[0] - about[0], turned[1] + centre[1] - about[1]];
                        }
                    }
                    let v = cost(&lo, &lr, &c);
                    if v + 1e-9 < best {
                        best = v;
                        t = c;
                        moved = true;
                    }
                }
            }
            if !moved {
                for s in &mut step {
                    *s /= 2.0;
                }
                if step[0] < 0.1 * factor as f64 {
                    break;
                }
            }
        }
    }
    t
}

/// The residual of a transform at full size (the [`cost`] the search minimises).
#[must_use]
pub fn residual(ours: &LabImage, reference: &LabImage, t: &Similarity) -> f64 {
    let longest = reference.w.max(reference.h);
    let mut f = 1;
    while longest / f > 512 {
        f *= 2;
    }
    cost(&Level::of(ours, f), &Level::of(reference, f), t)
}

/// Lines ours up with the reference by `method`.
#[must_use]
pub fn align(
    ours: &LabImage,
    reference: &LabImage,
    ours_silhouette: &[bool],
    reference_silhouette: &[bool],
    ours_points: Option<&Points>,
    reference_points: Option<&Points>,
    method: AlignMethod,
) -> Alignment {
    let points = ours_points.zip(reference_points).and_then(|(a, b)| from_points(a, b));
    let silhouette = || from_silhouettes(ours_silhouette, reference_silhouette, reference.w);
    let (name, transform) = match method {
        AlignMethod::Points => points.map_or(("none", Similarity::IDENTITY), |t| ("points", t)),
        AlignMethod::Silhouette => silhouette().map_or(("none", Similarity::IDENTITY), |t| ("silhouette", t)),
        AlignMethod::Pixels => ("pixels", refine(ours, reference, Similarity::IDENTITY)),
        AlignMethod::Auto => {
            let (name, start) = points
                .map(|t| ("points", t))
                .or_else(|| silhouette().map(|t| ("silhouette", t)))
                .unwrap_or(("pixels", Similarity::IDENTITY));
            (name, refine(ours, reference, start))
        }
    };
    Alignment {
        method: name,
        transform,
        residual_before: residual(ours, reference, &Similarity::IDENTITY),
        residual_after: residual(ours, reference, &transform),
        camera: CameraHint::of(&transform, reference.w, reference.h),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_recover_a_known_similarity() {
        let truth = Similarity { scale: 1.2, angle_deg: 7.0, shift: [14.0, -3.0] };
        let ours: Points = [("a", [10.0, 10.0]), ("b", [50.0, 12.0]), ("c", [30.0, 60.0])]
            .into_iter()
            .map(|(n, p)| (n.to_owned(), p))
            .collect();
        let reference: Points = ours.iter().map(|(n, p)| (n.clone(), truth.apply(*p))).collect();
        let t = from_points(&ours, &reference).unwrap();
        assert!((t.scale - 1.2).abs() < 1e-9 && (t.angle_deg - 7.0).abs() < 1e-9, "{t:?}");
        assert!((t.shift[0] - 14.0).abs() < 1e-9 && (t.shift[1] + 3.0).abs() < 1e-9, "{t:?}");
    }

    #[test]
    fn the_hint_reads_the_subject_motion() {
        let t = Similarity { scale: 1.0, angle_deg: 0.0, shift: [12.0, -4.0] };
        let hint = CameraHint::of(&t, 100, 200);
        assert_eq!(hint.subject_shift_px, [12.0, -4.0]);
        assert!(hint.summary.contains("12.0 px right") && hint.summary.contains("4.0 px up"), "{}", hint.summary);
    }
}
