//! Feature spacing (design D6 and 4.3): eye line to chin, to mouth, mouth to chin, the face's widths and aspect, all in
//! IOD, and the layout ratios that `face.layout.measure` compares against a character's layout.
//!
//! A field that cannot be measured is absent with a reason, never a guess.

use std::collections::BTreeMap;

use super::FaceMeasure;
use super::outline::{ANGLES, CHIN, angle};

/// The heights `width_max` is taken over, from the eye line to the chin.
pub const WIDTH_HEIGHTS: usize = 41;

/// What 4.3 reports for one face.
#[derive(Clone, Debug, PartialEq, serde::Serialize, schemars::JsonSchema)]
pub struct FeatureSpacing {
    /// The eye line to the chin, IOD (`LayoutRatios::drop_to_iod`).
    pub eye_to_chin: f64,
    /// The eye line to the mouth's top edge, IOD.
    pub eye_to_mouth: Option<f64>,
    /// The mouth to the chin, IOD.
    pub mouth_to_chin: Option<f64>,
    /// The mouth's height as a fraction of eye-to-chin (0 the eye line, 1 the chin).
    pub mouth: Option<f64>,
    /// The nose's height as a fraction of eye-to-chin.
    pub nose: Option<f64>,
    /// The face's width at the eye line, IOD.
    pub width_at_eyes: Option<f64>,
    /// The face's width at the mouth, IOD.
    pub width_at_mouth: Option<f64>,
    /// The largest width over 41 heights from the eye line to the chin, IOD.
    pub width_max: f64,
    /// `eye_to_chin / width_max`: larger is longer and narrower, "more oval".
    pub aspect: f64,
    /// Each field that could not be measured, with why (English; the op localises).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub unmeasured: BTreeMap<String, String>,
}

/// The ratios of a face's layout that an image gives: the ones `face.layout.measure {reference}` takes.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, schemars::JsonSchema)]
pub struct LayoutRatiosPartial {
    /// Eye line to chin, IOD.
    pub drop_to_iod: f64,
    /// The mouth's height as a fraction of eye-to-chin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mouth: Option<f64>,
    /// The nose's height as a fraction of eye-to-chin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nose: Option<f64>,
}

/// The `ok` samples of one side as points `(x, y)` in IOD, in order from the eye line down to the chin: the image's
/// right (`φ <= 90°`, walking up from the first sample) or its left (`φ >= 90°`, walking down from the last).
fn side(m: &FaceMeasure, right: bool) -> Vec<Option<[f64; 2]>> {
    let ks: Vec<usize> = if right { (0..=CHIN).collect() } else { (CHIN..ANGLES).rev().collect() };
    ks.into_iter()
        .map(|k| {
            let r = m.outline.ok(k)?;
            let (sin, cos) = angle(k).to_radians().sin_cos();
            Some([r * cos, r * sin])
        })
        .collect()
}

/// `x` where a side's polyline (consecutive `ok` samples only) first crosses the height `y`.
fn x_at(points: &[Option<[f64; 2]>], y: f64) -> Option<f64> {
    points.windows(2).find_map(|pair| {
        let (a, b) = (pair[0]?, pair[1]?);
        let (lo, hi) = if a[1] <= b[1] { (a[1], b[1]) } else { (b[1], a[1]) };
        if !(lo..=hi).contains(&y) {
            return None;
        }
        if (b[1] - a[1]).abs() < f64::EPSILON {
            return Some(a[0]);
        }
        Some(a[0] + (b[0] - a[0]) * (y - a[1]) / (b[1] - a[1]))
    })
}

/// The width at height `y`: the right side's crossing less the left side's.
fn width(right: &[Option<[f64; 2]>], left: &[Option<[f64; 2]>], y: f64) -> Option<f64> {
    Some(x_at(right, y)? - x_at(left, y)?)
}

/// The spacing of a measured face. The chin is `ok` (the measure refuses otherwise).
#[must_use]
pub fn spacing(m: &FaceMeasure) -> FeatureSpacing {
    let mut unmeasured = BTreeMap::new();
    let eye_to_chin = m.outline.ok(CHIN).unwrap_or(0.0);
    let (right, left) = (side(m, true), side(m, false));
    let eye_to_mouth = m.mouth_y();
    if eye_to_mouth.is_none() {
        unmeasured.insert("eye_to_mouth".to_owned(), "no mouth point given".to_owned());
    }
    let nose = m.nose_y().map(|y| y / eye_to_chin);
    if nose.is_none() {
        unmeasured.insert("nose".to_owned(), "no nose point given".to_owned());
    }
    let width_at_eyes = width(&right, &left, 0.0);
    if width_at_eyes.is_none() {
        unmeasured.insert("width_at_eyes".to_owned(), "the outline is not ok on both sides at the eye line".to_owned());
    }
    let width_at_mouth = eye_to_mouth.and_then(|y| width(&right, &left, y));
    if eye_to_mouth.is_some() && width_at_mouth.is_none() {
        unmeasured.insert("width_at_mouth".to_owned(), "the outline is not ok on both sides at the mouth".to_owned());
    }
    #[allow(clippy::cast_precision_loss)] // 41 heights
    let widths: Vec<f64> = (0..WIDTH_HEIGHTS)
        .filter_map(|i| width(&right, &left, eye_to_chin * i as f64 / (WIDTH_HEIGHTS - 1) as f64))
        .collect();
    let width_max = widths.iter().copied().fold(0.0_f64, f64::max);
    if widths.len() < WIDTH_HEIGHTS {
        unmeasured.insert(
            "width_max".to_owned(),
            format!(
                "the largest of {} of the {WIDTH_HEIGHTS} heights; the others are not ok on both sides",
                widths.len()
            ),
        );
    }
    FeatureSpacing {
        eye_to_chin,
        eye_to_mouth,
        mouth_to_chin: eye_to_mouth.map(|y| eye_to_chin - y),
        mouth: eye_to_mouth.map(|y| y / eye_to_chin),
        nose,
        width_at_eyes,
        width_at_mouth,
        width_max,
        aspect: if width_max > 0.0 { eye_to_chin / width_max } else { 0.0 },
        unmeasured,
    }
}

/// The layout ratios the spacing gives (the fields measured).
#[must_use]
pub fn layout_ratios(s: &FeatureSpacing) -> LayoutRatiosPartial {
    LayoutRatiosPartial { drop_to_iod: s.eye_to_chin, mouth: s.mouth, nose: s.nose }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_crossing_interpolates_and_a_gap_breaks_the_polyline() {
        let line = vec![Some([1.0, 0.0]), Some([0.8, 0.4]), None, Some([0.2, 0.9]), Some([0.0, 1.0])];
        assert!((x_at(&line, 0.2).unwrap() - 0.9).abs() < 1e-12);
        assert!(x_at(&line, 0.6).is_none(), "the gap between 0.4 and 0.9 is not crossed");
        assert!((x_at(&line, 0.95).unwrap() - 0.1).abs() < 1e-12);
        let mirrored: Vec<Option<[f64; 2]>> = line.iter().map(|p| p.map(|[x, y]| [-x, y])).collect();
        assert!((width(&line, &mirrored, 0.2).unwrap() - 1.8).abs() < 1e-12);
    }
}
