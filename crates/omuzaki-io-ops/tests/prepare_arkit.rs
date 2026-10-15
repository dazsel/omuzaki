//! `prepare::arkit`: every scene is built here from `VRoid`-like names only, with a one-triangle face and no mesh from
//! any model.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use omuzaki_core::glam::Vec3;
use omuzaki_io_api::{
    ARKIT_52, Avatar, Expression, ExpressionName, IoScene, Mesh, MeshData, MorphBind, MorphTarget, Node, NodeIx,
    PresetExpression, Primitive,
};
use omuzaki_io_ops::prepare::arkit::{Arkit52Report, Synthesis, synthesise};

fn name(n: &str) -> ExpressionName {
    ExpressionName::parse(n).unwrap()
}

/// A scene whose face node carries one morph target per name in `targets`, and an empty avatar. Returns the face node.
fn scene_with_targets(targets: &[&str]) -> (IoScene, NodeIx) {
    let mesh_data =
        MeshData { positions: vec![Vec3::ZERO, Vec3::X, Vec3::Y], indices: vec![0, 1, 2], ..MeshData::default() };
    let mut primitive = Primitive::new(mesh_data);
    primitive.morph_targets = targets.iter().map(|_| MorphTarget::new(vec![Vec3::ZERO, Vec3::Y, Vec3::ZERO])).collect();
    let mut mesh = Mesh::new("Face");
    mesh.primitives = vec![primitive];
    mesh.morph_target_names = targets.iter().map(|t| (*t).to_owned()).collect();
    let mut scene = IoScene::new();
    let mesh = scene.add_mesh(mesh);
    let mut face = Node::new("Face");
    face.mesh = Some(mesh);
    let face = scene.add_node(face);
    scene.roots.push(face);
    scene.avatar = Some(Avatar::new());
    (scene, face)
}

fn expressions(scene: &IoScene) -> &std::collections::BTreeMap<ExpressionName, Expression> {
    &scene.avatar.as_ref().unwrap().expressions
}

fn outcome<'a>(report: &'a Arkit52Report, channel: &str) -> &'a Synthesis {
    &report.channels.iter().find(|c| c.channel == channel).unwrap().how
}

fn binds(scene: &IoScene, channel: &str) -> Vec<MorphBind> {
    expressions(scene)[&name(channel)].morph_binds.clone()
}

fn morph_expression(node: NodeIx, index: u32, weight: f32) -> Expression {
    let mut e = Expression::new();
    e.morph_binds.push(MorphBind { node, index, weight });
    e
}

#[test]
fn exact_names_are_present() {
    let (mut scene, face) = scene_with_targets(&[]);
    for (i, channel) in ARKIT_52.iter().enumerate() {
        let e = morph_expression(face, u32::try_from(i).unwrap(), 1.0);
        scene.avatar.as_mut().unwrap().expressions.insert(name(channel), e);
    }
    let before = scene.clone();
    let report = synthesise(&mut scene);
    assert_eq!(report.channels.len(), 52);
    assert!(report.channels.iter().zip(ARKIT_52).all(|(c, n)| c.channel == n && c.how == Synthesis::Present));
    assert_eq!(scene, before, "nothing changes");
}

#[test]
fn an_exactly_named_morph_gets_an_expression() {
    let (mut scene, face) = scene_with_targets(&["Brow", "jawOpen"]);
    let report = synthesise(&mut scene);
    assert_eq!(outcome(&report, "jawOpen"), &Synthesis::Present);
    assert_eq!(binds(&scene, "jawOpen"), [MorphBind { node: face, index: 1, weight: 1.0 }]);
}

#[test]
fn vroid_names() {
    let (mut scene, face) = scene_with_targets(&["Fcl_MTH_A", "Fcl_EYE_Close_L", "Fcl_BRW_Angry"]);
    let report = synthesise(&mut scene);
    let from = |index, weight| [MorphBind { node: face, index, weight }];
    assert_eq!(binds(&scene, "jawOpen"), from(0, 1.0));
    assert_eq!(binds(&scene, "eyeBlinkLeft"), from(1, 1.0));
    assert_eq!(binds(&scene, "browDownLeft"), from(2, 0.5));
    assert_eq!(binds(&scene, "browDownRight"), from(2, 0.5));
    for (channel, target) in [
        ("jawOpen", "Fcl_MTH_A"),
        ("eyeBlinkLeft", "Fcl_EYE_Close_L"),
        ("browDownLeft", "Fcl_BRW_Angry"),
        ("browDownRight", "Fcl_BRW_Angry"),
    ] {
        assert_eq!(outcome(&report, channel), &Synthesis::FromMorph { target: target.to_owned() }, "{channel}");
    }
    assert_eq!(expressions(&scene).len(), 4);
    let notes: Vec<_> = report.channels.iter().filter(|c| c.note().is_some()).map(|c| c.channel).collect();
    assert_eq!(notes, ["browDownLeft", "browDownRight"], "only the shared sources are flagged symmetric");
}

#[test]
fn names_match_ignoring_case_underscores_and_a_prefix() {
    let (mut scene, face) = scene_with_targets(&["blendShape1.Mouth_Pucker", "JAWOPEN"]);
    let report = synthesise(&mut scene);
    assert_eq!(outcome(&report, "mouthPucker"), &Synthesis::FromMorph { target: "blendShape1.Mouth_Pucker".into() });
    assert_eq!(outcome(&report, "jawOpen"), &Synthesis::FromMorph { target: "JAWOPEN".into() });
    assert_eq!(binds(&scene, "jawOpen"), [MorphBind { node: face, index: 1, weight: 1.0 }]);
}

