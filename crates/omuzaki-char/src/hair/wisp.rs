//! Wisps and flyaways (#2412, #2516): fine stray strands along the edges of the outer locks, and short baby hairs along
//! the front hairline. In a close-up a lock's edge spans a few pixels a millimetre, and a clean lens of a lock reads as
//! plastic; a hand-drawn anime close-up breaks the silhouette with a few loose strands. A wisp leaves its lock part way
//! down, inside the lock, crosses its edge and strays a few millimetres off it by the tip, on a fine tube that the
//! hair's outline draws as a line.
//!
//! How many locks shed one, how far they stray and how frizzy they are come from the hair's [`HairStrays`] ([`Shed`]);
//! which locks shed one, which side, where it leaves and how far it strays come from a hash of the lock's index, so the
//! same hair grows the same wisps on every build. Only outer locks (drawn at full shade) shed them: an inner layer's
//! wisp would sit under the outer locks.
//!
//! Baby hairs ([`baby_hairs`]) root just behind the scalp shell's front hairline, spread across the forehead, and lie
//! on the skin a millimetre or two off it, combed down and a little out to the side, curling at their tips.

use omuzaki_char_api::{Bulge, GuidePoint, HairStrays, HeadSurface, Quality, Taper};
use omuzaki_core::glam::Vec3;

use super::tube::{OutwardFrame, along};

/// The share of outer locks that shed a wisp in the close-ups the wisps were first made for (#2412).
const SHARE: f32 = 0.45;
/// How much more of the locks a close-up (offline) build sheds than a live one: this share of those that don't.
const OFFLINE_MORE: f32 = 0.3;
/// A lock counts as outer at this shade or above (inner layers are drawn darker).
const OUTER_SHADE: f32 = 0.95;
/// A wisp's width at its root and at its tip, metres.
const WIDTH: (f64, f64) = (0.0007, 0.0002);
/// A wisp's thickness, metres.
const THICKNESS: f64 = 0.0005;
/// How far a wisp strays from its lock by the tip, metres: a hash picks from this range.
const STRAY: (f32, f32) = (0.004, 0.012);
/// Where along the lock a wisp leaves it: a hash picks from this range.
const START: (f32, f32) = (0.2, 0.55);
/// How far across the lock's half width a wisp sits where it leaves (inside the lock) and at the tip.
const EDGE: (f32, f32) = (0.6, 1.1);
/// The fewest points a wisp is drawn with.
const MIN_POINTS: usize = 3;
/// How far a frizzy strand kinks off its line at frizz 1, metres.
const KINK: f32 = 0.0016;
/// Kinks along a strand.
const KINKS: f32 = 5.0;
/// How far across the forehead the baby hairs spread, either side of the front, degrees of azimuth.
const BABY_SPREAD: f64 = 55.0;
/// Points along a baby hair.
const BABY_POINTS: usize = 6;
/// How far off the skin a baby hair lies at its root and at its tip, metres.
const BABY_LIFT: (f64, f64) = (0.0007, 0.0018);

/// How a hair sheds its wisps: the share of outer locks that shed one, the range each strays within and the frizz.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shed {
    /// The share of outer locks that shed one, `0..=1`.
    pub share: f32,
    /// How far a wisp strays by its tip, metres: a hash picks from this range.
    pub stray: (f32, f32),
    /// How frizzy, `0..=1`.
    pub frizz: f32,
}

impl Shed {
    /// The close-up wisps as first made (#2412): 45% of the outer locks, 4 to 12 mm, smooth.
    pub const CLOSE_UP: Self = Self { share: SHARE, stray: STRAY, frizz: 0.0 };

    /// How `strays` sheds at `quality`, or `None` for no wisps: an offline build sheds a third more of the locks.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)] // checked parameters
    pub fn of(strays: &HairStrays, quality: Quality) -> Option<Self> {
        let mut share = strays.share as f32;
        if share <= 0.0 {
            return None;
        }
        if quality == Quality::Offline {
            share += (1.0 - share) * OFFLINE_MORE;
        }
        let stray = strays.stray as f32;
        Some(Self { share, stray: (0.5 * stray, 1.5 * stray), frizz: strays.frizz as f32 })
    }
}

