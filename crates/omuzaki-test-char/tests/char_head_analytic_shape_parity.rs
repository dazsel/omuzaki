//! Parity of the head cranium, face-shape outline and chin stretch (`omuzaki_char::head`) with the prototype's `head.js`.
//!
//! Two oracles, because the sample character's dumps stay in a local assets folder and CI has none:
//!
//! - **Committed fixture** (`fixtures/char_head/head_shape.json`, made by the prototype's generator script running the prototype's own
//!   `head.js` under Node with three.js): the radius at 10 000 directions, and the head mesh on a 32 x 24 grid for seven
//!   synthetic face profiles (low, middle and high exponent; no puff and a puff; no chin, a chin, a deep chin and a chin
//!   above the head). Runs everywhere.
//! - **The JS dump** (`goldens/js/geometry/{game,close-up}-trace0`, skipped without the local assets): the full 128 x 96
//!   head with the traced map off. The two real face profiles are optional local test material:
//!   the prototype's generator script writes them to `geometry/char_head/profiles.local.json` beside the dumps, the tests read them from
//!   there, and nothing of them is committed.
//!
//! **The lower face is the redesign, not the prototype's.** The prototype's construction creased, flanged and
//! spiked away from its two tuned profiles, so T-GEOM parity holds above the reshaped lower face (the cranium, to f32
//! rounding) and is waived below it, as recorded. The close-up profile, the one tuned face, is held to its dump
//! as a surface: within 0.9 mm and 0.5 mm almost everywhere, except the eye-line band that was later smoothed (see its
//! test).
//!
//! Prototype constants map onto the contract as `head.js` documents them, in the head-local frame (world minus the head
//! centre): `W0` is `half_width`, `TOP` is `top`, `H` is `height`, `P` is `exponent`, `puff` is `cheek_puff`, and `chin`,
//! `stretchTop` and `minRatio` make the `Chin`.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers load fixtures; a bad one should stop the test with its reason
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // direction indices are small; f64 to f32 is the mesh's precision

use std::collections::BTreeMap;
use std::fs;

use omuzaki_char::head::mesh::{self, Resolution};
use omuzaki_char::head::outline::{MAX_BAND, plausible};
use omuzaki_char::head::surface;
use omuzaki_char_api::{Chin, Cranium, HeadOutline, HeadParams};
use omuzaki_core::glam::{DVec3, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_test::jsref::JsRef;
use omuzaki_test::meshcmp::MeshTolerance;
use serde::Deserialize;

/// The prototype's head centre (`HEAD.C`).
const CENTRE: [f64; 3] = [0.0, 0.778, -0.022];

#[derive(Deserialize)]
struct Constants {
    #[serde(rename = "W0")]
    half_width: f64,
    #[serde(rename = "TOP")]
    top: f64,
    #[serde(rename = "H")]
    height: f64,
    #[serde(rename = "P")]
    exponent: f64,
    puff: f64,
    chin: Option<f64>,
    #[serde(rename = "stretchTop")]
    stretch_top: Option<f64>,
    #[serde(rename = "minRatio")]
    min_ratio: Option<f64>,
}

#[derive(Deserialize)]
struct Grid {
    segments: u16,
    rings: u16,
}

#[derive(Deserialize)]
struct Profile {
    constants: Constants,
    positions: Vec<f32>,
    normals: Vec<f32>,
}

#[derive(Deserialize)]
struct Fixture {
    grid: Grid,
    radii_unit: f64,
    radii: Vec<i64>,
    indices: Vec<u32>,
    profiles: BTreeMap<String, Profile>,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!("fixtures/char_head/head_shape.json")).unwrap()
}

/// `head.js` lines 17 to 33: the cranium numbers `headRadius` hard-codes (the contract's defaults, but spelled out here
/// so a drifting default cannot hide), on the prototype's head centre.
fn prototype_cranium() -> Cranium {
    Cranium {
        centre: CENTRE,
        half_width: 0.186,
        half_depth_front: 0.184,
        half_depth_back: 0.181,
        half_height_up: 0.186,
        half_height_down_back: 0.150,
        half_height_down_front: 0.197,
        cheek_fullness: 0.010,
        face_flatten: 0.035,
    }
}

