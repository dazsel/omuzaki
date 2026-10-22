//! The five visemes and the vowel formants that pick them.
//!
//! Lip sync drives the five VRM mouth shapes (`aa`, `ih`, `ou`, `ee`, `oh`: the Japanese vowels a, i, u, e, o, which
//! `omuzaki-face-api` defines as expressions). Which one a sound is comes from its first two formants: F1 rises as the
//! jaw opens, F2 as the tongue moves forward. A [`VowelProfile`] holds one (F1, F2) point per vowel; the default is the
//! textbook mean of adult speakers, and the per-person vowel calibration (PF-08) replaces it with the person's own.

/// One of the five mouth shapes, in [`omuzaki_face_api::expression::VISEMES`] order.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Viseme {
    /// "a": the open jaw.
    Aa,
    /// "i": spread lips, nearly closed.
    Ih,
    /// "u": narrow, slightly forward.
    Ou,
    /// "e": spread, half open.
    Ee,
    /// "o": rounded, open.
    Oh,
}

impl Viseme {
    /// Every viseme, in [`omuzaki_face_api::expression::VISEMES`] order.
    pub const ALL: [Self; 5] = [Self::Aa, Self::Ih, Self::Ou, Self::Ee, Self::Oh];

    /// The expression id of its mouth shape (`"aa"` …).
    pub const fn id(self) -> &'static str {
        omuzaki_face_api::expression::VISEMES[self as usize]
    }

    /// The default (F1, F2) in Hz: the geometric mean of the usual adult male and female values for the Japanese
    /// vowel (male a 750/1200, i 300/2200, u 350/1300, e 470/1900, o 450/800; female a 900/1500, i 350/2800,
    /// u 400/1600, e 550/2300, o 500/900).
    pub const fn formants(self) -> [f32; 2] {
        match self {
            Self::Aa => [820.0, 1340.0],
            Self::Ih => [325.0, 2480.0],
            Self::Ou => [375.0, 1440.0],
            Self::Ee => [510.0, 2090.0],
            Self::Oh => [475.0, 850.0],
        }
    }

    /// Its index in [`Viseme::ALL`].
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// Spread of a vowel's region on each axis, in natural-log units of frequency: about ±25% on F1 and ±20% on F2, the
/// spread of one speaker's own vowels.
const SPREAD: [f32; 2] = [0.25, 0.2];

/// Where a person's vowels sit: one (F1, F2) point per viseme, in Hz.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct VowelProfile {
    points: [[f32; 2]; 5],
}

impl Default for VowelProfile {
    fn default() -> Self {
        Self { points: Viseme::ALL.map(Viseme::formants) }
    }
}

impl VowelProfile {
    /// The default profile with every formant scaled by `tract` (above 1 for a shorter vocal tract): the one-number
    /// fit a short calibration can make.
    pub fn scaled(tract: f32) -> Self {
        let tract = if tract.is_finite() && tract > 0.0 { tract } else { 1.0 };
        Self { points: Viseme::ALL.map(|v| v.formants().map(|f| f * tract)) }
    }

    /// With `viseme`'s point measured from the person (PF-08's vowel step).
    #[must_use]
    pub fn with(mut self, viseme: Viseme, f1: f32, f2: f32) -> Self {
        if f1.is_finite() && f2.is_finite() && f1 > 0.0 && f2 > 0.0 {
            self.points[viseme.index()] = [f1, f2];
        }
        self
    }

    /// The (F1, F2) point of `viseme`.
    pub fn point(&self, viseme: Viseme) -> [f32; 2] {
        self.points[viseme.index()]
    }

    /// How much the sound with formants `f1` and `f2` looks like each vowel: weights summing to at most 1, scaled down
    /// when the sound is far from every vowel (a consonant, a hum), so an unclear sound opens no particular shape.
    pub fn classify(&self, f1: f32, f2: f32) -> [f32; 5] {
        if !(f1 > 0.0 && f2 > 0.0 && f1.is_finite() && f2.is_finite()) {
            return [0.0; 5];
        }
        let (l1, l2) = (f1.ln(), f2.ln());
        let likeness = self.points.map(|[p1, p2]| {
            let d1 = (l1 - p1.ln()) / SPREAD[0];
            let d2 = (l2 - p2.ln()) / SPREAD[1];
            (-0.5 * (d1 * d1 + d2 * d2)).exp()
        });
        let total: f32 = likeness.iter().sum();
        if total <= 1e-6 {
            return [0.0; 5];
        }
        // Normalised shares, times how sure the best match is.
        let best = likeness.iter().fold(0.0f32, |m, &l| m.max(l));
        likeness.map(|l| l / total * best.sqrt())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_vowel_point_is_its_own_best_match() {
        let profile = VowelProfile::default();
        for viseme in Viseme::ALL {
            let [f1, f2] = viseme.formants();
            let weights = profile.classify(f1, f2);
            let best = (0..5).max_by(|&a, &b| weights[a].total_cmp(&weights[b])).unwrap();
            assert_eq!(best, viseme.index(), "{viseme:?}: {weights:?}");
            assert!(weights[best] > 0.6, "{viseme:?}: {weights:?}");
        }
        assert_eq!(profile.classify(0.0, 1000.0), [0.0; 5]);
        assert!(profile.classify(150.0, 6000.0).iter().sum::<f32>() < 0.05, "far from every vowel");
    }

    #[test]
    fn scaling_moves_every_point() {
        let scaled = VowelProfile::scaled(1.2);
        assert_eq!(scaled.point(Viseme::Aa), [820.0 * 1.2, 1340.0 * 1.2]);
        assert_eq!(VowelProfile::scaled(f32::NAN), VowelProfile::default());
        assert_eq!(VowelProfile::default().with(Viseme::Oh, 400.0, 700.0).point(Viseme::Oh), [400.0, 700.0]);
        assert_eq!(VowelProfile::default().with(Viseme::Oh, -1.0, 700.0), VowelProfile::default());
        assert_eq!(Viseme::Ou.id(), "ou");
    }
}
