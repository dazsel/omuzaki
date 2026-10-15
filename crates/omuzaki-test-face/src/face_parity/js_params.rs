//! The crosswalk from the JS prototype's face parameters to the Rust painter's inputs (`docs/design/char-face-api.md`,
//! "Prototype crosswalk").
//!
//! The prototype's face part (`face.js`) keeps one flat record `p` of absolute numbers, a camera-follow gaze (`look`),
//! a driven gaze (`lookD`) and an eye reflection offset (`reflV`). The golden dumps (`state/<variant>/face-state.json`)
//! hold all of them per case. [`paint_params`] and [`face_view`] turn them into the painter's [`PaintParams`] and
//! [`FaceView`]; [`apply_style`] sets the few `p` entries that belong to the *design* (eye scale, lip gloss, ...) on a
//! [`FaceProfile`]. A normal module: the M1 runner reuses it to drive the painter from the same records.
//!
//! Left and right are the character's own in both worlds (`L` is +x), so nothing is mirrored.

use std::collections::BTreeMap;

use omuzaki_core::glam::Vec2;
use omuzaki_core::time::Seconds;
use omuzaki_face_api::{
    BrowParams, EyeEffects, EyeParams, FaceProfile, FaceView, MouthParams, NoseKind, PaintParams, narrow,
};

use omuzaki_test::jsref::{FaceCase, Xy};

/// A smile-lid strength above this draws the lower lid as a full lash line (`eyes.lower_lid.cheek_line`).
const SMILE_LIDS_ON: f64 = 0.5;

/// The prototype draws its nose when `p.nose` is above this.
const NOSE_ON: f64 = 0.01;

/// A wink closes an eye when its value is above this (`face.js` `eye()`).
const WINK_MIN: f64 = 0.01;

/// A wink draws as a happy arc when its value is above this and `winkArc` is above [`WINK_ARC_ON`].
const WINK_ARC_MIN: f64 = 0.55;

/// `winkArc` above this means a wink is a ^ arc rather than a relaxed lash line.
const WINK_ARC_ON: f64 = 0.5;

/// A face record that cannot be mapped.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParamError {
    /// The record lacks a parameter the prototype always writes.
    #[error("the face record has no `{0}`")]
    Missing(&'static str),
}

/// Reads one parameter of the record.
fn get(p: &BTreeMap<String, f64>, key: &'static str) -> Result<f64, ParamError> {
    p.get(key).copied().ok_or(ParamError::Missing(key))
}

/// One eye of `face.js` `eye(side, P0)`: per-side openness and lower-lid deltas, and the wink.
///
/// `wink` is the side's `winkL` or `winkR`. A wink closes the lid (`close`) and, with `winkArc` on, draws it as the
/// happy arc; `smileEye` draws both as arcs.
fn eye(
    p: &BTreeMap<String, f64>,
    wink: &'static str,
    open_delta: &'static str,
    low_lid_delta: &'static str,
) -> Result<EyeParams, ParamError> {
    let wink = get(p, wink)?;
    let smile_eye = get(p, "smileEye")?;
    let wink_arc = if wink > WINK_ARC_MIN && get(p, "winkArc")? > WINK_ARC_ON { 1.0 } else { 0.0 };
    Ok(EyeParams {
        open: narrow((get(p, "open")? + get(p, open_delta)?).max(0.0)),
        close: narrow(if wink > WINK_MIN { wink } else { 0.0 }),
        arc: narrow(smile_eye.max(wink_arc)),
        lower_lid: narrow((get(p, "lowLid")? + get(p, low_lid_delta)?).max(0.0)),
    })
}

/// One brow: the shared value plus the side's delta (`face.js` `brow()`).
fn brow(p: &BTreeMap<String, f64>, y_delta: &'static str, tilt_delta: &'static str) -> Result<BrowParams, ParamError> {
    Ok(BrowParams {
        height: narrow(get(p, "browY")? + get(p, y_delta)?),
        tilt: narrow(get(p, "browTilt")? + get(p, tilt_delta)?),
    })
}

/// The painter's gaze from the prototype's two gazes: the driven one plus the camera-follow one at `follow_weight`.
fn gaze(look: Xy, look_d: Xy, follow_weight: f64) -> Vec2 {
    Vec2::new(narrow(look_d.x + look.x * follow_weight), narrow(look_d.y + look.y * follow_weight))
}

