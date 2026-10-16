//! `prepare::arkit_shapes` (#1090) on the original test head (`tests/common/head.rs`): split halves sum to their
//! source, each generated channel is reported and the scene stays valid, and the export's missing perfect-sync
//! channels fall by the number made. The renders are made by the ignored `generated_sheet` test and
//! `tools/arkit-shape-sheet.py`, and judged by eye.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::path::PathBuf;

use common::def;
use common::head::{dump_expression, head};
use omuzaki_core::glam::Vec3;
use omuzaki_io_api::{ARKIT_52, ExportOptions, IoScene, Mesh, preflight};
use omuzaki_io_ops::prepare::arkit::{Synthesis, synthesise};
use omuzaki_io_ops::prepare::arkit_shapes::{FALLOFF, generate, landmarks, worth_generating};
use omuzaki_io_ops::prepare::avatar::to_avatar;
use omuzaki_render_api::MtoonSupport;

/// The face mesh of the test head.
fn face(scene: &IoScene) -> &Mesh {
    let node = scene.nodes.iter().find(|n| n.name == "Face").unwrap();
    &scene.meshes[node.mesh.unwrap().index()]
}

/// The index of the target called `name` on the face.
fn target(scene: &IoScene, name: &str) -> usize {
    face(scene).morph_target_names.iter().position(|n| n == name).unwrap_or_else(|| panic!("no target {name}"))
}

/// The channels synthesis leaves for the generator, as `to_avatar` picks them.
fn wanted(scene: &mut IoScene) -> Vec<&'static str> {
    synthesise(scene).channels.iter().filter(|c| worth_generating(c)).map(|c| c.channel).collect()
}

#[test]
fn the_landmarks_find_both_eyes_and_the_mouth_below_them() {
    let scene = head().scene;
    let m = landmarks(&scene).expect("the head has a blink that moves two eyes");
    assert!(m.middle.abs() < 1e-4, "the head is centred on x = 0: {}", m.middle);
    let [left, right] = m.eyes;
    assert!(left.x > 0.0 && right.x < 0.0, "the character's left is +X: {left} {right}");
    assert!((left.x + right.x).abs() < 1e-4 && (left.y - right.y).abs() < 1e-4, "symmetric");
    assert!((m.eye_spacing - 0.064).abs() < 0.01, "about 6.4 cm apart: {}", m.eye_spacing);
    let mouth = m.mouth.expect("aa opens a mouth");
    assert!(mouth.y < left.y - 0.03, "the mouth is below the eyes: {mouth}");
    assert!(m.mouth_half_width > 0.005, "{}", m.mouth_half_width);
}

#[test]
fn split_halves_sum_to_their_source_and_each_moves_one_side() {
    let mut scene = head().scene;
    let outcomes = generate(&mut scene, &["eyeBlinkLeft", "eyeBlinkRight"]);
    assert!(outcomes.iter().all(|o| matches!(o.how, Synthesis::Generated { .. })), "{outcomes:?}");
    let (source, left, right) =
        (target(&scene, "Fcl_EYE_Close"), target(&scene, "eyeBlinkLeft"), target(&scene, "eyeBlinkRight"));
    let m = landmarks(&scene).unwrap();
    let mut worst = 0.0f32;
    for primitive in &face(&scene).primitives {
        let t = &primitive.morph_targets;
        for (v, &p) in primitive.mesh.positions.iter().enumerate() {
            let sum = t[left].positions[v] + t[right].positions[v];
            worst = worst.max(sum.distance(t[source].positions[v]));
            // Well past the blend, each half leaves the other side alone.
            let width = FALLOFF * 0.06;
            if p.x < m.middle - width {
                assert_eq!(t[left].positions[v], Vec3::ZERO, "the left half moves nothing on the right");
            }
            if p.x > m.middle + width {
                assert_eq!(t[right].positions[v], Vec3::ZERO, "the right half moves nothing on the left");
            }
        }
    }
    assert!(worst < 1e-5, "the halves sum to the source: {worst}");
}

