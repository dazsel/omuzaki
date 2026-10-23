//! The viseme fallback (FE-08): a mouth frame from a stretch of audio alone, for keys the model missed or hasn't
//! decided yet, and for the head key of a model with latency (§4.2 1).

use omuzaki_anim_api::{Channel, ChannelBuf};

/// Turns a stretch of the voice into mouth channels.
pub trait Visemes: Send {
    /// The mouth frame of `pcm` (mono, `rate` Hz), written into `out`'s mouth channels (the rest is left alone).
    /// `pcm` may be empty (an interval before the voice or after it): that is a closed mouth.
    fn frame(&mut self, pcm: &[f32], rate: u32, out: &mut ChannelBuf);
}

/// The jaw scale: `jawOpen = min(1, JAW_PER_RMS × RMS)`, so a vowel at 0.3 peak opens it to 0.53.
pub const JAW_PER_RMS: f32 = 2.5;

/// Energy and spectral tilt: the jaw opens with the level, and a bright, hissy stretch (a fricative or an `ee`)
/// stretches the lips.
#[derive(Copy, Clone, Debug, Default)]
pub struct EnergyVisemes;

impl Visemes for EnergyVisemes {
    #[allow(clippy::cast_precision_loss)] // interval lengths are small
    fn frame(&mut self, pcm: &[f32], rate: u32, out: &mut ChannelBuf) {
        let (mut power, mut crossings) = (0.0f32, 0u32);
        for (i, &x) in pcm.iter().enumerate() {
            power += x * x;
            if i > 0 && (x >= 0.0) != (pcm[i - 1] >= 0.0) {
                crossings += 1;
            }
        }
        let n = pcm.len().max(1) as f32;
        let jaw = (JAW_PER_RMS * (power / n).sqrt()).min(1.0);
        // Zero crossings per second, halved, is the dominant frequency: above 2 kHz the stretch is a fricative or `ee`.
        let hz = crossings as f32 * rate as f32 / n / 2.0;
        let stretch = (((hz - 2_000.0) / 3_000.0).clamp(0.0, 1.0) * jaw).min(1.0);
        out.set(Channel::JAW_OPEN, jaw);
        out.set(Channel::MOUTH_STRETCH_LEFT, stretch);
        out.set(Channel::MOUTH_STRETCH_RIGHT, stretch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_jaw_follows_the_level_and_a_hiss_stretches() {
        let tone: Vec<f32> = (0..320).map(|i| 0.3 * (i as f32 * 0.0785).sin()).collect();
        let mut out = ChannelBuf::NEUTRAL;
        EnergyVisemes.frame(&tone, 16_000, &mut out);
        assert!((out.get(Channel::JAW_OPEN) - 0.530).abs() < 0.002, "{}", out.get(Channel::JAW_OPEN));
        assert_eq!(out.get(Channel::MOUTH_STRETCH_LEFT), 0.0);
        let hiss: Vec<f32> = (0..320).map(|i| if i % 2 == 0 { 0.1 } else { -0.1 }).collect();
        EnergyVisemes.frame(&hiss, 16_000, &mut out);
        assert!(out.get(Channel::MOUTH_STRETCH_LEFT) > 0.2);
        EnergyVisemes.frame(&[], 16_000, &mut out);
        assert_eq!(out.get(Channel::JAW_OPEN), 0.0);
    }
}
