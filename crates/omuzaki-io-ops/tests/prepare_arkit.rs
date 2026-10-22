//! `prepare::arkit`: every scene is built here from `VRoid`-like names only, with a one-triangle face and no mesh from
//! any model.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use omuzaki_core::glam::{Vec2, Vec3, Vec4};
use omuzaki_io_api::{
    ARKIT_52, Avatar, ColorBind, ColorBindKind, Expression, ExpressionName, IoScene, Material, MaterialIx, Mesh,
    MeshData, MorphBind, MorphTarget, Node, NodeIx, PresetExpression, Primitive, Shading, UvBind,
};
use omuzaki_io_api::{ExportOptions, preflight};
use omuzaki_io_mock::fixtures::{BODY_NODE, tiny_avatar};
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
    let notes: Vec<_> =
        report.channels.iter().filter(|c| report.note(c.channel).is_some()).map(|c| c.channel).collect();
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
        ("eyeLookUpLeft", PresetExpression::LookUp, 0.5),
        ("eyeLookUpRight", PresetExpression::LookUp, 0.5),
        ("eyeLookDownLeft", PresetExpression::LookDown, 0.5),
        ("eyeLookDownRight", PresetExpression::LookDown, 0.5),
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
    assert!(report.note("mouthSmileLeft").is_some_and(|n| n.contains("symmetric")));
    assert_eq!(report.note("eyeBlinkLeft"), None, "left and right blink come from different presets");
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

/// Adds a second mesh node (a "Teeth" mesh) carrying morph targets `names`, and returns its node.
fn add_mesh_node(scene: &mut IoScene, name: &str, names: &[&str]) -> NodeIx {
    let (other, _) = scene_with_targets(names);
    let mut mesh = other.meshes[0].clone();
    name.clone_into(&mut mesh.name);
    let mesh = scene.add_mesh(mesh);
    let mut node = Node::new(name);
    node.mesh = Some(mesh);
    scene.add_node(node)
}

#[test]
fn an_exact_name_on_two_meshes_binds_both() {
    let (mut scene, face) = scene_with_targets(&["Brow", "jawOpen"]);
    let teeth = add_mesh_node(&mut scene, "Teeth", &["jawOpen"]);
    let report = synthesise(&mut scene);
    assert_eq!(outcome(&report, "jawOpen"), &Synthesis::Present);
    let want = [MorphBind { node: face, index: 1, weight: 1.0 }, MorphBind { node: teeth, index: 0, weight: 1.0 }];
    assert_eq!(binds(&scene, "jawOpen"), want);
}

#[test]
fn an_alias_on_two_meshes_binds_both() {
    for (face_names, teeth_names, channel, weight) in
        [(["Fcl_MTH_A"], ["fcl_mth_a"], "jawOpen", 1.0), (["Fcl_BRW_Angry"], ["Fcl_BRW_Angry"], "browDownLeft", 0.5)]
    {
        let (mut scene, face) = scene_with_targets(&face_names);
        let teeth = add_mesh_node(&mut scene, "Teeth", &teeth_names);
        let report = synthesise(&mut scene);
        assert_eq!(outcome(&report, channel), &Synthesis::FromMorph { target: face_names[0].to_owned() });
        let want = [MorphBind { node: face, index: 0, weight }, MorphBind { node: teeth, index: 0, weight }];
        assert_eq!(binds(&scene, channel), want, "{channel}");
    }
}

#[test]
fn the_exact_tier_wins_over_aliases() {
    let (mut scene, face) = scene_with_targets(&["JAWOPEN", "jawOpen"]);
    synthesise(&mut scene);
    assert_eq!(binds(&scene, "jawOpen"), [MorphBind { node: face, index: 1, weight: 1.0 }]);
}

/// A scene with a skin and a toon material, for the colour and texture-transform bind tests.
fn scene_with_material() -> (IoScene, NodeIx, MaterialIx) {
    let (mut scene, face) = scene_with_targets(&["Smile"]);
    let mut material = Material::new("skin", Shading::Toon(Box::default()));
    material.base_color = Vec4::new(1.0, 0.5, 0.25, 1.0);
    let material = scene.add_material(material);
    (scene, face, material)
}

fn blush(material: MaterialIx) -> ColorBind {
    ColorBind { material, kind: ColorBindKind::Color, target: Vec4::new(1.0, 0.0, 0.75, 1.0) }
}

