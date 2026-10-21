//! Parity of the `collar` and `tie` garments (`omuzaki_char::outfit::neckwear`, #191) with the prototype's pointed collar,
//! tie and tie stripe texture, as the Director's reference for #191 gives them.
//!
//! The oracle for the geometry is the sample character's dump (`geometry/game`: the `tie` mesh, and the collar's shells
//! inside the `shirt` mesh after each side's sleeve, cuff and pearl), private test material kept in the local assets
//! folder: without it (always, in CI and in the cloud) those tests skip with a message. The texture needs no dump: the
//! prototype's canvas is described exactly (stripes, slant, weave coverage), and the test draws it independently, by
//! supersampling, and compares the painted texture with it.
//!
//! The port hangs on the shirt it is given, which here is the soft-sd body's shirt at its least ease (1 mm) where the
//! prototype's has none; so surfaces are expected within a few millimetres, and the tie, built from the same heights,
//! vertex by vertex.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers load local files; a bad one should stop the test with its reason
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)] // test arithmetic
#![allow(clippy::many_single_char_names, clippy::needless_range_loop)] // x, y, c: pixels and channels

use omuzaki_char_api::{
    BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, PartDef, PartId, PartKindId,
    RegistryBuilder,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::Vec3;
use omuzaki_core::mesh::MeshData;
use omuzaki_face_mock::MockRasterizer;
use omuzaki_test::jsref::JsRef;
use serde_json::json;

const SET: &str = "game";
const TORSO: usize = 37 * 61;
const SLEEVE: usize = 30 * 25;
const CUFF: usize = 16 * 41;
const PEARL: usize = 100 + 119;
const SHELL: usize = 15 * 45;
/// The tie: a blade of 12 rings and a knot of 16, 27 points each.
const TIE: usize = (12 + 16) * 27;
/// The stripe texture's side, pixels, and the largest difference from the reference canvas in any channel.
const PX: usize = 128;
const PER_CHANNEL: i32 = 4;

fn built() -> BuiltCharacter {
    // Referring to the character crate links its part kinds.
    assert!(!omuzaki_char::template::registry::templates().is_empty());
    let mut def: CharacterDef = mock_character().unwrap();
    def.parts.clear();
    let part = |kind: &str, layer: i32, attach: bool, params: serde_json::Value| PartDef {
        kind: PartKindId::new(kind).unwrap(),
        parent: None,
        enabled: true,
        layer,
        attach: attach.then(|| {
            serde_json::from_value(json!({ "to": "anchor", "part": "shirt", "anchor": "collar_ring" })).unwrap()
        }),
        params,
    };
    def.template = "soft-sd".parse().unwrap();
    def.parts.insert(PartId::new("body").unwrap(), part("body", 0, false, json!({})));
    let shirt = json!({ "common": { "ease": 0.001, "fabric": { "kind": "plain", "colour": "#f5f2ed" } } });
    def.parts.insert(PartId::new("shirt").unwrap(), part("shirt", 20, false, shirt));
    def.parts.insert(PartId::new("collar").unwrap(), part("collar", 21, true, json!({})));
    def.parts.insert(PartId::new("tie").unwrap(), part("tie", 22, true, json!({})));
    let services = BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer));
    RegistryBuilder.build(&CharId::new("t").unwrap(), &def, &BuildOptions::default(), &services).unwrap()
}

fn mesh<'a>(built: &'a BuiltCharacter, name: &str) -> &'a MeshData {
    &built.meshes.iter().find(|m| m.mesh.name == name).unwrap_or_else(|| panic!("no mesh `{name}`")).mesh.geometry
}

fn dump(name: &str) -> Option<MeshData> {
    let js = JsRef::open()?;
    let geometry = js.geometry(SET).expect("the geometry dump loads");
    Some(geometry.mesh(name).unwrap_or_else(|| panic!("the dump has `{name}`")).mesh.clone())
}

