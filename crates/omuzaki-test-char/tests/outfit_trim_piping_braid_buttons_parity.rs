//! Parity of trim library I (`omuzaki_char::outfit::trim`, #188) with the prototype's trims, as #188's reference gives
//! them: the robe's rolled-edge strip (`addStrip`), the vest's piping cords (`ringTube`) and the gold buttons.
//!
//! The oracle is the sample character's dump, private test material kept in the local assets folder: without it
//! (always, in CI and in the cloud) every test here skips with a message. The prototype built these pieces on its own
//! robe and vest, which this crate doesn't have yet, so each test recovers the frame a piece was built in from the
//! dump itself and builds the port's piece in that frame:
//! - **buttons:** the `buttons` mesh in three clusters by height; each one's axis is its flattest direction and its
//!   foot the lowest point along it. The port's gold button there must cover the prototype's (every prototype vertex
//!   within [`BUTTON`] of the port's surface) and the buttons must be [`buttons::GOLD_RADIUS`]-sized, 0.026 m apart;
//! - **the strip:** a middle rung of the `robe-trim` mesh, its 11 vertices put in the rung's own frame (the surface
//!   normal at its free edge, the direction into the band); the port's strip, built on a large cylinder with the robe's
//!   numbers, must give the same cross-section near the edge (the lip, the rolled edge and the face's first two
//!   rungs) within [`STRIP`];
//! - **the vest's piping:** each closed tube of 85 rings of 7 in the `vest` mesh; the port's `ring_tube` through the
//!   same ring centres must cover it within [`CORD`].
//!
//! The braid has no prototype reference yet (#188 names `hair.js` 256-287, which the Director's reference doesn't
//! cover), so it has no parity test here. The numbers measured are printed so the first run with the dumps can propose
//! the final tolerances.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers load local files; a bad one should stop the test with its reason
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic on small counts
#![allow(clippy::many_single_char_names, clippy::chunks_exact_to_as_chunks)] // geometry reads best as a, b, c

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char::kit::grid::MeshBuilder;
use omuzaki_char::outfit::loft::Surface;
use omuzaki_char::outfit::trim::buttons::{self, ButtonShape, ButtonStyle};
use omuzaki_char::outfit::trim::piping::{self, Lip, Piping, ROBE};
use omuzaki_char::outfit::trim::{Finish, LIFT, Rail, RailPoint, RailSpec};
use omuzaki_char_api::{BodyFit, BodyRegion, JointRef, Section};
use omuzaki_core::glam::{DVec3, Vec2, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_test::jsref::JsRef;

/// How far a prototype button vertex may lie from the port's button, metres: the two tessellate the same primitives,
/// so this is their chord error and rounding.
const BUTTON: f64 = 1.5e-4;
/// How far the strip's cross-section may differ near its edge, metres: the robe's cloth curves, the test's cylinder
/// barely does.
const STRIP: f64 = 2.5e-4;
/// How far a prototype piping vertex may lie from the port's tube, metres.
const CORD: f64 = 5e-5;
/// The geometry set the dumps hold the uniform and robe in.
const SET: &str = "game";

/// The dump, or `None` (after saying why) so the caller skips.
fn dump() -> Option<omuzaki_test::jsref::GeometryDump> {
    let js = JsRef::open()?;
    Some(js.geometry(SET).expect("the geometry dump loads"))
}

fn positions(mesh: &MeshData) -> Vec<DVec3> {
    mesh.positions.iter().map(|p| p.as_dvec3()).collect()
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

/// The farthest any of `points` lies from the surface of `mesh`.
fn farthest(points: &[DVec3], mesh: &MeshData) -> f64 {
    let p = positions(mesh);
    let triangles: Vec<[DVec3; 3]> =
        mesh.indices.chunks_exact(3).map(|t| [p[t[0] as usize], p[t[1] as usize], p[t[2] as usize]]).collect();
    points
        .iter()
        .map(|q| {
            triangles
                .iter()
                .map(|[a, b, c]| q.distance(closest_on_triangle(*q, *a, *b, *c)))
                .fold(f64::INFINITY, f64::min)
        })
        .fold(0.0, f64::max)
}

/// The root of `k`'s set in the union-find `parent`, halving the path as it goes.
fn root(parent: &mut [usize], mut k: usize) -> usize {
    while parent[k] != k {
        parent[k] = parent[parent[k]];
        k = parent[k];
    }
    k
}

/// The connected pieces of `mesh`, as lists of vertex indices (a merged geometry keeps each primitive separate).
fn pieces(mesh: &MeshData) -> Vec<Vec<usize>> {
    let n = mesh.positions.len();
    let mut parent: Vec<usize> = (0..n).collect();
    for t in mesh.indices.chunks_exact(3) {
        let a = root(&mut parent, t[0] as usize);
        for &i in &t[1..] {
            let b = root(&mut parent, i as usize);
            parent[b] = a;
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for k in 0..n {
        let r = root(&mut parent, k);
        groups.entry(r).or_default().push(k);
    }
    groups.into_values().collect()
}

/// The direction in which `points` vary least: the normal of a flat or round piece (the cross product of the two
/// largest principal axes, found by power iteration).
fn flattest(points: &[DVec3]) -> DVec3 {
    let centre = points.iter().copied().sum::<DVec3>() / points.len() as f64;
    let mut cov = [[0.0_f64; 3]; 3];
    for p in points {
        let d = (*p - centre).to_array();
        for (i, row) in cov.iter_mut().enumerate() {
            for (j, c) in row.iter_mut().enumerate() {
                *c += d[i] * d[j];
            }
        }
    }
    let apply = |m: &[[f64; 3]; 3], v: DVec3| DVec3::from_array([0, 1, 2].map(|i| DVec3::from_array(m[i]).dot(v)));
    let power = |m: &[[f64; 3]; 3], start: DVec3| {
        let mut v = start;
        for _ in 0..200 {
            v = apply(m, v).normalize_or_zero();
        }
        v
    };
    let first = power(&cov, DVec3::new(0.3, 0.5, 0.8).normalize());
    let lambda = first.dot(apply(&cov, first));
    let mut deflated = cov;
    for (i, row) in deflated.iter_mut().enumerate() {
        for (j, c) in row.iter_mut().enumerate() {
            *c -= lambda * first[i] * first[j];
        }
    }
    let second = power(&deflated, first.any_orthonormal_vector());
    first.cross(second).normalize()
}

// ------------------------------------------------------------------------------------------------------------ buttons

#[test]
fn the_gold_buttons_match_the_prototypes() {
    let Some(dump) = dump() else { return };
    let mesh = &dump.mesh("buttons").expect("the dump has the buttons").mesh;
    let all = positions(mesh);
    let mut clusters: Vec<Vec<DVec3>> = vec![Vec::new(); 3];
    for p in &all {
        let k = [0.408, 0.382, 0.356]
            .iter()
            .enumerate()
            .min_by(|a, b| (p.y - a.1).abs().total_cmp(&(p.y - b.1).abs()))
            .unwrap()
            .0;
        clusters[k].push(*p);
    }
    let style =
        ButtonStyle { radius: buttons::GOLD_RADIUS, shank: 0.0, shape: ButtonShape::Dome, finish: Finish::default() };
    let mut centres = Vec::new();
    for (k, cluster) in clusters.iter().enumerate() {
        let mut n = flattest(cluster);
        if n.z < 0.0 {
            n = -n;
        }
        let along: Vec<f64> = cluster.iter().map(|p| p.dot(n)).collect();
        let low = along.iter().copied().fold(f64::INFINITY, f64::min);
        let centroid = cluster.iter().copied().sum::<DVec3>() / cluster.len() as f64;
        let foot = centroid - n * (centroid.dot(n) - low);
        let mut port = MeshBuilder::new();
        buttons::button(&mut port, foot, n, &style, &[(JointRef::HIPS, 1.0)]).unwrap();
        let off = farthest(cluster, port.mesh());
        println!("button {k}: {} prototype vertices, the farthest {:.4} mm from the port's", cluster.len(), off * 1e3);
        assert!(off <= BUTTON, "button {k}: a prototype vertex is {off} m from the port's button");
        centres.push(foot);
    }
    for pair in centres.windows(2) {
        let gap = pair[0].distance(pair[1]);
        println!("buttons {gap:.4} m apart");
        assert!((gap - 0.026).abs() < 5e-4, "the buttons are {gap} m apart, not 0.026");
    }
}

// --------------------------------------------------------------------------------------------------------- the strip

/// A tall cylinder of radius 1 m round the y axis, as the torso: cloth that is nearly flat across a strip.
#[derive(Debug)]
struct Cylinder;

impl BodyFit for Cylinder {
    fn section(&self, region: BodyRegion, t: f32) -> Option<Section> {
        (region == BodyRegion::Torso).then(|| Section {
            centre: Vec3::new(0.0, t, 0.0),
            axis: Vec3::Y,
            radii: Vec2::splat(1.0),
        })
    }

    fn joint(&self, joint: &JointRef) -> Option<Vec3> {
        (*joint == JointRef::Humanoid(HumanoidBone::Hips)).then_some(Vec3::ZERO)
    }
}

/// The 11 vertices of a rung in its own frame: across the band (positive into it) and up off the cloth, measured from
/// the free edge on the cloth. `face0` is the face's first vertex (at the free edge, the lift and margin up) and
/// `face1` its second: they give the normal (from `normal`) and the direction into the band.
fn local(rung: &[DVec3], normal: DVec3) -> Vec<(f64, f64)> {
    let (face0, face1) = (rung[5], rung[6]);
    let into = {
        let d = face1 - face0;
        (d - normal * d.dot(normal)).normalize()
    };
    let foot = face0 - normal * (LIFT + piping::FACE_MARGIN);
    rung.iter().map(|p| ((*p - foot).dot(into), (*p - foot).dot(normal))).collect()
}

#[test]
fn the_robes_rolled_edge_matches_the_prototypes_cross_section() {
    let Some(dump) = dump() else { return };
    let js = &dump.mesh("robe-trim").expect("the dump has the robe's strips").mesh;
    // Rungs of 11 vertices, one after another: a middle rung of the first strip, where the bevel and bulge are whole.
    let rung = 10;
    let theirs: Vec<DVec3> = (0..11).map(|j| js.positions[rung * 11 + j].as_dvec3()).collect();
    let span = theirs.iter().map(|p| p.distance(theirs[5])).fold(0.0, f64::max);
    assert!(span < ROBE.3 + 0.01, "the dump's rungs are not 11 vertices one after another (a rung spans {span} m)");
    let their_normal = js.normals[rung * 11 + 5].as_dvec3().normalize();

    let surface = Surface::new(Arc::new(Cylinder), BodyRegion::Torso, 2.0, 0.0).unwrap();
    let (thick, lip_height, lip_width, wide) = ROBE;
    let spec = Piping {
        rail: RailSpec::new(&[[0.8, 0.0], [0.2, 0.0]], false),
        width: wide,
        thickness: thick,
        bevel: 1.0,
        lip: Some(Lip { height: lip_height, width: lip_width }),
        bulge: 1.0,
        ramp: 0,
        caps: false,
        flip: false,
        tile: piping::TILE,
        finish: Finish::default(),
    };
    let port = piping::build(&surface, &spec).unwrap();
    let rail = Rail::from_polyline_on_surface(&surface, &spec.rail, spec.rail.step).unwrap();
    let middle = rail.points().len() / 2;
    let ours: Vec<DVec3> = (0..11).map(|j| port.mesh().positions[middle * 11 + j].as_dvec3()).collect();
    let our_normal = rail.points()[middle].normal;

    let (a, b) = (local(&theirs, their_normal), local(&ours, our_normal));
    let mut worst = 0.0_f64;
    // The lip, the rolled edge and the face's first two vertices: within a centimetre of the free edge.
    for j in 0..7 {
        let d = ((a[j].0 - b[j].0).powi(2) + (a[j].1 - b[j].1).powi(2)).sqrt();
        println!("vertex {j}: prototype {:?}, port {:?}, {:.4} mm apart", a[j], b[j], d * 1e3);
        worst = worst.max(d);
    }
    assert!(worst <= STRIP, "the cross-section differs by {worst} m near the edge");
}

// ---------------------------------------------------------------------------------------------------- vest piping

#[test]
fn the_vest_piping_matches_the_prototypes_ring_tubes() {
    let Some(dump) = dump() else { return };
    let vest = &dump.mesh("vest").expect("the dump has the vest").mesh;
    let all = positions(vest);
    let rings = 85;
    let tubes: Vec<Vec<usize>> = pieces(vest).into_iter().filter(|p| p.len() == rings * 7).collect();
    assert_eq!(tubes.len(), 2, "the rim's and the hem's piping");
    for (k, tube) in tubes.iter().enumerate() {
        let mut ordered = tube.clone();
        ordered.sort_unstable();
        // Each ring's centre: the mean of its 6 distinct vertices (the 7th repeats the first).
        let centres: Vec<RailPoint> = (0..rings)
            .map(|i| {
                let ring: Vec<DVec3> = (0..6).map(|j| all[ordered[i * 7 + j]]).collect();
                let centre = ring.iter().copied().sum::<DVec3>() / 6.0;
                let radial = DVec3::new(centre.x, 0.0, centre.z).normalize();
                RailPoint {
                    point: centre,
                    on_surface: centre,
                    normal: radial,
                    tangent: DVec3::ZERO,
                    side: DVec3::ZERO,
                    t: 0.0,
                    phi: 0.0,
                    distance: 0.0,
                    skin: vec![(JointRef::HIPS, 1.0)],
                }
            })
            .collect();
        let port = piping::ring_tube(&centres, piping::VEST_PIPING, piping::CORD_SEGMENTS as usize).unwrap();
        let theirs: Vec<DVec3> = ordered.iter().map(|&i| all[i]).collect();
        let off = farthest(&theirs, port.mesh());
        println!("vest piping {k}: the farthest prototype vertex is {:.4} mm from the port's tube", off * 1e3);
        assert!(off <= CORD, "vest piping {k}: {off} m off");
    }
}