#[test]
fn presets_compose() {
    let (mut scene, face) = scene_with_targets(&["A", "B"]);
    let av = scene.avatar.as_mut().unwrap();
    for preset in [
        PresetExpression::Aa,
        PresetExpression::Ih,
        PresetExpression::Ou,
        PresetExpression::Ee,
        PresetExpression::Oh,
        PresetExpression::BlinkLeft,
        PresetExpression::BlinkRight,
        PresetExpression::Happy,
        PresetExpression::LookUp,
        PresetExpression::LookDown,
    ] {
        let mut e = morph_expression(face, 0, 1.0);
        e.morph_binds.push(MorphBind { node: face, index: 1, weight: 0.8 });
        av.expressions.insert(ExpressionName::Preset(preset), e);
    }
    let report = synthesise(&mut scene);
    let rules = [
        ("jawOpen", PresetExpression::Aa, 1.0),
        ("mouthFunnel", PresetExpression::Oh, 1.0),
        ("mouthPucker", PresetExpression::Ou, 1.0),
        ("mouthStretchLeft", PresetExpression::Ee, 0.5),
        ("mouthStretchRight", PresetExpression::Ee, 0.5),
        ("mouthSmileLeft", PresetExpression::Happy, 0.5),
        ("mouthSmileRight", PresetExpression::Happy, 0.5),
        ("eyeBlinkLeft", PresetExpression::BlinkLeft, 1.0),
        ("eyeBlinkRight", PresetExpression::BlinkRight, 1.0),
        ("eyeLookUpLeft", PresetExpression::LookUp, 1.0),
        ("eyeLookUpRight", PresetExpression::LookUp, 1.0),
        ("eyeLookDownLeft", PresetExpression::LookDown, 1.0),
        ("eyeLookDownRight", PresetExpression::LookDown, 1.0),
    ];
    for (channel, preset, gain) in rules {
        assert_eq!(outcome(&report, channel), &Synthesis::FromPreset { preset, gain }, "{channel}");
        let want =
            [MorphBind { node: face, index: 0, weight: gain }, MorphBind { node: face, index: 1, weight: 0.8 * gain }];
        assert_eq!(binds(&scene, channel), want, "{channel}");
    }
    assert_eq!(outcome(&report, "cheekPuff"), &Synthesis::Missing);
    // No angry or sad preset: the brows stay missing.
    assert_eq!(outcome(&report, "browInnerUp"), &Synthesis::Missing);
    assert_eq!(expressions(&scene).len(), 10 + rules.len());
    let smile = report.channels.iter().find(|c| c.channel == "mouthSmileLeft").unwrap();
    assert!(smile.note().is_some_and(|n| n.contains("symmetric")));
    let blink = report.channels.iter().find(|c| c.channel == "eyeBlinkLeft").unwrap();
    assert_eq!(blink.note(), None, "left and right blink come from different presets");
}

#[test]
fn never_replaces() {
    let (mut scene, face) = scene_with_targets(&["Own", "Other"]);
    let av = scene.avatar.as_mut().unwrap();
    av.expressions.insert(name("jawOpen"), morph_expression(face, 0, 0.25));
    av.expressions.insert(ExpressionName::Preset(PresetExpression::Aa), morph_expression(face, 1, 1.0));
    let report = synthesise(&mut scene);
    assert_eq!(outcome(&report, "jawOpen"), &Synthesis::Present);
    assert_eq!(binds(&scene, "jawOpen"), [MorphBind { node: face, index: 0, weight: 0.25 }]);
}

#[test]
fn a_colour_only_preset_is_copied_whole_at_gain_one_and_skipped_below_it() {
    let (mut scene, _) = scene_with_targets(&[]);
    let mut blink = Expression::new();
    blink.is_binary = true;
    blink.uv_binds.push(omuzaki_io_api::UvBind {
        material: omuzaki_io_api::MaterialIx(0),
        scale: omuzaki_core::glam::Vec2::ONE,
        offset: omuzaki_core::glam::Vec2::new(0.5, 0.0),
    });
    let av = scene.avatar.as_mut().unwrap();
    av.expressions.insert(ExpressionName::Preset(PresetExpression::BlinkLeft), blink.clone());
    av.expressions.insert(ExpressionName::Preset(PresetExpression::Happy), blink.clone());
    let report = synthesise(&mut scene);
    assert_eq!(expressions(&scene)[&name("eyeBlinkLeft")], blink);
    assert_eq!(outcome(&report, "mouthSmileLeft"), &Synthesis::Missing, "a texture move cannot be halved");
}

#[test]
fn no_avatar_is_a_no_op() {
    let mut scene = IoScene::new();
    let report = synthesise(&mut scene);
    assert_eq!(scene, IoScene::new());
    assert_eq!(report.channels.len(), 52);
    assert!(report.channels.iter().all(|c| c.how == Synthesis::Missing));
}

#[test]
fn synthesis_is_idempotent() {
    let (mut scene, _) = scene_with_targets(&["Fcl_MTH_A", "Fcl_MTH_E"]);
    synthesise(&mut scene);
    let once = scene.clone();
    let report = synthesise(&mut scene);
    assert_eq!(scene, once);
    assert_eq!(outcome(&report, "jawOpen"), &Synthesis::Present);
}
