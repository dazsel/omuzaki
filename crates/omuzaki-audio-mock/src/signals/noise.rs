//! White and pink noise.

use omuzaki_core::rng::Rng;

/// Uniform white noise in `[-amplitude, amplitude)`.
#[must_use]
pub fn white_noise(len: usize, seed: u64, amplitude: f32) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    (0..len).map(|_| rng.range_f32(-amplitude, amplitude)).collect()
}

/// Pink noise (power falling 3 dB per octave), made by filtering white noise with Paul Kellet's refined filter, which
/// is within 0.05 dB of a straight line from 9 Hz to Nyquist at 44.1 kHz. The result is scaled so its peak is
/// `amplitude`.
#[must_use]
pub fn pink_noise(len: usize, seed: u64, amplitude: f32) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    let mut b = [0.0f64; 7];
    let mut out: Vec<f32> = (0..len)
        .map(|_| {
            let white = f64::from(rng.range_f32(-1.0, 1.0));
            b[0] = 0.998_86 * b[0] + white * 0.055_517_9;
            b[1] = 0.993_32 * b[1] + white * 0.075_075_9;
            b[2] = 0.969_00 * b[2] + white * 0.153_852_0;
            b[3] = 0.866_50 * b[3] + white * 0.310_485_6;
            b[4] = 0.550_00 * b[4] + white * 0.532_952_2;
            b[5] = -0.761_6 * b[5] - white * 0.016_898_0;
            let pink = b[0] + b[1] + b[2] + b[3] + b[4] + b[5] + b[6] + white * 0.536_2;
            b[6] = white * 0.115_926;
            // The filter's gain is about 0.11: bring it to roughly unit scale before the final normalisation.
            #[allow(clippy::cast_possible_truncation)] // an `f32` signal from `f64` arithmetic
            {
                (pink * 0.11) as f32
            }
        })
        .collect();
    super::normalise(&mut out, amplitude);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_noise_is_bounded_and_repeatable() {
        let a = white_noise(4096, 7, 0.5);
        assert_eq!(a, white_noise(4096, 7, 0.5));
        assert_ne!(a, white_noise(4096, 8, 0.5));
        assert!(a.iter().all(|s| (-0.5..0.5).contains(s)));
        let mean = a.iter().sum::<f32>() / 4096.0;
        assert!(mean.abs() < 0.02, "mean {mean}");
    }

    #[test]
    fn pink_noise_has_more_low_than_high_energy() {
        // One-pole low-pass and its complement as a crude split at about 1.5 kHz: white noise has equal energy in each
        // half of the band, pink noise has several times more below.
        let split = |x: &[f32]| {
            let (mut lp, mut low, mut high) = (0.0f32, 0.0f32, 0.0f32);
            for &s in x {
                lp += 0.17 * (s - lp);
                low += lp * lp;
                high += (s - lp) * (s - lp);
            }
            low / high
        };
        let pink = split(&pink_noise(48_000, 3, 1.0));
        let white = split(&white_noise(48_000, 3, 1.0));
        assert!(pink > 3.0 * white, "pink {pink}, white {white}");
        assert!((super::super::peak(&pink_noise(1000, 3, 0.25)) - 0.25).abs() < 1e-6);
    }
}
