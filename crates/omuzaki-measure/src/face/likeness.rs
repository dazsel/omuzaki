//! Likeness (design D7 and 4.4): a weighted distance between a reference face and a candidate face in units of
//! tolerance. **0 is identical, 1 is the weighted average difference at the tolerance, lower is better.** The gate passes
//! when every term is within its own tolerance, so one term far out fails it even when the others pull the score down.

use omuzaki_l10n_api::Text;
use serde::{Deserialize, Serialize};

use super::frame::FaceFrame;
use super::outline::{ANGLES, OutlineProfile, angle, runs, weight};
use super::spacing::FeatureSpacing;
use crate::messages;

/// The share of the weighted samples that must be `ok` in both faces for the outline to be compared.
pub const MIN_COVERAGE: f64 = 0.6;

/// Every sample from 60° to 120° must be `ok` in both faces.
pub const REQUIRED: std::ops::RangeInclusive<usize> = 30..=54;

/// Two mouths whose heights differ by more than this factor are different expressions.
pub const EXPRESSION_RATIO: f64 = 2.0;

/// Which kinds of picture are compared, which picks the tolerances.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Compare {
    /// The same kind of picture on both sides: a render against a render.
    SameLook,
    /// A drawing against a render (the likeness gate of a character made from a sheet).
    #[default]
    CrossLook,
}

/// Each term's tolerance, IOD.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, schemars::JsonSchema)]
pub struct Tolerances {
    /// The outline's weighted RMS difference.
    pub outline: f64,
    /// Eye line to chin.
    pub eye_to_chin: f64,
    /// Eye line to mouth.
    pub eye_to_mouth: f64,
    /// The width at the mouth.
    pub width_at_mouth: f64,
    /// The width at the eye line.
    pub width_at_eyes: f64,
}

impl Tolerances {
    /// The preset of design D7's table.
    #[must_use]
    pub fn preset(compare: Compare) -> Self {
        match compare {
            Compare::SameLook => Self {
                outline: 0.010,
                eye_to_chin: 0.010,
                eye_to_mouth: 0.010,
                width_at_mouth: 0.020,
                width_at_eyes: 0.020,
            },
            Compare::CrossLook => Self {
                outline: 0.025,
                eye_to_chin: 0.030,
                eye_to_mouth: 0.030,
                width_at_mouth: 0.035,
                width_at_eyes: 0.035,
            },
        }
    }

    /// The preset with each given override in place.
    #[must_use]
    pub fn with(mut self, over: &ToleranceOverrides) -> Self {
        let pairs = [
            (&mut self.outline, over.outline),
            (&mut self.eye_to_chin, over.eye_to_chin),
            (&mut self.eye_to_mouth, over.eye_to_mouth),
            (&mut self.width_at_mouth, over.width_at_mouth),
            (&mut self.width_at_eyes, over.width_at_eyes),
        ];
        for (slot, given) in pairs {
            if let Some(v) = given {
                *slot = v;
            }
        }
        self
    }
}

/// Per-call tolerance overrides; an absent term keeps the preset's value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToleranceOverrides {
    /// The outline's tolerance, IOD.
    pub outline: Option<f64>,
    /// Eye line to chin, IOD.
    pub eye_to_chin: Option<f64>,
    /// Eye line to mouth, IOD.
    pub eye_to_mouth: Option<f64>,
    /// The width at the mouth, IOD.
    pub width_at_mouth: Option<f64>,
    /// The width at the eye line, IOD.
    pub width_at_eyes: Option<f64>,
}

/// One term of the distance.
#[derive(Clone, Debug, PartialEq, Serialize, schemars::JsonSchema)]
pub struct Term {
    /// `outline`, `eye_to_chin`, `eye_to_mouth`, `width_at_mouth` or `width_at_eyes`.
    pub id: &'static str,
    /// The reference's value, for a spacing term.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<f64>,
    /// The candidate's value, for a spacing term.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate: Option<f64>,
    /// The non-negative difference, IOD.
    pub value: f64,
    /// Its tolerance, IOD.
    pub tolerance: f64,
    /// Its weight in the score.
    pub weight: f64,
    /// Whether `value <= tolerance`.
    pub within: bool,
}

/// The contiguous run of samples that differ most.
#[derive(Clone, Debug, PartialEq, Serialize, schemars::JsonSchema)]
pub struct WorstSector {
    /// The first angle of the run, degrees.
    pub from_deg: f64,
    /// The last angle of the run, degrees.
    pub to_deg: f64,
    /// The mean signed difference over the run (`+`: the candidate is larger), IOD.
    pub mean: f64,
    /// The run's outline points on the reference image, pixels (empty for a target read from its cache).
    pub reference_points: Vec<[f64; 2]>,
    /// The run's outline points on the candidate image, pixels.
    pub candidate_points: Vec<[f64; 2]>,
}

