//! Cloth on garments (#387): `cloth.set` turns a garment's sheet on; the skirt, the dress, the robe and the cloak
//! simulated are the skinned garments at rest; the skirt settles, lags a turn and comes back, and the presets differ.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]

mod common;

use omuzaki_char_api::{BuiltCharacter, JointRef, PartRuntime, PoseSource, StepCtx};
use omuzaki_core::glam::{Mat4, Quat, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_doc_mock::MemDoc;
use serde_json::json;

/// The default character: soft-sd, dressed in the starter's clothes.
fn kit() -> MemDoc {
    let mut doc = MemDoc::new();
    common::apply(&mut doc, "character.create", json!({"id": "kit", "name": "Kit", "template": "soft-sd"}));
    doc
}

/// The garments that drape: the part, and how the kit is dressed in it (the skirt is the kit's own).
const GARMENTS: [&str; 4] = ["skirt", "dress", "robe", "cloak"];

/// The kit dressed in `part` (see [`GARMENTS`]).
fn dressed(part: &str) -> MemDoc {
    let mut doc = kit();
    let add = |doc: &mut MemDoc, kind: &str, params: serde_json::Value| {
        common::apply(doc, "part.add", json!({"character": "kit", "kind": kind, "id": kind, "params": params}));
    };
    match part {
        "dress" => {
            common::apply(&mut doc, "part.set_enabled", json!({"character": "kit", "part": "skirt", "enabled": false}));
            add(&mut doc, "dress", json!({"length": 0.5}));
        }
        "robe" => add(&mut doc, "robe", json!({"length": 0.75})),
        "cloak" => add(&mut doc, "cloak", json!({})),
        _ => {}
    }
    doc
}

/// The kit with `part`'s cloth on, in `preset`.
fn simulated_part(part: &str, preset: &str) -> BuiltCharacter {
    let mut doc = dressed(part);
    common::apply(&mut doc, "cloth.set", json!({"character": "kit", "part": part, "enabled": true, "preset": preset}));
    common::build(&doc, "kit")
}

/// The whole body turned by `angle` radians about the vertical through `pivot`: every joint's skin matrix is the turn.
struct Turn {
    pivot: Vec3,
    angle: f32,
}

impl PoseSource for Turn {
    fn skin_matrix(&self, _: &JointRef) -> Option<Mat4> {
        Some(
            Mat4::from_translation(self.pivot)
                * Mat4::from_quat(Quat::from_rotation_y(self.angle))
                * Mat4::from_translation(-self.pivot),
        )
    }

    fn root(&self) -> Mat4 {
        Mat4::IDENTITY
    }
}

/// `part`'s runtime and its drawn mesh index in the built character's mesh list.
fn runtime_of<'a>(built: &'a mut BuiltCharacter, part: &str) -> (&'a mut Box<dyn PartRuntime>, usize) {
    let local = built.runtimes.iter().position(|r| r.part.as_str() == part).expect("the garment has a runtime");
    let mesh = built.runtimes[local].runtime.dynamic_mesh(0).unwrap().mesh;
    let index = built.meshes.iter().position(|m| m.part.as_str() == part && m.local == mesh).unwrap();
    (&mut built.runtimes[local].runtime, index)
}

/// The front hem vertex: the one furthest forward among the lowest centimetre.
fn front_hem(positions: &[Vec3]) -> usize {
    let lowest = positions.iter().fold(f32::INFINITY, |a, p| a.min(p.y));
    (0..positions.len())
        .filter(|&k| positions[k].y < lowest + 0.01)
        .max_by(|&a, &b| positions[a].z.total_cmp(&positions[b].z))
        .unwrap()
}

#[test]
fn cloth_set_turns_the_skirt_into_cloth_and_undo_restores_it() {
    let mut doc = kit();
    let before = common::at(&doc, "characters/kit/parts/skirt").unwrap();
    common::round_trip(
        &mut doc,
        "cloth.set",
        json!({"character": "kit", "part": "skirt", "enabled": true, "preset": "heavy"}),
    );
    common::apply(
        &mut doc,
        "cloth.set",
        json!({"character": "kit", "part": "skirt", "enabled": true, "preset": "heavy"}),
    );
    let cloth = common::at(&doc, "characters/kit/parts/skirt/params/common/cloth").unwrap();
    assert_eq!(cloth["enabled"], json!(true));
    assert_eq!(cloth["preset"], json!("heavy"));
    common::apply(
        &mut doc,
        "cloth.set",
        json!({"character": "kit", "part": "skirt", "field": "stiffness", "value": 2.0}),
    );
    assert_eq!(
        common::at(&doc, "characters/kit/parts/skirt/params/common/cloth/overrides/stiffness"),
        Some(json!(2.0))
    );
    assert_ne!(common::at(&doc, "characters/kit/parts/skirt").unwrap(), before);
    for (args, field) in [
        (json!({"character": "kit", "part": "skirt"}), "enabled"),
        (json!({"character": "kit", "part": "skirt", "field": "weight", "value": 1.0}), "field"),
        (json!({"character": "kit", "part": "skirt", "field": "density", "value": 9.0}), "value"),
        (json!({"character": "kit", "part": "head", "enabled": true}), "part"),
    ] {
        let e = common::refused(&doc, "cloth.set", args.clone());
        let text = format!("{e:?}");
        assert!(text.contains(field) || field == "value", "{args}: {text}");
    }
}

