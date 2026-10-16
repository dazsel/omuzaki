//! Parity of the `boots` garment's upper, sole, heel and cuff (#326) with the prototype's `boots.js` `buildUpper`,
//! `buildSole` and `buildCuff`, built from the sample character's explicit profile (her private `boots-profile.json`).
//!
//! The oracle is the sample character's geometry dump (`goldens/js/geometry/game`: the `boots` and `boot-soles`
//! meshes), private test material kept in the local assets folder: without it, or without her profile file, every test
//! skips with a message. The port runs the prototype's arithmetic in the prototype's order, so the vertices, normals,
//! triangles, vertex colours and skin weights of the left boot agree vertex for vertex (the right boot is checked as the
//! left's mirror image).

#[path = "support/boots_parity.rs"]
mod parity;

use parity::{CUFF, EXACT, Limits, UPPER};

#[test]
fn the_upper_matches_the_prototypes_vertex_for_vertex() {
    let Some(oracle) = parity::open() else { return };
    let out = parity::build(&oracle);
    let theirs = oracle.dump.mesh("boots").expect("the dump has the boots mesh");
    parity::check("upper", parity::mesh(&out, "leather"), theirs, &oracle.dump, (0, UPPER), Limits::EXACT);
}

#[test]
fn the_cuff_matches_the_prototypes_vertex_for_vertex() {
    let Some(oracle) = parity::open() else { return };
    let out = parity::build(&oracle);
    let theirs = oracle.dump.mesh("boots").expect("the dump has the boots mesh");
    parity::check("cuff", parity::mesh(&out, "leather"), theirs, &oracle.dump, (UPPER, UPPER + CUFF), Limits::EXACT);
}

#[test]
fn the_sole_and_the_heel_match_the_prototypes_vertex_for_vertex() {
    let Some(oracle) = parity::open() else { return };
    let out = parity::build(&oracle);
    let theirs = oracle.dump.mesh("boot-soles").expect("the dump has the sole mesh");
    let ours = parity::mesh(&out, "sole");
    let half = theirs.mesh.positions.len() / 2;
    parity::check("left sole and heel", ours, theirs, &oracle.dump, (0, half), Limits::EXACT);
    parity::check("right sole and heel", ours, theirs, &oracle.dump, (half, 2 * half), Limits::EXACT);
}

#[test]
fn the_whole_mirrored_boots_have_the_prototypes_counts_and_the_right_boot_mirrors_the_left() {
    let Some(oracle) = parity::open() else { return };
    let out = parity::build(&oracle);
    for (name, js) in [("leather", "boots"), ("sole", "boot-soles")] {
        let (ours, theirs) = (parity::mesh(&out, name), oracle.dump.mesh(js).unwrap());
        assert_eq!(ours.geometry.positions.len(), theirs.mesh.positions.len(), "{name}: vertices");
        assert_eq!(ours.geometry.indices.len(), theirs.mesh.indices.len(), "{name}: triangles");
        assert_eq!(ours.casts_shadow, theirs.cast_shadow, "{name}: shadow casting");
    }
    let leather = parity::mesh(&out, "leather");
    let theirs = oracle.dump.mesh("boots").unwrap();
    let report = omuzaki_test::meshcmp::compare_meshes(&leather.geometry, &theirs.mesh, EXACT);
    println!("both boots:\n{report}");
    assert!(report.passed(), "the mirrored boots differ from the prototype's:\n{report}");
}