/// The likeness of a candidate face to a reference face.
#[derive(Clone, Debug, PartialEq)]
pub struct Likeness {
    /// The weighted distance in units of tolerance; lower is better.
    pub score: f64,
    /// Whether every term is within its tolerance.
    pub within: bool,
    /// The terms present.
    pub terms: Vec<Term>,
    /// The weighted share of the samples `ok` in both faces.
    pub coverage: f64,
    /// The mean signed outline difference (`+`: the candidate's face is larger around the eyes' midpoint), IOD.
    pub mean_signed: f64,
    /// The candidate's radius less the reference's, per sample, where both are `ok`.
    pub per_angle: [Option<f64>; ANGLES],
    /// The run that differs most, when any sample differs by more than half the outline tolerance.
    pub worst: Option<WorstSector>,
    /// One sentence per term outside its tolerance.
    pub notes: Vec<Text>,
    /// The tolerances used.
    pub tolerances: Tolerances,
    /// Whether the mouths differ in height by more than [`EXPRESSION_RATIO`] (compare neutral faces).
    pub expression_differs: bool,
}

/// One face as the comparison sees it: its outline and spacing, and, when it was measured from its image now (not read
/// from a target's cache), its frame and its mouth's height.
#[derive(Clone, Copy, Debug)]
pub struct Side<'a> {
    /// The outline.
    pub outline: &'a OutlineProfile,
    /// The spacing.
    pub spacing: &'a FeatureSpacing,
    /// The face frame in the image's pixels, for the worst sector's points.
    pub frame: Option<&'a FaceFrame>,
    /// The mouth mark's height, IOD, for the expression check.
    pub mouth_height: Option<f64>,
}

/// Too little of the outline is `ok` in both faces: the sectors where it is missing, as `"62.5° to 80°"` strings.
#[derive(Clone, Debug, PartialEq)]
pub struct CoverageError {
    /// Where the outline is missing in either face.
    pub sectors: Vec<String>,
}

/// Formats an angle range, `"62.5° to 80°"`, or one angle.
pub(crate) fn sector_text(from: f64, to: f64) -> String {
    if (from - to).abs() < 1e-9 { format!("{from}°") } else { format!("{from}° to {to}°") }
}

/// Compares `candidate` with `reference` under `tolerances`.
///
/// # Errors
/// [`CoverageError`] when fewer than [`MIN_COVERAGE`] of the weighted samples, or not every sample from 60° to 120°, is
/// `ok` in both faces.
pub fn likeness(reference: Side<'_>, candidate: Side<'_>, tolerances: Tolerances) -> Result<Likeness, CoverageError> {
    let (ref_s, cand_s) = (reference.spacing, candidate.spacing);
    let both = |k: usize| Some((reference.outline.ok(k)?, candidate.outline.ok(k)?));
    let total: f64 = (0..ANGLES).map(weight).sum();
    let covered: f64 = (0..ANGLES).filter(|&k| both(k).is_some()).map(weight).sum();
    let coverage = covered / total;
    if coverage < MIN_COVERAGE || REQUIRED.into_iter().any(|k| both(k).is_none()) {
        let sectors = runs(|k| both(k).is_none()).into_iter().map(|(a, b)| sector_text(a, b)).collect();
        return Err(CoverageError { sectors });
    }
    let per_angle: [Option<f64>; ANGLES] = std::array::from_fn(|k| both(k).map(|(r, c)| c - r));
    let (mut sum, mut weights, mut signed, mut n) = (0.0, 0.0, 0.0, 0.0);
    for (k, diff) in per_angle.iter().enumerate() {
        if let Some(diff) = diff {
            sum += weight(k) * diff * diff;
            weights += weight(k);
            signed += diff;
            n += 1.0;
        }
    }
    let outline_value = (sum / weights).sqrt();
    let mut terms = vec![term("outline", None, None, outline_value, tolerances.outline, 2.0)];
    let pairs = [
        ("eye_to_chin", Some(ref_s.eye_to_chin), Some(cand_s.eye_to_chin), tolerances.eye_to_chin),
        ("eye_to_mouth", ref_s.eye_to_mouth, cand_s.eye_to_mouth, tolerances.eye_to_mouth),
        ("width_at_mouth", ref_s.width_at_mouth, cand_s.width_at_mouth, tolerances.width_at_mouth),
        ("width_at_eyes", ref_s.width_at_eyes, cand_s.width_at_eyes, tolerances.width_at_eyes),
    ];
    for (id, r, c, tolerance) in pairs {
        if let (Some(r), Some(c)) = (r, c) {
            terms.push(term(id, Some(r), Some(c), (c - r).abs(), tolerance, 1.0));
        }
    }
    let score_weights: f64 = terms.iter().map(|t| t.weight).sum();
    let score = (terms.iter().map(|t| t.weight * (t.value / t.tolerance).powi(2)).sum::<f64>() / score_weights).sqrt();
    let within = terms.iter().all(|t| t.within);
    let worst = worst_sector(&per_angle, tolerances.outline, reference, candidate);
    let notes = notes(&terms, worst.as_ref());
    let expression_differs = match (reference.mouth_height, candidate.mouth_height) {
        (Some(a), Some(b)) if a > 0.0 && b > 0.0 => a.max(b) / a.min(b) > EXPRESSION_RATIO,
        _ => false,
    };
    Ok(Likeness {
        score,
        within,
        terms,
        coverage,
        mean_signed: signed / n,
        per_angle,
        worst,
        notes,
        tolerances,
        expression_differs,
    })
}

