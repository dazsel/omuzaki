//! `prepare::arkit` report rows and shared-source notes: scenes are one-triangle faces built here from plain names.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use omuzaki_core::glam::Vec3;
use omuzaki_io_api::{
    ARKIT_52, Avatar, ExportOptions, Expression, ExpressionName, IoScene, Mesh, MeshData, MorphBind, MorphTarget, Node,
    NodeIx, PresetExpression, Primitive, Severity, preflight,
};
use omuzaki_io_ops::prepare::arkit::{Arkit52Report, Synthesis, synthesise};

/// A scene whose face carries one morph target per name, with an empty avatar.
fn scene_with_targets(targets: &[&str]) -> (IoScene, NodeIx) {
    let data =
        MeshData { positions: vec![Vec3::ZERO, Vec3::X, Vec3::Y], indices: vec![0, 1, 2], ..MeshData::default() };
    let mut primitive = Primitive::new(data);
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

fn preset(scene: &mut IoScene, face: NodeIx, preset: PresetExpression, index: u32) {
    let mut e = Expression::new();
    e.morph_binds.push(MorphBind { node: face, index, weight: 1.0 });
    scene.avatar.as_mut().unwrap().expressions.insert(ExpressionName::Preset(preset), e);
}

fn row<'a>(report: &'a omuzaki_io_api::Report, rule: &str, channel: &str) -> Vec<&'a omuzaki_io_api::Finding> {
    let tail = format!("/{channel}");
    report
        .findings
        .iter()
        .filter(|f| f.rule == rule && f.location.as_deref().is_some_and(|l| l.ends_with(&tail)))
        .collect()
}

fn message(report: &Arkit52Report, channel: &str) -> String {
    row(&report.report(), "arkit.synthesised", channel)[0].message.clone()
}

#[test]
fn missing_channels_are_located_by_region_and_built_ones_are_synthesised() {
    let (mut scene, face) = scene_with_targets(&["Fcl_MTH_A", "mouthSmileLeft"]);
    preset(&mut scene, face, PresetExpression::Happy, 0);
    let synth = synthesise(&mut scene);
    let rows = synth.report();
    assert!(rows.findings.iter().all(|f| f.severity == Severity::Info));

    let missing = row(&rows, "arkit.missing", "cheekPuff");
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].location.as_deref(), Some("arkit/mouth/cheekPuff"));
    assert_eq!(missing[0].message, "This model has no shape for puffing the cheeks, so cheekPuff won't move it.");
    assert_eq!(missing[0].hint.as_deref(), Some("Generate the shape, or map another expression to it."));
    // A morph target named exactly like the channel is Present: no row.
    assert_eq!(
        row(&rows, "arkit.synthesised", "mouthSmileLeft").len() + row(&rows, "arkit.missing", "mouthSmileLeft").len(),
        0
    );
    let gaze = row(&rows, "arkit.missing", "eyeLookUpLeft");
    assert_eq!(gaze[0].location.as_deref(), Some("arkit/gaze/eyeLookUpLeft"));

    for channel in ["jawOpen", "mouthSmileRight"] {
        assert_eq!(row(&rows, "arkit.synthesised", channel).len(), 1, "{channel}");
        assert_eq!(row(&rows, "arkit.missing", channel).len(), 0, "{channel}");
    }
    let built = synth
        .channels
        .iter()
        .filter(|c| matches!(c.how, Synthesis::FromMorph { .. } | Synthesis::FromPreset { .. }))
        .count();
    let absent = synth.channels.iter().filter(|c| c.how == Synthesis::Missing).count();
    assert_eq!(rows.findings.iter().filter(|f| f.rule == "arkit.synthesised").count(), built);
    assert_eq!(rows.findings.iter().filter(|f| f.rule == "arkit.missing").count(), absent);
    assert_eq!(rows.findings.len(), built + absent);
}

#[test]
fn a_present_channel_yields_no_row() {
    let (mut scene, face) = scene_with_targets(&["jawOpen"]);
    let mut own = Expression::new();
    own.morph_binds.push(MorphBind { node: face, index: 0, weight: 1.0 });
    scene.avatar.as_mut().unwrap().expressions.insert(ExpressionName::parse("mouthPucker").unwrap(), own);
    let synth = synthesise(&mut scene);
    let rows = synth.report();
    assert_eq!(row(&rows, "arkit.synthesised", "mouthPucker").len(), 0);
    assert_eq!(row(&rows, "arkit.missing", "mouthPucker").len(), 0);
    // A morph target with the exact name is Present too.
    assert_eq!(synth.channels.iter().find(|c| c.channel == "jawOpen").unwrap().how, Synthesis::Present);
    assert_eq!(row(&rows, "arkit.synthesised", "jawOpen").len(), 0);
}

