//! Parity of the `vest` garment (`omuzaki_char::outfit::vest`, #323) with the prototype's `buildVest`.
//!
//! The oracle is the sample character's geometry dump (`geometry/game`, meshes `vest` and `buttons`), private test
//! material kept in the local assets folder: without it (always, in CI and in the cloud) every test here skips with a
//! message. The port is built over the soft-sd body, whose torso is the prototype's `TORSO` (`char_body_loft_parity`),
//! so the shirt-surface the prototype's `tp` works on is the body fit and the vest's `ease` is its inner offset.
//!
//! - **The vest mesh** is compared vertex for vertex up to the seams' caps (which the prototype leaves open): the
//!   outer shell, the lining, the two piping tubes and the five seams in the prototype's order, their positions,
//!   normals and vertex colours, and the triangles' indices.
//! - **The buttons** are compared as surfaces (nearest-point distance both ways): the library's button generator
//!   tessellates the same primitives in its own order.
//! - **Skin weights** are compared per vertex against the prototype's `wTorso` ramps; the port's come from the fit's
//!   joints, so they are reported, with a loose bound.
//! - **Materials**: the toon settings, the vertex colours flag, the outline and the gold's highlight colour.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers load local files; a bad one should stop the test with its reason
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic on small counts
#![allow(clippy::many_single_char_names, clippy::chunks_exact_to_as_chunks)] // geometry reads best as a, b, c

#[path = "support/outfit_conformance.rs"]
mod conformance;

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char::outfit::common::LAYER_VEST;
use omuzaki_char::outfit::vest::VestParams;
use omuzaki_char_api::{BodyShape, BuildServices, JointRef, PartMesh, PartOutput};
use omuzaki_core::glam::DVec3;
use omuzaki_core::mesh::MeshData;
use omuzaki_render_api::{ParamValue, Shading};
use omuzaki_test::jsref::{GeometryDump, JsMesh, JsRef};

/// The seams start here in the vest mesh: two 35 x 85 shells and two piping tubes of 85 rings of 7.
const SEAMS_FROM: usize = 2 * 35 * 85 + 2 * 85 * 7;
/// The prototype's vertex count: the port adds the end caps after it.
const PROTOTYPE_VERTICES: usize = 7608;
/// How far a vertex may be from the prototype's, metres: the port's fit is the prototype's curves to rounding.
const POSITION: f64 = 2e-4;
/// How far a button vertex may be from the other's surface, metres: the two tessellate the same shapes.
const BUTTON: f64 = 3e-4;

fn dump() -> Option<(GeometryDump, PartOutput)> {
    let js = JsRef::open()?;
    let dump = js.geometry("game").expect("the geometry dump loads");
    let params = serde_json::to_value(VestParams::default()).unwrap();
    let def = conformance::character("soft-sd", &BodyShape::default(), &[("garment", "vest", LAYER_VEST, params)]);
    let mut stack = conformance::build(def, &BuildServices::default()).expect("the vest builds");
    Some((dump, stack.outputs.pop().unwrap().1))
}

fn mesh<'a>(output: &'a PartOutput, name: &str) -> &'a PartMesh {
    output.meshes.iter().find(|m| m.name == name).unwrap_or_else(|| panic!("no mesh `{name}`"))
}

fn js<'a>(dump: &'a GeometryDump, name: &str) -> &'a JsMesh {
    dump.mesh(name).unwrap_or_else(|| panic!("the dump has no `{name}`"))
}

/// The worst and the mean distance between the two meshes' vertices `range` of each.
fn vertex_error(a: &MeshData, b: &MeshData, range: std::ops::Range<usize>) -> (f64, f64) {
    let (mut worst, mut sum) = (0.0_f64, 0.0_f64);
    for k in range.clone() {
        let d = f64::from(a.positions[k].distance(b.positions[k]));
        worst = worst.max(d);
        sum += d;
    }
    (worst, sum / range.len() as f64)
}

/// The closest point of triangle `abc` to `p` (Ericson, Real-Time Collision Detection 5.1.5).
fn closest_on_triangle(p: DVec3, a: DVec3, b: DVec3, c: DVec3) -> DVec3 {
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return a + ab * (d1 / (d1 - d3));
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return a + ac * (d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6)));
    }
    let denom = 1.0 / (va + vb + vc);
    a + ab * (vb * denom) + ac * (vc * denom)
}

