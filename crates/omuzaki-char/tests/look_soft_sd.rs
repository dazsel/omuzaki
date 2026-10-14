//! Soft SD is the default look made explicit (delivered): applying it to a fresh character changes
//! no byte of its materials, and applying another preset and then Soft SD restores those bytes.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::{build, project};
use omuzaki_char_api::{
    BuiltCharacter, ClassLook, DEFAULT_PRESET, LookDef, LookParams, MaterialClass, find_look_preset,
};

/// The materials as bytes: `Debug` prints every float in its shortest round-tripping form, so equal text is equal bits
/// (unlike `==`, which takes `-0.0` for `0.0`).
fn bytes(built: &BuiltCharacter) -> String {
    format!("{:?}", built.materials)
}

fn soft_sd() -> LookParams {
    find_look_preset(DEFAULT_PRESET).unwrap().params
}

#[test]
fn soft_sd_on_a_fresh_character_changes_no_byte() {
    let fresh = build(&project(), "mock");
    let mut drawn = build(&project(), "mock");
    let changed = drawn.apply_look(&soft_sd()).unwrap();
    assert_eq!(changed, Vec::new(), "no material changes");
    assert_eq!(bytes(&drawn), bytes(&fresh));
    // A character without a look node is drawn in the same settings.
    let resolved = LookDef::default().resolve().unwrap();
    for class in MaterialClass::ALL {
        assert_eq!(resolved.class(class), soft_sd().class(class), "{class:?}");
    }
    assert_eq!(resolved.scene, soft_sd().scene);
}

#[test]
fn soft_sd_after_another_look_restores_the_bytes() {
    let fresh = build(&project(), "mock");
    let other = LookParams {
        all: ClassLook {
            softness_scale: Some(0.0),
            rim_scale: Some(2.0),
            outline_width: Some(0.004),
            ..ClassLook::default()
        },
        ..LookParams::default()
    };
    let mut drawn = build(&project(), "mock");
    let changed = drawn.apply_look(&other).unwrap();
    assert_eq!(changed.len(), fresh.materials.len(), "the other look changes every material");
    let restored = drawn.apply_look(&soft_sd()).unwrap();
    assert_eq!(restored, changed, "and soft-sd changes each of them back");
    assert_eq!(bytes(&drawn), bytes(&fresh));
}