#[test]
fn every_missing_outcome_is_still_one_export_error() {
    let (mut scene, face) = scene_with_targets(&["Lids"]);
    preset(&mut scene, face, PresetExpression::Blink, 0);
    let synth = synthesise(&mut scene);
    let missing = synth.channels.iter().filter(|c| c.how == Synthesis::Missing).count();
    assert_eq!(synth.report().findings.iter().filter(|f| f.rule == "arkit.missing").count(), missing);
    let export = preflight(&scene, &ExportOptions::default());
    assert_eq!(export.errors().filter(|f| f.rule == "perfect_sync.missing_channel").count(), missing);
    assert!(missing < ARKIT_52.len());
}

#[test]
fn the_four_look_channels_take_half_a_side_and_the_text_says_symmetric() {
    for (source, channels) in [
        (PresetExpression::LookUp, ["eyeLookUpLeft", "eyeLookUpRight"]),
        (PresetExpression::LookDown, ["eyeLookDownLeft", "eyeLookDownRight"]),
    ] {
        let (mut scene, face) = scene_with_targets(&["Gaze"]);
        preset(&mut scene, face, source, 0);
        let synth = synthesise(&mut scene);
        for channel in channels {
            let how = &synth.channels.iter().find(|c| c.channel == channel).unwrap().how;
            assert_eq!(how, &Synthesis::FromPreset { preset: source, gain: 0.5 }, "{channel}");
            assert!(message(&synth, channel).contains("symmetric"), "{channel}");
        }
    }
}

/// Builds the scene, synthesises, and returns the report.
fn run(targets: &[&str], presets: &[(PresetExpression, u32)]) -> Arkit52Report {
    let (mut scene, face) = scene_with_targets(targets);
    for &(p, index) in presets {
        preset(&mut scene, face, p, index);
    }
    synthesise(&mut scene)
}

#[test]
fn blink_and_blink_left_name_their_own_sources_and_claim_no_sharing() {
    let synth = run(&["Lids", "LidL"], &[(PresetExpression::Blink, 0), (PresetExpression::BlinkLeft, 1)]);
    let left = message(&synth, "eyeBlinkLeft");
    let right = message(&synth, "eyeBlinkRight");
    assert!(left.contains("blinkLeft expression"), "{left}");
    assert!(right.contains("blink expression") && !right.contains("blinkLeft"), "{right}");
    assert!(!left.contains("symmetric") && !right.contains("symmetric"));
    assert_eq!(synth.note("eyeBlinkLeft"), None);
    assert_eq!(synth.note("eyeBlinkRight"), None);
}

#[test]
fn blink_alone_drives_both_channels() {
    let synth = run(&["Lids"], &[(PresetExpression::Blink, 0)]);
    for channel in ["eyeBlinkLeft", "eyeBlinkRight"] {
        assert!(synth.note(channel).is_some_and(|n| n.contains("one source shape drives both")), "{channel}");
        assert!(message(&synth, channel).contains("symmetric"), "{channel}");
    }
}

#[test]
fn smile_with_one_side_on_its_own_is_not_shared() {
    let synth = run(&["mouthSmileLeft", "Joy"], &[(PresetExpression::Happy, 1)]);
    assert_eq!(synth.note("mouthSmileLeft"), None);
    assert_eq!(synth.note("mouthSmileRight"), None, "its partner has its own shape");
    // Without the lone side both come from the one preset.
    let both = run(&["Joy"], &[(PresetExpression::Happy, 0)]);
    assert!(both.note("mouthSmileLeft").is_some() && both.note("mouthSmileRight").is_some());
}

#[test]
fn gaze_with_one_side_on_its_own_is_not_shared() {
    let synth = run(&["eyeLookUpLeft", "Gaze"], &[(PresetExpression::LookUp, 1)]);
    assert_eq!(synth.note("eyeLookUpLeft"), None);
    assert_eq!(synth.note("eyeLookUpRight"), None, "its partner has its own shape");
}

#[test]
fn a_channel_that_is_not_synthesised_has_no_note() {
    let synth = run(&["Gaze"], &[]);
    assert_eq!(synth.note("cheekPuff"), None);
    assert_eq!(synth.note("notAChannel"), None);
}
