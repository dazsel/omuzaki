//! Parity of the human ear (`omuzaki_char::head::ears::human`) with the prototype's `buildEar` (`head.js` 142 to 175).
//!
//! The oracle is the dump (`geometry/{game,close-up}-trace0/ear.{L,R}`), private test material that lives on the
//! assets drive: the tests skip without it. The dump places the ears at the prototype's hard-coded roots
//! `(+-0.184, 0.712, -0.034)`; the port roots on the head as built, so the comparison passes the prototype's root, which
//! is what makes the shell itself comparable. The test also reports how far the port's own root is from it.
//!
//! There is no JS elf ear: the elf is the issue's extension of the same shell, tested in
//! `crates/omuzaki-char/tests/head_ears.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)] // a missing mesh in the dump should stop the test with its reason

use omuzaki_char::head::ears::{self, Side};
use omuzaki_char::head::mesh::{Resolution, build_head};
use omuzaki_char_api::{Ears, HeadParams, HeadSurface};
use omuzaki_core::glam::Vec3;
use omuzaki_test::jsref::JsRef;
use omuzaki_test::meshcmp::{MeshTolerance, Verdict, compare_meshes};

/// The dump sets that hold the head part's meshes.
const SETS: [&str; 2] = ["game-trace0", "close-up-trace0"];

/// The prototype's left-ear root, `head.js` 172.
const PROTOTYPE_ROOT: Vec3 = Vec3::new(0.184, 0.712, -0.034);

/// A head too small to touch anything: the clearance rule has nothing to push the ear out of.
#[derive(Debug)]
struct NoHead;

impl HeadSurface for NoHead {
    fn centre(&self) -> Vec3 {
        Vec3::ZERO
    }

    fn radius(&self, _: Vec3) -> f32 {
        0.0
    }
}

fn root_of(side: Side) -> Vec3 {
    match side {
        Side::Left => PROTOTYPE_ROOT,
        Side::Right => Vec3::new(-PROTOTYPE_ROOT.x, PROTOTYPE_ROOT.y, PROTOTYPE_ROOT.z),
    }
}

#[test]
fn the_human_ear_matches_build_ear_vertex_for_vertex() {
    let Some(js) = JsRef::open() else { return };
    // The prototype has no clearance rule (its ear can sit partly inside a head), so the shell is built against a head
    // of no size: nothing is pushed and what is compared is `buildEar` itself.
    let head = NoHead;
    for set in SETS {
        let dump = js.geometry(set).unwrap();
        for (side, name) in [(Side::Left, "ear.L"), (Side::Right, "ear.R")] {
            let mut expected = dump.mesh(name).unwrap().mesh.clone();
            let actual = ears::build(&Ears::default(), side, root_of(side), &head).unwrap().unwrap();
            // Vertex 0 (the concha's centre, first column) sits only on zero-area triangles: the prototype's normal there
            // is float noise (and the sign of the noise is not even a direction the shell has), so it is not compared.
            // The port's is the centre's shared normal, which must be a unit vector.
            assert!((actual.normals[0].length() - 1.0).abs() < 1e-5, "{set}/{name}: no normal at the centre");
            expected.normals[0] = actual.normals[0];
            assert_eq!((actual.positions.len(), actual.triangle_count()), (369, 640));
            assert_eq!(actual.indices, expected.indices, "{set}/{name}: the triangle list is the prototype's");
            let report = compare_meshes(&actual, &expected, MeshTolerance::T_GEOM);
            eprintln!("{set}/{name}:\n{report}");
            assert_eq!(report.verdict, Verdict::InOrder, "{set}/{name}: {:?}", report.findings);
        }
    }
}

#[test]
fn the_port_roots_the_ear_on_the_built_head_a_known_distance_inside_the_prototypes_point() {
    // A measurement more than a limit. The prototype's point is where `head.js` hard-codes the ear; the port roots on
    // the head as built, whose reshaped lower face is up to 1 cm inside the cranium there, and the prototype's
    // point is itself up to 1 cm outside the cranium (the shell's concha dips 4 mm back). So the port's ear sits about
    // 2 cm closer to the head centre, in the same direction.
    let params = HeadParams::default();
    let head = build_head(&params, Resolution::LIVE, None).unwrap().surface;
    let root = ears::root(&params.ears, head.as_ref());
    // The default head centre is the prototype's, so the prototype's point is directly comparable.
    let offset = (root - PROTOTYPE_ROOT).length();
    eprintln!("port root {root:?} is {offset:.4} m from the prototype's point");
    assert!(
        (root - head.centre()).length() < (PROTOTYPE_ROOT - head.centre()).length(),
        "inside the prototype's point"
    );
    assert!(offset < 0.03, "the root moved {offset} m");
}
