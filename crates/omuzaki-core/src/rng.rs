//! A tiny, seedable random number generator.
//!
//! Every system that uses randomness (blinks, saccades, life-layer noise, procedural parts) owns an [`Rng`] seeded
//! from the project, so recorded inputs replay bit-identically and golden renders are stable (spec §8). `SplitMix64` is
//! used because it's fast, has no dependencies and passes `BigCrush`; nothing here needs cryptographic strength.

/// `SplitMix64`: a deterministic RNG. Same seed, same sequence, on every platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// Creates a generator from a seed.
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A uniform float in `[0, 1)`, built from the top 24 bits so every value is exactly representable.
    #[allow(clippy::cast_precision_loss)] // 24 bits fit an f32 mantissa exactly
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// A uniform float in `[lo, hi)`.
    pub fn range_f32(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next_f32()
    }

    /// An independent generator for a sub-system, so adding draws in one stream never shifts another.
    #[must_use]
    pub fn fork(&mut self, stream: u64) -> Self {
        Self::new(self.next_u64() ^ stream.wrapping_mul(0xD1B5_4A32_D192_ED03))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let (mut a, mut b) = (Rng::new(42), Rng::new(42));
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn known_first_value_is_stable_across_platforms() {
        // Reference value of SplitMix64 for seed 0; if this changes, every golden render changes.
        assert_eq!(Rng::new(0).next_u64(), 0xE220_A839_7B1D_CDAF);
    }

    #[test]
    fn floats_stay_in_range_and_cover_it() {
        let mut r = Rng::new(7);
        let (mut lo, mut hi) = (1.0_f32, 0.0_f32);
        for _ in 0..10_000 {
            let x = r.next_f32();
            assert!((0.0..1.0).contains(&x));
            lo = lo.min(x);
            hi = hi.max(x);
        }
        assert!(lo < 0.01 && hi > 0.99);
    }

    #[test]
    fn forks_are_independent_of_later_parent_draws() {
        let mut p1 = Rng::new(1);
        let mut f1 = p1.fork(3);
        let mut p2 = Rng::new(1);
        let mut f2 = p2.fork(3);
        p2.next_u64(); // extra draw in the parent must not affect the fork
        assert_eq!(f1.next_u64(), f2.next_u64());
        assert_ne!(Rng::new(1).fork(3).next_u64(), Rng::new(1).fork(4).next_u64());
    }
}