/// The farthest any vertex of `from` lies from the surface of `to`.
fn hausdorff(from: &MeshData, to: &MeshData) -> f64 {
    let p: Vec<DVec3> = to.positions.iter().map(|v| v.as_dvec3()).collect();
    let triangles: Vec<[DVec3; 3]> =
        to.indices.chunks_exact(3).map(|t| [p[t[0] as usize], p[t[1] as usize], p[t[2] as usize]]).collect();
    from.positions
        .iter()
        .map(|q| {
            let q = q.as_dvec3();
            triangles
                .iter()
                .map(|[a, b, c]| q.distance(closest_on_triangle(q, *a, *b, *c)))
                .fold(f64::INFINITY, f64::min)
        })
        .fold(0.0, f64::max)
}

#[test]
fn the_vest_mesh_is_buildvest_vertex_for_vertex() {
    let Some((dump, output)) = dump() else { return };
    let (theirs, ours) = (&js(&dump, "vest").mesh, &mesh(&output, "garment-vest").geometry);
    assert_eq!(theirs.positions.len(), PROTOTYPE_VERTICES);
    // The port adds ten caps of five vertices after the prototype's vertices.
    assert_eq!(ours.positions.len(), PROTOTYPE_VERTICES + 50);
    let parts = [
        ("outer shell", 0..35 * 85),
        ("lining", 35 * 85..2 * 35 * 85),
        ("rim piping", 2 * 35 * 85..2 * 35 * 85 + 85 * 7),
        ("hem piping", 2 * 35 * 85 + 85 * 7..SEAMS_FROM),
        ("seams", SEAMS_FROM..PROTOTYPE_VERTICES),
    ];
    for (name, range) in parts {
        let (worst, mean) = vertex_error(ours, theirs, range);
        eprintln!("vest {name}: worst {worst:.3e} m, mean {mean:.3e} m");
        assert!(worst < POSITION, "T-GEOM, {name}: {worst:e} m");
    }
    // The triangles of the prototype's vertices are the same, index for index (the caps' come after).
    let prototype_indices = theirs.indices.len();
    assert_eq!(&ours.indices[..prototype_indices], &theirs.indices[..], "the same triangles over the same grid");
    assert_eq!(ours.indices.len(), prototype_indices + 10 * 3 * 3, "ten five-sided caps of three triangles");
    // Normals and vertex colours, over the prototype's vertices.
    let angle = |k: usize| f64::from(ours.normals[k].dot(theirs.normals[k]).clamp(-1.0, 1.0).acos()).to_degrees();
    let worst_angle = (0..PROTOTYPE_VERTICES).map(angle).fold(0.0, f64::max);
    let worst_colour = (0..PROTOTYPE_VERTICES)
        .flat_map(|k| (0..3).map(move |c| (k, c)))
        .map(|(k, c)| f64::from((ours.colours[k][c] - theirs.colours[k][c]).abs()))
        .fold(0.0, f64::max);
    eprintln!("vest normals: worst {worst_angle:.3} degrees; vertex colours: worst {worst_colour:.3e}");
    assert!(worst_angle < 2.0, "T-GEOM: {worst_angle} degrees");
    assert!(worst_colour < 1e-3, "vertex colours match: {worst_colour:e}");
    // The triangle count against the issue's budget: the prototype's 14160 and the caps.
    eprintln!("vest triangles: port {}, prototype {}", ours.indices.len() / 3, theirs.indices.len() / 3);
}

#[test]
fn the_buttons_cover_the_prototypes() {
    let Some((dump, output)) = dump() else { return };
    let (theirs, ours) = (&js(&dump, "buttons").mesh, &mesh(&output, "garment-buttons").geometry);
    let (forward, backward) = (hausdorff(theirs, ours), hausdorff(ours, theirs));
    eprintln!(
        "buttons: {} vertices and {} triangles (prototype {} and {}); Hausdorff {forward:.3e} m from the prototype, \
         {backward:.3e} m back",
        ours.positions.len(),
        ours.indices.len() / 3,
        theirs.positions.len(),
        theirs.indices.len() / 3
    );
    assert!(forward < BUTTON && backward < BUTTON, "T-GEOM: {forward:e} m and {backward:e} m");
}

