//! Parity of the mitten hand (`omuzaki_char::body::hands`) with the prototype's `buildHand` (`body.js` 52-159), against
//! the `hands` dump of the JS reference (skipped without it, as in CI and the cloud: the local lane runs it and posts the
//! numbers on #157).
//!
//! The dump holds both hands in one `hands` mesh. Its two sides are split by the sign of `x` (the right hand is the left
//! mirrored, so no vertex lies across), each side's triangles kept in the dump's order, and compared with the port's
//! matching hand (the block and thumb, then the finger-hint lines) by T-GEOM (per vertex where the order matches, else
//! Chamfer and Hausdorff), and the vertex and triangle counts must be the dump's. Skin weights are not compared: the
//! prototype puts the whole hand on the hand bone, where the port weights the finger block to the proximal finger
//! bones so that every hand style shares one skeleton (#157). The port's two hands are checked as mirror images.

#![allow(clippy::unwrap_used, clippy::expect_used)] // a missing dump mesh should stop the test with its name
#![allow(clippy::chunks_exact_to_as_chunks)] // triangles as slices of 3

use omuzaki_char::body::hands;
use omuzaki_char::template::registry;
use omuzaki_char_api::{BodyParams, MaterialName, PartMesh};
use omuzaki_core::glam::Vec3;
use omuzaki_core::mesh::MeshData;
use omuzaki_test::jsref::JsRef;
use omuzaki_test::meshcmp::{MeshTolerance, compare_meshes};

/// The vertices of `mesh` on one side of `x = 0` (`left`: `x >= 0`) and the triangles wholly on it, in the mesh's order.
fn side(mesh: &MeshData, left: bool) -> MeshData {
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

/// The port's meshes `names`, one after the other, as one mesh.
fn joined(parts: &[PartMesh], names: &[&str]) -> MeshData {
    let mut out = MeshData::default();
    for name in names {
        let m = &parts.iter().find(|m| m.name == *name).unwrap_or_else(|| panic!("no port mesh {name}")).geometry;
        let base = u32::try_from(out.positions.len()).unwrap();
        out.positions.extend(&m.positions);
        out.normals.extend(&m.normals);
        out.indices.extend(m.indices.iter().map(|i| i + base));
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
    let both = &dump.mesh("hands").unwrap_or_else(|| panic!("the dump has no `hands`: {names:?}")).mesh;
    for (left, port) in [(true, ["hand-l", "hand-lines-l"]), (false, ["hand-r", "hand-lines-r"])] {
        let expected = side(both, left);
        let actual = joined(&parts, &port);
        let report = compare_meshes(&actual, &expected, MeshTolerance::T_GEOM);
        eprintln!("{port:?} vs the dump's hands (x {} 0):\n{report}", if left { ">=" } else { "<" });
        assert_eq!(actual.positions.len(), expected.positions.len(), "{port:?}: vertex count");
        assert_eq!(actual.triangle_count(), expected.triangle_count(), "{port:?}: triangle count");
        assert!(report.passed(), "{port:?}: {:?}", report.findings);
    }
    // The port's right hand is the left mirrored, as the dump's is.
    let mesh = |name: &str| &parts.iter().find(|m| m.name == name).unwrap().geometry;
    let (left, right) = (mesh("hand-l"), mesh("hand-r"));
    assert!(left.positions.iter().zip(&right.positions).all(|(a, b)| Vec3::new(-a.x, a.y, a.z) == *b));
}
