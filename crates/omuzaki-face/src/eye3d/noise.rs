//! Seeded noise and randomness for the eye's textures and meshes: a small PCG stream, 2D value noise and its fractal
//! sum, and Worley cells. Pure functions of the seed, so one design always builds the same eye on every platform.

/// A PCG32 stream.
#[derive(Clone, Debug)]
pub struct Pcg(u64);

impl Pcg {
    /// The stream for `seed`, mixed with `stream` so independent parts of the eye draw independent numbers.
    pub fn new(seed: u32, stream: u32) -> Self {
        let mut p = Self((u64::from(seed) << 32 | u64::from(stream)).wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        p.next_u32();
        p
    }

    /// The next 32 random bits.
    pub fn next_u32(&mut self) -> u32 {
        let old = self.0;
        self.0 = old.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        // The PCG output permutation: the top bits, xor-shifted and rotated.
        #[allow(clippy::cast_possible_truncation)]
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        #[allow(clippy::cast_possible_truncation)]
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    /// Uniform in `0..1`.
    pub fn unit(&mut self) -> f32 {
        // 24 bits: exact in f32.
        #[allow(clippy::cast_precision_loss)]
        let v = (self.next_u32() >> 8) as f32;
        v / 16_777_216.0
    }

    /// Uniform in `lo..hi`.
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }

    /// Roughly normal, mean 0, deviation 1 (the sum of four uniforms, rescaled).
    pub fn normal(&mut self) -> f32 {
        let sum = self.unit() + self.unit() + self.unit() + self.unit();
        (sum - 2.0) * 1.732
    }
}

fn hash2(x: i32, y: i32, seed: u32) -> u32 {
    // Wrapping casts: only the bits matter.
    #[allow(clippy::cast_sign_loss)]
    let mut h =
        (x as u32).wrapping_mul(0x8da6_b343) ^ (y as u32).wrapping_mul(0xd816_3841) ^ seed.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a_2d39);
    h ^ (h >> 15)
}

fn hash_unit(x: i32, y: i32, seed: u32) -> f32 {
    #[allow(clippy::cast_precision_loss)] // 24 bits
    let v = (hash2(x, y, seed) >> 8) as f32;
    v / 16_777_216.0
}

fn fade(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// Value noise in `0..1` at `(x, y)`, lattice spacing 1.
pub fn value(x: f32, y: f32, seed: u32) -> f32 {
    let (fx, fy) = (x.floor(), y.floor());
    // Lattice coordinates stay far inside i32 for any texture this crate makes.
    #[allow(clippy::cast_possible_truncation)]
    let (ix, iy) = (fx as i32, fy as i32);
    let (u, v) = (fade(x - fx), fade(y - fy));
    let a = hash_unit(ix, iy, seed);
    let b = hash_unit(ix + 1, iy, seed);
    let c = hash_unit(ix, iy + 1, seed);
    let d = hash_unit(ix + 1, iy + 1, seed);
    let top = a + (b - a) * u;
    let bottom = c + (d - c) * u;
    top + (bottom - top) * v
}

/// Value noise periodic in `x` with period `period` (a whole number of lattice cells): for angles round a disc.
pub fn value_wrapped(x: f32, y: f32, period: i32, seed: u32) -> f32 {
    let (fx, fy) = (x.floor(), y.floor());
    #[allow(clippy::cast_possible_truncation)]
    let (ix, iy) = (fx as i32, fy as i32);
    let (u, v) = (fade(x - fx), fade(y - fy));
    let wrap = |i: i32| i.rem_euclid(period.max(1));
    let a = hash_unit(wrap(ix), iy, seed);
    let b = hash_unit(wrap(ix + 1), iy, seed);
    let c = hash_unit(wrap(ix), iy + 1, seed);
    let d = hash_unit(wrap(ix + 1), iy + 1, seed);
    let top = a + (b - a) * u;
    let bottom = c + (d - c) * u;
    top + (bottom - top) * v
}

/// A fractal sum of `octaves` octaves of value noise, in `0..1`.
pub fn fbm(x: f32, y: f32, octaves: u32, seed: u32) -> f32 {
    let (mut sum, mut amp, mut freq, mut norm) = (0.0, 1.0, 1.0, 0.0);
    for o in 0..octaves {
        sum += amp * value(x * freq, y * freq, seed.wrapping_add(o.wrapping_mul(0x9e37)));
        norm += amp;
        amp *= 0.5;
        freq *= 2.03;
    }
    sum / norm
}

/// Worley cells: the distances to the nearest and second-nearest feature points (one per unit cell).
pub fn worley(x: f32, y: f32, seed: u32) -> (f32, f32) {
    let (fx, fy) = (x.floor(), y.floor());
    #[allow(clippy::cast_possible_truncation)]
    let (ix, iy) = (fx as i32, fy as i32);
    let (mut f1, mut f2) = (f32::MAX, f32::MAX);
    for dy in -1..=1 {
        for dx in -1..=1 {
            let (cx, cy) = (ix + dx, iy + dy);
            #[allow(clippy::cast_precision_loss)] // small lattice coordinates
            let px = cx as f32 + hash_unit(cx, cy, seed);
            #[allow(clippy::cast_precision_loss)]
            let py = cy as f32 + hash_unit(cx, cy, seed ^ 0x5bd1_e995);
            let d = ((px - x).powi(2) + (py - y).powi(2)).sqrt();
            if d < f1 {
                f2 = f1;
                f1 = d;
            } else if d < f2 {
                f2 = d;
            }
        }
    }
    (f1, f2)
}

/// `0` below `e0`, `1` above `e1`, smooth between.
pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_stays_in_range_and_repeats() {
        for i in 0..500 {
            #[allow(clippy::cast_precision_loss)]
            let x = i as f32 * 0.37;
            let v = value(x, x * 0.7, 3);
            assert!((0.0..=1.0).contains(&v));
            assert!((value(x, x * 0.7, 3) - v).abs() < f32::EPSILON);
            let f = fbm(x, 1.0, 4, 9);
            assert!((0.0..=1.0).contains(&f));
        }
    }

    #[test]
    fn wrapped_noise_meets_itself_at_the_period() {
        for i in 0..20 {
            #[allow(clippy::cast_precision_loss)]
            let y = i as f32 * 0.31;
            assert!((value_wrapped(0.0, y, 16, 1) - value_wrapped(16.0, y, 16, 1)).abs() < 1e-6);
        }
    }

    #[test]
    fn the_stream_is_uniform_enough() {
        let mut rng = Pcg::new(1, 2);
        let mean: f32 = (0..10_000).map(|_| rng.unit()).sum::<f32>() / 10_000.0;
        assert!((mean - 0.5).abs() < 0.02, "{mean}");
    }
}
