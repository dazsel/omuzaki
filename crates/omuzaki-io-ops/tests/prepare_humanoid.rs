//! `prepare::humanoid`: every scene is a synthetic 25-bone T-pose built here (+Z front, the character's left at +X).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_io_api::{HumanBoneName, IoScene, Node, NodeIx, Skin, VrmVersion, vrm_profile};
use omuzaki_io_ops::prepare::humanoid::{Convention, HumanoidProposal, MatchHow, apply, auto_map};

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
    let (mut anchor, mut carry, mut nodes) = (Vec::<Option<usize>>::new(), Vec::<Vec3>::new(), Vec::new());
    for (i, (parent, o)) in LAYOUT.iter().enumerate() {
        let mut offset = Vec3::from(*o);
        if mirror {
            offset.x = -offset.x;
        }
        let (a, c) = match parent {
            None => (None, offset),
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
    (scene, nodes)
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

/// Maps `names` and checks that the 22 non-face bones are matched through `conv`.
fn convention(names: &[&str], conv: Convention) {
    let (mut scene, nodes) = rig(names, &[], false);
    let p = auto_map(&scene, &BTreeMap::new()).unwrap();
    for (i, name) in BONES.iter().enumerate().filter(|(i, _)| !(6..=8).contains(i)) {
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
