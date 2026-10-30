//! The One-Euro filter (Casiez et al. 2012) over 3D points: steady when still, little lag when moving. The body
//! solver filters landmarks in metres before it turns them into rotations, so a still hand holds still.

use omuzaki_core::glam::Vec3;

/// How strongly to smooth: the cutoff when still (Hz) and how fast it rises with speed (per metre per second).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct EuroParams {
    /// Cutoff when nothing moves, Hz: lower is steadier.
    pub min_cutoff: f32,
    /// How fast the cutoff rises with speed: higher follows fast moves with less lag.
    pub beta: f32,
}

/// Cutoff of the speed estimate's own low-pass, Hz.
const DERIVATIVE_CUTOFF: f32 = 1.0;

fn alpha(cutoff: f32, dt: f32) -> f32 {
    let tau = 1.0 / (std::f32::consts::TAU * cutoff);
    1.0 / (1.0 + tau / dt)
}

/// One filtered point.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct EuroPoint {
    value: Option<Vec3>,
    speed: Vec3,
}

impl EuroPoint {
    /// Filters `raw`, `dt` seconds after the last sample. A non-finite sample or `dt` resets the point to `raw`.
    pub fn filter(&mut self, raw: Vec3, dt: f32, params: EuroParams) -> Vec3 {
        let Some(previous) = self.value.filter(|_| dt.is_finite() && dt > 0.0 && raw.is_finite()) else {
            self.value = raw.is_finite().then_some(raw);
            self.speed = Vec3::ZERO;
            return raw;
        };
        let speed = (raw - previous) / dt;
        self.speed = self.speed.lerp(speed, alpha(DERIVATIVE_CUTOFF, dt));
        let cutoff = params.min_cutoff + params.beta * self.speed.length();
        let value = previous.lerp(raw, alpha(cutoff, dt));
        self.value = Some(value);
        value
    }

    /// Forgets the history: the next sample passes through.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARAMS: EuroParams = EuroParams { min_cutoff: 1.0, beta: 5.0 };

    #[test]
    fn the_first_sample_passes_and_a_still_point_stays() {
        let mut p = EuroPoint::default();
        assert_eq!(p.filter(Vec3::X, 1.0 / 30.0, PARAMS), Vec3::X);
        for _ in 0..30 {
            assert!(p.filter(Vec3::X, 1.0 / 30.0, PARAMS).abs_diff_eq(Vec3::X, 1e-6));
        }
    }

    #[test]
    fn jitter_is_damped_and_a_real_move_is_followed() {
        let mut p = EuroPoint::default();
        let mut worst = 0.0f32;
        for i in 0..60 {
            let jitter = if i % 2 == 0 { 0.002 } else { -0.002 };
            let out = p.filter(Vec3::new(jitter, 0.0, 0.0), 1.0 / 30.0, PARAMS);
            if i > 10 {
                worst = worst.max(out.x.abs());
            }
        }
        assert!(worst < 0.001, "2 mm of jitter is damped below 1 mm: {worst}");
        // A 30 cm step is mostly followed within a quarter second.
        let mut out = Vec3::ZERO;
        for _ in 0..8 {
            out = p.filter(Vec3::new(0.3, 0.0, 0.0), 1.0 / 30.0, PARAMS);
        }
        assert!(out.x > 0.25, "{out}");
    }

    #[test]
    fn a_bad_sample_resets_instead_of_poisoning() {
        let mut p = EuroPoint::default();
        p.filter(Vec3::X, 0.03, PARAMS);
        assert!(!p.filter(Vec3::NAN, 0.03, PARAMS).is_finite(), "the bad sample is reported as given");
        assert_eq!(p.filter(Vec3::Y, 0.03, PARAMS), Vec3::Y, "then the filter starts over");
    }
}
