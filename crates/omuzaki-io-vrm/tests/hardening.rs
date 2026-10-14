//! Circular node constraints: a hostile file can make bones drive each other, which no rig can evaluate. Import drops
//! them with a finding, so what remains passes the scene check, and the exports refuse a scene that has one (and `vrm1/write.rs` still leaves them out as a safety net).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::assert_is_empty)]

mod common;

use omuzaki_io_api::{ConstraintKind, Exporter, IoError, NodeConstraint};
use omuzaki_io_vrm::{Vrm0Exporter, Vrm1Exporter, Vrm1Importer};
use serde_json::json;

use common::{avatar, export_with, glb, import, minimal1, options};

fn rotation_body(source: usize) -> serde_json::Value {
    json!({"specVersion": "1.0", "constraint": {"rotation": {"source": source}}})
}

#[test]
fn constraints_that_drive_each_other_are_dropped_on_import() {
    let (mut doc, bin) = minimal1();
    // Nodes 9 and 10 are leftUpperArm and leftLowerArm: each is the other's source.
    for (node, source) in [(9, 10), (10, 9)] {
        doc["nodes"][node]["extensions"] = json!({"VRMC_node_constraint": rotation_body(source)});
    }
    doc["extensionsUsed"] = json!(["VRMC_vrm", "VRMC_node_constraint"]);
    let out = import(&Vrm1Importer, &glb(&doc, &bin));
    assert!(out.report.has_rule("vrm.constraint_dropped"), "{:#?}", out.report);
    assert!(out.scene.avatar.as_ref().unwrap().constraints.is_empty());
    assert!(!out.scene.check().has_errors(), "what remains satisfies the scene's invariants");
}

/// The fixture constrains `leftTwist` by `leftLowerArm`; this adds the opposite edge.
fn circular_avatar() -> omuzaki_io_api::IoScene {
    let (mut scene, ids) = avatar();
    scene.avatar.as_mut().unwrap().constraints.push(NodeConstraint {
        node: ids.node("leftLowerArm"),
        kind: ConstraintKind::Rotation { source: ids.node("leftTwist") },
        weight: 1.0,
    });
    scene
}

#[test]
fn a_circular_scene_is_rejected_by_the_scene_check_before_any_exporter_writes() {
    let scene = circular_avatar();
    for exporter in [&Vrm1Exporter as &dyn Exporter, &Vrm0Exporter] {
        let Err(IoError::Rejected(report)) = export_with(exporter, &scene, &options()) else {
            panic!("a circular scene must be rejected");
        };
        let found: Vec<_> = report.errors().collect();
        assert_eq!(found.len(), 1, "{report:#?}");
        assert_eq!(found[0].rule, "avatar.constraint_cycle");
        assert_eq!(found[0].location.as_deref(), Some("avatar/constraints/0"));
    }
}