/// The contract's parameters for a prototype profile.
fn params_for(c: &Constants) -> HeadParams {
    let chin = c.chin.map(|y| Chin {
        y: y - CENTRE[1],
        stretch_top: c.stretch_top.unwrap() - CENTRE[1],
        min_ratio: c.min_ratio.unwrap(),
    });
    HeadParams {
        cranium: prototype_cranium(),
        outline: HeadOutline {
            half_width: c.half_width,
            top: c.top - CENTRE[1],
            height: c.height,
            exponent: c.exponent,
            exponent_lower: None,
            chin,
            cheek_puff: c.puff,
        },
        ..HeadParams::default()
    }
}

/// The unit direction `i` of `count` on a Fibonacci lattice (what the prototype's generator script samples).
fn lattice(i: usize, count: usize) -> DVec3 {
    let y = 1.0 - (2.0 * (i as f64 + 0.5)) / count as f64;
    let s = (1.0 - y * y).sqrt();
    let phi = i as f64 * (std::f64::consts::PI * (3.0 - 5.0_f64.sqrt()));
    DVec3::new(s * phi.sin(), y, s * phi.cos())
}

fn mesh_of(positions: &[f32], normals: &[f32], indices: &[u32]) -> MeshData {
    let vec3s = |flat: &[f32]| flat.as_chunks::<3>().0.iter().map(|c| Vec3::from_array(*c)).collect::<Vec<_>>();
    MeshData { positions: vec3s(positions), normals: vec3s(normals), indices: indices.to_vec(), ..MeshData::default() }
}

#[test]
fn radius_matches_headradius_at_10000_directions() {
    let f = fixture();
    assert_eq!(f.radii.len(), 10_000);
    let surface = surface(&HeadParams { cranium: prototype_cranium(), ..HeadParams::default() }, None);
    assert_eq!(surface.centre(), Vec3::new(0.0, 0.778, -0.022));
    let mut worst = 0.0_f64;
    for (i, &expected) in f.radii.iter().enumerate() {
        let dir = lattice(i, f.radii.len());
        let got = f64::from(surface.radius(dir.as_vec3()));
        worst = worst.max((got - expected as f64 * f.radii_unit).abs());
    }
    eprintln!("radius vs headRadius over {} directions: worst difference {worst:.3e} m", f.radii.len());
    assert!(worst < 1e-5, "worst difference {worst:e} m");
}

/// Head-local height above which `params` leaves the cranium as it is: the higher of the stretch line and the top of
/// the tallest blend band above the outline's top.
fn reshaped_below(params: &HeadParams) -> f64 {
    let outline = plausible(&params.outline, &params.cranium);
    outline.chin.map_or(f64::NEG_INFINITY, |c| c.stretch_top).max(outline.top + MAX_BAND)
}

/// Compares `actual` with `expected` vertex by vertex above `params`' reshaped lower face, against T-GEOM: positions
/// at every such vertex, normals one cell clear of it (a normal averages the faces round its vertex). Returns how many
/// vertices it compared and the largest position error.
fn assert_cranium_matches(name: &str, actual: &MeshData, expected: &MeshData, params: &HeadParams) -> (usize, f32) {
    assert_eq!(actual.indices, expected.indices, "{name}: the triangle list is the prototype's");
    let limit = (params.cranium.centre[1] + reshaped_below(params)) as f32;
    let tolerance = MeshTolerance::T_GEOM;
    let (mut compared, mut worst) = (0, 0.0_f32);
    for (i, (a, e)) in actual.positions.iter().zip(&expected.positions).enumerate() {
        if e.y <= limit + 1e-4 {
            continue;
        }
        let error = (*a - *e).length();
        assert!(error <= tolerance.max_vertex_error, "{name}: vertex {i} is {error:e} m from the prototype's");
        if e.y > limit + 0.03 {
            let angle = actual.normals[i].angle_between(expected.normals[i]).to_degrees();
            assert!(angle <= tolerance.max_normal_angle_deg, "{name}: vertex {i}'s normal is {angle} deg off");
        }
        compared += 1;
        worst = worst.max(error);
    }
    (compared, worst)
}

#[test]
fn coarse_mesh_matches_the_prototype_above_the_lower_face_for_every_profile() {
    let f = fixture();
    let resolution = Resolution { segments: f.grid.segments, rings: f.grid.rings };
    assert_eq!(
        f.profiles.keys().map(String::as_str).collect::<Vec<_>>(),
        ["chin-plain", "chin-puff", "deep-chin", "high-chin", "pointed", "round", "square-puff"]
    );
    for (name, profile) in &f.profiles {
        let params = params_for(&profile.constants);
        let actual = mesh::build(&params, resolution, None).unwrap();
        let expected = mesh_of(&profile.positions, &profile.normals, &f.indices);
        let (compared, worst) = assert_cranium_matches(name, &actual, &expected, &params);
        eprintln!("{name} 32 x 24: {compared} vertices above the lower face, worst {worst:.2e} m");
        assert!(compared > actual.positions.len() / 2, "{name}: only {compared} vertices compared");
    }
}