fn term(
    id: &'static str,
    reference: Option<f64>,
    candidate: Option<f64>,
    value: f64,
    tolerance: f64,
    weight: f64,
) -> Term {
    Term { id, reference, candidate, value, tolerance, weight, within: value <= tolerance }
}

/// The contiguous run of samples differing by more than half the outline tolerance with the largest summed excess.
fn worst_sector(
    per_angle: &[Option<f64>; ANGLES],
    tolerance: f64,
    reference: Side<'_>,
    candidate: Side<'_>,
) -> Option<WorstSector> {
    let half = tolerance / 2.0;
    let mut best: Option<(f64, usize, usize)> = None;
    let mut k = 0;
    while k < ANGLES {
        if per_angle[k].is_some_and(|d| d.abs() > half) {
            let start = k;
            let mut excess = 0.0;
            while k < ANGLES
                && let Some(d) = per_angle[k].filter(|d| d.abs() > half)
            {
                excess += d.abs() - half;
                k += 1;
            }
            if best.is_none_or(|(e, _, _)| excess > e) {
                best = Some((excess, start, k - 1));
            }
        } else {
            k += 1;
        }
    }
    let (_, from, to) = best?;
    #[allow(clippy::cast_precision_loss)] // at most 85 samples
    let mean = (from..=to).filter_map(|k| per_angle[k]).sum::<f64>() / (to - from + 1) as f64;
    let points = |side: Side<'_>| -> Vec<[f64; 2]> {
        let Some(frame) = side.frame else { return Vec::new() };
        (from..=to).filter_map(|k| Some(frame.along(angle(k), side.outline.ok(k)? * frame.iod_px))).collect()
    };
    Some(WorstSector {
        from_deg: angle(from),
        to_deg: angle(to),
        mean,
        reference_points: points(reference),
        candidate_points: points(candidate),
    })
}

/// One note per term outside its tolerance.
fn notes(terms: &[Term], worst: Option<&WorstSector>) -> Vec<Text> {
    let pct = |v: f64| (v * 1000.0).round() / 10.0;
    terms
        .iter()
        .filter(|t| !t.within)
        .filter_map(|t| {
            let signed = t.candidate.zip(t.reference).map_or(0.0, |(c, r)| c - r);
            let amount = pct(t.value);
            Some(match t.id {
                "outline" => {
                    let (from, to) = worst.map_or((angle(0), angle(ANGLES - 1)), |w| (w.from_deg, w.to_deg));
                    messages::note_outline(amount, sector_text(from, to))
                }
                "eye_to_chin" if signed > 0.0 => messages::note_lower_face_longer(amount),
                "eye_to_chin" => messages::note_lower_face_shorter(amount),
                "eye_to_mouth" if signed > 0.0 => messages::note_mouth_lower(amount),
                "eye_to_mouth" => messages::note_mouth_higher(amount),
                "width_at_mouth" if signed > 0.0 => messages::note_wider_at_mouth(amount),
                "width_at_mouth" => messages::note_narrower_at_mouth(amount),
                "width_at_eyes" if signed > 0.0 => messages::note_wider_at_eyes(amount),
                "width_at_eyes" => messages::note_narrower_at_eyes(amount),
                _ => return None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_presets_are_design_d7s_table_and_overrides_replace_only_their_term() {
        let cross = Tolerances::preset(Compare::CrossLook);
        assert_eq!((cross.outline, cross.eye_to_chin, cross.width_at_eyes), (0.025, 0.030, 0.035));
        let same = Tolerances::preset(Compare::SameLook);
        assert_eq!((same.outline, same.eye_to_mouth, same.width_at_mouth), (0.010, 0.010, 0.020));
        let tight = cross.with(&ToleranceOverrides { outline: Some(0.02), ..ToleranceOverrides::default() });
        assert_eq!((tight.outline, tight.eye_to_chin), (0.02, 0.030));
        assert_eq!(Compare::default(), Compare::CrossLook);
    }

    #[test]
    fn sectors_read_as_ranges() {
        assert_eq!(sector_text(62.5, 80.0), "62.5° to 80°");
        assert_eq!(sector_text(90.0, 90.0), "90°");
    }
}