#[test]
fn each_generated_channel_is_reported_and_the_scene_stays_valid() {
    let mut scene = head().scene;
    let wanted = wanted(&mut scene);
    let outcomes = generate(&mut scene, &wanted);
    let made: Vec<&str> =
        outcomes.iter().filter(|o| matches!(o.how, Synthesis::Generated { .. })).map(|o| o.channel).collect();
    for channel in [
        "eyeBlinkLeft",
        "eyeBlinkRight",
        "eyeWideLeft",
        "eyeSquintRight",
        "cheekPuff",
        "mouthRollUpper",
        "mouthShrugLower",
        "mouthSmileLeft",
        "browDownRight",
        "eyeLookInLeft",
        "eyeLookUpRight",
    ] {
        assert!(made.contains(&channel), "{channel} is made: {made:?}");
    }
    assert!(!scene.check().has_errors(), "{:?}", scene.check());

    let (_, report) = to_avatar(&head().scene, &def(), MtoonSupport::NONE).expect("imports");
    let reported: Vec<String> = report
        .findings
        .iter()
        .filter(|f| f.rule == "arkit.generated")
        .filter_map(|f| f.location.as_deref().and_then(|l| l.rsplit('/').next()).map(str::to_owned))
        .collect();
    for channel in &made {
        assert!(reported.iter().any(|r| r == channel), "{channel} is reported as arkit.generated: {reported:?}");
    }
}

#[test]
fn the_exports_missing_channels_fall_by_the_number_made() {
    let mut before = head().scene;
    synthesise(&mut before);
    let missing = |scene: &IoScene| {
        preflight(scene, &ExportOptions::default())
            .errors()
            .filter(|f| f.rule == "perfect_sync.missing_channel")
            .count()
    };
    let mut after = head().scene;
    let wanted = wanted(&mut after);
    let made = generate(&mut after, &wanted).iter().filter(|o| matches!(o.how, Synthesis::Generated { .. })).count();
    // Halves of symmetric presets replace channels synthesis already built; only the rest were missing.
    let replaced =
        wanted.len() - synthesise(&mut head().scene).channels.iter().filter(|c| c.how == Synthesis::Missing).count();
    assert_eq!(missing(&before) - missing(&after), made - replaced);
    assert!(missing(&after) < ARKIT_52.len());
}

#[test]
fn generation_is_deterministic() {
    let (mut a, mut b) = (head().scene, head().scene);
    let (wa, wb) = (wanted(&mut a), wanted(&mut b));
    assert_eq!(generate(&mut a, &wa), generate(&mut b, &wb));
    assert_eq!(a, b);
}

/// Writes the contact-sheet OBJs for the generated channels into `OMUZAKI_SHEET_DIR`: `before/` holds each channel as
/// synthesis alone builds it (a symmetric preset at half weight, or nothing), `after/` as the generator makes it.
#[test]
#[ignore = "writes OBJ files for tools/arkit-shape-sheet.py; run by hand"]
#[allow(clippy::disallowed_methods, reason = "test tooling: the dump is plain files the sheet script reads")]
fn generated_sheet() {
    let Some(dir) = std::env::var_os("OMUZAKI_SHEET_DIR").map(PathBuf::from) else { return };
    let mut before = head().scene;
    let wanted = wanted(&mut before);
    let mut after = before.clone();
    let made: Vec<&str> = generate(&mut after, &wanted)
        .into_iter()
        .filter(|o| matches!(o.how, Synthesis::Generated { .. }))
        .map(|o| o.channel)
        .collect();
    for (stage, scene) in [("before", &before), ("after", &after)] {
        let out = dir.join(stage);
        std::fs::create_dir_all(&out).unwrap();
        // Any expression at weight 0 is the neutral face; a channel synthesis could not build shows as neutral.
        dump_expression(scene, "blink", 0.0, &out.join("neutral.obj"));
        for channel in &made {
            let known = scene.avatar.as_ref().unwrap().expressions.keys().any(|k| k.to_string() == *channel);
            for weight in [0.5f32, 1.0] {
                let (name, posed) = if known { (*channel, weight) } else { ("blink", 0.0) };
                dump_expression(scene, name, posed, &out.join(format!("{channel}-{weight}.obj")));
            }
        }
    }
}
