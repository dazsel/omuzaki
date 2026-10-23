//! The VRMA exporter (`docs/design/vrma-export.md` V1 to V7): the file's structure checked against the
//! `VRMC_vrm_animation` 1.0 text (no schema is vendored), the canonical order, step holds, rights in the asset block,
//! and a read back.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_io_api::{
    AnimationExporter, ExportCtx, ExportOptions, ExpressionName, HUMAN_BONES, HumanBoneName, Interpolation,
    IoAnimation, IoError, Keys, Layout, PresetExpression, RestBone, RestHumanoid, Track, animation_formats,
    find_animation_exporter, run_export_animation,
};
use omuzaki_io_vrm::{VrmaExporter, read_vrma};
use omuzaki_store_api::{Commercial, Provenance, Source};
use serde_json::Value;

fn bone(name: &str) -> HumanBoneName {
    HumanBoneName::new(name).unwrap()
}

/// Every bone but the eyes, `upperChest` and the fingers: a rest tree with gaps, 10 cm per bone, hips 0.9 m up.
fn rest() -> RestHumanoid {
    let bones = HUMAN_BONES
        .iter()
        .filter(|s| !matches!(s.name, "leftEye" | "rightEye" | "upperChest") && !s.name.contains("Thumb"))
        .filter(|s| !["Index", "Middle", "Ring", "Little"].iter().any(|f| s.name.contains(f)))
        .map(|s| {
            let at = if s.name == "hips" { Vec3::new(0.0, 0.9, 0.0) } else { Vec3::new(0.0, 0.1, 0.0) };
            RestBone::new(bone(s.name), at)
        })
        .collect();
    RestHumanoid::new(bones)
}

fn record() -> Provenance {
    Provenance {
        source: Source::Authored,
        author: Some("Omuzaki contributors".into()),
        licence: "CC0-1.0".into(),
        attribution: None,
        commercial: Commercial::Ok,
        consent: None,
        private_test: false,
    }
}

fn wave() -> IoAnimation {
    let mut a = IoAnimation::new("wave", rest());
    let t = vec![0.0, 0.5, 1.0];
    a.tracks = vec![
        Track::look_at(Keys::linear(t.clone(), vec![Quat::IDENTITY, Quat::from_rotation_y(0.3), Quat::IDENTITY])),
        Track::expression(ExpressionName::parse("omuzaki_smug").unwrap(), Keys::step(t.clone(), vec![0.0, 1.0, 1.0])),
        Track::rotation(
            bone("rightUpperArm"),
            Keys::linear(t.clone(), vec![Quat::IDENTITY, Quat::from_rotation_z(-1.0), Quat::from_rotation_z(-0.5)]),
        ),
        Track::expression(
            ExpressionName::Preset(PresetExpression::Happy),
            Keys::linear(t.clone(), vec![0.0, 1.0, 0.0]),
        ),
        Track::rotation(bone("spine"), Keys::linear(vec![0.0, 1.0], vec![Quat::IDENTITY, Quat::from_rotation_x(0.1)])),
        Track::hips(Keys::linear(
            t,
            vec![Vec3::new(0.0, 0.9, 0.0), Vec3::new(0.0, 0.88, 0.0), Vec3::new(0.0, 0.9, 0.0)],
        )),
    ];
    a.provenance = Some(record());
    a
}

fn export_with(a: &IoAnimation, options: &ExportOptions) -> Result<Vec<u8>, IoError> {
    let out = run_export_animation(&VrmaExporter, a, &ExportCtx::new(options))?;
    assert_eq!(out.files.len(), 1);
    assert_eq!((out.files[0].path.as_str(), out.files[0].media_type.as_str()), ("animation.vrma", "model/gltf-binary"));
    Ok(out.files[0].bytes.clone())
}

fn export(a: &IoAnimation) -> Vec<u8> {
    export_with(a, &ExportOptions::default().with_generator("Omuzaki test")).unwrap()
}

/// The GLB's JSON.
fn json(bytes: &[u8]) -> Value {
    let parsed = omuzaki_gltf::parse(bytes, &omuzaki_io_api::Limits::default()).unwrap();
    serde_json::to_value(&parsed.document).unwrap()
}

fn node_index(v: &Value) -> usize {
    usize::try_from(v.as_u64().unwrap()).unwrap()
}

#[test]
fn vrma_is_registered() {
    let _ = VrmaExporter.info();
    let info = animation_formats().into_iter().find(|f| f.id == "vrma").expect("registered");
    assert_eq!((info.name, info.extensions, info.media_type), ("VRM Animation", &["vrma"][..], "model/gltf-binary"));
    assert_eq!(find_animation_exporter("vrma").unwrap().info().id, "vrma");
}

