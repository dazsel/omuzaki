//! Seeded randomness and paper grain, the same on every machine: a counter hash (`SplitMix64`) and value noise on it.

/// `SplitMix64`'s finaliser: a well-mixed 64-bit hash of `x`.
pub(crate) fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// A seeded stream of numbers in 0..1.
#[derive(Clone, Debug)]
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(mix(seed ^ 0x6f6d_757a_616b_6970))
    }

    /// The next number in `0..1`.
    #[allow(clippy::cast_precision_loss)] // 53 bits fit an f64 exactly
    pub(crate) fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(1);
        (mix(self.0) >> 11) as f64 / (1u64 << 53) as f64
    }

    /// The next number in `-1..1`.
    pub(crate) fn signed(&mut self) -> f64 {
        self.next() * 2.0 - 1.0
    }
}

/// A lattice value in 0..1 for cell `(x, y)` of grain `seed`.
#[allow(clippy::cast_precision_loss, clippy::cast_sign_loss)] // a hash's bits, and two's-complement cell numbers
fn lattice(seed: u64, x: i64, y: i64) -> f32 {
    let h = mix(seed ^ mix(x as u64 ^ mix(y as u64)));
    (h >> 40) as f32 / (1u64 << 24) as f32
}

/// Paper grain at canvas point `(x, y)`: smooth value noise in 0..1 with cells `scale` pixels wide, two octaves.
#[allow(clippy::cast_possible_truncation)] // cell numbers of an 8k canvas fit easily
pub(crate) fn grain(seed: u64, x: f64, y: f64, scale: f64) -> f32 {
    let octave = |s: u64, f: f64| {
        let (u, v) = (x / f, y / f);
        let (cx, cy) = (u.floor(), v.floor());
        let (tx, ty) = ((u - cx) as f32, (v - cy) as f32);
        let (sx, sy) = (tx * tx * (3.0 - 2.0 * tx), ty * ty * (3.0 - 2.0 * ty));
        let (ix, iy) = (cx as i64, cy as i64);
        let a = lattice(s, ix, iy) + (lattice(s, ix + 1, iy) - lattice(s, ix, iy)) * sx;
        let b = lattice(s, ix, iy + 1) + (lattice(s, ix + 1, iy + 1) - lattice(s, ix, iy + 1)) * sx;
        a + (b - a) * sy
    };
    let scale = scale.max(0.5);
    0.65 * octave(seed, scale) + 0.35 * octave(seed.wrapping_add(1), scale * 0.43)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stream_repeats_for_a_seed_and_differs_between_seeds() {
        let a: Vec<f64> = (0..4).scan(Rng::new(7), |r, _| Some(r.next())).collect();
        let b: Vec<f64> = (0..4).scan(Rng::new(7), |r, _| Some(r.next())).collect();
        let c: Vec<f64> = (0..4).scan(Rng::new(8), |r, _| Some(r.next())).collect();
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.iter().all(|v| (0.0..1.0).contains(v)));
    }

    #[test]
    fn grain_is_in_range_and_varies() {
        let vals: Vec<f32> = (0..200).map(|i| grain(3, f64::from(i) * 0.7, f64::from(i) * 1.3, 4.0)).collect();
        assert!(vals.iter().all(|v| (0.0..=1.0).contains(v)));
        let (lo, hi) = vals.iter().fold((1.0f32, 0.0f32), |(l, h), &v| (l.min(v), h.max(v)));
        assert!(hi - lo > 0.3);
    }
}
