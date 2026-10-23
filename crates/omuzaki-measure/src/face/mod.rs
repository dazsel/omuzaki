//! The face measures of VF-01 (design 4): one image and its landmarks in, the outline, the spacing and the warnings
//! out. A reference and a render go through exactly this code (D1).
//!
//! | Module | Holds |
//! |---|---|
//! | [`frame`] | the face frame (eye midpoint, eye line, IOD) and the region of interest |
//! | [`segment`] | the lit skin, the line, the face region and its holes, the background colour |
//! | [`snap`] | the windowed mean shift that moves given landmarks onto the drawn marks |
//! | [`outline`] | the 85-ray radial profile, its states and the leak guard |
//! | [`spacing`] | the IOD ratios of 4.3 and the layout ratios |
//! | [`likeness`] | the terms, the score, the gate, the worst sector and the notes of D7 |

pub mod draw;
pub mod frame;
pub mod likeness;
pub mod outline;
pub mod segment;
pub mod snap;
pub mod spacing;

use omuzaki_l10n_api::Text;
use omuzaki_store_api::ContentHash;
use serde::{Deserialize, Serialize};

use self::frame::{FaceFrame, roi};
use self::outline::{CHIN, OutlineProfile, SampleState, Sector, angle};
use self::segment::{SegmentError, segment};
use self::snap::{Given, SnappedLandmarks, snap};
use crate::image::Rgba8;
use crate::messages;

/// The version of the measures. A target's cached numbers from another version are stale.
pub const MEASURE_VERSION: u32 = 1;

/// The smallest IOD a measure reads, pixels.
pub const MIN_IOD: f64 = 64.0;

/// Above this asymmetry the face looks turned.
pub const YAW_WARNING: f64 = 0.06;

/// Below this line contrast (`L*`) a reference's measurement may be less repeatable.
pub const FAINT_LINE: f64 = 25.0;

/// Who placed the landmarks. Recorded, never used to branch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LandmarkSource {
    /// Clicked by a person.
    Person,
    /// Given by an agent that can see.
    #[default]
    Agent,
    /// Reported by the renderer.
    Render,
    /// From a landmark model (reserved).
    Detector,
}

/// Image points in pixels: origin top-left, x right, y down, pixel centres at `.5`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FaceLandmarks {
    /// The character's right eye (on the image's left in a front view).
    pub eye_right: [f64; 2],
    /// The character's left eye.
    pub eye_left: [f64; 2],
    /// Anywhere on the mouth; the snap finds its top edge. Absent: no mouth terms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mouth: Option<[f64; 2]>,
    /// The nose mark. Absent: no `nose` ratio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nose: Option<[f64; 2]>,
    /// Who placed them: `person`, `agent` (the default), `render` or `detector`.
    #[serde(default)]
    pub source: LandmarkSource,
}

impl FaceLandmarks {
    /// The points the measure starts from.
    #[must_use]
    pub fn given(&self) -> Given {
        Given { eye_right: self.eye_right, eye_left: self.eye_left, mouth: self.mouth, nose: self.nose }
    }
}

/// What a measurement takes besides the image.
#[derive(Clone, Debug, PartialEq)]
pub struct MeasureInput {
    /// The landmarks.
    pub landmarks: FaceLandmarks,
    /// Snap the landmarks to the drawn marks.
    pub refine: bool,
    /// Sectors to leave out.
    pub exclude: Vec<Sector>,
    /// The lit skin colour (CIELAB), when given instead of estimated.
    pub skin: Option<[f64; 3]>,
}

/// A warning: the face was measured, with something to know.
#[derive(Clone, Debug, PartialEq, Serialize, schemars::JsonSchema)]
pub struct Warning {
    /// `snap_failed`, `yaw`, `yaw-unchecked`, `occluded`, `faint_line`, `expression` or `downscaled`.
    pub code: &'static str,
    /// What it means here, in English.
    pub detail: String,
}

/// One measured face.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceMeasure {
    /// The image's hash.
    pub hash: ContentHash,
    /// The image's size, pixels.
    pub size: [u32; 2],
    /// The face frame of the snapped eyes, in image pixels.
    pub frame: FaceFrame,
    /// The region of interest's downscale factor.
    pub downscale: u32,
    /// The landmarks as given and as used.
    pub landmarks: SnappedLandmarks,
    /// The outline.
    pub outline: OutlineProfile,
    /// The lit skin, CIELAB.
    pub skin: [f64; 3],
    /// The lit skin's lightness less the line's, when a line was found.
    pub line_contrast: Option<f64>,
    /// The warnings.
    pub warnings: Vec<Warning>,
}

impl FaceMeasure {
    /// The mouth's top edge below the eye line, IOD, when a mouth was given.
    #[must_use]
    pub fn mouth_y(&self) -> Option<f64> {
        Some(self.frame.to_face(self.landmarks.mouth?.used)[1])
    }

    /// The nose below the eye line, IOD, when a nose was given.
    #[must_use]
    pub fn nose_y(&self) -> Option<f64> {
        Some(self.frame.to_face(self.landmarks.nose?.used)[1])
    }

    /// The chin: its distance below the eye midpoint (IOD) and its image point.
    #[must_use]
    pub fn chin(&self) -> (f64, [f64; 2]) {
        let r = self.outline.ok(CHIN).unwrap_or(0.0);
        (r, self.frame.along(angle(CHIN), r * self.frame.iod_px))
    }
}

