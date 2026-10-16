//! `prepare::humanoid`: every scene is a synthetic 25-bone T-pose built here (+Z front, the character's left at +X).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_io_api::{HumanBoneName, IoScene, Node, NodeIx, Skin, VrmVersion, vrm_profile};
use omuzaki_io_ops::prepare::humanoid::{Convention, HumanoidProposal, MapError, MatchHow, apply, auto_map};

const BONES: [&str; 25] = [
    "hips",
    "spine",
    "chest",
    "upperChest",
    "neck",
    "head",
    "leftEye",
    "rightEye",
    "jaw",
    "leftUpperLeg",
    "leftLowerLeg",
    "leftFoot",
    "leftToes",
    "rightUpperLeg",
    "rightLowerLeg",
    "rightFoot",
    "rightToes",
    "leftShoulder",
    "leftUpperArm",
    "leftLowerArm",
    "leftHand",
    "rightShoulder",
    "rightUpperArm",
    "rightLowerArm",
    "rightHand",
];

/// Parent slot and local offset of each bone in `BONES`.
#[rustfmt::skip]
const LAYOUT: [(Option<usize>, [f32; 3]); 25] = [
    (None, [0.0, 1.0, 0.0]), (Some(0), [0.0, 0.1, 0.0]), (Some(1), [0.0, 0.15, 0.0]), (Some(2), [0.0, 0.1, 0.0]),
    (Some(3), [0.0, 0.1, 0.0]), (Some(4), [0.0, 0.1, 0.0]), (Some(5), [0.03, 0.05, 0.08]),
    (Some(5), [-0.03, 0.05, 0.08]), (Some(5), [0.0, -0.03, 0.05]),
    (Some(0), [0.08, -0.12, 0.0]), (Some(9), [0.0, -0.45, 0.0]), (Some(10), [0.0, -0.45, 0.0]),
    (Some(11), [0.0, -0.05, 0.12]),
    (Some(0), [-0.08, -0.12, 0.0]), (Some(13), [0.0, -0.45, 0.0]), (Some(14), [0.0, -0.45, 0.0]),
    (Some(15), [0.0, -0.05, 0.12]),
    (Some(3), [0.06, 0.03, 0.0]), (Some(17), [0.12, 0.0, 0.0]), (Some(18), [0.28, 0.0, 0.0]),
    (Some(19), [0.25, 0.0, 0.0]),
    (Some(3), [-0.06, 0.03, 0.0]), (Some(21), [-0.12, 0.0, 0.0]), (Some(22), [-0.28, 0.0, 0.0]),
    (Some(23), [-0.25, 0.0, 0.0]),
];

