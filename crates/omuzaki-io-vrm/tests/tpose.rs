//! The `vrm.tpose` rule: a warning when an exported rest pose has an arm or a leg off its T-pose axis.

mod common;

use common::{Ids, avatar, export_with, options, vrm0_friendly, with_arkit52};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_io_api::{ExportOptions, Exporter, IoError, IoScene, PerfectSync, Report, Severity};
use omuzaki_io_vrm::{Vrm0Exporter, Vrm1Exporter};

/// Rotates `bone` by `degrees` about `axis` and counter-rotates `child`, so only the segment that starts at `bone`
/// leaves its axis. Without a `child` (the last segments) the bone is just rotated.
fn bend(scene: &mut IoScene, ids: &Ids, bone: &str, child: Option<&str>, axis: Vec3, degrees: f32) {
    let q = Quat::from_axis_angle(axis, degrees.to_radians());
    let n = &mut scene.nodes[ids.node(bone).index()].transform;
    n.rotation = q * n.rotation;
    if let Some(child) = child {
        let c = &mut scene.nodes[ids.node(child).index()].transform;
        c.rotation = q.inverse() * c.rotation;
    }
}

/// The checked segments: first bone, the bone whose direction is counter-rotated, and the bend axis.
const SEGMENTS: [(&str, Option<&str>, Vec3); 8] = [
    ("leftUpperArm", Some("leftLowerArm"), Vec3::Z),
    ("leftLowerArm", None, Vec3::Z),
    ("rightUpperArm", Some("rightLowerArm"), Vec3::Z),
    ("rightLowerArm", None, Vec3::Z),
    ("leftUpperLeg", Some("leftLowerLeg"), Vec3::X),
    ("leftLowerLeg", None, Vec3::X),
    ("rightUpperLeg", Some("rightLowerLeg"), Vec3::X),
    ("rightLowerLeg", None, Vec3::X),
];

/// The report of both exporters for a VRM 1.0 scene and its 0.x twin.
fn both(make: impl Fn(&mut IoScene, &Ids)) -> [Report; 2] {
    let (mut s1, ids1) = avatar();
    make(&mut s1, &ids1);
    let (mut s0, ids0) = vrm0_friendly(avatar());
    make(&mut s0, &ids0);
    [Vrm1Exporter.check(&s1, &options()), Vrm0Exporter.check(&s0, &options())]
}

fn tpose(report: &Report) -> Vec<&omuzaki_io_api::Finding> {
    report.findings.iter().filter(|f| f.rule == "vrm.tpose").collect()
}

#[test]
fn fixtures_are_clean() {
    let (s1, _) = avatar();
    assert_eq!(tpose(&Vrm1Exporter.check(&s1, &options())).len(), 0);
    let (s0, _) = vrm0_friendly(avatar());
    assert_eq!(tpose(&Vrm0Exporter.check(&s0, &options())).len(), 0);
    let (s, _) = with_arkit52(avatar());
    let strict = ExportOptions::default().strict();
    export_with(&Vrm1Exporter, &s, &strict).expect("a T-pose avatar exports under strict");
}

#[test]
fn every_checked_segment_is_named() {
    for (bone, child, axis) in SEGMENTS {
        for report in both(|s, ids| bend(s, ids, bone, child, axis, 30.0)) {
            let found = tpose(&report);
            assert_eq!(found.len(), 1, "{bone}: {:?}", report.findings);
            let f = found[0];
            assert_eq!(f.severity, Severity::Warning);
            assert_eq!(f.location.as_deref(), Some(format!("avatar/humanoid/{bone}").as_str()));
            assert!(f.message.contains("30"), "{bone}: {}", f.message);
        }
    }
}

#[test]
fn a_pose_names_the_worst_bone_and_counts_the_rest() {
    for report in both(|s, ids| {
        bend(s, ids, "leftUpperArm", None, Vec3::Z, -45.0);
        bend(s, ids, "rightUpperArm", None, Vec3::Z, 45.0);
        bend(s, ids, "leftLowerArm", None, Vec3::Z, -20.0);
    }) {
        let found = tpose(&report);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].location.as_deref(), Some("avatar/humanoid/leftLowerArm"));
        assert!(found[0].message.contains("65"), "{}", found[0].message);
        assert!(found[0].message.contains("3 more"), "{}", found[0].message);
    }
}

