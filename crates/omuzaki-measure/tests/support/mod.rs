//! Test support shared by the likeness tests: the synthetic faces (Appendix A) and helpers that measure them.

#![allow(dead_code, reason = "each test file uses its own share of the support")]

pub mod synth;

use omuzaki_measure::face::likeness::{Compare, Likeness, Side, Tolerances, likeness};
use omuzaki_measure::face::outline::ANGLES;
use omuzaki_measure::face::spacing::{FeatureSpacing, spacing};
use omuzaki_measure::face::{FaceLandmarks, FaceMeasure, LandmarkSource, MeasureInput, Refusal, measure_face};

use self::synth::{Drawn, Rng};

/// The landmarks of a drawn face where they truly are.
#[must_use]
pub fn true_landmarks(drawn: &Drawn) -> FaceLandmarks {
    FaceLandmarks {
        eye_right: drawn.eye_right(),
        eye_left: drawn.eye_left(),
        mouth: Some(drawn.mouth()),
        nose: None,
        source: LandmarkSource::Agent,
    }
}

/// The landmarks as clicked: each moved by Gaussian noise of `sd` IOD.
#[must_use]
pub fn clicked(drawn: &Drawn, rng: &mut Rng, sd: f64) -> FaceLandmarks {
    let px = sd * drawn.iod_px();
    FaceLandmarks {
        eye_right: rng.jitter(drawn.eye_right(), px),
        eye_left: rng.jitter(drawn.eye_left(), px),
        mouth: Some(rng.jitter(drawn.mouth(), px)),
        nose: None,
        source: LandmarkSource::Agent,
    }
}

/// Measures a drawn face with the given landmarks.
///
/// # Errors
/// The measure's refusal.
pub fn measure(drawn: &Drawn, landmarks: FaceLandmarks, refine: bool) -> Result<FaceMeasure, Refusal> {
    measure_face(&drawn.image, &MeasureInput { landmarks, refine, exclude: Vec::new(), skin: None })
}

/// A measurement with its spacing, ready to compare.
#[derive(Debug)]
pub struct Measured {
    /// The measurement.
    pub m: FaceMeasure,
    /// Its spacing.
    pub s: FeatureSpacing,
}

impl Measured {
    /// Measures `drawn` with `landmarks`.
    ///
    /// # Errors
    /// The measure's refusal.
    pub fn new(drawn: &Drawn, landmarks: FaceLandmarks, refine: bool) -> Result<Self, Refusal> {
        let m = measure(drawn, landmarks, refine)?;
        let s = spacing(&m);
        Ok(Self { m, s })
    }

    /// The side of a likeness comparison.
    #[must_use]
    pub fn side(&self) -> Side<'_> {
        Side {
            outline: &self.m.outline,
            spacing: &self.s,
            frame: Some(&self.m.frame),
            mouth_height: self.m.landmarks.mouth_height,
        }
    }

    /// The likeness of `self` (candidate) to `reference` under `compare`'s preset.
    ///
    /// # Panics
    /// When the two do not overlap enough to compare, which a test reports as a failure.
    #[must_use]
    #[allow(clippy::expect_used)]
    pub fn against(&self, reference: &Self, compare: Compare) -> Likeness {
        likeness(reference.side(), self.side(), Tolerances::preset(compare)).expect("the faces overlap")
    }

    /// The outline term `O` against `reference`, IOD.
    #[must_use]
    pub fn outline_to(&self, reference: &Self) -> f64 {
        self.against(reference, Compare::SameLook).terms[0].value
    }

    /// RMS and largest error of the `ok` samples against `truth`, IOD.
    #[must_use]
    pub fn error(&self, truth: &[f64; ANGLES]) -> (f64, f64) {
        let errors: Vec<f64> = (0..ANGLES).filter_map(|k| Some(self.m.outline.ok(k)? - truth[k])).collect();
        #[allow(clippy::cast_precision_loss)]
        let n = errors.len().max(1) as f64;
        let rms = (errors.iter().map(|e| e * e).sum::<f64>() / n).sqrt();
        (rms, errors.iter().fold(0.0, |a: f64, e| a.max(e.abs())))
    }
}

/// The value at quantile `q` of `values` (nearest rank).
#[must_use]
pub fn quantile(values: &[f64], q: f64) -> f64 {
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
    let i = ((q * v.len() as f64).ceil() as usize).clamp(1, v.len().max(1)) - 1;
    v.get(i).copied().unwrap_or(f64::NAN)
}

/// The standard deviation of `values`.
#[must_use]
pub fn sd(values: &[f64]) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let n = values.len() as f64;
    let mean = values.iter().sum::<f64>() / n;
    (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1.0).max(1.0)).sqrt()
}

/// Spearman's rank correlation of `ys` with `xs` (no ties expected).
#[must_use]
pub fn spearman(xs: &[f64], ys: &[f64]) -> f64 {
    let rank = |v: &[f64]| {
        let mut idx: Vec<usize> = (0..v.len()).collect();
        idx.sort_by(|&a, &b| v[a].total_cmp(&v[b]));
        let mut r = vec![0.0; v.len()];
        for (i, &k) in idx.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let rank = i as f64;
            r[k] = rank;
        }
        r
    };
    let (rx, ry) = (rank(xs), rank(ys));
    #[allow(clippy::cast_precision_loss)]
    let n = xs.len() as f64;
    let d2: f64 = rx.iter().zip(&ry).map(|(a, b)| (a - b).powi(2)).sum();
    1.0 - 6.0 * d2 / (n * (n * n - 1.0))
}