/// Why a face could not be measured: the argument at fault and the sentence (design 7.3).
#[derive(Clone, Debug, PartialEq)]
pub struct Refusal {
    /// The argument at fault, relative to the subject (`landmarks.eye_left`, `image`).
    pub field: &'static str,
    /// The message.
    pub text: Text,
}

impl Refusal {
    fn new(field: &'static str, text: Text) -> Self {
        Self { field, text }
    }
}

/// Measures one face.
///
/// # Errors
/// The refusals of design 7.3: too small, a landmark outside, the eyes swapped, no skin, low contrast, a leak, no chin.
pub fn measure_face(image: &Rgba8, input: &MeasureInput) -> Result<FaceMeasure, Refusal> {
    let lm = &input.landmarks;
    let (w, h) = (f64::from(image.width), f64::from(image.height));
    let points = [
        ("landmarks.eye_right", "right eye", Some(lm.eye_right)),
        ("landmarks.eye_left", "left eye", Some(lm.eye_left)),
        ("landmarks.mouth", "mouth", lm.mouth),
        ("landmarks.nose", "nose", lm.nose),
    ];
    for (field, name, p) in points {
        if let Some([x, y]) = p
            && !(x.is_finite() && y.is_finite() && (0.0..=w).contains(&x) && (0.0..=h).contains(&y))
        {
            return Err(Refusal::new(field, messages::landmark_outside(name)));
        }
    }
    let too_small = |d: f64| Refusal::new("landmarks", messages::too_small((d * 10.0).round() / 10.0, 64));
    let given_frame = FaceFrame::from_eyes(lm.eye_right, lm.eye_left).ok_or_else(|| too_small(0.0))?;
    if given_frame.iod_px < MIN_IOD {
        return Err(too_small(given_frame.iod_px));
    }
    if let Some(mouth) = lm.mouth
        && given_frame.to_face(mouth)[1] <= 0.0
    {
        return Err(Refusal::new("landmarks.mouth", messages::eyes_swapped()));
    }
    let roi = roi(image, &given_frame);
    let scale = f64::from(roi.scale);
    let seg = segment(&roi, &given_frame.mapped(roi.offset, scale), input.skin).map_err(|e| match e {
        SegmentError::NoSkin => Refusal::new("landmarks", messages::no_skin()),
        SegmentError::LowContrast(c) => Refusal::new("image", messages::low_contrast((c * 10.0).round() / 10.0, 15)),
    })?;
    let landmarks = snap(&lm.given(), &roi, &seg, input.refine);
    let frame = FaceFrame::from_eyes(landmarks.eye_right.used, landmarks.eye_left.used).unwrap_or(given_frame);
    let outline = outline::outline(&roi, &seg, &frame.mapped(roi.offset, scale), &input.exclude);
    if let Some(&(from, to)) = outline.sectors(SampleState::Leak).first() {
        return Err(Refusal::new("image", messages::leak(likeness::sector_text(from, to))));
    }
    if outline.state[CHIN] != SampleState::Ok {
        return Err(Refusal::new("image", messages::no_chin()));
    }
    let line_contrast = seg.line_l.map(|l| seg.skin[0] - l);
    let mut m = FaceMeasure {
        hash: image.hash,
        size: [image.width, image.height],
        frame,
        downscale: roi.scale,
        landmarks,
        outline,
        skin: seg.skin,
        line_contrast,
        warnings: Vec::new(),
    };
    m.warnings = warnings(&m, input.refine);
    Ok(m)
}

/// The warnings of design 7.3 that one face can raise.
fn warnings(m: &FaceMeasure, refine: bool) -> Vec<Warning> {
    let mut out = Vec::new();
    let mut push = |code: &'static str, detail: String| out.push(Warning { code, detail });
    let lm = &m.landmarks;
    if refine {
        let named = [
            ("right eye", Some(lm.eye_right)),
            ("left eye", Some(lm.eye_left)),
            ("mouth", lm.mouth),
            ("nose", lm.nose),
        ];
        for (name, _) in named.into_iter().filter(|(_, s)| s.is_some_and(|s| !s.snapped)) {
            push("snap_failed", format!("the {name} was not snapped to a drawn mark; the given point is used"));
        }
    }
    match m.outline.asymmetry() {
        None => push(
            "yaw-unchecked",
            "fewer than 8 of the 24 mirror pairs are usable, so the turn is not checked".to_owned(),
        ),
        Some(a) if a > YAW_WARNING => {
            push("yaw", format!("the face looks turned (asymmetry {a:.3}); measure a front view"));
        }
        Some(_) => {}
    }
    let occluded = m.outline.sectors(SampleState::Occluded);
    if !occluded.is_empty() {
        let count = m.outline.state.iter().filter(|s| **s == SampleState::Occluded).count();
        let sectors: Vec<String> = occluded.iter().map(|&(a, b)| likeness::sector_text(a, b)).collect();
        push("occluded", format!("{count} samples are covered by hair: {}", sectors.join(", ")));
    }
    if let Some(c) = m.line_contrast
        && c < FAINT_LINE
    {
        push("faint_line", format!("the line contrast is {c:.1}; reference measurements may be less repeatable"));
    }
    if m.downscale > 1 {
        push("downscaled", format!("the region of interest was downscaled by {}", m.downscale));
    }
    out
}
