//! The design doc (`docs/design/char-face-api.md`) lists the contract in tables. These tests keep the tables honest: a
//! channel, viseme, layer or profile section that the code has and the doc lacks fails here, so the doc can't drift.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_anim_api::{CHANNELS, Region};
use omuzaki_face_api::{DEFAULT_PX_PER_M, ExpressionSet, FaceLayerId, FaceMapping, FaceProfile, UNMAPPED, VISEMES};

const DOC: &str = include_str!("../../../docs/design/char-face-api.md");

fn assert_listed(what: &str, name: &str) {
    assert!(DOC.contains(name), "the design doc doesn't mention {what} `{name}`: add it to the tables");
}

#[test]
fn every_channel_the_face_reads_is_in_the_mapping_table() {
    for info in CHANNELS.iter().filter(|c| c.channel.is_arkit() || matches!(c.region, Region::Eyes | Region::Extras)) {
        // The table writes sided channels once, as `stem*` (browDown*) or both names; the stem is what must appear.
        let stem = info.name.trim_end_matches("Left").trim_end_matches("Right");
        assert_listed("channel", stem);
    }
}

#[test]
fn the_unmapped_channels_are_named() {
    for c in UNMAPPED {
        assert_listed("unmapped channel", c.name());
    }
}

#[test]
fn layers_visemes_and_starter_expressions_are_documented() {
    for layer in FaceLayerId::ALL {
        assert_listed("face layer", &format!("{layer:?}"));
    }
    for v in VISEMES {
        assert_listed("viseme", &format!("`{v}`"));
    }
    for id in ExpressionSet::starter().0.keys().filter(|k| !VISEMES.contains(&k.as_str())) {
        assert_listed("starter expression", id.as_str());
    }
    assert_listed("default raster density", &format!("{DEFAULT_PX_PER_M:.0}"));
}

#[test]
fn every_profile_and_mapping_section_has_a_row() {
    let profile = serde_json::to_value(FaceProfile::default()).unwrap();
    for key in profile.as_object().unwrap().keys() {
        assert!(DOC.contains(&format!("| `{key}`")), "the face profile section `{key}` has no table row");
    }
    let mapping = serde_json::to_value(FaceMapping::default()).unwrap();
    for (section, fields) in mapping.as_object().unwrap() {
        assert_listed("mapping section", &format!("{section}."));
        if let Some(fields) = fields.as_object().filter(|_| section != "rest") {
            for field in
                fields.keys().filter(|f| !f.contains('_') || f.starts_with("tilt_from") || f.starts_with("shock"))
            {
                assert!(
                    DOC.contains(&format!("{section}.{field}")) || DOC.contains(&format!("{section}.*")),
                    "mapping.{section}.{field} isn't documented"
                );
            }
        }
    }
}
