//! `Validate::ranges` reports what `validate` enforces (#123): every reported range accepts its ends and refuses a
//! value just outside, through `FaceProfile::validate`.

use omuzaki_face_api::{FaceMark, FaceProfile, FieldRange, Validate};
use serde_json::Value;

/// The default profile with one mark, so the mark's paths exist.
fn profile() -> FaceProfile {
    FaceProfile { marks: vec![FaceMark::default()], ..FaceProfile::default() }
}

/// `profile` with the number at `path` replaced by `value`, or `None` when the result doesn't read as a profile.
fn with(profile: &FaceProfile, path: &str, value: f64) -> Option<FaceProfile> {
    let mut json = serde_json::to_value(profile).ok()?;
    *json.pointer_mut(&format!("/{path}"))? = Value::from(value);
    serde_json::from_value(json).ok()
}

/// Whether `validate` has an error at `path`.
fn refuses(profile: &FaceProfile, path: &str) -> bool {
    profile.validate().err().is_some_and(|e| e.0.iter().any(|p| p.path == path))
}

fn ranges() -> Vec<FieldRange> {
    profile().ranges()
}

#[test]
fn the_profile_reports_a_range_for_every_numeric_style_field() {
    let found = ranges();
    for path in ["layout/eye_y", "eyes/iris/width", "eyes/iris/gaze_travel/1", "brows/outer_x", "mouth/fang/height"] {
        assert!(found.iter().any(|r| r.path == path), "{path} missing");
    }
    assert!(found.iter().any(|r| r.path == "marks/0/opacity"), "a mark's fields are reported");
    let width = found.iter().find(|r| r.path == "eyes/iris/width").map(|r| (r.lo, r.hi));
    assert_eq!(width, Some((0.2, 1.0)));
}

#[test]
fn every_range_accepts_its_ends_and_refuses_a_value_just_outside() {
    let base = profile();
    let found = ranges();
    assert!(found.len() > 50, "only {} ranges", found.len());
    for r in &found {
        let step = (r.hi - r.lo) * 1e-3;
        // A field with a rule between fields may fail that rule at an end, so only its outside is checked.
        if r.note.is_none() {
            for end in [r.lo, r.hi] {
                let p = with(&base, &r.path, end).unwrap_or_else(|| panic!("{} can't be set", r.path));
                assert!(!refuses(&p, &r.path), "{} refuses its own end {end}", r.path);
            }
        }
        for outside in [r.lo - step, r.hi + step] {
            let p = with(&base, &r.path, outside).unwrap_or_else(|| panic!("{} can't be set", r.path));
            assert!(refuses(&p, &r.path), "{} accepts {outside}, outside {}..={}", r.path, r.lo, r.hi);
        }
    }
}

#[test]
fn a_rule_between_fields_is_the_note_of_its_field() {
    let found = ranges();
    let note = |path: &str| found.iter().find(|r| r.path == path).and_then(|r| r.note.clone());
    assert_eq!(note("layout/mouth_y").as_deref(), Some("the mouth must be below the eye line"));
    assert!(note("brows/outer_x").is_some_and(|n| n.contains("farther from the midline")));
    assert_eq!(note("layout/eye_y"), note("layout/mouth_y"), "the rule is on both fields it limits");
    assert_eq!(note("layout/blush_y"), None);
}

#[test]
fn the_rule_still_fails_validation() {
    let mut p = profile();
    p.layout.mouth_y = 0.1;
    assert!(refuses(&p, "layout/mouth_y"));
}
