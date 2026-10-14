//! Smooth value noise: the "never white" wobble behind head, brow, mouth-corner, lid and gaze texture.
//!
//! A hashed lattice of values, quintic-faded between lattice points, so there are no velocity kinks. Two octaves,
//! normalised so peaks sit near +-1 (a lone value-noise octave rarely leaves +-0.7). Integer hashing and plain
//! arithmetic only, so the same seed gives the same wobble on every platform; the sample time is `f64` so a
//! many-hour stream does not lose resolution.

/// Which wobble a sample drives. Each has its own lattice seed, so no two of them move together.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Texture {
    HeadYaw,
    HeadPitch,
    HeadRoll,
    BrowLeft,
    BrowRight,
    MouthLeft,
    MouthRight,
    GazeX,
    GazeY,
    Lid,
    Breath,
    Laugh,
}

impl Texture {
    const COUNT: usize = Self::Laugh as usize + 1;
}

/// The per-texture lattice seeds of one life layer, derived from [`LifeTraits::noise_seed`](omuzaki_anim_api::LifeTraits).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NoiseSeeds([u32; Texture::COUNT]);

impl NoiseSeeds {
    /// Spreads `base` over the textures with an integer hash, so neighbouring textures are unrelated.
    pub fn new(base: u32) -> Self {
        let mut seeds = [0; Texture::COUNT];
        for (i, seed) in seeds.iter_mut().enumerate() {
            #[allow(clippy::cast_possible_truncation)] // the index is below 16
            let index = i as u32 + 1;
            *seed = mix(base ^ index.wrapping_mul(0x9E37_79B1));
        }
        Self(seeds)
    }

    /// The wobble of `texture` at `time` seconds, in `-1..=1`, varying over about `period` seconds.
    pub fn sample(&self, texture: Texture, time: f64, period: f32) -> f32 {
        let seed = self.0[texture as usize];
        let x = time / f64::from(period.max(1e-3));
        let a = value(x, seed);
        let b = value(x * 2.3 + 11.7, seed.wrapping_add(977));
        (1.25 * (0.7 * a + 0.3 * b)).clamp(-1.0, 1.0)
    }
}

/// A 32-bit avalanche hash (the finaliser of `MurmurHash3`, with the prototype's constants).
fn mix(mut h: u32) -> u32 {
    h = h.wrapping_mul(0x9E37_79B1);
    h ^= h >> 15;
    h = h.wrapping_mul(0x85EB_CA77);
    h ^= h >> 13;
    h = h.wrapping_mul(0xC2B2_AE3D);
    h ^ (h >> 16)
}

/// The lattice value at integer `i`, in `-1..=1`.
#[allow(clippy::cast_sign_loss, clippy::cast_precision_loss)] // the sign bits are just more hash input; 32 bits to f32 rounds, deterministically
fn lattice(i: i32, seed: u32) -> f32 {
    mix((i as u32) ^ seed) as f32 / 2_147_483_648.0 - 1.0
}

/// One octave: lattice values blended with the quintic `6u^5 - 15u^4 + 10u^3`.
#[allow(clippy::cast_possible_truncation)] // lattice index: far beyond any session length, and saturating is harmless
fn value(x: f64, seed: u32) -> f32 {
    if !x.is_finite() {
        return 0.0; // a time so large it overflowed: flat, never NaN
    }
    let floor = x.floor();
    let f = (x - floor) as f32;
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let i = floor as i32;
    lattice(i, seed) * (1.0 - u) + lattice(i.wrapping_add(1), seed) * u
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(seed: u32, texture: Texture, period: f32) -> Vec<f32> {
        let seeds = NoiseSeeds::new(seed);
        (0..60_000).map(|i| seeds.sample(texture, f64::from(i) / 60.0, period)).collect()
    }

    #[test]
    fn stays_in_range_and_uses_most_of_it() {
        let v = samples(7, Texture::HeadYaw, 2.0);
        assert!(v.iter().all(|x| (-1.0..=1.0).contains(x)));
        let peak = v.iter().fold(0.0_f32, |m, x| m.max(x.abs()));
        assert!(peak > 0.85, "normalised peaks sit near 1, got {peak}");
    }

    #[test]
    fn is_deterministic_and_seed_and_texture_dependent() {
        assert_eq!(samples(3, Texture::GazeX, 1.0), samples(3, Texture::GazeX, 1.0));
        assert_ne!(samples(3, Texture::GazeX, 1.0), samples(4, Texture::GazeX, 1.0));
        assert_ne!(samples(3, Texture::GazeX, 1.0), samples(3, Texture::GazeY, 1.0));
    }

    #[test]
    fn is_smooth_with_no_jumps() {
        let v = samples(11, Texture::Lid, 0.5);
        let steepest = v.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0_f32, f32::max);
        // 0.5 s period sampled at 60 Hz: a whole lattice step is 1/30 of a period at most.
        assert!(steepest < 0.2, "steepest step {steepest}");
    }

    #[test]
    fn has_no_bias() {
        let v = samples(5, Texture::Breath, 3.0);
        let mean = v.iter().sum::<f32>() / 60_000.0;
        assert!(mean.abs() < 0.08, "mean {mean}");
    }

    #[test]
    fn survives_huge_and_negative_times() {
        let seeds = NoiseSeeds::new(1);
        for t in [-1e9, -3.5, 0.0, 1e12, f64::MAX] {
            let v = seeds.sample(Texture::Laugh, t, 1.3);
            assert!(v.is_finite() && v.abs() <= 1.0, "t = {t}");
        }
    }
}