const MIXAMO: [&str; 25] = [
    "mixamorig:Hips",
    "mixamorig:Spine",
    "mixamorig:Spine1",
    "mixamorig:Spine2",
    "mixamorig:Neck",
    "mixamorig:Head",
    "mixamorig:LeftEye",
    "mixamorig:RightEye",
    "mixamorig:Jaw",
    "mixamorig:LeftUpLeg",
    "mixamorig:LeftLeg",
    "mixamorig:LeftFoot",
    "mixamorig:LeftToeBase",
    "mixamorig:RightUpLeg",
    "mixamorig:RightLeg",
    "mixamorig:RightFoot",
    "mixamorig:RightToeBase",
    "mixamorig:LeftShoulder",
    "mixamorig:LeftArm",
    "mixamorig:LeftForeArm",
    "mixamorig:LeftHand",
    "mixamorig:RightShoulder",
    "mixamorig:RightArm",
    "mixamorig:RightForeArm",
    "mixamorig:RightHand",
];
const VROID: [&str; 25] = [
    "J_Bip_C_Hips",
    "J_Bip_C_Spine",
    "J_Bip_C_Chest",
    "J_Bip_C_UpperChest",
    "J_Bip_C_Neck",
    "J_Bip_C_Head",
    "J_Adj_L_FaceEye",
    "J_Adj_R_FaceEye",
    "Jaw",
    "J_Bip_L_UpperLeg",
    "J_Bip_L_LowerLeg",
    "J_Bip_L_Foot",
    "J_Bip_L_ToeBase",
    "J_Bip_R_UpperLeg",
    "J_Bip_R_LowerLeg",
    "J_Bip_R_Foot",
    "J_Bip_R_ToeBase",
    "J_Bip_L_Shoulder",
    "J_Bip_L_UpperArm",
    "J_Bip_L_LowerArm",
    "J_Bip_L_Hand",
    "J_Bip_R_Shoulder",
    "J_Bip_R_UpperArm",
    "J_Bip_R_LowerArm",
    "J_Bip_R_Hand",
];
const UNITY_BIP: [&str; 25] = [
    "Bip001 Pelvis",
    "Bip001 Spine",
    "Bip001 Spine1",
    "Bip001 Spine2",
    "Bip001 Neck",
    "Bip001 Head",
    "LeftEye",
    "RightEye",
    "Jaw",
    "Bip001 L Thigh",
    "Bip001 L Calf",
    "Bip001 L Foot",
    "Bip001 L Toe0",
    "Bip001 R Thigh",
    "Bip001 R Calf",
    "Bip001 R Foot",
    "Bip001 R Toe0",
    "Bip001 L Clavicle",
    "Bip001 L UpperArm",
    "Bip001 L Forearm",
    "Bip001 L Hand",
    "Bip001 R Clavicle",
    "Bip001 R UpperArm",
    "Bip001 R Forearm",
    "Bip001 R Hand",
];
const RIGIFY: [&str; 25] = [
    "spine",
    "spine.001",
    "spine.002",
    "spine.003",
    "spine.004",
    "spine.006",
    "eye.L",
    "eye.R",
    "jaw",
    "thigh.L",
    "shin.L",
    "foot.L",
    "toe.L",
    "thigh.R",
    "shin.R",
    "foot.R",
    "toe.R",
    "shoulder.L",
    "upper_arm.L",
    "forearm.L",
    "hand.L",
    "shoulder.R",
    "upper_arm.R",
    "forearm.R",
    "hand.R",
];
const MMD: [&str; 25] = [
    "センター",
    "上半身",
    "上半身2",
    "上半身3",
    "首",
    "頭",
    "左目",
    "右目",
    "Jaw",
    "左足",
    "左ひざ",
    "左足首",
    "左つま先",
    "右足",
    "右ひざ",
    "右足首",
    "右つま先",
    "左肩",
    "左腕",
    "左ひじ",
    "左手首",
    "右肩",
    "右腕",
    "右ひじ",
    "右手首",
];
const BLENDER: [&str; 25] = [
    "Hips",
    "Spine",
    "Chest",
    "UpperChest",
    "Neck",
    "Head",
    "LeftEye",
    "RightEye",
    "Jaw",
    "LeftUpLeg",
    "LeftLeg",
    "LeftFoot",
    "LeftToes",
    "RightUpLeg",
    "RightLeg",
    "RightFoot",
    "RightToes",
    "LeftShoulder",
    "LeftArm",
    "LeftForeArm",
    "LeftHand",
    "RightShoulder",
    "RightArm",
    "RightForeArm",
    "RightHand",
];

/// Builds the rig with `names`, leaving out the bones in `skip` (their children move up to the nearest kept parent) and
/// mirroring it left to right if asked. Returns the node of each slot.
fn rig(names: &[&str], skip: &[usize], mirror: bool) -> (IoScene, Vec<Option<NodeIx>>) {
    let mut scene = IoScene::new();
    let nodes = rig_in(&mut scene, names, skip, mirror, Vec3::ZERO);
    (scene, nodes)
}