/// The dump sets: the game head (the default face profile) and the close-up head (the close-up profile), both optional local files.
const DUMP_SETS: [&str; 2] = ["game-trace0", "close-up-trace0"];

#[derive(Deserialize)]
struct PrivateSet {
    constants: Constants,
}

/// The local face profiles the prototype's generator script wrote beside the dumps, by dump set.
#[derive(Deserialize)]
struct PrivateProfiles {
    sets: BTreeMap<String, PrivateSet>,
}

/// The dump reader and the local profiles, or `None` (after saying why) so the caller skips: no local assets, or no
/// `geometry/char_head/profiles.local.json` yet (run the prototype's generator script with the dump root).
fn private_dump() -> Option<(JsRef, PrivateProfiles)> {
    let js = JsRef::open()?;
    let path = js.root().join("geometry/char_head/profiles.local.json");
    let Ok(text) = fs::read_to_string(&path) else {
        eprintln!(
            "skipped: no local face profiles at {} (run the prototype's generator script with the dump root)",
            path.display()
        );
        return None;
    };
    Some((js, serde_json::from_str(&text).unwrap()))
}

#[test]
fn full_mesh_meets_t_geom_against_the_dump_above_the_lower_face() {
    let Some((js, private)) = private_dump() else { return };
    for set in DUMP_SETS {
        let params = params_for(&private.sets[set].constants);
        let actual = mesh::build(&params, Resolution::LIVE, None).unwrap();
        let dump = js.geometry(set).unwrap();
        let expected = &dump.mesh("head").unwrap().mesh;
        assert_eq!((actual.positions.len(), actual.triangle_count()), (12_513, 24_576));
        assert_eq!(actual.positions.len(), expected.positions.len(), "{set}: vertex counts");
        let (compared, worst) = assert_cranium_matches(set, &actual, expected, &params);
        eprintln!("private profile vs {set}/head: {compared} vertices above the lower face, worst {worst:.2e} m");
        assert!(compared > 6_000, "{set}: only {compared} vertices compared");
    }
}

/// The distance from `p` to the triangle `abc` (Ericson, "Real-Time Collision Detection", 5.1.5).
fn point_triangle_distance(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> f32 {
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return p.distance(a);
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return p.distance(b);
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return p.distance(a + ab * (d1 / (d1 - d3)));
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return p.distance(c);
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return p.distance(a + ac * (d2 / (d2 - d6)));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && d4 - d3 >= 0.0 && d5 - d6 >= 0.0 {
        return p.distance(b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6))));
    }
    let denominator = 1.0 / (va + vb + vc);
    p.distance(a + ab * (vb * denominator) + ac * (vc * denominator))
}

/// For each vertex of `from`, its distance to the surface of `to`, a head on the same 128 x 96 grid: the nearest of
/// `to`'s triangles within two cells of the same grid position (the reshaping slides a vertex less than a cell).
fn surface_distances(from: &MeshData, to: &MeshData) -> Vec<f32> {
    let (segments, rings) = (128_usize, 96_usize);
    let columns = segments + 1;
    let at = |ring: usize, column: usize| to.positions[ring * columns + column % segments];
    (0..from.positions.len())
        .map(|i| {
            let (ring, column) = (i / columns, i % columns);
            let mut best = f32::INFINITY;
            for r in ring.saturating_sub(2)..(ring + 2).min(rings) {
                for c in (column + segments - 2)..(column + segments + 2) {
                    let (a, b, a1, b1) = (at(r, c), at(r + 1, c), at(r, c + 1), at(r + 1, c + 1));
                    best = best.min(point_triangle_distance(from.positions[i], a, b, a1)).min(point_triangle_distance(
                        from.positions[i],
                        b,
                        b1,
                        a1,
                    ));
                }
            }
            best
        })
        .collect()
}