/// The painter's [`PaintParams`] for the prototype's face record `p` and gazes.
///
/// | Prototype | [`PaintParams`] |
/// |---|---|
/// | `open` + `openL`/`openR` | `left_eye.open` / `right_eye.open` (never below 0) |
/// | `winkL`/`winkR` | `close` |
/// | `smileEye`, `winkArc` | `arc` (`smileEye`, or 1 for a wink above 0.55 with `winkArc` above 0.5) |
/// | `lowLid` + `lowLidL`/`lowLidR` | `lower_lid` |
/// | `tilt`, `pupil` | `eye_tilt`, `pupil` |
/// | `browY` + `browYL`/`browYR`, `browTilt` + `browTiltL`/`browTiltR`, `browArch` | `left_brow`/`right_brow`, `brow_arch` |
/// | `mOpen`, `mWide`, `mRound`, `pucker`, `mPress`, `mX` | `mouth.{open, wide, round, pucker, press, shift}` |
/// | `mSmile` + `mSmileL`/`mSmileR` | `mouth.smile_left` / `smile_right` |
/// | `teeth`, `teethLow`, `fang`, `tongue` | `mouth.{teeth_upper, teeth_lower, fangs, tongue}` |
/// | `blush` | `blush` |
/// | `tear`, `heart`, `spark` | `effects.{tears, heart, sparkle}` |
/// | `look` * `follow_weight` + `lookD` | `gaze` |
///
/// `follow_weight` says how much of the camera-follow gaze `look` counts as driven gaze. The prototype moves the iris
/// by a fixed distance per unit of each, tuned to its character's eye, so the number comes from the private setup file
/// ([`Setup::follow_weight`](super::Setup)), never from code.
///
/// The effects the prototype has no counterpart for (`star`, `gloom`, `shock`, `squeeze`, `spiral`), `vergence`,
/// `cheek_puff` and `sweat` stay 0.
///
/// # Errors
/// [`ParamError::Missing`] when `p` lacks a parameter the prototype always writes.
pub fn paint_params(
    p: &BTreeMap<String, f64>,
    look: Xy,
    look_d: Xy,
    follow_weight: f64,
) -> Result<PaintParams, ParamError> {
    let smile = get(p, "mSmile")?;
    Ok(PaintParams {
        left_eye: eye(p, "winkL", "openL", "lowLidL")?,
        right_eye: eye(p, "winkR", "openR", "lowLidR")?,
        eye_tilt: narrow(get(p, "tilt")?),
        pupil: narrow(get(p, "pupil")?),
        gaze: gaze(look, look_d, follow_weight),
        vergence: 0.0,
        effects: EyeEffects {
            heart: narrow(get(p, "heart")?),
            sparkle: narrow(get(p, "spark")?),
            tears: narrow(get(p, "tear")?),
            ..EyeEffects::default()
        },
        left_brow: brow(p, "browYL", "browTiltL")?,
        right_brow: brow(p, "browYR", "browTiltR")?,
        brow_arch: narrow(get(p, "browArch")?),
        mouth: MouthParams {
            open: narrow(get(p, "mOpen")?),
            wide: narrow(get(p, "mWide")?),
            smile_left: narrow(smile + get(p, "mSmileL")?),
            smile_right: narrow(smile + get(p, "mSmileR")?),
            round: narrow(get(p, "mRound")?),
            shift: narrow(get(p, "mX")?),
            press: narrow(get(p, "mPress")?),
            pucker: narrow(get(p, "pucker")?),
            teeth_upper: narrow(get(p, "teeth")?),
            teeth_lower: narrow(get(p, "teethLow")?),
            tongue: narrow(get(p, "tongue")?),
            fangs: narrow(get(p, "fang")?),
        },
        blush: narrow(get(p, "blush")?),
        cheek_puff: 0.0,
        sweat: 0.0,
    })
}

/// The painter's [`FaceView`]: the prototype's eye reflection offset (`reflV`) is the highlight shift, unchanged.
pub fn face_view(refl_v: Xy) -> FaceView {
    FaceView { highlight_shift: Vec2::new(narrow(refl_v.x), narrow(refl_v.y)) }
}

