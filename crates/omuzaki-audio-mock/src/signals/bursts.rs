//! Claps and plosives: short decaying noise bursts.

use omuzaki_core::rng::Rng;

use super::{SAMPLE_RATE, samples_in};

/// A clap: white noise under an exponential decay, starting at once. `decay_ms` is the time to fall 60 dB (a clap in a
/// room is about 40 to 150 ms); the signal runs for `len` samples and its peak is `amplitude`.
///
/// The onset is the first sample, so a test that needs to know where it is has nothing to find: place it by padding.
#[must_use]
pub fn clap(len: usize, seed: u64, decay_ms: f64, amplitude: f32) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    let per_sample = decay_ms_to_factor(decay_ms);
    let mut envelope = 1.0f64;
    let mut out: Vec<f32> = (0..len)
        .map(|_| {
            let sample = rng.range_f32(-1.0, 1.0) * envelope as f32;
            envelope *= per_sample;
            sample
        })
        .collect();
    super::normalise(&mut out, amplitude);
    out
}

/// A plosive burst ("p", "t"): a 12 ms noise burst with a 1 ms attack, low-passed so most of its energy is below 4 kHz,
/// followed by silence to `len` samples. Its peak is `amplitude`.
#[must_use]
pub fn plosive(len: usize, seed: u64, amplitude: f32) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    let burst = samples_in(0.012).min(len);
    let attack = samples_in(0.001).max(1);
    let mut low = 0.0f32;
    let mut out = vec![0.0f32; len];
    for (n, slot) in out.iter_mut().take(burst).enumerate() {
        low += 0.45 * (rng.range_f32(-1.0, 1.0) - low);
        let rise = (n as f32 / attack as f32).min(1.0);
        let fall = 1.0 - n as f32 / burst as f32;
        *slot = low * rise * fall;
    }
    super::normalise(&mut out, amplitude);
    out
}

/// The per-sample factor of an exponential that falls 60 dB in `decay_ms`.
fn decay_ms_to_factor(decay_ms: f64) -> f64 {
    let samples = (decay_ms * 1e-3 * f64::from(SAMPLE_RATE)).max(1.0);
    super::math::pow10(-3.0 / samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clap_decays_by_sixty_decibels_in_its_decay_time() {
        let c = clap(9600, 1, 100.0, 1.0);
        let rms = |x: &[f32]| (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt();
        let (early, late) = (rms(&c[..240]), rms(&c[4560..4800]));
        let drop_db = 20.0 * (late / early).log10();
        // 100 ms is 4800 samples: the window centred near 4680 is about 58 dB down from the one near 120.
        assert!((-70.0..-48.0).contains(&drop_db), "{drop_db}");
        assert_eq!(c, clap(9600, 1, 100.0, 1.0));
        assert!((super::super::peak(&c) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_plosive_is_a_short_burst_then_silence() {
        let p = plosive(2400, 5, 0.8);
        assert!(p[600..].iter().all(|s| *s == 0.0), "the burst is 12 ms = 576 samples");
        assert!(p[..576].iter().any(|s| s.abs() > 0.2));
        assert!(p[0].abs() < 0.1, "the attack starts from silence");
        assert_eq!(p, plosive(2400, 5, 0.8));
    }
}