/// Adds the rig to `scene` as a new skin, with its hips at `origin` instead of the default height-1 standing spot.
fn rig_in(scene: &mut IoScene, names: &[&str], skip: &[usize], mirror: bool, origin: Vec3) -> Vec<Option<NodeIx>> {
    let (mut anchor, mut carry, mut nodes) = (Vec::<Option<usize>>::new(), Vec::<Vec3>::new(), Vec::new());
    for (i, (parent, o)) in LAYOUT.iter().enumerate() {
        let mut offset = Vec3::from(*o);
        if mirror {
            offset.x = -offset.x;
        }
        let (a, c) = match parent {
            None => (None, offset + origin),
            Some(p) if skip.contains(p) => (anchor[*p], carry[*p] + offset),
            Some(p) => (Some(*p), offset),
        };
        anchor.push(a);
        carry.push(c);
        if skip.contains(&i) {
            nodes.push(None);
            continue;
        }
        let node = scene.add_node(Node::new(names[i]).at(c));
        match a {
            Some(a) => scene.attach(nodes[a].unwrap(), node),
            None => scene.roots.push(node),
        }
        nodes.push(Some(node));
    }
    let joints: Vec<NodeIx> = nodes.iter().flatten().copied().collect();
    let binds = vec![Mat4::IDENTITY; joints.len()];
    scene.add_skin(Skin::new("skin", joints, binds));
    nodes
}

fn bone(name: &str) -> HumanBoneName {
    HumanBoneName::new(name).unwrap()
}

fn how(p: &HumanoidProposal, name: &str) -> Option<MatchHow> {
    p.matches.iter().find(|m| m.bone == bone(name)).map(|m| m.how)
}

/// The humanoid findings of the VRM 1.0 profile after applying `p`.
fn profile_errors(scene: &mut IoScene, p: &HumanoidProposal) -> Vec<String> {
    apply(scene, p);
    let report = vrm_profile(scene, VrmVersion::V1);
    report.findings.iter().filter(|f| f.rule.starts_with("vrm.humanoid")).map(|f| f.message.clone()).collect()
}

/// Maps `names` and checks that all 25 bones, the optional eyes and jaw included, are matched through `conv`.
fn convention(names: &[&str], conv: Convention) {
    let (mut scene, nodes) = rig(names, &[], false);
    let p = auto_map(&scene, &BTreeMap::new()).unwrap();
    for (i, name) in BONES.iter().enumerate() {
        assert_eq!(p.humanoid.node(name), nodes[i], "{name}");
        assert_eq!(how(&p, name), Some(MatchHow::Convention(conv)), "{name}");
    }
    assert!(!p.report.has_rule("humanoid.inferred"));
    assert_eq!(profile_errors(&mut scene, &p), Vec::<String>::new());
}

#[test]
fn mixamo_names() {
    convention(&MIXAMO, Convention::Mixamo);
}

#[test]
fn vroid_names() {
    convention(&VROID, Convention::Vrm);
}

#[test]
fn unity_bip_names() {
    convention(&UNITY_BIP, Convention::UnityBip);
}

#[test]
fn rigify_names() {
    convention(&RIGIFY, Convention::Rigify);
}

#[test]
fn mmd_japanese_names() {
    convention(&MMD, Convention::Mmd);
}

#[test]
fn blender_default_names() {
    let (mut scene, nodes) = rig(&BLENDER, &[], false);
    let p = auto_map(&scene, &BTreeMap::new()).unwrap();
    for (i, name) in BONES.iter().enumerate() {
        assert_eq!(p.humanoid.node(name), nodes[i], "{name}");
        assert_eq!(how(&p, name), Some(MatchHow::Exact), "{name}");
    }
    assert_eq!(p.report.count(omuzaki_io_api::Severity::Warning), 0);
    assert_eq!(profile_errors(&mut scene, &p), Vec::<String>::new());
}

/// Adds a skin joint named `name` under `parent`.
fn add_joint(scene: &mut IoScene, parent: NodeIx, name: &str) -> NodeIx {
    let node = scene.add_node(Node::new(name));
    scene.attach(parent, node);
    scene.skins[0].joints.push(node);
    scene.skins[0].inverse_bind.push(Mat4::IDENTITY);
    node
}