#[test]
fn a_material_only_preset_is_scaled_from_the_rest_values() {
    let (mut scene, _, material) = scene_with_material();
    let mut happy = Expression::new();
    happy.color_binds.push(blush(material));
    happy.uv_binds.push(UvBind { material, scale: Vec2::new(0.5, 3.0), offset: Vec2::new(0.5, 0.25) });
    scene.avatar.as_mut().unwrap().expressions.insert(ExpressionName::Preset(PresetExpression::Happy), happy);
    let report = synthesise(&mut scene);
    let want = Synthesis::FromPreset { preset: PresetExpression::Happy, gain: 0.5 };
    assert_eq!(outcome(&report, "mouthSmileLeft"), &want);
    let e = &expressions(&scene)[&name("mouthSmileRight")];
    assert_eq!(e.color_binds, [ColorBind { target: Vec4::new(1.0, 0.25, 0.5, 1.0), ..blush(material) }]);
    assert_eq!(e.uv_binds[0].scale, Vec2::new(0.75, 2.0));
    assert_eq!(e.uv_binds[0].offset, Vec2::new(0.25, 0.125));
    assert!(e.morph_binds.is_empty() && !e.is_binary);
}

#[test]
fn a_mixed_preset_keeps_its_morphs_and_its_blush() {
    let (mut scene, face, material) = scene_with_material();
    let mut happy = morph_expression(face, 0, 1.0);
    happy.color_binds.push(blush(material));
    scene.avatar.as_mut().unwrap().expressions.insert(ExpressionName::Preset(PresetExpression::Happy), happy);
    synthesise(&mut scene);
    let e = &expressions(&scene)[&name("mouthSmileLeft")];
    assert_eq!(e.morph_binds, [MorphBind { node: face, index: 0, weight: 0.5 }]);
    assert_eq!(e.color_binds[0].target, Vec4::new(1.0, 0.25, 0.5, 1.0));
}

#[test]
fn a_colour_the_material_lacks_is_dropped_and_a_gain_of_one_copies_whole() {
    let (mut scene, _, material) = scene_with_material();
    let pbr = scene.add_material(Material::new("plain", Shading::Pbr(omuzaki_io_api::Pbr::default())));
    let mut happy = Expression::new();
    happy.color_binds.push(ColorBind { material: pbr, kind: ColorBindKind::RimColor, target: Vec4::ONE });
    let mut blink = Expression::new();
    blink.is_binary = true;
    blink.color_binds.push(ColorBind { material: pbr, kind: ColorBindKind::RimColor, target: Vec4::ONE });
    blink.uv_binds.push(UvBind { material, scale: Vec2::ONE, offset: Vec2::new(0.5, 0.0) });
    let av = scene.avatar.as_mut().unwrap();
    av.expressions.insert(ExpressionName::Preset(PresetExpression::Happy), happy);
    av.expressions.insert(ExpressionName::Preset(PresetExpression::BlinkLeft), blink.clone());
    let report = synthesise(&mut scene);
    assert_eq!(outcome(&report, "mouthSmileLeft"), &Synthesis::Missing, "nothing left to drive");
    assert_eq!(expressions(&scene)[&name("eyeBlinkLeft")], blink);
}