#[test]
fn at_rest_every_simulated_garment_is_the_skinned_garment() {
    for part in GARMENTS {
        let still = common::build(&dressed(part), "kit");
        let mut built = simulated_part(part, "cotton");
        let pose = Turn { pivot: Vec3::ZERO, angle: 0.0 };
        let (runtime, index) = runtime_of(&mut built, part);
        runtime.reset(&pose);
        let drawn = runtime.dynamic_mesh(0).unwrap().positions.to_vec();
        let mesh = &built.meshes[index].mesh;
        assert!(
            mesh.dynamic && mesh.joint_bones.is_empty() && !mesh.geometry.is_skinned(),
            "{part}: the runtime draws it"
        );
        let skinned = &still.meshes.iter().find(|m| m.mesh.name == mesh.name).unwrap().mesh.geometry.positions;
        assert_eq!(drawn.len(), skinned.len(), "{part}: the whole mesh: pleats, lining and roll");
        let worst = drawn.iter().zip(skinned).map(|(a, b)| a.distance(*b)).fold(0.0, f32::max);
        assert!(worst < 1e-5, "{part}: at rest the cloth is the skinned garment to {worst} m");
    }
}

/// Steps `runtime` through `frames` of 1/60 s with the pose `at(t)`, recording the front hem's angle about the
/// vertical each frame.
fn swing(runtime: &mut Box<dyn PartRuntime>, frames: usize, at: impl Fn(f32) -> Turn) -> Vec<(f32, f32, Vec3)> {
    let dt = 1.0 / 60.0;
    runtime.reset(&at(0.0));
    let hem = front_hem(runtime.dynamic_mesh(0).unwrap().positions);
    let start = runtime.dynamic_mesh(0).unwrap().positions[hem];
    // The angle unwrapped from frame to frame, so a hem at the back (near the branch cut at pi) turns smoothly.
    let mut angle = start.x.atan2(start.z);
    (1..=frames)
        .map(|k| {
            let t = k as f32 * dt;
            let pose = at(t);
            runtime.step(&StepCtx::new(dt, Seconds(f64::from(t)), &pose, Vec3::ZERO));
            let p = runtime.dynamic_mesh(0).unwrap().positions[hem];
            let step = (p.x.atan2(p.z) - angle + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI;
            angle += step;
            (t, angle, p)
        })
        .collect()
}

/// The time the hem settles after a still start: when it stops moving more than 0.2 mm a frame for good.
fn settle_time(part: &str, preset: &str) -> f32 {
    let mut built = simulated_part(part, preset);
    let (runtime, _) = runtime_of(&mut built, part);
    let track = swing(runtime, 120, |_| Turn { pivot: Vec3::ZERO, angle: 0.0 });
    let mut settled = 0.0;
    for w in track.windows(2) {
        if w[1].2.distance(w[0].2) > 2e-4 {
            settled = w[1].0;
        }
    }
    settled
}

/// The hem's lag behind a quarter turn of the body over 0.25 s: the time from the body passing half the turn to the
/// hem passing it, and the overshoot past the turn, radians, and whether it comes back within 2 degrees.
fn turn_lag(part: &str, preset: &str) -> (f32, f32, bool) {
    let mut built = simulated_part(part, preset);
    let (runtime, _) = runtime_of(&mut built, part);
    let quarter = std::f32::consts::FRAC_PI_2;
    let turn = |t: f32| {
        let s = (t / 0.25).clamp(0.0, 1.0);
        Turn { pivot: Vec3::ZERO, angle: quarter * s * s * (3.0 - 2.0 * s) }
    };
    let track = swing(runtime, 150, turn);
    let start = track[0].1 - turn(track[0].0).angle;
    let half = start + quarter / 2.0;
    let crossed = track.iter().find(|(_, a, _)| *a >= half).map_or(f32::INFINITY, |(t, _, _)| *t);
    let overshoot = track.iter().map(|(_, a, _)| a - start - quarter).fold(0.0, f32::max);
    let back = (track.last().unwrap().1 - start - quarter).abs() < 2f32.to_radians();
    (crossed - 0.125, overshoot, back)
}

#[test]
fn the_skirt_settles_lags_a_turn_and_comes_back_and_the_presets_differ() {
    let mut rows = Vec::new();
    for preset in ["light", "cotton", "heavy", "leather"] {
        let settle = settle_time("skirt", preset);
        let (lag, overshoot, back) = turn_lag("skirt", preset);
        println!(
            "{preset:>8}: settles in {settle:.3} s; lags the turn {:.0} ms, overshoots {:.1} deg, back {back}",
            lag * 1e3,
            overshoot.to_degrees()
        );
        assert!(settle <= 0.8, "{preset} settles in {settle} s");
        assert!(back, "{preset} comes back to the turned body");
        assert!((0.0..=0.2).contains(&lag) && overshoot < 10f32.to_radians(), "{preset} follows the turn");
        rows.push((preset, settle, lag, overshoot));
    }
    let distinct = |f: fn(&(&str, f32, f32, f32)) -> f32| {
        rows.iter().map(f).zip(rows.iter().skip(1).map(f)).filter(|(a, b)| (a - b).abs() > 1e-3).count()
    };
    assert!(distinct(|r| r.2) + distinct(|r| r.3) + distinct(|r| r.1) >= 3, "the presets move differently: {rows:?}");
}

#[test]
fn a_garment_that_cannot_drape_ignores_its_cloth() {
    let mut doc = kit();
    common::apply(&mut doc, "cloth.set", json!({"character": "kit", "part": "shirt", "enabled": true}));
    let built = common::build(&doc, "kit");
    assert!(built.runtimes.iter().all(|r| r.part.as_str() != "shirt"), "the shirt stays skinned");
}
