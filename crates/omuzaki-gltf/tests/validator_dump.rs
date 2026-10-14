//! Writes files for an independent check with the Khronos glTF Validator. Not a test of anything in this crate: run it
//! by hand when the writer changes, and read the validator's verdict.
//!
//! ```text
//! OMUZAKI_VALIDATOR_DIR=$TMPDIR/scratch/validate/files cargo test -p omuzaki-gltf --test validator_dump -- --ignored
//! # in a scratch directory outside the repository: npm install gltf-validator, then run it on every file
//! # (`validateBytes` for the .glb files, `validateString` with an `externalResourceFunction` for the .gltf files).
//! ```

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_possible_truncation, clippy::cast_precision_loss)]

mod common;

use std::fs;
use std::path::Path;

use common::{export_files, export_glb, random_scene, sparse_morph_scene};
use omuzaki_core::glam::Vec3;
use omuzaki_io_api::{ExportOptions, IoScene, Layout, Mesh, MeshData, MorphTarget, Primitive};
use omuzaki_io_mock::fixtures::{authored_provenance, options_without_face_channels, tiny_avatar};

/// A mesh over 65,535 vertices (32-bit indices) with one sparse, one all-zero and one dense morph target.
fn big_mesh_scene() -> IoScene {
    let n = 70_000usize;
    let positions: Vec<Vec3> = (0..n).map(|i| Vec3::new((i % 300) as f32, (i / 300) as f32, 0.0)).collect();
    let mut data = MeshData { positions, normals: vec![Vec3::Z; n], ..MeshData::default() };
    data.indices = (0..n as u32 - 2).flat_map(|i| [i, i + 1, i + 2]).collect();
    let mut prim = Primitive::new(data);
    let mut sparse = vec![Vec3::ZERO; n];
    sparse[3] = Vec3::new(0.0, 0.5, 0.0);
    sparse[69_999] = Vec3::new(0.25, -0.5, 0.0);
    prim.morph_targets.push(MorphTarget::new(sparse));
    prim.morph_targets.push(MorphTarget::new(vec![Vec3::ZERO; n]));
    prim.morph_targets.push(MorphTarget::new((0..n).map(|i| Vec3::splat(i as f32 * 1e-5)).collect()));
    let mut mesh = Mesh::new("big");
    mesh.primitives.push(prim);
    let mut scene = IoScene::new();
    scene.provenance = Some(authored_provenance());
    let ix = scene.add_mesh(mesh);
    let mut node = omuzaki_io_api::Node::new("holder");
    node.mesh = Some(ix);
    scene.roots = vec![scene.add_node(node)];
    scene
}

#[test]
#[ignore = "writes files for the Khronos validator; run by hand"]
fn dump_files_for_the_validator() {
    let Ok(dir) = std::env::var("OMUZAKI_VALIDATOR_DIR") else {
        println!("skipped: set OMUZAKI_VALIDATOR_DIR to a directory");
        return;
    };
    let dir = Path::new(&dir);
    fs::create_dir_all(dir).unwrap();
    for seed in 0..40 {
        fs::write(dir.join(format!("random-{seed}.glb")), export_glb(&random_scene(seed))).unwrap();
    }
    fs::write(dir.join("sparse-morphs.glb"), export_glb(&sparse_morph_scene())).unwrap();
    fs::write(dir.join("big-mesh.glb"), export_glb(&big_mesh_scene())).unwrap();
    let options = options_without_face_channels();
    let avatar =
        export_files(&tiny_avatar(), &ExportOptions::default().with_perfect_sync(omuzaki_io_api::PerfectSync::Off));
    fs::write(dir.join("tiny-avatar.glb"), &avatar.unwrap()[0].bytes).unwrap();
    for seed in [3, 11, 21] {
        let sub = dir.join(format!("separate-{seed}"));
        fs::create_dir_all(&sub).unwrap();
        for file in export_files(&random_scene(seed), &options.clone().with_layout(Layout::Separate)).unwrap() {
            fs::write(sub.join(&file.path), &file.bytes).unwrap();
        }
    }
    println!("wrote files to {}", dir.display());
}