/// The close-up profile (the one tuned face, private) built by the redesigned lower face against the prototype's dump,
/// as surfaces: the larger of each vertex's distance to the other mesh, both ways, everywhere but the eye-line band.
///
/// The redesign target is 0.5 mm. The redesign follows the outline; the prototype measured the cranium's width in 2 mm bins
/// of mesh vertices, which read up to 1% narrow near the eye line (vertices miss the silhouette between columns), so
/// its face sits up to 0.9 mm outside its own outline there, and its smoothing pinched the chin's last 1.5 mm. Matching
/// that would mean rebuilding the bins' error (and their jitter). The test holds what is met: 0.9 mm everywhere, 0.5 mm
/// for 99% of the vertices.
///
/// The band above the outline's top, up to `outline::MAX_BAND`, is excluded: an accepted, deliberate deviation from the
/// prototype (a maintainer decision), because the eye-line blend there fixes a prototype defect. The prototype
/// eased the face into the cranium over 3 cm, which left a ledge across the face at the eye line; the band is now as
/// tall as the slope mismatch needs. Its largest departure is printed. Below the top and above the band the head is
/// built as before.
#[test]
fn the_close_up_profile_stays_within_a_millimetre_of_the_prototype() {
    let Some((js, private)) = private_dump() else { return };
    let params = params_for(&private.sets["close-up-trace0"].constants);
    let actual = mesh::build(&params, Resolution::LIVE, None).unwrap();
    let dump = js.geometry("close-up-trace0").unwrap();
    let expected = &dump.mesh("head").unwrap().mesh;
    let top = plausible(&params.outline, &params.cranium).top + params.cranium.centre[1];
    let in_band = |p: Vec3| f64::from(p.y) > top && f64::from(p.y) <= top + MAX_BAND;
    let (band, distances): (Vec<_>, Vec<_>) = surface_distances(&actual, expected)
        .into_iter()
        .zip(surface_distances(expected, &actual))
        .zip(&expected.positions)
        .map(|((a, b), p)| (in_band(*p), a.max(b)))
        .partition(|(in_band, _)| *in_band);
    let largest = |d: &[(bool, f32)]| d.iter().map(|(_, d)| *d).fold(0.0, f32::max);
    let (worst, band_worst) = (largest(&distances), largest(&band));
    let over = distances.iter().filter(|(_, d)| *d > 5e-4).count();
    eprintln!(
        "close-up vs the prototype: worst {:.3} mm, {over} of {} vertices over 0.5 mm; the eye-line band's {} vertices \
         depart by up to {:.2} mm",
        worst * 1e3,
        distances.len(),
        band.len(),
        band_worst * 1e3
    );
    assert!(worst < 9e-4, "worst surface distance {worst:e} m");
    assert!(over * 100 < distances.len(), "{over} vertices over 0.5 mm");
}

#[test]
fn surface_matches_the_dump_where_the_outline_leaves_the_cranium_alone() {
    let Some((js, private)) = private_dump() else { return };
    for set in DUMP_SETS {
        let params = params_for(&private.sets[set].constants);
        let surface = surface(&params, None);
        let dump = js.geometry(set).unwrap();
        let head = &dump.mesh("head").unwrap().mesh;
        // The outline reshapes only the lower face: above it every vertex is the cranium itself.
        let limit = reshaped_below(&params) as f32;
        let centre = surface.centre();
        let (mut checked, mut worst) = (0_usize, 0.0_f32);
        for p in head.positions.iter().filter(|p| p.y - centre.y > limit + 1e-3) {
            let offset = *p - centre;
            worst = worst.max((surface.radius(offset.normalize()) - offset.length()).abs());
            checked += 1;
        }
        eprintln!("{set}: radius at {checked} dump vertices, worst difference {worst:.3e} m");
        assert!(checked > 5_000, "{checked} vertices above the shaping reach");
        assert!(worst < 1e-5, "{set}: worst difference {worst:e} m");
    }
}

#[test]
fn the_chin_lands_on_the_close_up_profiles_chin_in_the_dump() {
    let Some((js, private)) = private_dump() else { return };
    let params = params_for(&private.sets["close-up-trace0"].constants);
    let actual = mesh::build(&params, Resolution::LIVE, None).unwrap();
    let dump = js.geometry("close-up-trace0").unwrap();
    let lowest = |m: &MeshData| m.positions.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
    let chin_y = (CENTRE[1] + params.outline.chin_y()) as f32;
    eprintln!(
        "lowest vertex: rust {}, js {}, chin {chin_y}",
        lowest(&actual),
        lowest(&dump.mesh("head").unwrap().mesh)
    );
    assert!((lowest(&actual) - chin_y).abs() < 1e-4);
    assert!((lowest(&dump.mesh("head").unwrap().mesh) - chin_y).abs() < 1e-4);
}