/// The structural checker: what the extension's text says a file must be, checked on our output.
#[test]
fn the_file_has_the_shape_the_specification_describes() {
    let a = wave();
    let doc = json(&export(&a));
    assert_eq!(doc["extensionsUsed"], serde_json::json!(["VRMC_vrm_animation"]), "three-vrm reads it only from here");
    assert!(doc.get("extensionsRequired").is_none(), "not required (V1)");
    let ext = &doc["extensions"]["VRMC_vrm_animation"];
    assert_eq!(ext["specVersion"], "1.0");

    // Rest nodes first, in table order, named by their bone, identity rotation, the hips the only bone root.
    let rest = a.rest.sorted();
    let nodes = doc["nodes"].as_array().unwrap();
    let bones = ext["humanoid"]["humanBones"].as_object().unwrap();
    assert_eq!(bones.len(), rest.len());
    assert!(!bones.contains_key("leftEye") && !bones.contains_key("rightEye"), "the eyes cannot be defined");
    for spec in HUMAN_BONES.iter().filter(|s| s.required) {
        assert!(bones.contains_key(spec.name), "required bone {}", spec.name);
    }
    for (i, b) in rest.iter().enumerate() {
        assert_eq!(node_index(&bones[b.bone.as_str()]["node"]), i);
        assert_eq!(nodes[i]["name"], b.bone.as_str());
        assert!(nodes[i].get("rotation").is_none() && nodes[i].get("scale").is_none());
        let children: Vec<usize> =
            nodes[i]["children"].as_array().map(|c| c.iter().map(node_index).collect()).unwrap_or_default();
        for c in children {
            assert_eq!(
                a.rest.parent_of(rest[c].bone),
                Some(b.bone),
                "{} under its nearest present ancestor",
                rest[c].bone
            );
        }
    }
    let chest = rest.iter().position(|b| b.bone == bone("chest")).unwrap();
    let neck = rest.iter().position(|b| b.bone == bone("neck")).unwrap();
    assert!(
        nodes[chest]["children"].as_array().unwrap().iter().any(|c| node_index(c) == neck),
        "upperChest is missing"
    );
    let hips: Vec<f32> = serde_json::from_value(nodes[0]["translation"].clone()).unwrap();
    assert_eq!(hips, [0.0, 0.9, 0.0], "the true standing hips height");

    // Then the expressions in name order (presets first), then the look-at.
    let names: Vec<&str> = nodes[rest.len()..].iter().map(|n| n["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["expression_happy", "expression_omuzaki_smug", "lookAt"]);
    assert_eq!(node_index(&ext["expressions"]["preset"]["happy"]["node"]), rest.len());
    assert_eq!(node_index(&ext["expressions"]["custom"]["omuzaki_smug"]["node"]), rest.len() + 1);
    assert_eq!(node_index(&ext["lookAt"]["node"]), rest.len() + 2);
    assert_eq!(ext["lookAt"]["offsetFromHeadBone"], serde_json::json!([0.0, 0.06, 0.0]));
    let roots: Vec<usize> = doc["scenes"][0]["nodes"].as_array().unwrap().iter().map(node_index).collect();
    assert_eq!(roots, [0, rest.len(), rest.len() + 1, rest.len() + 2]);

    // One animation; channels in canonical order; every sampler LINEAR; the paths the extension allows.
    let anims = doc["animations"].as_array().unwrap();
    assert_eq!(anims.len(), 1);
    assert_eq!(anims[0]["name"], "wave");
    let targets: Vec<(usize, &str)> = anims[0]["channels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (node_index(&c["target"]["node"]), c["target"]["path"].as_str().unwrap()))
        .collect();
    let spine = rest.iter().position(|b| b.bone == bone("spine")).unwrap();
    let arm = rest.iter().position(|b| b.bone == bone("rightUpperArm")).unwrap();
    assert_eq!(
        targets,
        [
            (0, "translation"),
            (spine, "rotation"),
            (arm, "rotation"),
            (rest.len(), "translation"),
            (rest.len() + 1, "translation"),
            (rest.len() + 2, "rotation"),
        ]
    );
    for s in anims[0]["samplers"].as_array().unwrap() {
        assert_eq!(s["interpolation"], "LINEAR", "three-vrm drops the interpolation");
        let input = &doc["accessors"][node_index(&s["input"])];
        assert!(input.get("min").is_some() && input.get("max").is_some());
        assert_eq!(input["componentType"], 5126);
    }
    assert!(doc.get("meshes").is_none() && doc.get("materials").is_none() && doc.get("skins").is_none());
    assert_eq!(doc["asset"]["generator"], "Omuzaki test");
}

#[test]
fn track_order_never_changes_the_bytes() {
    let a = wave();
    let mut b = wave();
    b.tracks.reverse();
    b.rest.bones.reverse();
    assert_eq!(export(&a), export(&b));
}

#[test]
fn it_reads_back_track_for_track_with_holds_as_two_keys() {
    let a = wave();
    let back = read_vrma(&export(&a)).unwrap();
    assert_eq!(back.name, "wave");
    assert_eq!(back.rest.sorted(), a.rest.sorted());
    assert_eq!(back.look_at_offset, Some(Vec3::new(0.0, 0.06, 0.0)));
    for (mine, theirs) in a.canonical_tracks().into_iter().zip(back.canonical_tracks()) {
        assert_eq!(mine.target(), theirs.target());
        assert_eq!(theirs.interpolation(), Interpolation::Linear);
        if mine.interpolation() == Interpolation::Linear {
            assert_eq!(mine, theirs);
        }
    }
    let Some(Track::Expression { keys, .. }) =
        back.tracks.iter().find(|t| matches!(t, Track::Expression { name: ExpressionName::Custom(_), .. }))
    else {
        panic!("the custom expression")
    };
    assert_eq!(keys.values, [0.0, 0.0, 1.0, 1.0], "a hold before the change, none where the value stays");
    assert!((f64::from(keys.times[1]) - (0.5 - 1e-4)).abs() < 1e-6, "{:?}", keys.times);
}

#[test]
fn holds_that_32_bit_time_cannot_place_are_refused() {
    for times in [vec![0.0, 0.999_999_94, 1.0], vec![0.0, 3599.0, 3600.0]] {
        let mut a = wave();
        a.tracks[1] =
            Track::expression(ExpressionName::parse("omuzaki_smug").unwrap(), Keys::step(times, vec![0.0, 0.0, 1.0]));
        let Err(IoError::Rejected(report)) = export_with(&a, &ExportOptions::default()) else { panic!("refused") };
        assert!(report.has_rule("anim_export.step_unrepresentable"), "{report:?}");
    }
    let mut long = wave();
    long.tracks[1] = Track::expression(
        ExpressionName::parse("omuzaki_smug").unwrap(),
        Keys::step(vec![0.0, 599.0, 600.0], vec![0.0, 0.0, 1.0]),
    );
    let back = read_vrma(&export(&long)).unwrap();
    let Some(Track::Expression { keys, .. }) =
        back.tracks.iter().find(|t| matches!(t, Track::Expression { name: ExpressionName::Custom(_), .. }))
    else {
        panic!("the custom expression")
    };
    assert!(keys.times.windows(2).all(|w| w[0] < w[1]), "{:?}", keys.times);
    assert!(600.0 - f64::from(keys.times[2]) <= 1e-4 + 3.1e-5);
}

#[test]
fn the_asset_block_carries_the_clips_credit_and_record_only() {
    let mut a = wave();
    a.looped = true;
    a.provenance = Some(Provenance {
        attribution: Some("Wave by Omuzaki contributors, CC BY 4.0".into()),
        licence: "CC-BY-4.0".into(),
        private_test: true,
        ..record()
    });
    let doc = json(&export(&a));
    assert_eq!(doc["asset"]["copyright"], "Wave by Omuzaki contributors, CC BY 4.0");
    assert_eq!(
        doc["asset"]["extras"]["omuzaki"],
        serde_json::json!({
            "licence": "CC-BY-4.0", "commercial": "ok", "source": "authored", "author": "Omuzaki contributors",
            "looped": true, "private_test": true
        })
    );
    a.provenance = Some(record());
    let doc = json(&export(&a));
    assert_eq!(doc["asset"]["copyright"], "Omuzaki contributors", "the author when there is no attribution text");
    assert!(doc["asset"]["extras"]["omuzaki"].get("private_test").is_none());
    assert!(read_vrma(&export(&a)).unwrap().looped);
}

#[test]
fn its_own_checks_cover_the_head_and_the_layout() {
    let mut a = wave();
    a.rest.bones.retain(|b| b.bone != bone("head"));
    let r = VrmaExporter.check(&a, &ExportOptions::default());
    assert!(r.has_rule("vrma.look_at_without_head") && r.has_errors());
    let r = VrmaExporter.check(&wave(), &ExportOptions::default().with_layout(Layout::Separate));
    assert!(r.has_rule("vrma.layout_ignored") && !r.has_errors());
    assert!(export_with(&wave(), &ExportOptions::default().with_layout(Layout::Separate)).is_ok());
}