#[test]
fn fingers_are_mapped_by_convention() {
    let (mut scene, nodes) = rig(&MIXAMO, &[], false);
    let mut tip = nodes[20].unwrap();
    for name in ["Thumb1", "Thumb2", "Thumb3"] {
        tip = add_joint(&mut scene, tip, &format!("mixamorig:LeftHand{name}"));
    }
    let index = add_joint(&mut scene, nodes[20].unwrap(), "mixamorig:LeftHandIndex1");
    add_joint(&mut scene, index, "mixamorig:LeftHandIndex2");
    let p = auto_map(&scene, &BTreeMap::new()).unwrap();
    assert_eq!(how(&p, "leftThumbMetacarpal"), Some(MatchHow::Convention(Convention::Mixamo)));
    assert_eq!(p.humanoid.node("leftThumbDistal"), Some(tip));
    assert!(p.humanoid.node("leftIndexIntermediate").is_some());
    assert!(p.humanoid.node("leftIndexDistal").is_none(), "never invented");
}

fn nameless() -> Vec<String> {
    (0..25).map(|i| format!("bone_{i:03}")).collect()
}

#[test]
fn nameless_skeleton_is_inferred() {
    let owned = nameless();
    let names: Vec<&str> = owned.iter().map(String::as_str).collect();
    // `mirror` flips the rest positions: the slot that was the character's left is now at -X, so sides swap.
    for (mirror, left_leg, left_arm) in [(false, 9, 18), (true, 13, 22)] {
        let (mut scene, nodes) = rig(&names, &[], mirror);
        let p = auto_map(&scene, &BTreeMap::new()).unwrap();
        for spec in omuzaki_io_api::HUMAN_BONES.iter().filter(|b| b.required) {
            assert_eq!(how(&p, spec.name), Some(MatchHow::Inferred), "{}", spec.name);
        }
        assert!(p.report.has_rule("humanoid.inferred"));
        assert_eq!(p.humanoid.node("leftUpperLeg"), nodes[left_leg]);
        assert_eq!(p.humanoid.node("leftUpperArm"), nodes[left_arm]);
        assert_eq!(p.humanoid.node("head"), nodes[5]);
        assert_eq!(profile_errors(&mut scene, &p), Vec::<String>::new());
    }
}

#[test]
fn override_wins() {
    let (mut scene, nodes) = rig(&VROID, &[], false);
    let twist = add_joint(&mut scene, nodes[19].unwrap(), "wrist_twist");
    let overrides = BTreeMap::from([(bone("leftHand"), twist)]);
    let p = auto_map(&scene, &overrides).unwrap();
    assert_eq!(p.humanoid.node("leftHand"), Some(twist));
    assert_eq!(how(&p, "leftHand"), Some(MatchHow::Override));

    // Under the right arm, past the end of the scene, or already used: all refused.
    for bad in [nodes[24].unwrap(), NodeIx(999), nodes[1].unwrap()] {
        let e = auto_map(&scene, &BTreeMap::from([(bone("leftHand"), bad)])).unwrap_err();
        assert!(e.report.has_rule("humanoid.override_invalid"), "{bad:?}");
    }
}

#[test]
fn missing_required() {
    let (scene, _) = rig(&VROID, &[10], false);
    let e = auto_map(&scene, &BTreeMap::new()).unwrap_err();
    assert_eq!(e.missing_required, vec![bone("leftLowerLeg")]);
    let f = e.report.findings.iter().find(|f| f.rule == "humanoid.required_missing").unwrap();
    assert!(f.hint.as_deref().is_some_and(|h| h.contains("override")));
}

#[test]
fn no_neck_and_no_chest_still_map() {
    let (mut scene, _) = rig(&VROID, &[2, 3, 4], false);
    let p = auto_map(&scene, &BTreeMap::new()).unwrap();
    assert!(["chest", "upperChest", "neck"].iter().all(|b| p.humanoid.node(b).is_none()));
    assert!(p.humanoid.node("leftShoulder").is_some());
    assert_eq!(profile_errors(&mut scene, &p), Vec::<String>::new());
}