/// One wisp: the polyline and the tube it is drawn as.
#[derive(Clone, Debug, PartialEq)]
pub struct Wisp {
    /// The rest points, root to tip.
    pub points: Vec<Vec3>,
    /// Its width along it.
    pub width: Taper,
    /// Its thickness.
    pub thickness: Bulge,
}

/// The wisp the `index`-th lock sheds, if it sheds one by `shed`: `rest` is the lock's polyline, `width` its width and
/// `shade` its shade; `frame` gives the outward direction it strays toward.
pub fn wisp(index: usize, rest: &[Vec3], width: &Taper, shade: f32, frame: &OutwardFrame, shed: &Shed) -> Option<Wisp> {
    if shade < OUTER_SHADE || rest.len() < MIN_POINTS {
        return None;
    }
    let [pick, side, start, stray] = hashes(index);
    if pick >= shed.share {
        return None;
    }
    let side = if side < 0.5 { -1.0 } else { 1.0 };
    let (start, stray) = (mix(START, start), mix(shed.stray, stray));
    let phase = pick * 37.0;
    let last = rest.len() - 1;
    let points: Vec<Vec3> = (0..rest.len())
        .filter(|&i| along(i, last) >= start)
        .map(|i| {
            let (p, t) = (rest[i], along(i, last));
            let tangent = (rest[(i + 1).min(last)] - rest[i.saturating_sub(1)]).normalize_or_zero();
            let out = frame.rest(p);
            let across = tangent.cross(out).try_normalize().unwrap_or(Vec3::X);
            let s = (t - start) / (1.0 - start).max(1e-3);
            let drift = stray * s * s;
            let edge = side * width.at(t) * 0.5 * mix(EDGE, s);
            let kink = kink(shed.frizz, s, phase);
            p + across * (edge + side * drift * 0.7) + out * (drift * 0.7 + kink)
        })
        .collect();
    (points.len() >= MIN_POINTS).then(|| Wisp {
        points,
        width: Taper { start: WIDTH.0, mid: WIDTH.0, tip: WIDTH.1, tip_at: 0.3, ..Taper::default() },
        thickness: Bulge { base: THICKNESS, swell: 0.0 },
    })
}

/// The baby hairs `strays` asks for (`strays.baby` of them) along a scalp shell whose front hairline is at
/// `hairline_deg` of polar angle on `head`: each roots a degree behind the hairline, at an azimuth spread across the
/// forehead (with a hash's jitter), and runs `baby_length` down over the skin, combed a little out to its side and
/// curling at its tip, lifted [`BABY_LIFT`] off it.
#[must_use]
pub fn baby_hairs(strays: &HairStrays, head: &dyn HeadSurface, hairline_deg: f64) -> Vec<Wisp> {
    let count = strays.baby as usize;
    if count == 0 {
        return Vec::new();
    }
    // Degrees of polar angle per metre over the forehead.
    let per_metre = 180.0 / (std::f64::consts::PI * f64::from(head.radius(Vec3::Z)).max(0.03));
    #[allow(clippy::cast_possible_truncation)] // checked parameters
    let frizz = strays.frizz as f32;
    (0..count)
        .map(|i| {
            let [jitter, scale, lean, phase] = hashes(i.wrapping_add(0x5eed));
            #[allow(clippy::cast_precision_loss)] // at most 200
            let (place, total) = (i as f64, count as f64);
            let phi = (2.0 * (place + 0.5) / total - 1.0) * BABY_SPREAD
                + (f64::from(jitter) - 0.5) * 2.0 * BABY_SPREAD / total;
            let length = strays.baby_length * (0.6 + 0.8 * f64::from(scale));
            let theta0 = hairline_deg - 1.0;
            let reach = length * per_metre;
            // Combed out toward its own side, and the tip curls further that way.
            let side = if phi.abs() < 4.0 { if lean < 0.5 { -1.0 } else { 1.0 } } else { phi.signum() };
            let curl = side * (6.0 + 14.0 * f64::from(lean));
            let last = BABY_POINTS - 1;
            let points = (0..BABY_POINTS)
                .map(|k| {
                    let s = f64::from(along(k, last));
                    let theta = theta0 + reach * s;
                    let phi = phi + curl * s * s * reach / 10.0;
                    let lift = BABY_LIFT.0 + (BABY_LIFT.1 - BABY_LIFT.0) * s;
                    #[allow(clippy::cast_possible_truncation)] // metres
                    let kink = f64::from(kink(frizz, s as f32, phase * 37.0));
                    GuidePoint::Surface([theta, phi, lift + kink.abs()]).resolve(head)
                })
                .collect();
            Wisp {
                points,
                width: Taper { start: WIDTH.0, mid: WIDTH.0, tip: WIDTH.1, tip_at: 0.4, ..Taper::default() },
                thickness: Bulge { base: THICKNESS, swell: 0.0 },
            }
        })
        .collect()
}

