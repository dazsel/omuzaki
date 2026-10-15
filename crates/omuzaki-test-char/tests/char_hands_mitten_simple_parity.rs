//! Parity of the mitten hand (`omuzaki_char::body::hands`) with the prototype's `buildHand` (`body.js` 52-159), against
//! the `hands` dump of the JS reference (skipped without it, as in CI and the cloud: the local lane runs it and posts the
//! numbers on #157).
//!
//! The left mitten is compared with T-GEOM (per vertex where the order matches, else Chamfer and Hausdorff), and its
//! vertex and triangle counts must be the dump's. Skin weights are not compared: the prototype puts the whole hand on
//! the hand bone, where the port weights the finger block to the proximal finger bones so that every hand style shares
//! one skeleton (#157). Both hands of the dump are checked as mirror images.

#![allow(clippy::unwrap_used, clippy::expect_used)] // a missing dump mesh should stop the test with its name

use omuzaki_char::body::hands;
use omuzaki_char::template::registry;
use omuzaki_char_api::{BodyParams, MaterialName};
use omuzaki_core::glam::Vec3;
use omuzaki_test::jsref::JsRef;
use omuzaki_test::meshcmp::{MeshTolerance, compare_meshes};

#[test]
fn the_left_mitten_meets_t_geom_against_the_hands_dump() {
    let Some(js) = JsRef::open() else { return };
    let dump = js.geometry("game").unwrap();
    let params = BodyParams::default();
    let layout =
        registry::find("soft-sd").unwrap().try_layout(&params.proportions, &params.shape, &params.hand).unwrap();
    let parts = hands::build(&params, &layout, &MaterialName::new("body-skin").unwrap()).unwrap();
    let mesh = |name: &str| parts.iter().find(|m| m.name == name).unwrap_or_else(|| panic!("no port mesh {name}"));
    let names: Vec<&str> = dump.meshes.iter().map(|m| m.name.as_str()).collect();
    for (port, js_name) in [("hand-l", "hand.L"), ("hand-r", "hand.R")] {
        let expected = &dump.mesh(js_name).unwrap_or_else(|| panic!("the dump has no `{js_name}`: {names:?}")).mesh;
        let actual = &mesh(port).geometry;
        let report = compare_meshes(actual, expected, MeshTolerance::T_GEOM);
        eprintln!("{port} vs {js_name}:\n{report}");
        assert_eq!(actual.positions.len(), expected.positions.len(), "{port}: vertex count");
        assert_eq!(actual.triangle_count(), expected.triangle_count(), "{port}: triangle count");
        assert!(report.passed(), "{port}: {:?}", report.findings);
    }
    // The dump's right hand is the left mirrored, as the port builds it.
    let (left, right) = (&mesh("hand-l").geometry, &mesh("hand-r").geometry);
    assert!(left.positions.iter().zip(&right.positions).all(|(a, b)| Vec3::new(-a.x, a.y, a.z) == *b));
}