/// (mean, maximum) of the distances between matching vertices.
fn pairwise(a: &[Vec3], b: &[Vec3]) -> (f64, f64) {
    assert_eq!(a.len(), b.len());
    let d: Vec<f64> = a.iter().zip(b).map(|(p, q)| f64::from(p.distance(*q))).collect();
    (d.iter().sum::<f64>() / d.len() as f64, d.iter().copied().fold(0.0, f64::max))
}

#[test]
fn the_tie_is_the_prototypes_vertex_by_vertex_within_the_shirts_ease() {
    let Some(js) = dump("tie") else {
        eprintln!("skipped: no JS dumps ($OMUZAKI_ASSETS/goldens/js)");
        return;
    };
    assert_eq!((js.positions.len(), js.triangle_count()), (TIE, 1352));
    let built = built();
    let port = mesh(&built, "tie-tie");
    assert_eq!((port.positions.len(), port.triangle_count()), (TIE, 1352), "the same grids");
    let (mean, max) = pairwise(&port.positions, &js.positions);
    println!("tie: mean {:.2} max {:.2} mm per vertex", mean * 1e3, max * 1e3);
    assert!(max <= 0.0025, "the tie is the prototype's, 1 mm further out with the shirt's ease: {max}");
    let uv_error =
        port.uvs.iter().zip(&js.uvs).map(|(a, b)| (a.x - b.x).abs().max((a.y - b.y).abs())).fold(0.0, f32::max);
    println!("tie: uv difference {uv_error:.2e}");
    assert!(uv_error <= 1e-4, "the texture coordinates are the prototype's: {uv_error}");
}

/// The distance from each of `from` to the nearest of `to`: (mean, maximum).
fn nearest(from: &[Vec3], to: &[Vec3]) -> (f64, f64) {
    let d: Vec<f64> = from
        .iter()
        .map(|p| to.iter().map(|q| f64::from(p.distance_squared(*q))).fold(f64::INFINITY, f64::min).sqrt())
        .collect();
    (d.iter().sum::<f64>() / d.len() as f64, d.iter().copied().fold(0.0, f64::max))
}

#[test]
fn the_collar_lies_on_the_prototypes_shells() {
    let Some(js) = dump("shirt") else {
        eprintln!("skipped: no JS dumps ($OMUZAKI_ASSETS/goldens/js)");
        return;
    };
    let built = built();
    let port = mesh(&built, "collar-shell");
    // The port's shells are one grid of both halves (15 x 89, the back column shared), the prototype's two halves of 15 x 45.
    assert_eq!(port.positions.len(), 2 * 15 * 89);
    assert_eq!(port.triangle_count(), 4 * 14 * 44 * 2, "the prototype's triangles: two shells on two halves");
    let side = SLEEVE + CUFF + PEARL + 2 * SHELL;
    let mut prototype = Vec::new();
    for half in 0..2 {
        let start = TORSO + half * side + SLEEVE + CUFF + PEARL;
        prototype.extend_from_slice(&js.positions[start..start + 2 * SHELL]);
    }
    let (to_prototype, max) = nearest(&port.positions, &prototype);
    let (to_port, max_back) = nearest(&prototype, &port.positions);
    println!(
        "collar: port to prototype mean {:.2} max {:.2} mm; prototype to port mean {:.2} max {:.2} mm",
        to_prototype * 1e3,
        max * 1e3,
        to_port * 1e3,
        max_back * 1e3
    );
    assert!(to_prototype <= 0.0035 && max <= 0.008, "the collar sits where the prototype's does: {to_prototype} {max}");
    assert!(to_port <= 0.0035 && max_back <= 0.008, "and the prototype's is covered: {to_port} {max_back}");
    let lines = mesh(&built, "collar-lines");
    assert_eq!(lines.positions.len(), 2 * (45 + 15) * 6, "two cords of 45 and 15 points on each half");
    assert_eq!(lines.triangle_count(), 2 * (44 + 14) * 5 * 2);
}

