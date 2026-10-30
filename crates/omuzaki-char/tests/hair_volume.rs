//! The hair as one mass (#2479, #2481, #2482): the locks' normals bend toward the hair's volume, so neighbouring locks
//! shade alike and the outline hull grows only off the silhouette; each lock's sides thin out through its vertex alpha;
//! and the strand texture follows the shading's parameters. Built on the kit's default style on the template body.

#![allow(clippy::expect_used, clippy::unwrap_used)] // fixtures; a bad one should stop the test with its reason
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use std::collections::HashMap;

use omuzaki_char_api::{
    Assembler, BuildOptions, BuildServices, CharId, CharacterDef, HairParams, HairShading, PartDef, PartId, PartKindId,
    PartMesh, PartOutput, build_part, plan,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::Vec3;
use serde_json::json;

fn part(kind: &str, params: serde_json::Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer: 0, attach: None, params }
}

/// The default hair on the template body and head, with `shading`.
fn hair(shading: HairShading) -> PartOutput {
    omuzaki_char::link();
    let mut def: CharacterDef = mock_character().unwrap();
    def.parts.clear();
    def.parts.insert(PartId::new("body").unwrap(), part("body", json!({})));
    def.parts.insert(PartId::new("head").unwrap(), part("head", json!({})));
    let params = HairParams { shading, ..HairParams::default() };
    def.parts.insert(PartId::new("hair").unwrap(), part("hair", serde_json::to_value(params).unwrap()));
    let (id, options, services) = (CharId::new("t").unwrap(), BuildOptions::default(), BuildServices::default());
    let mut assembler = Assembler::new(&id, &def, &options);
    let mut result = None;
    for p in plan(&def).unwrap() {
        let out = build_part(&def, &p, &options, &services, assembler.published()).unwrap();
        if p.as_str() == "hair" {
            result = Some(build_part(&def, &p, &options, &services, assembler.published()).unwrap());
        }
        assembler.add(&p, out).unwrap();
    }
    result.unwrap()
}

fn with(f: impl FnOnce(&mut HairShading)) -> HairShading {
    let mut shading = HairShading::default();
    f(&mut shading);
    shading
}

fn static_mesh(out: &PartOutput) -> &PartMesh {
    out.meshes.iter().find(|m| m.name == "hair-static").expect("a static mesh")
}

/// The mean angle, degrees, between the normals of vertices that sit within 4 mm of each other but belong to different
/// locks (a different strand seed in `hair-info`): how much neighbouring locks disagree about which way they face.
fn neighbour_disagreement(mesh: &PartMesh) -> f32 {
    const CELL: f32 = 0.004;
    let g = &mesh.geometry;
    let info = &g.extra_attribute(omuzaki_char_api::HAIR_INFO).expect("hair-info").data;
    let cell = |p: Vec3| ((p.x / CELL).floor() as i32, (p.y / CELL).floor() as i32, (p.z / CELL).floor() as i32);
    let mut grid: HashMap<(i32, i32, i32), Vec<usize>> = HashMap::new();
    for (i, p) in g.positions.iter().enumerate() {
        grid.entry(cell(*p)).or_default().push(i);
    }
    let (mut sum, mut count) = (0.0_f32, 0_u32);
    for members in grid.values() {
        for (k, &a) in members.iter().enumerate() {
            for &b in &members[k + 1..] {
                if info[3 * a] == info[3 * b] {
                    continue;
                }
                sum += g.normals[a].dot(g.normals[b]).clamp(-1.0, 1.0).acos().to_degrees();
                count += 1;
            }
        }
    }
    assert!(count > 100, "too few neighbouring pairs ({count}) to measure");
    sum / count as f32
}

#[test]
fn at_full_blend_neighbouring_locks_face_the_same_way() {
    let own = neighbour_disagreement(static_mesh(&hair(with(|s| s.normal_blend = 0.0))));
    let blended = neighbour_disagreement(static_mesh(&hair(HairShading::default())));
    // Each lock's own tube normals point every way round it; the volume's agree from lock to lock.
    assert!(own > 45.0, "own normals disagree by {own:.1} degrees");
    assert!(blended < 12.0, "blended normals disagree by {blended:.1} degrees");
}

#[test]
fn at_full_blend_every_normal_faces_out_of_the_head() {
    let out = hair(HairShading::default());
    let g = &static_mesh(&out).geometry;
    let centre = g.positions.iter().copied().sum::<Vec3>() / g.positions.len() as f32;
    let inward = g.positions.iter().zip(&g.normals).filter(|(p, n)| n.dot(**p - centre) < 0.0).count();
    // The outline's hull grows along these normals, so with none pointing back in it only rings the silhouette.
    assert!(inward * 100 < g.positions.len(), "{inward} of {} normals face in", g.positions.len());
    assert!(g.normals.iter().all(|n| (n.length() - 1.0).abs() < 1e-3));
}

#[test]
fn a_locks_sides_thin_out_through_its_alpha() {
    let alphas = |shading| {
        let out = hair(shading);
        static_mesh(&out).geometry.colours.iter().map(|c| c[3]).collect::<Vec<f32>>()
    };
    let feathered = alphas(HairShading::default());
    let low = feathered.iter().copied().fold(1.0_f32, f32::min);
    let feather = HairShading::default().edge_feather as f32;
    assert!((low - (1.0 - feather)).abs() < 0.05, "the sides keep {low}, not {}", 1.0 - feather);
    assert!(feathered.iter().filter(|a| **a > 0.999).count() * 4 > feathered.len(), "the middles keep every strand");
    let solid = alphas(with(|s| s.edge_feather = 0.0));
    assert!(solid.iter().all(|a| *a > 0.999), "no feather keeps every strand to the edge");
}

#[test]
fn the_strand_texture_follows_the_shading() {
    let texture = |shading| hair(shading).textures.first().expect("the strand texture").1.clone();
    let base = texture(HairShading::default());
    for shading in [
        with(|s| s.strands = 12),
        with(|s| s.strand_gap = 0.5),
        with(|s| s.strand_grain = 0.0),
        with(|s| s.tip_length = 0.04),
    ] {
        let changed = texture(shading.clone());
        // `tip_length` moves the locks' `v`, not the texture; the others repaint it.
        if (shading.tip_length - HairShading::default().tip_length).abs() > 0.0 {
            assert_eq!(changed, base);
        } else {
            assert_ne!(changed, base, "{shading:?} left the texture as it was");
        }
    }
}

#[test]
fn a_longer_tip_maps_more_of_each_lock_onto_the_tip_band() {
    let tip_share = |shading| {
        let out = hair(shading);
        let g = &static_mesh(&out).geometry;
        g.uvs.iter().filter(|uv| uv.y > 1.0 - omuzaki_char::hair::strands::TIP).count()
    };
    let short = tip_share(HairShading::default());
    let long = tip_share(with(|s| s.tip_length = 0.04));
    assert!(long > short, "a 4 cm tip put {long} vertices on the tip band against {short}");
}