/// Sets the entries of `p` that the prototype keeps beside the animated ones but that describe the face's *design*,
/// on `profile`:
///
/// | Prototype | Profile |
/// |---|---|
/// | `eyeScale`, `eyeDrop` | `eyes.scale`, `eyes.drop` |
/// | `refl` | `eyes.highlights.shimmer` |
/// | `lips` | `mouth.lips.strength` |
/// | `nose` (above 0.01) | `nose.kind` (`tick`, else `none`) |
/// | `mScale`, `mScaleClosed` | `mouth.open_scale`, `mouth.closed_scale` |
/// | `smileLids` (above 0.5) | `eyes.lower_lid.cheek_line` |
///
/// # Errors
/// [`ParamError::Missing`] when `p` lacks one of them; `profile` is then unchanged.
pub fn apply_style(profile: &mut FaceProfile, p: &BTreeMap<String, f64>) -> Result<(), ParamError> {
    // Read everything first so a missing key leaves the profile untouched.
    let (eye_scale, eye_drop, refl) = (get(p, "eyeScale")?, get(p, "eyeDrop")?, get(p, "refl")?);
    let (lips, nose, smile_lids) = (get(p, "lips")?, get(p, "nose")?, get(p, "smileLids")?);
    let (open_scale, closed_scale) = (get(p, "mScale")?, get(p, "mScaleClosed")?);
    profile.eyes.scale = eye_scale;
    profile.eyes.drop = eye_drop;
    profile.eyes.highlights.shimmer = refl;
    profile.mouth.lips.strength = lips;
    profile.nose.kind = if nose > NOSE_ON { NoseKind::Tick } else { NoseKind::None };
    profile.mouth.open_scale = open_scale;
    profile.mouth.closed_scale = closed_scale;
    profile.eyes.lower_lid.cheek_line = smile_lids > SMILE_LIDS_ON;
    Ok(())
}

/// Everything the painter needs for one golden case, except the profile's base design.
#[derive(Debug, Clone)]
pub struct CaseInputs {
    /// The paint parameters.
    pub params: PaintParams,
    /// The view-dependent inputs.
    pub view: FaceView,
    /// The shot clock (`twT`, the twinkle clock of the prototype).
    pub time: Seconds,
}