/// The prototype's bone name for a joint of the port's torso chain.
fn prototype_bone(joint: &JointRef) -> &'static str {
    match joint {
        JointRef::Humanoid(HumanoidBone::Hips) => "hips",
        JointRef::Humanoid(HumanoidBone::Spine) => "spine",
        JointRef::Humanoid(HumanoidBone::Chest | HumanoidBone::UpperChest) => "chest",
        JointRef::Humanoid(HumanoidBone::Neck) => "neck",
        other => panic!("{other:?} isn't a torso bone"),
    }
}

#[test]
fn the_skin_follows_the_torso_like_wtorso() {
    let Some((dump, output)) = dump() else { return };
    let (theirs, ours) = (&js(&dump, "vest").mesh, mesh(&output, "garment-vest"));
    let weight_of = |names: &dyn Fn(usize) -> &'static str, m: &MeshData, k: usize, want: &str| -> f32 {
        (0..4).filter(|&c| names(usize::from(m.joints[k][c])) == want).map(|c| m.weights[k][c]).sum()
    };
    let their_name = |i: usize| -> &'static str {
        match dump.bones[i].as_str() {
            "hips" => "hips",
            "spine" => "spine",
            "chest" => "chest",
            "neck" => "neck",
            other => panic!("the prototype's vest uses bone {other}"),
        }
    };
    let our_name = |i: usize| prototype_bone(&ours.joint_bones[i]);
    let (mut worst, mut sum) = (0.0_f32, 0.0_f32);
    for k in 0..PROTOTYPE_VERTICES {
        for want in ["hips", "spine", "chest", "neck"] {
            let d = (weight_of(&their_name, theirs, k, want) - weight_of(&our_name, &ours.geometry, k, want)).abs();
            worst = worst.max(d);
            sum += d;
        }
    }
    let mean = sum / (4 * PROTOTYPE_VERTICES) as f32;
    eprintln!("vest skin weights: worst {worst:.3} per bone, mean {mean:.4}");
    assert!(mean < 0.05, "the weights hand over where the prototype's do: mean {mean}");
}

#[test]
fn the_materials_are_the_prototypes() {
    let Some((dump, output)) = dump() else { return };
    let vest = js(&dump, "vest");
    let toon = vest.material.toon.as_ref().unwrap();
    let ours = |name: &str| &output.materials.iter().find(|(m, _)| m.as_str() == name).unwrap().1;
    let Shading::Toon(cloth) = &ours("garment-shell").shading else { panic!("the cloth is toon") };
    let hex = |c: omuzaki_core::colour::Colour| format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b);
    assert_eq!(hex(cloth.shade), toon.shade);
    assert_eq!(hex(cloth.rim.colour), toon.rim);
    assert!((f64::from(cloth.shift) - toon.shift).abs() < 1e-6 && (f64::from(cloth.softness) - toon.soft).abs() < 1e-6);
    assert!((f64::from(cloth.rim.strength) - toon.rim_strength).abs() < 1e-6);
    assert!((f64::from(cloth.rim.power) - toon.rim_power).abs() < 1e-6);
    assert!(ours("garment-shell").vertex_colours && vest.material.vertex_colors);
    assert_eq!(hex(ours("garment-shell").base), vest.material.color);
    let line = ours("garment-shell").outline.unwrap();
    let theirs = vest.outline.as_ref().unwrap();
    assert_eq!((hex(line.colour), f64::from(line.width)), (theirs.color.clone(), theirs.width));
    // The gold: the band-highlight program with the prototype's highlight colour, the base colour and the outline.
    let gold = js(&dump, "buttons");
    let ours_gold = ours("garment-gold");
    assert_eq!(hex(ours_gold.base), gold.material.color);
    let Shading::Program { name, params } = &ours_gold.shading else { panic!("gold is a program") };
    assert_eq!(name.as_str(), "band-highlight");
    let Some(ParamValue::Colour(highlight)) = params.iter().find(|(n, _)| n.as_str() == "colour").map(|(_, v)| *v)
    else {
        panic!("the highlight colour")
    };
    assert_eq!(hex(highlight), gold.material.other_uniforms["uHlG"].as_str().unwrap().to_lowercase());
    let line = ours_gold.outline.unwrap();
    let theirs = gold.outline.as_ref().unwrap();
    assert_eq!((hex(line.colour), f64::from(line.width)), (theirs.color.clone(), theirs.width));
}