#[test]
fn a_half_gain_copy_of_a_binary_source_stays_binary() {
    let (mut scene, _, material) = scene_with_material();
    let mut blink = Expression::new();
    blink.is_binary = true;
    blink.uv_binds.push(UvBind { material, scale: Vec2::ONE, offset: Vec2::new(0.5, 0.0) });
    scene.avatar.as_mut().unwrap().expressions.insert(ExpressionName::Preset(PresetExpression::Blink), blink);
    let report = synthesise(&mut scene);
    assert_eq!(outcome(&report, "eyeBlinkLeft"), &Synthesis::FromPreset { preset: PresetExpression::Blink, gain: 0.5 });
    for channel in ["eyeBlinkLeft", "eyeBlinkRight"] {
        let e = &expressions(&scene)[&name(channel)];
        assert!(e.is_binary, "{channel}");
        assert_eq!(e.uv_binds[0].offset, Vec2::new(0.25, 0.0));
    }
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

/// The weight a morph target gets when every listed channel is driven at 1: the sum over their binds on it.
fn combined_weight(scene: &IoScene, channels: &[&str], node: NodeIx, index: u32) -> f32 {
    channels.iter().flat_map(|c| binds(scene, c)).filter(|b| b.node == node && b.index == index).map(|b| b.weight).sum()
}

#[test]
fn the_combined_blink_is_the_symmetric_fallback() {
    let (mut scene, face) = scene_with_targets(&["Lids"]);
    let av = scene.avatar.as_mut().unwrap();
    av.expressions.insert(ExpressionName::Preset(PresetExpression::Blink), morph_expression(face, 0, 1.0));
    let report = synthesise(&mut scene);
    let want = Synthesis::FromPreset { preset: PresetExpression::Blink, gain: 0.5 };
    assert_eq!(outcome(&report, "eyeBlinkLeft"), &want);
    assert_eq!(outcome(&report, "eyeBlinkRight"), &want);
    assert_eq!(binds(&scene, "eyeBlinkLeft"), [MorphBind { node: face, index: 0, weight: 0.5 }]);
    assert_eq!(
        combined_weight(&scene, &["eyeBlinkLeft", "eyeBlinkRight"], face, 0),
        1.0,
        "both eyes at 1 are one blink"
    );
    for channel in ["eyeBlinkLeft", "eyeBlinkRight"] {
        assert!(report.note(channel).is_some_and(|n| n.contains("symmetric")), "{channel}");
    }
}

#[test]
fn per_side_blink_presets_win_over_the_combined_one() {
    let (mut scene, face) = scene_with_targets(&["Lids", "LidL"]);
    let av = scene.avatar.as_mut().unwrap();
    av.expressions.insert(ExpressionName::Preset(PresetExpression::Blink), morph_expression(face, 0, 1.0));
    av.expressions.insert(ExpressionName::Preset(PresetExpression::BlinkLeft), morph_expression(face, 1, 1.0));
    let report = synthesise(&mut scene);
    let left = Synthesis::FromPreset { preset: PresetExpression::BlinkLeft, gain: 1.0 };
    assert_eq!(outcome(&report, "eyeBlinkLeft"), &left);
    assert_eq!(binds(&scene, "eyeBlinkLeft"), [MorphBind { node: face, index: 1, weight: 1.0 }]);
    let right = Synthesis::FromPreset { preset: PresetExpression::Blink, gain: 0.5 };
    assert_eq!(outcome(&report, "eyeBlinkRight"), &right, "only the missing side falls back");
}

#[test]
fn symmetric_gaze_presets_bind_both_eyes_at_half_gain() {
    // Looking left is the left eye out and the right eye in; the mirror for right.
    let cases = [
        (PresetExpression::LookUp, ["eyeLookUpLeft", "eyeLookUpRight"]),
        (PresetExpression::LookDown, ["eyeLookDownLeft", "eyeLookDownRight"]),
        (PresetExpression::LookLeft, ["eyeLookOutLeft", "eyeLookInRight"]),
        (PresetExpression::LookRight, ["eyeLookInLeft", "eyeLookOutRight"]),
    ];
    for (preset, channels) in cases {
        let (mut scene, face) = scene_with_targets(&["Gaze"]);
        let av = scene.avatar.as_mut().unwrap();
        av.expressions.insert(ExpressionName::Preset(preset), morph_expression(face, 0, 0.8));
        let report = synthesise(&mut scene);
        for channel in channels {
            assert_eq!(outcome(&report, channel), &Synthesis::FromPreset { preset, gain: 0.5 }, "{channel}");
            assert_eq!(binds(&scene, channel), [MorphBind { node: face, index: 0, weight: 0.4 }], "{channel}");
        }
        let sum = combined_weight(&scene, &channels, face, 0);
        assert!((sum - 0.8).abs() < 1e-6, "{preset:?}: both channels at 1 give the preset's own weight, got {sum}");
        let others = ARKIT_52.iter().filter(|c| c.starts_with("eyeLook") && !channels.contains(c));
        for channel in others {
            assert_eq!(outcome(&report, channel), &Synthesis::Missing, "{channel}");
        }
    }
}

/// A valid VRM scene whose body mesh carries `VRoid`-like morph targets and whose avatar has two presets bound to them.
/// Built by a function so another test crate can reuse it.
fn vroid_like_scene() -> IoScene {
    let mut scene = tiny_avatar();
    let names = ["Fcl_MTH_A", "Fcl_MTH_O", "Fcl_EYE_Close_L", "Fcl_BRW_Angry"];
    let mesh = &mut scene.meshes[0];
    let first = mesh.primitives[0].morph_targets[0].clone();
    mesh.primitives[0].morph_targets = vec![first; names.len()];
    mesh.morph_target_names = names.iter().map(|n| (*n).to_owned()).collect();
    let expressions = &mut scene.avatar.as_mut().unwrap().expressions;
    expressions.clear();
    expressions.insert(ExpressionName::Preset(PresetExpression::Happy), morph_expression(BODY_NODE, 1, 1.0));
    expressions.insert(ExpressionName::Preset(PresetExpression::LookUp), morph_expression(BODY_NODE, 2, 1.0));
    scene
}

/// `omuzaki-io-mock`'s perfect-sync preflight stands in for the real exporter here: `omuzaki-io-ops` may not depend on
/// `omuzaki-io-vrm` (`cargo xtask lint-deps`). The export check against the real VRM exporter is follow-up #1663, in
/// `omuzaki-test`, after #1614.
#[test]
fn the_result_exports_and_only_missing_channels_are_reported() {
    let mut scene = vroid_like_scene();
    assert!(scene.check().findings.is_empty(), "{:#?}", scene.check());
    let report = synthesise(&mut scene);
    let missing = report.channels.iter().filter(|c| c.how == Synthesis::Missing).count();
    assert!(missing > 0 && missing < 52);
    assert!(scene.check().findings.is_empty(), "{:#?}", scene.check());
    let preflight = preflight(&scene, &ExportOptions::default());
    assert_eq!(preflight.errors().filter(|f| f.rule == "perfect_sync.missing_channel").count(), missing);
}