/// [`paint_params`], [`face_view`] and the clock for one case of the face-state dump, and `base` with
/// [`apply_style`] applied.
///
/// # Errors
/// [`ParamError::Missing`] as the functions it calls.
pub fn case_inputs(
    case: &FaceCase,
    base: &FaceProfile,
    follow_weight: f64,
) -> Result<(CaseInputs, FaceProfile), ParamError> {
    let mut profile = base.clone();
    apply_style(&mut profile, &case.p)?;
    let inputs = CaseInputs {
        params: paint_params(&case.p, case.look, case.look_d, follow_weight)?,
        view: face_view(case.refl_v),
        time: Seconds(case.tw_t),
    };
    Ok((inputs, profile))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every key the prototype writes, each a distinct made-up number so a swapped mapping shows.
    fn record() -> BTreeMap<String, f64> {
        let keys = [
            "open",
            "tilt",
            "smileEye",
            "lowLid",
            "browY",
            "browTilt",
            "browArch",
            "mOpen",
            "mWide",
            "mSmile",
            "mRound",
            "teeth",
            "blush",
            "tear",
            "pupil",
            "spark",
            "winkL",
            "winkR",
            "fang",
            "teethLow",
            "mScaleClosed",
            "mScale",
            "pucker",
            "lips",
            "nose",
            "eyeScale",
            "eyeDrop",
            "refl",
            "heart",
            "openL",
            "openR",
            "lowLidL",
            "lowLidR",
            "winkArc",
            "browYL",
            "browYR",
            "browTiltL",
            "browTiltR",
            "mSmileL",
            "mSmileR",
            "mX",
            "mPress",
            "tongue",
            "smileLids",
        ];
        keys.iter().zip(1_u32..).map(|(k, i)| ((*k).to_owned(), 0.01 * f64::from(i))).collect()
    }

    fn with(entries: &[(&str, f64)]) -> BTreeMap<String, f64> {
        let mut p = record();
        for (k, v) in entries {
            p.insert((*k).to_owned(), *v);
        }
        p
    }

    const NO_GAZE: Xy = Xy { x: 0.0, y: 0.0 };

    fn params(p: &BTreeMap<String, f64>) -> PaintParams {
        paint_params(p, NO_GAZE, NO_GAZE, 0.0).unwrap()
    }

    #[test]
    fn open_and_the_per_side_deltas_make_each_eyes_openness() {
        let q = params(&with(&[("open", 1.0), ("openL", 0.3), ("openR", -0.4)]));
        assert_eq!(q.left_eye.open, 1.3);
        assert_eq!(q.right_eye.open, 0.6);
        // A squint cannot push the lid below shut.
        let q = params(&with(&[("open", 0.2), ("openR", -0.9)]));
        assert_eq!(q.right_eye.open, 0.0);
    }

    #[test]
    fn winks_close_their_own_eye_only() {
        let q = params(&with(&[("winkL", 0.9), ("winkR", 0.0)]));
        assert_eq!((q.left_eye.close, q.right_eye.close), (0.9, 0.0));
        // Below the prototype's threshold a wink does nothing.
        assert_eq!(params(&with(&[("winkL", 0.005)])).left_eye.close, 0.0);
    }

    #[test]
    fn arcs_come_from_smile_eye_or_a_wink_with_wink_arc() {
        let none = with(&[("smileEye", 0.0), ("winkL", 0.0), ("winkR", 0.0), ("winkArc", 1.0)]);
        let q = params(&none);
        assert_eq!((q.left_eye.arc, q.right_eye.arc), (0.0, 0.0));
        let q = params(&with(&[("smileEye", 0.7), ("winkL", 0.0), ("winkR", 0.0)]));
        assert_eq!((q.left_eye.arc, q.right_eye.arc), (0.7, 0.7));
        let q = params(&with(&[("smileEye", 0.0), ("winkL", 1.0), ("winkR", 0.0), ("winkArc", 1.0)]));
        assert_eq!((q.left_eye.arc, q.right_eye.arc), (1.0, 0.0));
        // winkArc off: a plain blink-style wink, no arc.
        let q = params(&with(&[("smileEye", 0.0), ("winkL", 1.0), ("winkR", 0.0), ("winkArc", 0.0)]));
        assert_eq!(q.left_eye.arc, 0.0);
        // A wink at or under 0.55 is not strong enough for an arc.
        let q = params(&with(&[("smileEye", 0.0), ("winkL", 0.5), ("winkR", 0.0), ("winkArc", 1.0)]));
        assert_eq!((q.left_eye.close, q.left_eye.arc), (0.5, 0.0));
    }

    #[test]
    fn lower_lids_add_the_side_delta_and_stay_non_negative() {
        let q = params(&with(&[("lowLid", 0.2), ("lowLidL", 0.1), ("lowLidR", -0.5)]));
        assert_eq!(q.left_eye.lower_lid, 0.3);
        assert_eq!(q.right_eye.lower_lid, 0.0);
    }

    #[test]
    fn tilt_pupil_and_brows() {
        let q = params(&with(&[
            ("tilt", 0.85),
            ("pupil", 0.75),
            ("browY", 0.2),
            ("browYL", 0.1),
            ("browYR", -0.3),
            ("browTilt", 1.0),
            ("browTiltL", 0.5),
            ("browTiltR", -0.25),
            ("browArch", 0.6),
        ]));
        assert_eq!((q.eye_tilt, q.pupil, q.brow_arch), (0.85, 0.75, 0.6));
        assert_eq!((q.left_brow.height, q.left_brow.tilt), (0.3, 1.5));
        assert_eq!((q.right_brow.height, q.right_brow.tilt), (-0.1, 0.75));
    }

    #[test]
    fn the_mouth_channels_keep_their_meaning() {
        let q = params(&with(&[
            ("mOpen", 0.42),
            ("mWide", 1.15),
            ("mRound", 0.8),
            ("pucker", 0.3),
            ("mPress", 0.6),
            ("mX", -0.4),
            ("teeth", 1.0),
            ("teethLow", 0.5),
            ("fang", 0.9),
            ("tongue", 0.7),
        ]));
        let m = q.mouth;
        assert_eq!((m.open, m.wide, m.round), (0.42, 1.15, 0.8));
        assert_eq!((m.pucker, m.press, m.shift), (0.3, 0.6, -0.4));
        assert_eq!((m.teeth_upper, m.teeth_lower, m.fangs, m.tongue), (1.0, 0.5, 0.9, 0.7));
    }

    #[test]
    fn corner_lifts_are_the_shared_smile_plus_each_corner() {
        let q = params(&with(&[("mSmile", -0.45), ("mSmileL", 0.5), ("mSmileR", -0.25)]));
        assert_eq!(q.mouth.smile_left, 0.05);
        assert_eq!(q.mouth.smile_right, -0.7);
    }

    #[test]
    fn blush_and_the_effects() {
        let q = params(&with(&[("blush", 0.75), ("tear", 1.0), ("heart", 0.4), ("spark", 0.9)]));
        assert_eq!(q.blush, 0.75);
        assert_eq!((q.effects.tears, q.effects.heart, q.effects.sparkle), (1.0, 0.4, 0.9));
        // The prototype has none of the other effects.
        assert_eq!((q.effects.star, q.effects.gloom, q.effects.shock), (0.0, 0.0, 0.0));
        assert_eq!((q.effects.squeeze, q.effects.spiral, q.vergence, q.sweat, q.cheek_puff), (0.0, 0.0, 0.0, 0.0, 0.0));
    }

    #[test]
    fn the_gaze_is_the_driven_gaze_plus_the_weaker_camera_follow() {
        let p = record();
        let g = paint_params(&p, Xy { x: 1.0, y: -0.26 }, Xy { x: 0.5, y: 0.25 }, 0.4).unwrap().gaze;
        assert!((f64::from(g.x) - (0.5 + 0.4)).abs() < 1e-6, "{g}");
        assert!((f64::from(g.y) - (0.25 - 0.26 * 0.4)).abs() < 1e-6, "{g}");
        // Driven alone is the driven gaze.
        assert_eq!(paint_params(&p, NO_GAZE, Xy { x: -0.3, y: 0.1 }, 0.4).unwrap().gaze, Vec2::new(-0.3, 0.1));
    }

    #[test]
    fn the_reflection_offset_is_the_highlight_shift() {
        assert_eq!(face_view(Xy { x: -0.1, y: 0.03 }).highlight_shift, Vec2::new(-0.1, 0.03));
    }

    #[test]
    fn the_design_entries_land_on_the_profile() {
        let mut profile = FaceProfile::default();
        let p = with(&[
            ("eyeScale", 1.25),
            ("eyeDrop", 0.01),
            ("refl", 0.8),
            ("lips", 1.0),
            ("nose", 1.0),
            ("mScale", 1.5),
            ("mScaleClosed", 1.7),
            ("smileLids", 1.0),
        ]);
        apply_style(&mut profile, &p).unwrap();
        assert_eq!((profile.eyes.scale, profile.eyes.drop, profile.eyes.highlights.shimmer), (1.25, 0.01, 0.8));
        assert_eq!(profile.mouth.lips.strength, 1.0);
        assert_eq!(profile.nose.kind, NoseKind::Tick);
        assert_eq!((profile.mouth.open_scale, profile.mouth.closed_scale), (1.5, 1.7));
        assert!(profile.eyes.lower_lid.cheek_line);

        apply_style(&mut profile, &with(&[("nose", 0.0), ("smileLids", 0.0)])).unwrap();
        assert_eq!(profile.nose.kind, NoseKind::None);
        assert!(!profile.eyes.lower_lid.cheek_line);
    }

    #[test]
    fn a_missing_parameter_is_named_and_leaves_the_profile_alone() {
        let mut p = record();
        p.remove("mScale");
        assert_eq!(apply_style(&mut FaceProfile::default(), &p), Err(ParamError::Missing("mScale")));
        let mut profile = FaceProfile::default();
        let _ = apply_style(&mut profile, &p);
        assert_eq!(profile, FaceProfile::default());
        p.remove("open");
        assert_eq!(paint_params(&p, NO_GAZE, NO_GAZE, 0.0), Err(ParamError::Missing("open")));
    }
}
