//! The `rig.weights` ops against the in-memory document: each op planned as the executor plans it, the editing
//! contract on every edit (see `common`), and the built mesh the brushes land in.
//!
//! The mock character has a torso (`body`), a head, hair and two garments. A test arm (`mock-arm`, in [`stand_ins`]) is
//! skinned to the left arm's bones on the mock body's fit, so the brushes can be checked against a mesh with an elbow.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{BrushFalloff, BuiltCharacter, JointRef, PartMesh, WeightBrush};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{ErrorCode, OpKind, Scope, Undo, find};
use serde_json::{Value, json};

use common::{apply, assert_error, at, build, plan, project, refused, round_trip};

const OPS: [&str; 4] = ["rig.weights.brush", "rig.weights.mirror", "rig.weights.clear", "rig.weights.report"];

// ------------------------------------------------------------------------------------------------ helpers

/// The mock project, with `edit` applied to its character node. The node also carries the keys a definition does not
/// (variants, a look), so the tests can check that the weight ops leave them alone.
fn project_with(edit: impl FnOnce(&mut Value)) -> MemDoc {
    let mut node = common::mock_node();
    node["variants"] = json!({"night": {"overrides": {}}});
    node["look"] = json!({"hair": {"colour": "@hair"}});
    edit(&mut node);
    MemDoc::from_value(json!({ "characters": { "mock": node } }))
}

/// The mock project with a test arm (`arm`) added to the body.
fn with_arm() -> MemDoc {
    let mut doc = project();
    apply(&mut doc, "part.add", json!({"character": "mock", "kind": "mock-arm", "id": "arm"}));
    doc
}

/// The built meshes of `part`.
fn meshes_of<'a>(built: &'a BuiltCharacter, part: &str) -> Vec<&'a PartMesh> {
    built.meshes.iter().filter(|m| m.part.as_str() == part).map(|m| &m.mesh).collect()
}

/// The weight `vertex` of `mesh` gives `joint`, summed over its influence slots.
fn weight_on(mesh: &PartMesh, vertex: usize, joint: &JointRef) -> f32 {
    let (joints, weights) = (mesh.geometry.joints[vertex], mesh.geometry.weights[vertex]);
    joints
        .iter()
        .zip(weights)
        .filter(|(j, _)| mesh.joint_bones.get(usize::from(**j)) == Some(joint))
        .map(|(_, w)| w)
        .sum()
}

/// Checks the built skin of `part` directly (every skinned vertex sums to 1 and names at most four joints), and the
/// report's own guards: nothing unnormalised, nothing malformed.
#[track_caller]
fn assert_normalised(doc: &MemDoc, part: &str) {
    let built = build(doc, "mock");
    for mesh in meshes_of(&built, part).into_iter().filter(|m| m.geometry.is_skinned()) {
        for (vertex, weights) in mesh.geometry.weights.iter().enumerate() {
            let sum: f32 = weights.iter().sum();
            assert!((sum - 1.0).abs() < 1e-5, "{part} vertex {vertex} sums to {sum}");
            assert!(weights.iter().filter(|w| **w > 0.0).count() <= 4, "{part} vertex {vertex} has over four joints");
        }
    }
    let report = plan(doc, "rig.weights.report", json!({"character": "mock", "part": part})).unwrap();
    let output = report.output.unwrap();
    assert_eq!((output["unnormalised"].as_u64(), output["malformed"].as_u64()), (Some(0), Some(0)), "{output}");
}

/// A brush as the character stores it.
fn brush(joint: JointRef, centre: [f64; 3], radius: f64, strength: f64) -> WeightBrush {
    WeightBrush { centre, radius, joint, strength, falloff: BrushFalloff::Smooth }
}

fn brush_json(joint: JointRef, centre: [f64; 3], radius: f64, strength: f64) -> Value {
    serde_json::to_value(brush(joint, centre, radius, strength)).unwrap()
}

