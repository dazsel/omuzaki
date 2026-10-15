//! Parity of the hair `cap` (scalp shell) and `part_sweep` generators (`omuzaki_char::hair::gen`) with the prototype's
//! `hair.js` sections 1 and 2.
//!
//! The oracle is the sample character's dump, which is private test material kept in the local assets folder: without it
//! (always, in CI and in the cloud) every test here skips with a message. The head surface and the style's numbers are
//! the character's too, so a local file next to the dumps supplies them, `geometry/char_hair/cap_part_sweep.local.json`:
//!
//! ```json
//! { "variant": "game", "mesh": "<the hair-static mesh>", "cap_vertices": 4753,
//!   "head": { "centre": [x, y, z], "semi_axes": [a, b, c] },
//!   "cap": { ... CapGen ... }, "sweep": { ... PartSweepGen ... }, "first_sweep_clump": 0 }
//! ```
//!
//! The head is an ellipsoid fitted to the character's head by the dump script. Neither generator uses randomness, so the
//! vertex order is expected to match: the cap shell is held to `T-GEOM` (2e-4 m, 1 degree) and every part-sweep clump's
//! rest points to 1e-4 m. The numbers measured are printed so the first run with the dumps can propose the final
//! tolerances (the noise floor).

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers load local files; a bad one should stop the test with its reason

use std::fs;

use omuzaki_char::hair::r#gen::cap;
use omuzaki_char::hair::r#gen::part_sweep::{self, Lock};
use omuzaki_char_api::{CapGen, HAIR_INFO, HAIR_UV, HeadSurface, PartSweepGen};
use omuzaki_core::glam::Vec3;
use omuzaki_core::mesh::MeshData;
use omuzaki_test::jsref::JsRef;
use omuzaki_test::meshcmp::{MeshTolerance, compare_meshes};
use serde::Deserialize;

/// Rest-point tolerance, metres.
const REST: f32 = 1e-4;

#[derive(Deserialize)]
struct Local {
    variant: String,
    mesh: String,
    cap_vertices: usize,
    head: Ellipsoid,
    cap: CapGen,
    sweep: PartSweepGen,
    first_sweep_clump: usize,
}

#[derive(Debug, Deserialize)]
struct Ellipsoid {
    centre: Vec3,
    semi_axes: Vec3,
}

impl HeadSurface for Ellipsoid {
    fn centre(&self) -> Vec3 {
        self.centre
    }
    fn radius(&self, dir: Vec3) -> f32 {
        1.0 / (dir / self.semi_axes).length()
    }
}

/// The dump and the character's local numbers, or `None` (after saying why) so the caller skips.
fn private() -> Option<(JsRef, Local)> {
    let js = JsRef::open()?;
    let path = js.root().join("geometry/char_hair/cap_part_sweep.local.json");
    let Ok(text) = fs::read_to_string(&path) else {
        eprintln!("skipped: no local cap/part-sweep numbers at {}", path.display());
        return None;
    };
    Some((js, serde_json::from_str(&text).expect("the local cap/part-sweep file parses")))
}

#[test]
fn the_cap_shell_matches_the_prototypes_in_order() {
    let Some((js, local)) = private() else { return };
    let dump = js.geometry(&local.variant).unwrap();
    let expected = &dump.mesh(&local.mesh).expect("the hair mesh").mesh;
    let mut actual = MeshData::default();
    cap::fill_shell(&local.cap, &local.head, &mut actual);
    assert_eq!(actual.positions.len(), local.cap_vertices);
    // The shell is the first `cap_vertices` of the hair mesh's static part; compare that prefix.
    let mut prefix = expected.clone();
    prefix.positions.truncate(local.cap_vertices);
    prefix.normals.truncate(local.cap_vertices);
    prefix.uvs.clear();
    prefix.colours.clear();
    prefix.joints.clear();
    prefix.weights.clear();
    prefix.extra.retain(|a| a.name.as_str() == HAIR_UV || a.name.as_str() == HAIR_INFO);
    for a in &mut prefix.extra {
        a.data.truncate(local.cap_vertices * usize::from(a.components));
    }
    prefix.indices.retain(|i| (*i as usize) < local.cap_vertices);
    actual.uvs.clear();
    let report = compare_meshes(&actual, &prefix, MeshTolerance::T_GEOM);
    eprintln!("cap shell vs {}: {report}", local.mesh);
    assert!(report.passed(), "{report}");
}

#[test]
fn every_part_sweep_clump_matches_the_prototypes_rest_points() {
    let Some((js, local)) = private() else { return };
    let dump = js.hair(&local.variant).unwrap();
    let mut locks: Vec<Lock> = Vec::new();
    part_sweep::rebuild(&local.sweep, &local.head, &mut locks);
    let mut worst = 0.0_f32;
    for (n, lock) in locks.iter().enumerate() {
        let clump = &dump.clumps[local.first_sweep_clump + n];
        assert_eq!(clump.rest.len(), lock.len(), "clump {n}: point counts");
        for (a, b) in lock.iter().zip(&clump.rest) {
            worst = worst.max(a.distance(*b));
        }
    }
    eprintln!("part sweep: {} clumps, worst rest-point error {worst:.2e} m", locks.len());
    assert!(worst <= REST, "worst rest-point error {worst:.2e} m exceeds {REST:.0e} m");
}
