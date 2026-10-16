//! `prepare::arkit_shapes` (#1090) on the original test head (`tests/common/head.rs`): split halves sum to their
//! source, each generated channel is reported and the scene stays valid, and the export's missing perfect-sync
//! channels fall by the number made. The renders are made by the ignored `generated_sheet` test and
//! `tools/arkit-shape-sheet.py`, and judged by eye.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::path::PathBuf;

use common::def;
use common::head::{MOUTH_HALF_WIDTH, dump_expression, head};
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
    // The lips' width, not the jaw skin's that follows `aa` a little out to the cheeks.
    #[allow(clippy::cast_possible_truncation)]
    let lips = MOUTH_HALF_WIDTH as f32;
    assert!((0.8 * lips..1.3 * lips).contains(&m.mouth_half_width), "{} for lips {lips}", m.mouth_half_width);
}

/// The primitive of the face whose material is called `name`.
fn primitive(scene: &IoScene, name: &str) -> usize {
    let face = face(scene);
    face.primitives
        .iter()
        .position(|p| p.material.is_some_and(|m| scene.materials[m.index()].name == name))
        .unwrap_or_else(|| panic!("no {name} primitive"))
}

#[test]
fn a_wide_eye_lifts_the_top_of_its_white_with_the_lid_and_leaves_the_iris() {
    let mut scene = head().scene;
    generate(&mut scene, &["eyeWideLeft"]);
    let t = target(&scene, "eyeWideLeft");
    let (white, iris, lash) = (primitive(&scene, "EyeWhite"), primitive(&scene, "Iris"), primitive(&scene, "Lash"));
    let face = face(&scene);
    let m = landmarks(&scene).unwrap();
    let eye = m.eyes[0];
    // The lash's lower edge in the middle of the eye, and how far it lifts.
    let lash_lift = face.primitives[lash]
        .mesh
        .positions
        .iter()
        .zip(&face.primitives[lash].morph_targets[t].positions)
        .filter(|(p, _)| p.x > m.middle && (p.x - eye.x).abs() < 0.002)
        .map(|(_, d)| d.y)
        .fold(0.0, f32::max);
    assert!(lash_lift > 0.002, "the lid lifts: {lash_lift}");
    let (positions, deltas) =
        (&face.primitives[white].mesh.positions, &face.primitives[white].morph_targets[t].positions);
    let top = positions.iter().zip(deltas).filter(|(p, _)| p.x > m.middle).max_by(|a, b| a.0.y.total_cmp(&b.0.y));
    let (top, lift) = top.unwrap();
    assert!((eye.x - top.x).abs() < 0.003, "the white's top is above the eye's centre: {top}");
    assert!(
        (lift.y - lash_lift).abs() < 0.15 * lash_lift,
        "the white's top follows the lid: {} for {lash_lift}",
        lift.y
    );
    for (p, d) in positions.iter().zip(deltas) {
        if p.y < eye.y - 1e-4 || p.x < m.middle {
            assert_eq!(*d, Vec3::ZERO, "the lower half and the other eye stay: {p}");
        }
    }
    assert!(face.primitives[iris].morph_targets[t].positions.iter().all(|d| *d == Vec3::ZERO), "the iris stays");
}

#[test]
fn the_cheek_puff_is_one_smooth_bulge_clear_of_the_lips_and_eyes() {
    let mut scene = head().scene;
    let m = landmarks(&scene).unwrap();
    generate(&mut scene, &["cheekPuff"]);
    let puff = target(&scene, "cheekPuff");
    let face = face(&scene);
    let skin = primitive(&scene, "Skin");
    for (p, primitive) in face.primitives.iter().enumerate() {
        let moved = primitive.morph_targets[puff].positions.iter().any(|d| *d != Vec3::ZERO);
        assert_eq!(moved, p == skin, "only the skin puffs, not the lips, eyes or brows (primitive {p})");
    }
    let skin = &face.primitives[skin];
    let (points, moves) = (&skin.mesh.positions, &skin.morph_targets[puff].positions);
    let most = moves.iter().map(|d| d.length()).fold(0.0, f32::max);
    assert!(most > 0.05 * m.eye_spacing, "the cheeks puff: {most}");
    // No crease and no corner: along every edge of the skin the puff changes gently, and it meets the still skin with
    // no slope (a cut where it met the jaw changed it by about 1.1 of the edge's length).
    let steepest = skin
        .mesh
        .indices
        .as_chunks::<3>()
        .0
        .iter()
        .flat_map(|&[a, b, c]| [(a, b), (b, c), (c, a)])
        .map(|(a, b)| {
            let (a, b) = (a as usize, b as usize);
            moves[a].distance(moves[b]) / points[a].distance(points[b]).max(1e-9)
        })
        .fold(0.0, f32::max);
    assert!(steepest < 0.3, "the puff's steepest change along an edge: {steepest}");
}