#[test]
fn tied_candidates_take_the_lower_node() {
    let (mut scene, nodes) = rig(&BLENDER, &[], false);
    add_joint(&mut scene, nodes[19].unwrap(), "LeftHand");
    let p = auto_map(&scene, &BTreeMap::new()).unwrap();
    assert_eq!(p.humanoid.node("leftHand"), nodes[20]);
    assert!(p.report.has_rule("humanoid.ambiguous"));
}

#[test]
fn the_same_scene_gives_the_same_proposal() {
    let (scene, _) = rig(&UNITY_BIP, &[], false);
    assert_eq!(auto_map(&scene, &BTreeMap::new()).unwrap(), auto_map(&scene, &BTreeMap::new()).unwrap());
}

#[test]
fn nameless_skeleton_without_a_neck_keeps_the_head() {
    let owned = nameless();
    let names: Vec<&str> = owned.iter().map(String::as_str).collect();
    let (mut scene, nodes) = rig(&names, &[4], false);
    let p = auto_map(&scene, &BTreeMap::new()).unwrap();
    assert_eq!(p.humanoid.node("head"), nodes[5], "the eye must not become the head");
    assert!(p.humanoid.node("neck").is_none());
    assert_eq!(profile_errors(&mut scene, &p), Vec::<String>::new());
}

#[test]
fn an_override_may_depend_on_an_inferred_parent() {
    let owned = nameless();
    let names: Vec<&str> = owned.iter().map(String::as_str).collect();
    let (scene, nodes) = rig(&names, &[], false);
    let overrides = BTreeMap::from([(bone("upperChest"), nodes[3].unwrap())]);
    let p = auto_map(&scene, &overrides).unwrap();
    assert_eq!(p.humanoid.node("upperChest"), nodes[3]);
    assert_eq!(how(&p, "upperChest"), Some(MatchHow::Override));
    assert_eq!(p.humanoid.node("chest"), nodes[2]);

    // Every required bone named, only the optional chest not: the override still finds its parent.
    let mut vroid = VROID;
    vroid[2] = "bone_002";
    let (mut scene, nodes) = rig(&vroid, &[], false);
    let p = auto_map(&scene, &BTreeMap::from([(bone("upperChest"), nodes[3].unwrap())])).unwrap();
    assert_eq!(how(&p, "upperChest"), Some(MatchHow::Override));
    assert_eq!(p.humanoid.node("chest"), nodes[2]);
    assert_eq!(how(&p, "chest"), Some(MatchHow::Inferred));
    assert_eq!(profile_errors(&mut scene, &p), Vec::<String>::new());
}

/// Maps the nameless rig without the bones in `skip`.
fn nameless_without(skip: &[usize]) -> (Result<HumanoidProposal, MapError>, Vec<Option<NodeIx>>) {
    let owned = nameless();
    let names: Vec<&str> = owned.iter().map(String::as_str).collect();
    let (scene, nodes) = rig(&names, skip, false);
    (auto_map(&scene, &BTreeMap::new()), nodes)
}

#[test]
fn nameless_limbs_with_a_missing_joint_are_not_shifted() {
    // The foot must not become the lower leg, nor a toe the foot, nor a hand the forearm.
    for (skip, missing) in [(10, "leftLowerLeg"), (11, "leftFoot"), (19, "leftLowerArm"), (20, "leftHand")] {
        let e = nameless_without(&[skip]).0.unwrap_err();
        assert!(e.missing_required.contains(&bone(missing)), "{skip}: {:?}", e.missing_required);
        assert!(e.report.has_rule("humanoid.required_missing"), "{skip}");
    }
    // Optional joints may be missing: no shoulder, no toes.
    let (p, nodes) = nameless_without(&[17, 12]);
    let p = p.unwrap();
    assert_eq!(p.humanoid.node("leftUpperArm"), nodes[18]);
    assert_eq!(p.humanoid.node("leftHand"), nodes[20]);
    assert_eq!(p.humanoid.node("leftFoot"), nodes[11]);
    assert!(p.humanoid.node("leftShoulder").is_none() && p.humanoid.node("leftToes").is_none());
    assert_eq!(p.humanoid.node("rightShoulder"), nodes[21]);
    assert_eq!(p.humanoid.node("rightToes"), nodes[16]);
}

