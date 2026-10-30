//! The lock layer (#2480): how every lock of a group is shaped, whichever generator made it. A group's [`LockShape`]
//! gives its locks a cross-section ([`LockProfile`], read when the tubes are meshed), a tip ([`LockTip`]) and a split
//! into sub-locks, applied here to the clumps the generators compiled, so one setting serves bangs, framing locks,
//! sweeps, tails and guides alike.
//!
//! **Sub-locks.** A lock that splits into `n` becomes `n` narrower locks side by side across it, whose outer edges keep
//! the lock's own; from [`LockShape::split_from`] down they fan apart, to [`LockShape::spread`] lock widths at their
//! tips, and each ends a little shorter or longer than the next. A [`LockTip::Fork`] splits every (sub-)lock in two over
//! its last quarter the same way.
//!
//! **Tips.** A blunt tip keeps most of the lock's width to its end; a curl bends the last third under (toward the head
//! and body) or out (away from them) about the lock's across axis, keeping every segment's length.
//!
//! Every point the layer moves is kept clear of the head and, when there is one, the body, as the generators keep their
//! own; a point that started inside stays where the generator put it. The locks are then respaced evenly by arc length,
//! as every clump is.

use omuzaki_char_api::{HairClump, HeadSurface, LockShape, LockTip, Taper};
use omuzaki_core::glam::{Quat, Vec3};

use crate::hair::compile::{CompiledEntry, GroupGeometry};
use crate::hair::r#gen::curtain::BodyKeepOut;
use crate::hair::tube::OutwardFrame;

/// How far moved points are kept off the head and body, metres.
pub const CLEARANCE: f32 = 0.003;
/// Where a forked tip parts, as a share of the lock's length.
const FORK_FROM: f32 = 0.75;
/// Where a curl starts, as a share of the lock's length.
const CURL_FROM: f32 = 0.67;
/// How far a curl under turns over its length, radians.
const CURL_IN: f32 = 2.4;
/// How far a flick out turns, radians.
const CURL_OUT: f32 = 1.5;
/// A blunt tip keeps this share of the lock's widest width.
const BLUNT: f64 = 0.75;
/// How much shorter a sub-lock may end than the lock, as a share of its length.
const SHORTER: f32 = 0.12;

/// Shapes the clumps of every entry drawn as tubes by its group's [`LockShape`] (entries at the default are left as they
/// are). `frame` gives each lock's across axis, as the tubes take it; moved points keep clear of `head` and `keep_out`.
pub fn shape(
    entries: &mut [CompiledEntry],
    frame: &OutwardFrame,
    head: &dyn HeadSurface,
    keep_out: Option<&BodyKeepOut>,
) {
    for entry in entries {
        let lock = entry.lock;
        if lock.is_default() || !matches!(entry.group.geometry, GroupGeometry::Tube | GroupGeometry::Tied { .. }) {
            continue;
        }
        let clear = Clear { head, keep_out };
        let clumps = std::mem::take(&mut entry.group.clumps);
        let mut shaped = Vec::with_capacity(clumps.len() * lock.tubes_per_clump());
        for (i, clump) in clumps.into_iter().enumerate() {
            let tipped = tip(clump, lock.tip, frame, &clear);
            for (j, sub) in split(&tipped, lock, frame, &clear, i).into_iter().enumerate() {
                if lock.tip == LockTip::Fork {
                    shaped.extend(part(&sub, 2, FORK_FROM, lock.spread, frame, &clear, i * 4 + j));
                } else {
                    shaped.push(sub);
                }
            }
        }
        entry.group.clumps = shaped;
    }
}

/// What moved points keep clear of.
struct Clear<'a> {
    head: &'a dyn HeadSurface,
    keep_out: Option<&'a BodyKeepOut>,
}

impl Clear<'_> {
    /// `moved`, kept out of the head and body, unless `from` (where it started) was already inside one: then the
    /// generator meant it there, and it stays as moved.
    fn keep(&self, from: Vec3, moved: Vec3) -> Vec3 {
        let mut p = moved;
        let centre = self.head.centre();
        let dir = (p - centre).normalize_or_zero();
        let reach = |q: Vec3| (q - centre).length() - self.head.radius((q - centre).normalize_or_zero());
        if reach(from) >= 0.0 && reach(p) < CLEARANCE {
            p = self.head.point(dir, CLEARANCE);
        }
        if let Some(body) = self.keep_out
            && body.push_out(from, 0.0) == from
        {
            p = body.push_out(p, CLEARANCE);
        }
        p
    }
}

