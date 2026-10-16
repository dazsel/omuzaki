//! The original test head the `ARKit` shape generator is judged on (#1090): it is a valid avatar, every one of its morph
//! targets is mirror-symmetric, and an ignored `dump` test writes its expressions as OBJ files for
//! `tools/arkit-shape-sheet.py`.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::path::PathBuf;

use common::def;
use common::head::{PRESETS, TARGET_NAMES, dump_expression, head};
use omuzaki_core::glam::Vec3;
use omuzaki_render_api::MtoonSupport;

/// Largest difference allowed between a delta and its mirror image.
const MIRROR_TOLERANCE: f32 = 1e-6;

fn mirrored(v: Vec3) -> Vec3 {
    Vec3::new(-v.x, v.y, v.z)
}

#[test]
fn the_test_head_is_a_valid_symmetric_avatar() {
    let f = head();
    let report = f.scene.check();
    assert!(!report.has_errors(), "the scene is valid: {report:?}");
    let (avatar, _) =
        omuzaki_io_ops::prepare::avatar::to_avatar(&f.scene, &def(), MtoonSupport::NONE).expect("imports");
    assert_eq!(avatar.meshes.len(), 7, "one primitive per material");

    let face = &f.scene.meshes[f.scene.nodes[f.node("Face").index()].mesh.unwrap().index()];
    assert_eq!(face.morph_target_names, TARGET_NAMES);
    for (index, primitive) in face.primitives.iter().enumerate() {
        assert_eq!(primitive.morph_targets.len(), TARGET_NAMES.len(), "primitive {index} has every target");
        let positions = &primitive.mesh.positions;
        for (t, target) in primitive.morph_targets.iter().enumerate() {
            for (v, &p) in positions.iter().enumerate() {
                // Some vertex sits at the mirror position and moves as the mirror image of this one.
                let twin = positions.iter().zip(&target.positions).any(|(&q, &d)| {
                    q.distance(mirrored(p)) < MIRROR_TOLERANCE
                        && d.distance(mirrored(target.positions[v])) < MIRROR_TOLERANCE
                });
                assert!(twin, "primitive {index}, target {}, vertex {v} has no mirror twin", TARGET_NAMES[t]);
            }
        }
    }
    // Every target moves something, or the symmetry check above would be vacuous.
    for (t, name) in TARGET_NAMES.iter().enumerate() {
        let moved = face.primitives.iter().flat_map(|p| &p.morph_targets[t].positions).any(|d| d.length() > 1e-3);
        assert!(moved, "target {name} moves vertices");
    }
    // Every triangle faces outward from the skull.
    let skin = &face.primitives[0].mesh;
    let centre = Vec3::new(0.0, 1.53, 0.0);
    for tri in skin.indices.as_chunks::<3>().0 {
        let [p0, p1, p2] = tri.map(|i| skin.positions[i as usize]);
        let normal = (p1 - p0).cross(p2 - p0);
        assert!(normal.dot((p0 + p1 + p2) / 3.0 - centre) > 0.0, "a skin triangle winds inward");
    }
}

#[test]
fn every_preset_drives_its_own_target() {
    let f = head();
    let avatar = f.scene.avatar.as_ref().unwrap();
    for (t, preset) in PRESETS.iter().enumerate() {
        let e = &avatar.expressions[&omuzaki_io_api::ExpressionName::parse(preset).unwrap()];
        assert_eq!(e.morph_binds.len(), 1);
        assert_eq!((e.morph_binds[0].node, e.morph_binds[0].index as usize), (f.node("Face"), t));
    }
}

/// Writes `neutral.obj` and each expression at weights 0.5 and 1 into `$OMUZAKI_SHEET_DIR`.
#[test]
#[ignore = "writes OBJ files for tools/arkit-shape-sheet.py when OMUZAKI_SHEET_DIR is set"]
#[allow(clippy::disallowed_methods)] // test tooling: the dump is a plain folder of files for the sheet script
fn dump() {
    let Some(dir) = std::env::var_os("OMUZAKI_SHEET_DIR").map(PathBuf::from) else { return };
    std::fs::create_dir_all(&dir).unwrap();
    let f = head();
    dump_expression(&f.scene, PRESETS[0], 0.0, &dir.join("neutral.obj"));
    for preset in PRESETS {
        for weight in [0.5, 1.0] {
            dump_expression(&f.scene, preset, weight, &dir.join(format!("{preset}-{weight}.obj")));
        }
    }
}