#[test]
fn nameless_fingers_do_not_shift_the_arm() {
    let owned = nameless();
    let names: Vec<&str> = owned.iter().map(String::as_str).collect();
    let (mut scene, nodes) = rig(&names, &[17], false);
    let mut tip = nodes[20].unwrap();
    for i in 0..3 {
        let finger = add_joint(&mut scene, tip, &format!("bone_1{i:02}"));
        scene.nodes[finger.index()].transform.translation = Vec3::new(0.03, 0.0, 0.0);
        tip = finger;
    }
    let p = auto_map(&scene, &BTreeMap::new()).unwrap();
    assert_eq!(p.humanoid.node("leftUpperArm"), nodes[18]);
    assert_eq!(p.humanoid.node("leftHand"), nodes[20]);
}

#[test]
fn a_zero_scaled_bone_is_refused() {
    let (mut scene, nodes) = rig(&MIXAMO, &[], false);
    scene.nodes[nodes[20].unwrap().index()].transform.scale = Vec3::new(1.0, 0.0, 1.0);
    let e = auto_map(&scene, &BTreeMap::new()).unwrap_err();
    let f = e.report.findings.iter().find(|f| f.rule == "humanoid.scale_invalid").unwrap();
    assert_eq!(f.location.as_deref(), Some("avatar/humanoid/leftHand"));
    assert!(f.hint.is_some());
}

#[test]
fn moving_the_rig_along_x_keeps_the_sides() {
    let owned = nameless();
    let names: Vec<&str> = owned.iter().map(String::as_str).collect();
    for (names, shift) in [(&MIXAMO[..], 5.0), (&names[..], -5.0), (&names[..], 5.0)] {
        let (mut scene, _) = rig(names, &[], false);
        let root = scene.roots[0].index();
        scene.nodes[root].transform.translation.x += shift;
        let p = auto_map(&scene, &BTreeMap::new()).unwrap();
        assert!(!p.report.has_rule("humanoid.sides_overlap"), "{shift}");
    }
}

/// The proposal's nodes for every bone in `BONES`, against the rig's nodes.
fn assert_maps(p: &HumanoidProposal, nodes: &[Option<NodeIx>]) {
    for (i, name) in BONES.iter().enumerate() {
        assert_eq!(p.humanoid.node(name), nodes[i], "{name}");
    }
}

#[test]
fn two_humanoids_in_one_scene_are_each_mappable() {
    let mut scene = IoScene::new();
    let a = rig_in(&mut scene, &MIXAMO, &[], false, Vec3::ZERO);
    let b = rig_in(&mut scene, &VROID, &[], false, Vec3::new(3.0, 0.0, 0.0));
    // With no overrides the first skeleton (lowest hips) is the avatar and the other is reported, not mixed in.
    let p = auto_map(&scene, &BTreeMap::new()).unwrap();
    assert_maps(&p, &a);
    assert!(!p.report.has_rule("humanoid.ambiguous"));
    assert_eq!(p.report.findings.iter().filter(|f| f.rule == "humanoid.other_skeleton").count(), 1);
    assert_eq!(p.report.count(omuzaki_io_api::Severity::Warning), 0);
    // An override inside the second skeleton selects it.
    let pick = BTreeMap::from([(bone("hips"), b[0].unwrap())]);
    let q = auto_map(&scene, &pick).unwrap();
    assert_maps(&q, &b);
    assert!(q.report.has_rule("humanoid.other_skeleton"));
    assert!(!q.report.has_errors());
}

