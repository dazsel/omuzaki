//! Parity of the `boots` details (#327) with the prototype's `boots.js` `buildStrap`, `buildBuckle`, `buildStitching`,
//! `buildSeams` and `buildPullTab`, built from the sample character's explicit profile (her private `boots-profile.json`).
//!
//! The oracle is the sample character's geometry dump (`goldens/js/geometry/game`: the `boots`, `boot-brass`,
//! `boot-stitches`, `boot-holes` and `boot-seams` meshes), private test material kept in the local assets folder:
//! without it, or without her profile file, every test skips with a message.
//!
//! Skin weights of the flat dashes and discs are looser than the rest: the prototype weights each corner of a dash where
//! it stands, and the kit's polygon takes one blend for the whole dash, at its middle.

#![allow(clippy::unwrap_used, clippy::expect_used)] // a test: a bad oracle should stop it with its reason

#[path = "support/boots_parity.rs"]
mod parity;

use parity::{CUFF, Limits, STRAPS, TAB, TAILS, UPPER};

/// A flat dash's or disc's corner weights may differ more: a dash is 2.8 mm long, and the weights ramp over 5 cm (the measured worst is 0.08).
const POLYGONS: Limits = Limits { weight: 0.1, ..Limits::EXACT };
/// The hinge ball: the same vertices; its normals are the grid's smooth ones, not the prototype's analytic radial ones.
const BALL: Limits = Limits {
    tolerance: omuzaki_test::meshcmp::MeshTolerance { max_normal_angle_deg: 30.0, ..parity::EXACT },
    exact_triangles: false,
    ..Limits::EXACT
};

fn run(label: &str, ours: &str, theirs: &str, range: (usize, usize), limits: Limits) {
    let Some(oracle) = parity::open() else { return };
    let out = parity::build(&oracle);
    let js = oracle.dump.mesh(theirs).expect("the dump has the mesh");
    let mesh = parity::mesh(&out, ours);
    let range = if range.1 == usize::MAX { (range.0, js.mesh.positions.len() / 2) } else { range };
    parity::check(label, mesh, js, &oracle.dump, range, limits);
}

#[test]
fn the_strap_bands_match_the_prototypes() {
    run("straps", "leather", "boots", (UPPER + CUFF, UPPER + CUFF + STRAPS), Limits::EXACT);
}

#[test]
fn the_pull_tab_matches_the_prototypes() {
    let start = UPPER + CUFF + STRAPS;
    run("pull tab", "leather", "boots", (start, start + TAB), Limits::EXACT);
}

#[test]
fn the_strap_tails_match_the_prototypes() {
    let start = UPPER + CUFF + STRAPS + TAB;
    run("strap tails", "leather", "boots", (start, start + TAILS), Limits::EXACT);
}

/// The brass of one strap: the frame (25 x 8), the prong (10 x 7) and the hinge ball (9 x 11).
const FRAME: usize = 25 * 8;
const PRONG: usize = 10 * 7;
const BALL_VERTICES: usize = 9 * 11;

#[test]
fn the_brass_frames_match_the_prototypes() {
    for strap in 0..2 {
        let at = strap * (FRAME + PRONG + BALL_VERTICES);
        run(&format!("frame {strap}"), "brass", "boot-brass", (at, at + FRAME), Limits::EXACT);
    }
}

#[test]
fn the_brass_prongs_match_the_prototypes() {
    for strap in 0..2 {
        let at = strap * (FRAME + PRONG + BALL_VERTICES) + FRAME;
        run(&format!("prong {strap}"), "brass", "boot-brass", (at, at + PRONG), Limits::EXACT);
    }
}

#[test]
fn the_brass_hinge_balls_match_the_prototypes() {
    for strap in 0..2 {
        let at = strap * (FRAME + PRONG + BALL_VERTICES) + FRAME + PRONG;
        run(&format!("hinge {strap}"), "brass", "boot-brass", (at, at + BALL_VERTICES), BALL);
    }
}

#[test]
fn the_stitching_matches_the_prototypes() {
    run("stitches", "stitches", "boot-stitches", (0, usize::MAX), POLYGONS);
}

#[test]
fn the_holes_match_the_prototypes() {
    run("holes", "holes", "boot-holes", (0, usize::MAX), POLYGONS);
}

#[test]
fn the_seams_match_the_prototypes() {
    run("seams", "seams", "boot-seams", (0, usize::MAX), Limits::EXACT);
}

#[test]
fn the_materials_carry_the_prototypes_colours_and_outlines() {
    let Some(oracle) = parity::open() else { return };
    let out = parity::build(&oracle);
    for (ours, theirs) in [("leather", "boots"), ("sole", "boot-soles"), ("brass", "boot-brass")] {
        let js = oracle.dump.mesh(theirs).unwrap();
        let mesh = parity::mesh(&out, ours);
        let spec = &out.materials.iter().find(|(m, _)| *m == mesh.material).expect("the mesh's material").1;
        let outline = spec.outline.expect("an outline");
        let reference = js.outline.as_ref().expect("the prototype's outline");
        assert!((f64::from(outline.width) - reference.width).abs() < 1e-7, "{ours}: outline width");
        assert_eq!(spec.vertex_colours, js.material.vertex_colors, "{ours}: vertex colours");
    }
    for (ours, theirs) in [("stitches", "boot-stitches"), ("holes", "boot-holes"), ("seams", "boot-seams")] {
        let js = oracle.dump.mesh(theirs).unwrap();
        assert!(!parity::mesh(&out, ours).casts_shadow && !js.cast_shadow, "{ours}: casts no shadow");
    }
}