fn humanoid(bone: HumanoidBone) -> JointRef {
    JointRef::Humanoid(bone)
}

/// A humanoid bone's VRM name (`leftUpperArm`).
fn name(bone: HumanoidBone) -> String {
    serde_json::to_value(bone).unwrap().as_str().unwrap().to_owned()
}

/// The name with its side swapped (`leftThumbProximal` to `rightThumbProximal`); a centre-line name is unchanged.
fn swapped(name: &str) -> String {
    if let Some(rest) = name.strip_prefix("left") {
        format!("right{rest}")
    } else if let Some(rest) = name.strip_prefix("right") {
        format!("left{rest}")
    } else {
        name.to_owned()
    }
}

/// The brushes of `part` as the document stores them.
fn stored(doc: &MemDoc, part: &str) -> Vec<WeightBrush> {
    at(doc, &format!("characters/mock/weight_brushes/{part}"))
        .map(|v| serde_json::from_value(v).unwrap())
        .unwrap_or_default()
}

/// The vertex of the arm nearest a 50/50 blend of upper arm and forearm (the elbow), and its position.
fn elbow(doc: &MemDoc) -> ([f64; 3], usize) {
    let built = build(doc, "mock");
    let arm = meshes_of(&built, "arm")[0];
    let (upper, lower) = (humanoid(HumanoidBone::LeftUpperArm), humanoid(HumanoidBone::LeftLowerArm));
    let vertex = (0..arm.geometry.positions.len())
        .min_by(|&a, &b| {
            let gap = |v: usize| (weight_on(arm, v, &lower) - weight_on(arm, v, &upper)).abs();
            gap(a).total_cmp(&gap(b))
        })
        .unwrap();
    let p = arm.geometry.positions[vertex];
    ([f64::from(p.x), f64::from(p.y), f64::from(p.z)], vertex)
}

// ------------------------------------------------------------------------------------------------ the ops as an agent sees them