/// Each vertex's normal as the surface of the face's primitives gives it, `moved` by `target` (or at rest when `None`):
/// the area-weighted normals of its triangles, the vertices welded by position across seams and primitives.
#[allow(clippy::cast_possible_truncation, reason = "a head is metres across, far inside i64 micrometres")]
fn surface_normals(face: &Mesh, target: Option<usize>) -> Vec<Vec<Vec3>> {
    let key = |p: Vec3| (p * 1e6).round().to_array().map(|c| c as i64);
    let placed = |pr: &omuzaki_io_api::Primitive| -> Vec<Vec3> {
        let deltas = target.map(|t| &pr.morph_targets[t].positions);
        pr.mesh.positions.iter().enumerate().map(|(v, p)| *p + deltas.map_or(Vec3::ZERO, |d| d[v])).collect()
    };
    let mut sums: std::collections::BTreeMap<[i64; 3], Vec3> = std::collections::BTreeMap::new();
    for pr in &face.primitives {
        let (rest, at) = (&pr.mesh.positions, placed(pr));
        for &[a, b, c] in pr.mesh.indices.as_chunks::<3>().0 {
            let [a, b, c] = [a, b, c].map(|i| i as usize);
            let n = (at[b] - at[a]).cross(at[c] - at[a]);
            for v in [a, b, c] {
                *sums.entry(key(rest[v])).or_default() += n;
            }
        }
    }
    face.primitives
        .iter()
        .map(|pr| {
            pr.mesh
                .positions
                .iter()
                .map(|p| sums.get(&key(*p)).copied().unwrap_or_default().normalize_or_zero())
                .collect()
        })
        .collect()
}

#[test]
fn shapes_made_from_landmarks_shade_as_they_are_shaped() {
    let mut scene = head().scene;
    generate(&mut scene, &["cheekPuff", "mouthShrugUpper", "eyeBlinkLeft"]);
    let face = face(&scene);
    for name in ["cheekPuff", "mouthShrugUpper"] {
        let t = target(&scene, name);
        let mut turned = 0;
        for primitive in &face.primitives {
            let target = &primitive.morph_targets[t];
            // A primitive the shape turns nothing on carries no normal deltas; one it does carries one per vertex.
            assert!(target.normals.is_empty() || target.normals.len() == primitive.mesh.positions.len(), "{name}");
            for (n, d) in primitive.mesh.normals.iter().zip(&target.normals) {
                // A turned normal is still a unit normal.
                assert!(((*n + *d).length() - n.length()).abs() < 1e-4, "{name}: a normal delta keeps the length");
                turned += usize::from(d.length() > 1e-3);
            }
        }
        assert!(turned > 10, "{name} turns the normals it moves: {turned}");
        // The turned normals are the deformed surface's own: wherever the file's normal is the rest surface's, the
        // normal plus its delta is the moved surface's, within a degree and a half.
        let (rest, moved) = (surface_normals(face, None), surface_normals(face, Some(t)));
        let mut checked = 0;
        for (p, primitive) in face.primitives.iter().enumerate() {
            let normals = &primitive.morph_targets[t].normals;
            for (v, n) in primitive.mesh.normals.iter().enumerate() {
                if normals.is_empty() || n.angle_between(rest[p][v]).to_degrees() > 1.0 {
                    continue;
                }
                let shaded = (*n + normals[v]).angle_between(moved[p][v]).to_degrees();
                assert!(shaded < 1.5, "{name}: primitive {p} vertex {v} shades {shaded} degrees off its surface");
                checked += 1;
            }
        }
        assert!(checked > 100, "{name}: the check reached the moved skin: {checked}");
    }
    // A split of a source without normals (the test head's targets have none) gets them the same way.
    let blink = target(&scene, "eyeBlinkLeft");
    assert!(face.primitives.iter().any(|p| p.morph_targets[blink].normals.iter().any(|d| d.length() > 1e-3)));
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