/// How far a strand with `frizz` kinks off its line `s` (`0..1`) along it, from its `phase`: nothing at its root.
fn kink(frizz: f32, s: f32, phase: f32) -> f32 {
    frizz * KINK * s * (s * KINKS * std::f32::consts::TAU + phase).sin()
}

/// `lo` to `hi` by `t`.
fn mix((lo, hi): (f32, f32), t: f32) -> f32 {
    lo + (hi - lo) * t
}

/// Four numbers in `0..1` for lock `index`: `SplitMix64` steps from the index.
#[allow(clippy::cast_precision_loss)] // 24 bits of the hash, exactly representable
fn hashes(index: usize) -> [f32; 4] {
    let mut state = u64::try_from(index).unwrap_or(u64::MAX).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x5157_15a5;
    std::array::from_fn(|_| {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        (z >> 40) as f32 / (1u64 << 24) as f32
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use omuzaki_core::glam::Vec2;

    use super::*;

    fn lock() -> Vec<Vec3> {
        (0..12_u8).map(|i| Vec3::new(0.08, -0.01 * f32::from(i), 0.0)).collect()
    }

    fn frame() -> OutwardFrame {
        OutwardFrame { head: Vec3::ZERO, body_axis: Vec2::ZERO }
    }

    #[test]
    fn about_the_share_of_outer_locks_shed_a_wisp_and_inner_ones_none() {
        let width = Taper::default();
        let shed = (0..1000).filter(|&i| wisp(i, &lock(), &width, 1.0, &frame(), &Shed::CLOSE_UP).is_some()).count();
        assert!((380..=520).contains(&shed), "{shed} of 1000");
        assert!((0..200).all(|i| wisp(i, &lock(), &width, 0.8, &frame(), &Shed::CLOSE_UP).is_none()));
        let a: Vec<_> = (0..50).map(|i| wisp(i, &lock(), &width, 1.0, &frame(), &Shed::CLOSE_UP)).collect();
        let b: Vec<_> = (0..50).map(|i| wisp(i, &lock(), &width, 1.0, &frame(), &Shed::CLOSE_UP)).collect();
        assert_eq!(a, b, "the same locks shed the same wisps");
    }

    #[test]
    fn a_wisp_leaves_inside_its_lock_and_strays_off_it_by_the_tip() {
        let (rest, width) = (lock(), Taper::default());
        let (index, w) =
            (0..100).find_map(|i| wisp(i, &rest, &width, 1.0, &frame(), &Shed::CLOSE_UP).map(|w| (i, w))).unwrap();
        let root = w.points[0];
        let nearest = |p: Vec3| rest.iter().map(|q| q.distance(p)).fold(f32::INFINITY, f32::min);
        assert!(nearest(root) < width.at(0.5) * 0.5, "lock {index}: the root is inside the lock");
        let tip = *w.points.last().unwrap();
        assert!(nearest(tip) > STRAY.0 * 0.5, "lock {index}: the tip strays off the lock ({})", nearest(tip));
        assert!(w.width.start < 0.001 && w.thickness.base < 0.001, "a fine strand");
    }
}
