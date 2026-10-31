//! The head's own size (`HeadParams::size`): the whole head grows or shrinks about its joint, with its painted face's
//! windows, its eyes, its ears and their covers, while the neck stays the body's and the chin still meets it.

#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers build fixtures; a bad one should stop the test with its reason

use omuzaki_char_api::{
    BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, JointRef, PartDef, PartId,
    PartKindId, RegistryBuilder,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::Vec3;
use omuzaki_face_api::FaceLayerId;
use serde_json::{Value, json};

// Naming the crate links its part kinds (body, head) into the test binary.
use omuzaki_char as _;

fn part(kind: &str, params: Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer: 0, attach: None, params }
}

fn build(head: Value) -> BuiltCharacter {
    let mut def: CharacterDef = mock_character().unwrap();
    def.template = "standard".parse().unwrap();
    def.parts.clear();
    def.parts.insert(PartId::new("body").unwrap(), part("body", json!({})));
    def.parts.insert(PartId::new("head").unwrap(), part("head", head));
    RegistryBuilder
        .build(&CharId::new("t").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap()
}

/// The lowest and highest point of the mesh called `name`.
fn bounds(built: &BuiltCharacter, name: &str) -> (Vec3, Vec3) {
    let mesh = built.meshes.iter().find(|m| m.mesh.name == name).unwrap_or_else(|| panic!("no {name} mesh"));
    let ps = &mesh.mesh.geometry.positions;
    (
        ps.iter().copied().fold(Vec3::splat(f32::INFINITY), Vec3::min),
        ps.iter().copied().fold(Vec3::splat(f32::NEG_INFINITY), Vec3::max),
    )
}

fn joint(built: &BuiltCharacter, joint: &JointRef) -> Vec3 {
    let id = built.rig.bone(joint).unwrap();
    built.rig.inverse_bind()[id.index()].inverse().w_axis.truncate()
}

/// The base face layer's window: where the painted face lands on the head.
fn window(built: &BuiltCharacter) -> (f32, f32) {
    let overlay = built
        .materials
        .values()
        .flat_map(|m| m.overlays.iter())
        .find(|o| o.layer == FaceLayerId::Base)
        .expect("a painted face");
    (overlay.window.max.x - overlay.window.min.x, overlay.window.max.y - overlay.window.min.y)
}

#[test]
fn a_smaller_head_shrinks_whole_about_its_joint_on_the_same_neck() {
    let one = build(json!({}));
    let unset = build(json!({ "size": 1.0 }));
    assert_eq!(bounds(&one, "head-head"), bounds(&unset, "head-head"), "size 1 is the template's head, bit for bit");
    for size in [0.7_f32, 0.85, 1.25] {
        let built = build(json!({ "size": size }));
        let (lo1, hi1) = bounds(&one, "head-head");
        let (lo, hi) = bounds(&built, "head-head");
        let ratio = (hi - lo) / (hi1 - lo1);
        eprintln!("size {size}: head extent ratio {ratio:?}");
        assert!((ratio - Vec3::splat(size)).abs().max_element() < 1e-3, "the head mesh scales by {size}: {ratio:?}");
        assert_eq!(bounds(&built, "head-neck"), bounds(&one, "head-neck"), "the neck stays the body's");
        let pivot = joint(&one, &JointRef::HEAD);
        assert!((joint(&built, &JointRef::HEAD) - pivot).length() < 1e-5, "the head joint stays put");
        for eye in [
            JointRef::Humanoid(omuzaki_anim_api::HumanoidBone::LeftEye),
            JointRef::Humanoid(omuzaki_anim_api::HumanoidBone::RightEye),
        ] {
            let (from, to) = (joint(&one, &eye) - pivot, joint(&built, &eye) - pivot);
            assert!((to - from * size).length() < 1e-4, "the eyes move with the head: {from:?} -> {to:?}");
        }
        let (w1, h1) = window(&one);
        let (w, h) = window(&built);
        assert!((w / w1 - size).abs() < 1e-4 && (h / h1 - size).abs() < 1e-4, "the painted face scales with the head");
        let chin = lo.y;
        let neck_top = bounds(&built, "head-neck").1.y;
        assert!(chin < neck_top, "size {size}: the chin still meets the neck ({chin} against {neck_top})");
    }
}

#[test]
fn a_size_out_of_range_is_refused() {
    let mut def: CharacterDef = mock_character().unwrap();
    def.template = "standard".parse().unwrap();
    def.parts.clear();
    def.parts.insert(PartId::new("body").unwrap(), part("body", json!({})));
    def.parts.insert(PartId::new("head").unwrap(), part("head", json!({ "size": 2.5 })));
    let built =
        RegistryBuilder.build(&CharId::new("t").unwrap(), &def, &BuildOptions::default(), &BuildServices::default());
    let message = built.expect_err("refused").to_string();
    assert!(message.contains("size"), "{message}");
}
