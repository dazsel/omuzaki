//! Wisps (#2412): fine stray strands along the edges of the outer locks, built only at offline quality. In a close-up a
//! lock's edge spans a few pixels a millimetre, and a clean lens of a lock reads as plastic; a hand-drawn anime close-up
//! breaks the silhouette with a few loose strands. A wisp leaves its lock part way down, inside the lock, crosses its
//! edge and strays a few millimetres off it by the tip, on a fine tube that the hair's outline draws as a line.
//!
//! Which locks shed one, which side, where it leaves and how far it strays all come from a hash of the lock's index, so
//! the same hair grows the same wisps on every build. Only outer locks (drawn at full shade) shed them: an inner layer's
//! wisp would sit under the outer locks.

use omuzaki_char_api::{Bulge, Taper};
use omuzaki_core::glam::Vec3;

use super::tube::{OutwardFrame, along};

/// The share of outer locks that shed a wisp.
const SHARE: f32 = 0.45;
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

/// The wisp the `index`-th lock sheds, if it sheds one: `rest` is the lock's polyline, `width` its width and `shade` its
/// shade; `frame` gives the outward direction it strays toward.
pub fn wisp(index: usize, rest: &[Vec3], width: &Taper, shade: f32, frame: &OutwardFrame) -> Option<Wisp> {
    if shade < OUTER_SHADE || rest.len() < MIN_POINTS {
        return None;
    }
    let [pick, side, start, stray] = hashes(index);
    if pick >= SHARE {
        return None;
    }
    let side = if side < 0.5 { -1.0 } else { 1.0 };
    let (start, stray) = (mix(START, start), mix(STRAY, stray));
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
            p + across * (edge + side * drift * 0.7) + out * (drift * 0.7)
        })
        .collect();
    (points.len() >= MIN_POINTS).then(|| Wisp {
        points,
        width: Taper { start: WIDTH.0, mid: WIDTH.0, tip: WIDTH.1, tip_at: 0.3, ..Taper::default() },
        thickness: Bulge { base: THICKNESS, swell: 0.0 },
    })
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
        let shed = (0..1000).filter(|&i| wisp(i, &lock(), &width, 1.0, &frame()).is_some()).count();
        assert!((380..=520).contains(&shed), "{shed} of 1000");
        assert!((0..200).all(|i| wisp(i, &lock(), &width, 0.8, &frame()).is_none()));
        let a: Vec<_> = (0..50).map(|i| wisp(i, &lock(), &width, 1.0, &frame())).collect();
        let b: Vec<_> = (0..50).map(|i| wisp(i, &lock(), &width, 1.0, &frame())).collect();
        assert_eq!(a, b, "the same locks shed the same wisps");
    }

    #[test]
    fn a_wisp_leaves_inside_its_lock_and_strays_off_it_by_the_tip() {
        let (rest, width) = (lock(), Taper::default());
        let (index, w) = (0..100).find_map(|i| wisp(i, &rest, &width, 1.0, &frame()).map(|w| (i, w))).unwrap();
        let root = w.points[0];
        let nearest = |p: Vec3| rest.iter().map(|q| q.distance(p)).fold(f32::INFINITY, f32::min);
        assert!(nearest(root) < width.at(0.5) * 0.5, "lock {index}: the root is inside the lock");
        let tip = *w.points.last().unwrap();
        assert!(nearest(tip) > STRAY.0 * 0.5, "lock {index}: the tip strays off the lock ({})", nearest(tip));
        assert!(w.width.start < 0.001 && w.thickness.base < 0.001, "a fine strand");
    }
}