#[test]
fn an_unrelated_skinned_object_does_not_hide_the_humanoid() {
    // The stranger comes first, with a node that reads as `Hips`: a union of candidates would take it.
    let mut scene = IoScene::new();
    let hips = scene.add_node(Node::new("Hips"));
    let tail = scene.add_node(Node::new("Tail").at(Vec3::new(0.0, -0.5, 0.0)));
    scene.attach(hips, tail);
    scene.roots.push(hips);
    scene.add_skin(Skin::new("prop", vec![hips, tail], vec![Mat4::IDENTITY; 2]));
    let nodes = rig_in(&mut scene, &BLENDER, &[], false, Vec3::new(2.0, 0.0, 0.0));
    let p = auto_map(&scene, &BTreeMap::new()).unwrap();
    assert_maps(&p, &nodes);
    assert!(p.report.has_rule("humanoid.other_skeleton"));
}

#[test]
fn helper_nodes_between_joints_do_not_hide_limbs() {
    let owned = nameless();
    let names: Vec<&str> = owned.iter().map(String::as_str).collect();
    let (mut scene, nodes) = rig(&names, &[], false);
    // Wrap the spine and the left leg in plain grouping nodes that are not skin joints.
    let hips = nodes[0].unwrap();
    for slot in [1, 9] {
        let child = nodes[slot].unwrap();
        let group = scene.add_node(Node::new(format!("group_{slot}")));
        scene.nodes[hips.index()].children.retain(|c| *c != child);
        scene.attach(hips, group);
        scene.attach(group, child);
    }
    let p = auto_map(&scene, &BTreeMap::new()).unwrap();
    for spec in omuzaki_io_api::HUMAN_BONES.iter().filter(|b| b.required) {
        assert_eq!(how(&p, spec.name), Some(MatchHow::Inferred), "{}", spec.name);
    }
    assert_eq!(p.humanoid.node("spine"), nodes[1]);
    assert_eq!(p.humanoid.node("leftUpperLeg"), nodes[9]);
    assert_eq!(p.humanoid.node("leftFoot"), nodes[11]);
    assert_eq!(p.humanoid.node("head"), nodes[5]);
}

#[test]
fn a_skirt_joint_below_the_feet_does_not_break_the_legs() {
    let owned = nameless();
    let names: Vec<&str> = owned.iter().map(String::as_str).collect();
    let (mut scene, nodes) = rig(&names, &[], false);
    let skirt = add_joint(&mut scene, nodes[0].unwrap(), "hem");
    scene.nodes[skirt.index()].transform.translation = Vec3::new(0.0, -2.0, 0.0);
    let p = auto_map(&scene, &BTreeMap::new()).unwrap();
    for (slot, name) in [(10, "leftLowerLeg"), (11, "leftFoot"), (12, "leftToes")] {
        assert_eq!(p.humanoid.node(name), nodes[slot], "{name}");
    }
    for (slot, name) in [(14, "rightLowerLeg"), (15, "rightFoot"), (16, "rightToes")] {
        assert_eq!(p.humanoid.node(name), nodes[slot], "{name}");
    }
}

#[test]
fn legs_set_wider_of_the_hips_than_below_them_are_still_found() {
    // VRoid's proportions: each upper leg sits further to the side of the hips than below them, so the first joint
    // alone does not point down; the leg as a whole does.
    let owned = nameless();
    let names: Vec<&str> = owned.iter().map(String::as_str).collect();
    let (mut scene, nodes) = rig(&names, &[], false);
    for (slot, x) in [(9, 0.1f32), (13, -0.1)] {
        scene.nodes[nodes[slot].unwrap().index()].transform.translation = Vec3::new(x, -0.05, 0.0);
    }
    let p = auto_map(&scene, &BTreeMap::new()).unwrap();
    for (slot, name) in
        [(9, "leftUpperLeg"), (10, "leftLowerLeg"), (11, "leftFoot"), (13, "rightUpperLeg"), (15, "rightFoot")]
    {
        assert_eq!(p.humanoid.node(name), nodes[slot], "{name}");
    }
}
