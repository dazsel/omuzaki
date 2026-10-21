//! Parity of the mitten hand (`omuzaki_char::body::hands`) with the prototype's `buildHand` (`body.js` 52-159), against
//! the `hands` and `hand-lines` dumps of the JS reference (skipped without them, as in CI and the cloud: the local lane
//! runs it and posts the numbers on #157).
//!
//! The dump holds both hands in one `hands` mesh (block and thumb) and their finger hints in one `hand-lines` mesh. Their
//! two sides are split by the sign of `x` (the right hand is the left mirrored, so no vertex lies across), each side's
//! triangles kept in the dump's order, and compared by T-GEOM (per vertex where the order matches, else Chamfer and
//! Hausdorff) with the port's matching meshes, whose vertex and triangle counts must be the dump's. The port's hand mesh
//! ends with two short pieces the prototype has no need of (its cuff hides the wrist and its thumb root): the wrist
//! sleeve and the thumb's stem ([`KIT_ADDED`] vertices). They come after the prototype's block and thumb, so they are
//! left out and everything before them is compared. Skin weights are not compared: the prototype puts the whole hand
//! on the hand bone, where the port weights the finger block to the proximal finger bones so that every hand style
//! shares one skeleton (#157). The port's two hands are checked as mirror images.

#![allow(clippy::unwrap_used, clippy::expect_used)] // a missing dump mesh should stop the test with its name
#![allow(clippy::chunks_exact_to_as_chunks)] // triangles as slices of 3

use omuzaki_char::body::hands;
use omuzaki_char::template::registry;
use omuzaki_char_api::{BodyParams, MaterialName};
use omuzaki_core::glam::Vec3;
use omuzaki_core::mesh::MeshData;
use omuzaki_test::jsref::JsRef;
use omuzaki_test::meshcmp::{MeshTolerance, compare_meshes};

/// The vertices of `mesh` on one side of `x = 0` (`left`: `x >= 0`) and the triangles wholly on it, in the mesh's order.
fn side_of(mesh: &MeshData, left: bool) -> MeshData {
    let on = |p: &Vec3| (p.x >= 0.0) == left;
    let mut index = vec![None; mesh.positions.len()];
    let mut out = MeshData::default();
    for (v, p) in mesh.positions.iter().enumerate().filter(|(_, p)| on(p)) {
        index[v] = Some(u32::try_from(out.positions.len()).unwrap());
        out.positions.push(*p);
        out.normals.push(mesh.normals[v]);
    }
    for t in mesh.indices.chunks_exact(3) {
        if let [Some(a), Some(b), Some(c)] = [t[0], t[1], t[2]].map(|v| index[v as usize]) {
            out.indices.extend([a, b, c]);
        }
    }
    out
}

/// The vertices the port adds after the prototype's block and thumb in a mitten's hand mesh: the wrist sleeve (two
/// rings of the block's 28 segments, plus the seam) and the thumb's stem (two rings of its 18, plus the seam).
const KIT_ADDED: usize = 2 * 29 + 2 * 19;

/// The first `keep` vertices of `mesh` and the triangles among them, in the mesh's order.
fn first(mesh: &MeshData, keep: usize) -> MeshData {
    let limit = u32::try_from(keep).unwrap();
    let mut out = MeshData {
        positions: mesh.positions[..keep].to_vec(),
        normals: mesh.normals[..keep].to_vec(),
        ..MeshData::default()
    };
    for t in mesh.indices.chunks_exact(3).filter(|t| t.iter().all(|&v| v < limit)) {
        out.indices.extend(t);
    }
    out
}

#[test]
fn the_left_mitten_meets_t_geom_against_the_hands_dump() {
    let Some(js) = JsRef::open() else { return };
    let dump = js.geometry("game").unwrap();
    let params = BodyParams::default();
    let layout =
        registry::find("soft-sd").unwrap().try_layout(&params.proportions, &params.shape, &params.hand).unwrap();
    let parts = hands::build(&params, &layout, &MaterialName::new("body-hands").unwrap()).unwrap();
    let names: Vec<&str> = dump.meshes.iter().map(|m| m.name.as_str()).collect();
    let dumped = |name: &str| &dump.mesh(name).unwrap_or_else(|| panic!("the dump has no `{name}`: {names:?}")).mesh;
    let port =
        |name: &str| &parts.iter().find(|m| m.name == name).unwrap_or_else(|| panic!("no port mesh {name}")).geometry;
    for (left, side) in [(true, "l"), (false, "r")] {
        let hand = port(&format!("hand-{side}"));
        for (dump_name, actual) in [
            ("hands", first(hand, hand.positions.len() - KIT_ADDED)),
            ("hand-lines", port(&format!("hand-lines-{side}")).clone()),
        ] {
            let expected = side_of(dumped(dump_name), left);
            let report = compare_meshes(&actual, &expected, MeshTolerance::T_GEOM);
            eprintln!("hand-{side} against the dump's {dump_name} (x {} 0):\n{report}", if left { ">=" } else { "<" });
            assert_eq!(actual.positions.len(), expected.positions.len(), "{dump_name} {side}: vertex count");
            assert_eq!(actual.triangle_count(), expected.triangle_count(), "{dump_name} {side}: triangle count");
            assert!(report.passed(), "{dump_name} {side}: {:?}", report.findings);
        }
    }
    // The port's right hand is the left mirrored, as the dump's is.
    let (left, right) = (port("hand-l"), port("hand-r"));
    assert!(left.positions.iter().zip(&right.positions).all(|(a, b)| Vec3::new(-a.x, a.y, a.z) == *b));
}