#[test]
fn the_four_ops_are_registered_with_closed_documented_schemas() {
    for name in OPS {
        let registration = find(name).unwrap_or_else(|| panic!("{name} isn't registered"));
        let meta = (registration.meta)();
        let report = name.ends_with("report");
        let (kind, scope, undo) = if report {
            (OpKind::Query, Scope::Read, Undo::None)
        } else {
            (OpKind::Edit, Scope::EditCharacter, Undo::Auto)
        };
        assert_eq!((meta.kind, meta.scope, meta.undo), (kind, scope, undo), "{name}");

        let schema = serde_json::to_value((registration.schema)()).unwrap();
        assert_eq!(schema["additionalProperties"], json!(false), "{name}: a typo'd argument must be refused");
        assert!(!schema.to_string().contains("$ref"), "{name}: MCP schemas are inlined");
        assert!(schema["description"].as_str().unwrap().len() > 100, "{name}: the struct docs are the agent's manual");
        for (field, property) in schema["properties"].as_object().unwrap() {
            assert!(property["description"].is_string(), "{name}.{field} has no description");
        }
    }
    let required = |name: &str| {
        let schema = serde_json::to_value((find(name).unwrap().schema)()).unwrap();
        let mut required: Vec<String> =
            schema["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_owned()).collect();
        required.sort();
        required
    };
    assert_eq!(required("rig.weights.brush"), ["centre", "character", "joint", "part", "radius", "strength"]);
    assert_eq!(required("rig.weights.mirror"), ["character", "part"]);
    assert_eq!(required("rig.weights.clear"), ["character", "part"]);
    assert_eq!(required("rig.weights.report"), ["character", "part"]);
}

#[test]
fn a_brush_round_trips_and_is_stored_under_its_part_leaving_the_siblings_alone() {
    let mut doc = project_with(|_| {});
    let (variants, look) = (at(&doc, "characters/mock/variants"), at(&doc, "characters/mock/look"));
    let planned = round_trip(
        &mut doc,
        "rig.weights.brush",
        json!({"character": "mock", "part": "vest", "centre": [0.0, 1.2, 0.0], "radius": 0.2,
               "joint": {"humanoid": "chest"}, "strength": 0.5}),
    );
    assert_eq!(
        at(&doc, "characters/mock/weight_brushes/vest").unwrap(),
        json!([{"centre": [0.0, 1.2, 0.0], "radius": 0.2, "joint": {"humanoid": "chest"}, "strength": 0.5, "falloff": "smooth"}]),
        "the falloff is smooth when left out"
    );
    assert_eq!(planned.output.unwrap()["index"], json!(0));
    assert_eq!((at(&doc, "characters/mock/variants"), at(&doc, "characters/mock/look")), (variants, look));
}

#[test]
fn a_brush_refusal_names_the_exact_field() {
    let doc = project();
    let base = json!({"character": "mock", "part": "vest", "centre": [0.0, 1.2, 0.0], "radius": 0.2,
                      "joint": {"humanoid": "chest"}, "strength": 0.5});
    let with = |field: &str, value: Value| {
        let mut args = base.clone();
        args[field] = value;
        args
    };
    let zero_radius = refused(&doc, "rig.weights.brush", with("radius", json!(0.0)));
    assert_error(&zero_radius, ErrorCode::Invalid, "radius", &["greater than 0"], &["radius is above 0"]);

    let off_body = refused(&doc, "rig.weights.brush", with("centre", json!([0.0, 11.0, 0.0])));
    assert_error(&off_body, ErrorCode::Invalid, "centre/1", &["centre/1"], &["-10 to 10"]);

    let strong = refused(&doc, "rig.weights.brush", with("strength", json!(1.5)));
    assert_error(&strong, ErrorCode::Invalid, "strength", &["strength"], &["from 0 to 1"]);

    let no_part = refused(&doc, "rig.weights.brush", with("part", json!("nope")));
    assert_error(&no_part, ErrorCode::NotFound, "part", &["`nope`"], &["`vest`"]);

    let no_character = refused(&doc, "rig.weights.brush", with("character", json!("ghost")));
    assert_error(&no_character, ErrorCode::NotFound, "character", &["`ghost`"], &["`mock`"]);
}

#[test]
fn a_joint_the_skeleton_lacks_is_refused_with_the_field_and_the_nearest_bone() {
    let doc = project();
    let args = json!({"character": "mock", "part": "body", "centre": [0.0, 0.45, 0.0], "radius": 0.12,
                      "joint": {"named": "ghost_bone"}, "strength": 0.5});
    let e = refused(&doc, "rig.weights.brush", args);
    assert_error(
        &e,
        ErrorCode::Invalid,
        "joint",
        &["`ghost_bone`", "not a bone of `mock`"],
        &["the nearest bone is `spine`"],
    );
    assert_eq!(
        at(&doc, "characters/mock/weight_brushes"),
        None,
        "a refused brush is not stored (the plan changes nothing)"
    );
}

#[test]
fn mirror_gives_every_left_and_right_pair_including_the_thirty_finger_bones() {
    let bones = HumanoidBone::ALL;
    let side_of = |n: &str| {
        if n.starts_with("left") {
            1.0
        } else if n.starts_with("right") {
            -1.0
        } else {
            0.0
        }
    };
    // Only the left side and the centre line are painted; the mirror must supply every right-side partner.
    let painted: Vec<Value> = bones
        .iter()
        .filter(|b| side_of(&name(**b)) >= 0.0)
        .map(|b| {
            let x = 0.1 * side_of(&name(*b));
            brush_json(humanoid(*b), [x, 1.0, 0.0], 0.05, 0.5)
        })
        .collect();
    let left = bones.iter().filter(|b| name(**b).starts_with("left")).count();
    let centre = bones.len() - 2 * left;
    let mut doc = project_with(|node| node["weight_brushes"] = json!({ "vest": painted }));

    let planned = round_trip(&mut doc, "rig.weights.mirror", json!({"character": "mock", "part": "vest"}));
    let output = planned.output.unwrap();
    assert_eq!((output["added"].as_u64(), output["skipped"].as_u64()), (Some(left as u64), Some(centre as u64)));
    assert_eq!(output["straddling"], json!([]), "every brush here is off the plane");

    let after = stored(&doc, "vest");
    let finger = |n: &str| ["Thumb", "Index", "Middle", "Ring", "Little"].iter().any(|f| n.contains(f));
    let fingers = after.iter().filter(|b| matches!(&b.joint, JointRef::Humanoid(h) if finger(&name(*h)))).count();
    assert_eq!(fingers, 30, "all 30 finger bones have a partner on the other hand");
    for b in &after {
        let JointRef::Humanoid(bone) = &b.joint else { panic!("only humanoid joints were painted") };
        let image = WeightBrush {
            centre: [0.0 - b.centre[0], b.centre[1], b.centre[2]],
            joint: JointRef::Humanoid(serde_json::from_value(json!(swapped(&name(*bone)))).unwrap()),
            ..b.clone()
        };
        assert!(after.contains(&image), "the mirror of {b:?} is missing");
    }

    let again = plan(&doc, "rig.weights.mirror", json!({"character": "mock", "part": "vest"})).unwrap();
    assert!(again.patch.is_empty(), "mirroring twice adds nothing");
    assert_eq!(again.output.unwrap()["added"], json!(0));
}

#[test]
fn mirror_swaps_named_extra_bones_as_well() {
    let hair = brush_json(JointRef::Named("hair_l_2".parse().unwrap()), [0.1, 1.5, 0.0], 0.05, 0.5);
    let mut doc = project_with(|node| node["weight_brushes"] = json!({ "vest": [hair] }));
    apply(&mut doc, "rig.weights.mirror", json!({"character": "mock", "part": "vest"}));
    let after = stored(&doc, "vest");
    assert_eq!(after[1].joint, JointRef::Named("hair_r_2".parse().unwrap()));
    assert_eq!(after[1].centre, [-0.1, 1.5, 0.0]);
}

#[test]
fn mirror_round_trips_and_a_centre_line_brush_is_its_own_mirror() {
    let centre_line = brush_json(humanoid(HumanoidBone::Chest), [0.0, 1.2, 0.0], 0.2, 0.5);
    let left = brush_json(humanoid(HumanoidBone::LeftUpperArm), [0.2, 1.3, 0.0], 0.1, 0.5);
    let mut doc = project_with(|node| node["weight_brushes"] = json!({ "vest": [centre_line, left] }));
    let planned = round_trip(&mut doc, "rig.weights.mirror", json!({"character": "mock", "part": "vest"}));
    assert_eq!(planned.output.unwrap()["added"], json!(1));
    let after = stored(&doc, "vest");
    assert_eq!(after.len(), 3);
    assert_eq!(after[2].centre, [-0.2, 1.3, 0.0]);
    assert_eq!(after[2].joint, humanoid(HumanoidBone::RightUpperArm));
}

#[test]
fn a_brush_that_crosses_the_plane_is_listed_by_mirror_and_by_the_report() {
    // The chest brush sits on the plane and is its own mirror, so it is not listed. The left brush crosses the plane
    // (|x| < radius) and its mirror is a different brush, so the mirror output and the report both list it, as the
    // appended right-side twin too.
    let centre_line = brush_json(humanoid(HumanoidBone::Chest), [0.0, 0.45, 0.0], 0.12, 0.5);
    let crossing = brush_json(humanoid(HumanoidBone::LeftUpperArm), [0.02, 0.45, 0.0], 0.05, 0.5);
    let mut doc = project_with(|node| node["weight_brushes"] = json!({ "body": [centre_line, crossing] }));
    let planned = round_trip(&mut doc, "rig.weights.mirror", json!({"character": "mock", "part": "body"}));
    assert_eq!(planned.output.unwrap()["straddling"], json!([1, 2]));
    let report = plan(&doc, "rig.weights.report", json!({"character": "mock", "part": "body"})).unwrap();
    assert_eq!(report.output.unwrap()["straddling"], json!([1, 2]));
}

#[test]
fn clear_removes_one_brush_or_all_of_them_and_undo_restores_their_order() {
    let brushes: Vec<Value> =
        [0.1, 0.2, 0.3].iter().map(|r| brush_json(humanoid(HumanoidBone::Chest), [0.0, 0.45, 0.0], *r, 0.5)).collect();
    let mut doc = project_with(|node| node["weight_brushes"] = json!({ "vest": brushes }));

    let planned = round_trip(&mut doc, "rig.weights.clear", json!({"character": "mock", "part": "vest", "index": 1}));
    let radii: Vec<f64> = stored(&doc, "vest").iter().map(|b| b.radius).collect();
    assert_eq!(radii, [0.1, 0.3]);
    assert_eq!(planned.output.unwrap()["removed"], json!(1));

    let beyond = refused(&doc, "rig.weights.clear", json!({"character": "mock", "part": "vest", "index": 5}));
    assert_error(&beyond, ErrorCode::Invalid, "index", &["no brush 5"], &["numbered 0 to 1"]);

    round_trip(&mut doc, "rig.weights.clear", json!({"character": "mock", "part": "vest"}));
    assert_eq!(at(&doc, "characters/mock/weight_brushes/vest"), None, "an empty list leaves no entry behind");

    let none = refused(&doc, "rig.weights.clear", json!({"character": "mock", "part": "vest", "index": 0}));
    assert_error(&none, ErrorCode::Invalid, "index", &["no brush 0"], &["no weight brushes"]);

    let nothing = plan(&doc, "rig.weights.clear", json!({"character": "mock", "part": "hair"})).unwrap();
    assert!(nothing.patch.is_empty(), "clearing a part with no brushes changes nothing");
    assert_eq!(nothing.output.unwrap()["removed"], json!(0));
}

#[test]
fn a_brush_that_reaches_no_vertex_is_flagged_by_the_report() {
    let near = brush_json(humanoid(HumanoidBone::Chest), [0.0, 0.45, 0.0], 0.12, 0.5);
    let far = brush_json(humanoid(HumanoidBone::Chest), [5.0, 5.0, 5.0], 0.1, 0.5);
    let mut doc = project_with(|node| node["weight_brushes"] = json!({ "body": [near, far] }));
    let report = plan(&doc, "rig.weights.report", json!({"character": "mock", "part": "body"})).unwrap();
    let output = report.output.unwrap();
    assert_eq!(output["brushes_without_vertices"], json!([1]), "{output}");
    assert!(output["brushes"][0]["reaches"].as_u64().unwrap() > 0, "{output}");
    assert!(output["vertices"].as_u64().unwrap() > 0, "{output}");
    assert_eq!(output["enabled"], json!(true));
    assert!(report.patch.is_empty(), "a report changes nothing");
    apply(&mut doc, "rig.weights.clear", json!({"character": "mock", "part": "body", "index": 1}));
    assert_normalised(&doc, "body");
}

#[test]
fn a_report_on_a_disabled_part_says_it_is_not_built() {
    let mut doc = project();
    apply(&mut doc, "part.set_enabled", json!({"character": "mock", "part": "hair", "enabled": false}));
    let report = plan(&doc, "rig.weights.report", json!({"character": "mock", "part": "hair"})).unwrap();
    let output = report.output.unwrap();
    assert_eq!((output["enabled"].clone(), output["vertices"].clone()), (json!(false), json!(0)));
    assert!(output["note"].as_str().unwrap().contains("not built"), "{output}");
}

// ------------------------------------------------------------------------------------------------ the built mesh

#[test]
fn a_brush_at_the_elbow_moves_weight_from_the_upper_arm_to_the_lower_arm() {
    let mut doc = with_arm();
    let (centre, vertex) = elbow(&doc);
    let (upper, lower) = (humanoid(HumanoidBone::LeftUpperArm), humanoid(HumanoidBone::LeftLowerArm));
    let before = build(&doc, "mock");
    let arm_before = meshes_of(&before, "arm")[0].clone();
    let (upper_before, lower_before) = (weight_on(&arm_before, vertex, &upper), weight_on(&arm_before, vertex, &lower));
    // A vertex far from the elbow, which the brush must leave alone.
    let far = (0..arm_before.geometry.positions.len())
        .max_by(|&a, &b| {
            let d = |v: usize| arm_before.geometry.positions[v].distance(arm_before.geometry.positions[vertex]);
            d(a).total_cmp(&d(b))
        })
        .unwrap();

    round_trip(
        &mut doc,
        "rig.weights.brush",
        json!({"character": "mock", "part": "arm", "centre": centre, "radius": 0.1,
               "joint": {"humanoid": "leftLowerArm"}, "strength": 0.8}),
    );
    let after = build(&doc, "mock");
    let arm_after = meshes_of(&after, "arm")[0];
    let (upper_after, lower_after) = (weight_on(arm_after, vertex, &upper), weight_on(arm_after, vertex, &lower));
    assert!(lower_after - lower_before > 0.3, "the forearm gains at the elbow: {lower_before} -> {lower_after}");
    assert!(upper_before - upper_after > 0.3, "the upper arm loses there: {upper_before} -> {upper_after}");
    for joint in [&upper, &lower] {
        assert_eq!(
            weight_on(arm_after, far, joint),
            weight_on(&arm_before, far, joint),
            "a vertex out of reach is unchanged"
        );
    }
    assert_normalised(&doc, "arm");
}

#[test]
fn brushes_that_stack_past_four_joints_are_trimmed_to_four_and_stay_normalised() {
    // The elbow's generated skin already gives weight to the arm's bones. Three more brush joints reach the same
    // vertices, so before the build keeps the four heaviest, a vertex names more than four joints: the report counts
    // it, and the built skin must come out at four or fewer and still sum to 1.
    let mut doc = with_arm();
    let (centre, _) = elbow(&doc);
    for joint in ["chest", "hips", "rightUpperArm"] {
        apply(
            &mut doc,
            "rig.weights.brush",
            json!({"character": "mock", "part": "arm", "centre": centre, "radius": 0.2,
                   "joint": {"humanoid": joint}, "strength": 0.6}),
        );
    }
    let report = plan(&doc, "rig.weights.report", json!({"character": "mock", "part": "arm"})).unwrap();
    let output = report.output.unwrap();
    assert!(output["over_four_influences"].as_u64().unwrap() > 0, "the stack really exceeds four joints: {output}");
    assert_normalised(&doc, "arm");
}

#[test]
fn every_op_leaves_the_built_weights_normalised_and_the_guards_at_zero() {
    let mut doc = with_arm();
    let (elbow_at, _) = elbow(&doc);
    let steps: [(&str, Value); 6] = [
        (
            "rig.weights.brush",
            json!({"character": "mock", "part": "arm", "centre": elbow_at, "radius": 0.12,
                                     "joint": {"humanoid": "leftLowerArm"}, "strength": 0.9}),
        ),
        (
            "rig.weights.brush",
            json!({"character": "mock", "part": "arm", "centre": elbow_at, "radius": 0.12,
                                     "joint": {"humanoid": "leftUpperArm"}, "strength": 1.0}),
        ),
        (
            "rig.weights.brush",
            json!({"character": "mock", "part": "arm", "centre": elbow_at, "radius": 0.2,
                                     "joint": {"humanoid": "leftHand"}, "strength": 0.5, "falloff": "linear"}),
        ),
        ("rig.weights.mirror", json!({"character": "mock", "part": "arm"})),
        ("rig.weights.clear", json!({"character": "mock", "part": "arm", "index": 0})),
        ("rig.weights.clear", json!({"character": "mock", "part": "arm"})),
    ];
    for (op, args) in steps {
        apply(&mut doc, op, args);
        assert_normalised(&doc, "arm");
    }
}

#[test]
fn a_mirrored_brush_on_a_symmetric_torso_gives_a_symmetric_skin() {
    // The mock torso is about 0.17 m across. This brush stays on its left flank. The next test reaches across the
    // centre plane, where the pair has to treat a vertex alike.
    let painted = brush_json(humanoid(HumanoidBone::LeftUpperArm), [0.085, 0.45, 0.0], 0.05, 0.6);
    symmetric_torso(&painted);
}

#[test]
fn a_mirrored_brush_that_crosses_the_centre_plane_gives_a_symmetric_skin() {
    symmetric_torso(&brush_json(humanoid(HumanoidBone::LeftUpperArm), [0.04, 0.45, 0.0], 0.09, 1.0));
}

/// Mirrors `painted` with `rig.weights.mirror`, builds, and checks every pair of mirror vertices (a plane vertex is its own
/// partner) gives mirrored weights.
fn symmetric_torso(painted: &Value) {
    let mut doc = project_with(|node| node["weight_brushes"] = json!({ "body": [painted] }));
    apply(&mut doc, "rig.weights.mirror", json!({"character": "mock", "part": "body"}));
    let built = build(&doc, "mock");
    let torso = meshes_of(&built, "body")[0];
    let other = |joint: &JointRef| match joint {
        JointRef::Humanoid(HumanoidBone::LeftUpperArm) => humanoid(HumanoidBone::RightUpperArm),
        JointRef::Humanoid(HumanoidBone::RightUpperArm) => humanoid(HumanoidBone::LeftUpperArm),
        other => other.clone(),
    };
    let joints = [
        humanoid(HumanoidBone::Chest),
        humanoid(HumanoidBone::Hips),
        humanoid(HumanoidBone::LeftUpperArm),
        humanoid(HumanoidBone::RightUpperArm),
    ];
    let reached =
        (0..torso.geometry.positions.len()).any(|v| weight_on(torso, v, &humanoid(HumanoidBone::LeftUpperArm)) > 0.1);
    assert!(reached, "the brush must move weight on the torso, or the symmetry below checks nothing");
    let mut pairs = 0;
    for (p, pos) in torso.geometry.positions.iter().enumerate() {
        let mirror_at = [-pos.x, pos.y, pos.z];
        let partner = torso.geometry.positions.iter().position(|q| {
            (q.x - mirror_at[0]).abs() < 1e-4 && (q.y - mirror_at[1]).abs() < 1e-4 && (q.z - mirror_at[2]).abs() < 1e-4
        });
        let Some(q) = partner else { continue };
        pairs += 1;
        for joint in &joints {
            let (here, there) = (weight_on(torso, p, joint), weight_on(torso, q, &other(joint)));
            assert!(
                (here - there).abs() < 1e-5,
                "vertex {p} and its mirror {q} differ on {joint:?}: {here} vs {there}"
            );
        }
    }
    assert!(pairs > 0, "the torso has mirror pairs to compare");
}

// ------------------------------------------------------------------------------------------------ stand-ins

/// The test arm: the fixture's body, head and hair are the mock kinds already, so only the arm is a stand-in.
mod stand_ins {
    use std::f32::consts::TAU;

    use omuzaki_anim_api::HumanoidBone;
    use omuzaki_char::kit::skin::{CapsuleParams, SegmentSet};
    use omuzaki_char_api::{
        BuildCtx, BuildError, JointRef, MaterialName, MaterialSpec, PartCategory, PartKind, PartMesh, PartMeta,
        PartOutput, register_part_kind,
    };
    use omuzaki_char_mock::GarmentParams;
    use omuzaki_core::glam::Vec3;
    use omuzaki_core::mesh::MeshData;

    struct Arm;

    /// The bones the arm is skinned to: upper arm, forearm and hand, in order along the arm.
    const ARM: [HumanoidBone; 3] = [HumanoidBone::LeftUpperArm, HumanoidBone::LeftLowerArm, HumanoidBone::LeftHand];
    /// Samples per bone along the arm, and vertices per ring around it.
    const SAMPLES: u16 = 6;
    const SIDES: u16 = 8;
    /// The arm's radius, metres.
    const RADIUS: f32 = 0.04;

    /// A tube along the arm's joints, one ring every sample, so every ring lies on the arm's axis.
    fn arm_tube(ctx: &BuildCtx<'_>, joints: &[JointRef]) -> Result<MeshData, BuildError> {
        let points: Vec<Vec3> = joints
            .iter()
            .map(|j| ctx.joint(j).ok_or_else(|| ctx.fail(format!("the body has no joint {j:?}"))))
            .collect::<Result<_, _>>()?;
        let mut centres: Vec<Vec3> = points
            .windows(2)
            .flat_map(|pair| (0..SAMPLES).map(move |k| pair[0].lerp(pair[1], f32::from(k) / f32::from(SAMPLES))))
            .collect();
        centres.extend(points.last().copied());

        let mut mesh = MeshData::default();
        let last = centres.len() - 1;
        for (i, centre) in centres.iter().enumerate() {
            let dir = (centres[(i + 1).min(last)] - centres[i.saturating_sub(1)]).normalize_or_zero();
            let (u, v) = (dir.any_orthonormal_vector(), dir.cross(dir.any_orthonormal_vector()));
            for side in 0..SIDES {
                let angle = TAU * f32::from(side) / f32::from(SIDES);
                let normal = u * angle.cos() + v * angle.sin();
                mesh.positions.push(*centre + normal * RADIUS);
                mesh.normals.push(normal);
            }
        }
        let sides = usize::from(SIDES);
        for ring in 0..last {
            for side in 0..sides {
                let at = |r: usize, s: usize| u32::try_from(r * sides + s % sides).unwrap();
                let (a, b) = (at(ring, side), at(ring, side + 1));
                let (c, d) = (at(ring + 1, side), at(ring + 1, side + 1));
                mesh.indices.extend([a, c, b, b, c, d]);
            }
        }
        Ok(mesh)
    }

    impl PartKind for Arm {
        type Params = GarmentParams;
        fn meta() -> PartMeta {
            PartMeta {
                id: "mock-arm",
                summary: "Test arm: a tube along the left arm, skinned to its bones.",
                category: PartCategory::Garment,
                requires: &["mock-body"],
            }
        }
        fn build(params: &GarmentParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
            let fit = ctx.fit()?;
            let joints: Vec<JointRef> = ARM.iter().map(|b| JointRef::Humanoid(*b)).collect();
            let geometry = arm_tube(ctx, &joints)?;
            let segments = SegmentSet::from_fit(fit.as_ref(), &joints).map_err(|e| ctx.fail(e.to_string()))?;
            let material: MaterialName = "mock-arm-skin".parse().map_err(|_| ctx.fail("bad material name"))?;
            let mut mesh = PartMesh {
                name: ctx.part.to_string(),
                geometry,
                joint_bones: Vec::new(),
                material: material.clone(),
                casts_shadow: true,
                render_order: 0,
                dynamic: false,
            };
            segments.skin_mesh(&mut mesh, &joints, CapsuleParams::default()).map_err(|e| ctx.fail(e.to_string()))?;
            let colour = ctx.colour(&params.colour)?;
            Ok(PartOutput::new().with_meshes(vec![mesh]).with_materials(vec![(material, MaterialSpec::toon(colour))]))
        }
    }

    register_part_kind!(Arm);
}