/// `clump` with its tip shaped.
fn tip(mut clump: HairClump, tip: LockTip, frame: &OutwardFrame, clear: &Clear<'_>) -> HairClump {
    match tip {
        LockTip::Point | LockTip::Fork => clump,
        LockTip::Blunt => {
            let w = &mut clump.width;
            w.tip = w.tip.max(BLUNT * w.mid.max(w.start));
            clump
        }
        LockTip::CurlIn | LockTip::CurlOut => {
            let turn = if tip == LockTip::CurlIn { -CURL_IN } else { CURL_OUT };
            let curled = curl(&clump.rest, turn, frame);
            clump.rest = respace(&kept(&clump.rest, &curled, clear), clump.rest.len());
            clump
        }
    }
}

/// `points` with their last third bent by `turn` radians about the across axis where the bend starts: toward the
/// outward direction for a positive turn, away from it for a negative one. Each segment keeps its length.
fn curl(points: &[Vec3], turn: f32, frame: &OutwardFrame) -> Vec<Vec3> {
    let n = points.len();
    let last = n.saturating_sub(1);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // a short lock
    let from = ((last as f32 * CURL_FROM).round() as usize).clamp(1, last.max(1));
    if n < 3 || from >= last {
        return points.to_vec();
    }
    let tangent = (points[from] - points[from - 1]).normalize_or_zero();
    let axis = tangent.cross(frame.rest(points[from])).normalize_or_zero();
    if axis == Vec3::ZERO {
        return points.to_vec();
    }
    let mut out = points[..=from].to_vec();
    let steps = last - from;
    for k in from..last {
        #[allow(clippy::cast_precision_loss)] // a few segments
        let share = (k - from + 1) as f32 / steps as f32;
        let segment = Quat::from_axis_angle(axis, turn * share) * (points[k + 1] - points[k]);
        let next = out[k] + segment;
        out.push(next);
    }
    out
}

/// `moved` (the same points as `from`, moved) kept clear of the head and body, point by point.
fn kept(from: &[Vec3], moved: &[Vec3], clear: &Clear<'_>) -> Vec<Vec3> {
    from.iter().zip(moved).map(|(a, b)| if a == b { *b } else { clear.keep(*a, *b) }).collect()
}

/// The sub-locks of `clump` ([`LockShape::split`]); `index` (the clump's) varies their lengths from lock to lock.
fn split(clump: &HairClump, lock: LockShape, frame: &OutwardFrame, clear: &Clear<'_>, index: usize) -> Vec<HairClump> {
    let n = usize::from(lock.split.clamp(1, 4));
    if n == 1 {
        return vec![clump.clone()];
    }
    #[allow(clippy::cast_possible_truncation)] // a checked 0..=0.9
    let from = lock.split_from as f32;
    part(clump, n, from, lock.spread, frame, clear, index)
}

/// `clump` parted into `n` locks side by side across it, fanning apart from `from` (a share of its length) to `spread`
/// widths at the tips. Their outer edges keep the lock's; each is a little narrower than an even share, so they overlap
/// without a gap, and a little thinner; each ends up to [`SHORTER`] short, by `index` and its place.
fn part(
    clump: &HairClump,
    n: usize,
    from: f32,
    spread: f64,
    frame: &OutwardFrame,
    clear: &Clear<'_>,
    index: usize,
) -> Vec<HairClump> {
    let points = &clump.rest;
    let last = points.len().saturating_sub(1);
    if last == 0 || n < 2 {
        return vec![clump.clone()];
    }
    #[allow(clippy::cast_precision_loss)] // up to four
    let share = (1.25 / n as f32).min(1.0);
    #[allow(clippy::cast_possible_truncation)] // a checked 0..=2
    let spread = spread as f32;
    let widest = (0..=last).map(|i| clump.width.at(along(i, last))).fold(0.0_f32, f32::max);
    let across: Vec<Vec3> = (0..=last)
        .map(|i| {
            let (a, b) = (points[i.saturating_sub(1)], points[(i + 1).min(last)]);
            let t = (b - a).normalize_or_zero().cross(frame.rest(points[i]));
            t.try_normalize().unwrap_or(Vec3::X)
        })
        .collect();
    (0..n)
        .map(|j| {
            #[allow(clippy::cast_precision_loss)] // up to four
            let side = -1.0 + 2.0 * j as f32 / (n - 1) as f32;
            let moved: Vec<Vec3> = (0..=last)
                .map(|i| {
                    let t = along(i, last);
                    let w = clump.width.at(t);
                    let fan = ((t - from) / (1.0 - from).max(1e-3)).clamp(0.0, 1.0);
                    let offset = side * (0.5 * w * (1.0 - share) + 0.5 * spread * widest * fan * fan);
                    points[i] + across[i] * offset
                })
                .collect();
            let mut sub = clump.clone();
            let length = 1.0 - SHORTER * hash(index, j);
            sub.rest = respace(&trimmed(&kept(points, &moved, clear), length), points.len());
            sub.width = scaled(&clump.width, f64::from(share));
            sub.thickness.base *= 0.85;
            sub.thickness.swell *= 0.85;
            sub.shade *= 1.0 - 0.04 * hash(index, j + 7);
            sub
        })
        .collect()
}