// ------------------------------------------------------------------------------------------------ the texture

/// The prototype's canvas, drawn independently: a pixel's colour is the average over a grid of samples inside it, each
/// sample given by the canvas's drawing order (the ground, the slanted stripes, then the weave). The mock rasteriser the
/// build is given samples a pixel's centre only, so the reference does too (`N = 1`); a rasteriser that weighs coverage, as
/// the production one does, differs only on the stripes' edges and the weave's columns, where this test does not look.
fn reference(base: [f64; 3], main: [f64; 3], edge: [f64; 3], thin: [f64; 3]) -> Vec<[f64; 3]> {
    const N: usize = 1;
    let stripes: [(f64, f64, [f64; 3]); 4] =
        [(6.0, 15.0, main), (2.0, 3.2, edge), (21.0, 3.2, edge), (36.0, 4.0, thin)];
    let mut out = Vec::with_capacity(PX * PX);
    for py in 0..PX {
        for px in 0..PX {
            let mut sum = [0.0; 3];
            for sy in 0..N {
                for sx in 0..N {
                    let (x, y) = (px as f64 + (sx as f64 + 0.5) / N as f64, py as f64 + (sy as f64 + 0.5) / N as f64);
                    let mut c = base;
                    for (start, width, colour) in stripes {
                        // The band between x - y = start + 64 k and start + width + 64 k, 45 degrees, k in -2..=3.
                        let u = x - y;
                        if (-2..=3).any(|k| {
                            let a = start + 64.0 * f64::from(k);
                            u >= a && u < a + width
                        }) {
                            c = colour;
                        }
                    }
                    // The weave: a 1 px line at x = 0, 3, 6, ... at 10 % black, centred on the integer.
                    let line = (x / 3.0).round() * 3.0;
                    if line < PX as f64 && (x - line).abs() < 0.5 {
                        c = c.map(|v| v * 0.9);
                    }
                    for k in 0..3 {
                        sum[k] += c[k];
                    }
                }
            }
            out.push(sum.map(|v| v / (N * N) as f64));
        }
    }
    out
}

#[test]
fn the_stripe_texture_is_the_prototypes_canvas_within_four_levels() {
    let built = built();
    let (_, texture) = built.textures.iter().find(|(n, _)| n.as_str() == "tie-stripes").expect("the tie's texture");
    assert_eq!((texture.desc.size.x as usize, texture.desc.size.y as usize), (PX, PX));
    let hex = |h: u32| [(h >> 16) & 255, (h >> 8) & 255, h & 255].map(f64::from);
    let reference = reference(hex(0x0035_6a52), hex(0x00c9_ccd3), hex(0x0027_493a), hex(0x00a8_b3ad));
    let mut worst = 0;
    let mut bad = 0;
    let mut compared = 0;
    for (k, expected) in reference.iter().enumerate() {
        let (px, py) = ((k % PX) as i64, (k / PX) as i64);
        // Not under the weave's bands, and not where a pixel's centre sits exactly on a stripe's edge.
        let edge = [2, 6, 21, 36, 40].contains(&(px - py).rem_euclid(64));
        if px % 3 != 1 || edge {
            continue;
        }
        compared += 1;
        for c in 0..3 {
            let got = i32::from(texture.rgba8[k * 4 + c]);
            let difference = (got - expected[c].round() as i32).abs();
            worst = worst.max(difference);
            bad += usize::from(difference > PER_CHANNEL);
        }
    }
    println!(
        "tie texture: {compared} pixels, worst channel difference {worst} levels, {bad} channel values over {PER_CHANNEL}"
    );
    assert!(compared > PX * PX / 4, "most of the canvas is compared");
    assert!(worst <= PER_CHANNEL, "{bad} channel values differ by more than {PER_CHANNEL} levels (worst {worst})");
}