#[test]
fn tolerance_edges() {
    for report in both(|s, ids| bend(s, ids, "leftUpperLeg", Some("leftLowerLeg"), Vec3::X, 9.0)) {
        assert_eq!(tpose(&report).len(), 0);
    }
    for report in both(|s, ids| bend(s, ids, "leftUpperLeg", Some("leftLowerLeg"), Vec3::X, 11.0)) {
        assert_eq!(tpose(&report).len(), 1);
    }
}

#[test]
fn exactly_the_tolerance_is_clean() {
    for (bone, child, axis) in SEGMENTS {
        for degrees in [10.0, -10.0] {
            for report in both(|s, ids| bend(s, ids, bone, child, axis, degrees)) {
                assert_eq!(tpose(&report).len(), 0, "{bone} at {degrees}: {:?}", report.findings);
            }
        }
    }
}

#[test]
fn strict_mode_rejects_it() {
    let (mut s, ids) = with_arkit52(avatar());
    bend(&mut s, &ids, "leftUpperArm", Some("leftLowerArm"), Vec3::Z, 40.0);
    let strict = ExportOptions::default().with_perfect_sync(PerfectSync::Off).strict();
    match export_with(&Vrm1Exporter, &s, &strict) {
        Err(IoError::Rejected(r)) => assert!(r.has_rule("vrm.tpose")),
        other => panic!("expected a rejection, got {:?}", other.map(|(_, r)| r)),
    }
    let (_, report) = export_with(&Vrm1Exporter, &s, &options()).expect("non-strict exports");
    assert!(report.has_rule("vrm.tpose"));
}

#[test]
fn world_space_not_local() {
    // The armature turned 90 degrees about Y and the hips turned back: local transforms are far from identity,
    // the world pose is unchanged.
    for report in both(|s, ids| {
        let turn = Quat::from_rotation_y(90.0_f32.to_radians());
        s.nodes[s.roots[0].index()].transform.rotation = turn;
        let hips = &mut s.nodes[ids.node("hips").index()].transform;
        hips.rotation = turn.inverse();
    }) {
        assert_eq!(tpose(&report).len(), 0, "{:?}", report.findings);
    }
    for report in both(|s, _| {
        s.nodes[s.roots[0].index()].transform.rotation = Quat::from_rotation_y(90.0_f32.to_radians());
    }) {
        let found = tpose(&report);
        assert_eq!(found.len(), 1);
        assert!(found[0].message.contains("90"), "{}", found[0].message);
    }
}

#[test]
fn mirrored_sides_warn() {
    for report in both(|s, ids| {
        for bone in ["leftLowerArm", "leftHand"] {
            s.nodes[ids.node(bone).index()].transform.translation.x *= -1.0;
        }
    }) {
        let found = tpose(&report);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].location.as_deref(), Some("avatar/humanoid/leftUpperArm"));
        assert!(found[0].message.contains("180"), "{}", found[0].message);
    }
}

#[test]
fn hostile_scenes_do_not_panic_or_hang() {
    let check = |s: &IoScene| Vrm1Exporter.check(s, &options());

    let (mut s, _) = avatar();
    if let Some(av) = &mut s.avatar {
        av.humanoid.bones.clear();
    }
    assert_eq!(tpose(&check(&s)).len(), 0, "missing bones");

    let (mut s, _) = avatar();
    s.avatar = None;
    assert_eq!(tpose(&check(&s)).len(), 0, "no avatar");

    let (mut s, _) = avatar();
    for n in &mut s.nodes {
        n.transform.translation = Vec3::ZERO;
    }
    assert_eq!(tpose(&check(&s)).len(), 0, "zero-length segments");

    let (mut s, ids) = avatar();
    s.nodes[ids.node("leftLowerArm").index()].transform.translation.x = f32::NAN;
    let _ = check(&s);

    let (mut s, ids) = avatar();
    let hips = ids.node("hips");
    s.nodes[ids.node("leftHand").index()].children.push(hips);
    let _ = check(&s);

    // A chain of 100 000 nodes above the armature.
    let (mut s, ids) = avatar();
    let armature = s.roots[0];
    let mut above = Vec::new();
    for i in 0..100_000 {
        above.push(s.add_node(omuzaki_io_api::Node::new(format!("c{i}"))));
    }
    for pair in above.windows(2) {
        s.attach(pair[0], pair[1]);
    }
    if let Some(last) = above.last() {
        s.attach(*last, armature);
    }
    s.roots = above.first().copied().into_iter().collect();
    assert_eq!(tpose(&check(&s)).len(), 0, "deep chain, still a T");
    let _ = ids;
}