/// `taper` with every width times `k`.
fn scaled(taper: &Taper, k: f64) -> Taper {
    Taper { start: taper.start * k, mid: taper.mid * k, tip: taper.tip * k, ..*taper }
}

/// The first `length` (a share, `0..=1`) of the polyline `points`, by arc length.
fn trimmed(points: &[Vec3], length: f32) -> Vec<Vec3> {
    let total: f32 = points.windows(2).map(|w| w[0].distance(w[1])).sum();
    let keep = total * length.clamp(0.0, 1.0);
    let mut out = vec![points[0]];
    let mut run = 0.0;
    for w in points.windows(2) {
        let d = w[0].distance(w[1]);
        if run + d >= keep {
            out.push(w[0].lerp(w[1], ((keep - run) / d.max(f32::EPSILON)).clamp(0.0, 1.0)));
            return out;
        }
        run += d;
        out.push(w[1]);
    }
    out
}

/// `count` points evenly spaced by arc length along the polyline `points`, from its first point to its last.
pub fn respace(points: &[Vec3], count: usize) -> Vec<Vec3> {
    if points.len() < 2 || count < 2 {
        return points.to_vec();
    }
    let lengths: Vec<f32> = std::iter::once(0.0)
        .chain(points.windows(2).scan(0.0, |run, w| {
            *run += w[0].distance(w[1]);
            Some(*run)
        }))
        .collect();
    let total = lengths[lengths.len() - 1];
    let mut seg = 0;
    (0..count)
        .map(|k| {
            let at = total * along(k, count - 1);
            while seg + 2 < points.len() && lengths[seg + 1] < at {
                seg += 1;
            }
            let d = (lengths[seg + 1] - lengths[seg]).max(f32::EPSILON);
            points[seg].lerp(points[seg + 1], ((at - lengths[seg]) / d).clamp(0.0, 1.0))
        })
        .collect()
}

/// `i / last` as a float, `0..=1`.
#[allow(clippy::cast_precision_loss)] // a few dozen points
fn along(i: usize, last: usize) -> f32 {
    if last == 0 { 0.0 } else { i as f32 / last as f32 }
}

/// A stable number in `0..1` for sub-lock `j` of clump `i`.
#[allow(clippy::cast_precision_loss)] // the top 24 bits
fn hash(i: usize, j: usize) -> f32 {
    let mut h = (i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ (j as u64).wrapping_mul(0xc2b2_ae3d_27d4_eb4f);
    h ^= h >> 29;
    h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h ^= h >> 32;
    (h >> 40) as f32 / (1u64 << 24) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn respacing_keeps_the_ends_and_spaces_evenly() {
        let p = [Vec3::ZERO, Vec3::new(0.1, 0.0, 0.0), Vec3::new(0.1, 0.3, 0.0)];
        let r = respace(&p, 5);
        assert_eq!((r[0], r[4]), (p[0], p[2]));
        let gaps: Vec<f32> = r.windows(2).map(|w| w[0].distance(w[1])).collect();
        assert!(gaps.iter().all(|g| (g - 0.1).abs() < 1e-5), "{gaps:?}");
    }

    #[test]
    fn trimming_shortens_by_arc_length() {
        let p = [Vec3::ZERO, Vec3::new(0.0, -0.2, 0.0), Vec3::new(0.0, -0.4, 0.0)];
        let t = trimmed(&p, 0.75);
        assert!((t[t.len() - 1].y + 0.3).abs() < 1e-6);
    }

    #[test]
    fn the_hash_is_spread_and_stable() {
        let v: Vec<f32> = (0..64).map(|i| hash(i, 1)).collect();
        assert!(v.iter().all(|x| (0.0..1.0).contains(x)));
        assert_eq!(hash(5, 2), hash(5, 2));
        assert!(v.iter().copied().fold(0.0, f32::max) > 0.8 && v.iter().copied().fold(1.0, f32::min) < 0.2);
    }
}
