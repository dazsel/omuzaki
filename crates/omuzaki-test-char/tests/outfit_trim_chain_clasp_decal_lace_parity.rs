//! Parity of trim library II (`omuzaki_char::outfit::trim`, #189) with the prototype's robe clasps and chain, as #189's
//! reference gives them: the clasp (stud, bail, ring, beads, drop), the chain's links and the pendant cross.
//!
//! The oracle is the sample character's dump, private test material kept in the local assets folder: without it
//! (always, in CI and in the cloud) every test here skips with a message. The prototype built these on its robe, which
//! this crate doesn't have yet, so the tests recover what they need from the `robe-gold` mesh itself, piece by piece (a
//! merged geometry keeps each primitive a separate connected piece):
//! - **clasps:** each ring (a torus of 33 x 9 vertices) gives the clasp's frame: its centre and the normal of its plane
//!   from the welded vertices (three.js repeats the seams, which would bias an average; a synthetic test recovers known
//!   tori exactly), and its up toward the collar bead on the ring's top; the anchor is the ring's hang above it. The
//!   port's clasp in that frame must lie on the prototype's surfaces, every vertex within [`CLASP`];
//! - **the chain:** the links (tori of 11 x 5 vertices) give their centres, welded as the rings are; the port's links,
//!   hung from the attach points on the two rings with the prototype's sag, bow and pitch, must be as many and within
//!   [`LINK`] of them;
//! - **the pendant cross:** built at the chain's middle in the world's frame, the port's cross must lie on the
//!   prototype's surfaces within [`CROSS`] (outside the centre box, which hides how the arms meet).
//!
//! The decal and the lace edge have no prototype counterpart, so they have no parity test. The numbers measured are
//! printed so the first run with the dumps can propose the final tolerances.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers load local files; a bad one should stop the test with its reason
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic on small counts
#![allow(clippy::many_single_char_names, clippy::chunks_exact_to_as_chunks)] // geometry reads best as a, b, c

use std::collections::BTreeMap;

use omuzaki_char::kit::grid::MeshBuilder;
use omuzaki_char::outfit::trim::Basis;
use omuzaki_char::outfit::trim::chain::{self, Hang, PITCH, SAG};
use omuzaki_char::outfit::trim::clasp::{self, CROSS_BOX, ClaspStyle, RING, RING_HANG};
use omuzaki_char_api::JointRef;
use omuzaki_core::glam::DVec3;
use omuzaki_core::mesh::MeshData;
use omuzaki_test::jsref::JsRef;

/// How far a port clasp vertex may lie from the prototype's surfaces, metres.
const CLASP: f64 = 2e-4;
/// How far a port link's centre may lie from the prototype's, metres.
const LINK: f64 = 1e-4;
/// How far a port cross vertex may lie from the prototype's surfaces, metres (its bevels are approximated).
const CROSS: f64 = 3e-4;
/// The geometry set the dumps hold the robe in.
const SET: &str = "game";
/// Vertices of the clasp's ring (three.js torus, 8 x 32 segments) and of a link (4 x 10). The dump keeps three.js's
/// repeated seam vertices; a welded count can't stand in, since a small sphere of the clasp (6 x 5 segments, its two
/// unused pole vertices apart) has the 40 vertices a welded link would.
const RING_VERTICES: usize = 33 * 9;
const LINK_VERTICES: usize = 11 * 5;
/// How close two positions must be to be one seam vertex, metres.
const WELD: f64 = 1e-7;

/// The robe's gold mesh, or `None` (after saying why) so the caller skips.
fn gold() -> Option<MeshData> {
    let js = JsRef::open()?;
    let dump = js.geometry(SET).expect("the geometry dump loads");
    Some(dump.mesh("robe-gold").expect("the dump has the robe's gold").mesh.clone())
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

/// The farthest any of `points` lies from the surface of `mesh`, and that point.
fn farthest(points: &[DVec3], mesh: &MeshData) -> (f64, DVec3) {
    let p = positions(mesh);
    let triangles: Vec<[DVec3; 3]> =
        mesh.indices.chunks_exact(3).map(|t| [p[t[0] as usize], p[t[1] as usize], p[t[2] as usize]]).collect();
    points
        .iter()
        .map(|q| {
            let d = triangles
                .iter()
                .map(|[a, b, c]| q.distance(closest_on_triangle(*q, *a, *b, *c)))
                .fold(f64::INFINITY, f64::min);
            (d, *q)
        })
        .fold((0.0, DVec3::ZERO), |a, b| if b.0 > a.0 { b } else { a })
}

/// The root of `k`'s set in the union-find `parent`, halving the path as it goes.
fn root(parent: &mut [usize], mut k: usize) -> usize {
    while parent[k] != k {
        parent[k] = parent[parent[k]];
        k = parent[k];
    }
    k
}

/// The connected pieces of `mesh`, as lists of vertex indices.
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

fn centroid(points: &[DVec3]) -> DVec3 {
    points.iter().copied().sum::<DVec3>() / points.len() as f64
}

/// `points` with each position kept once: three.js repeats a torus's seam vertices (the first column and row again at
/// the end), which would pull an average toward the seam.
fn welded(points: &[DVec3]) -> Vec<DVec3> {
    let mut unique: Vec<DVec3> = Vec::with_capacity(points.len());
    for p in points {
        if !unique.iter().any(|q| q.distance(*p) < WELD) {
            unique.push(*p);
        }
    }
    unique
}

/// The unit eigenvector of the symmetric 3×3 matrix `m` with the smallest eigenvalue: power iteration on
/// `trace·I - m`, whose largest eigenvector it is (all eigenvalues of a covariance are non-negative).
fn smallest_axis(m: [[f64; 3]; 3]) -> DVec3 {
    let trace = m[0][0] + m[1][1] + m[2][2];
    let apply = |v: DVec3| {
        let row = |i: usize| DVec3::from_array(m[i]).dot(v);
        v * trace - DVec3::new(row(0), row(1), row(2))
    };
    let mut v = DVec3::new(0.3, 0.5, 0.8).normalize();
    for _ in 0..500 {
        v = apply(v).normalize();
    }
    v
}

/// A torus's centre and the normal of its plane, from its vertices: the centroid and the axis of least spread of the
/// welded vertex set. three.js lays a torus's vertices evenly round both circles, so once the seam is welded the set is
/// symmetric about the centre and the plane, and both are exact (a link stretched along one in-plane axis too).
fn torus_frame(points: &[DVec3]) -> (DVec3, DVec3) {
    let unique = welded(points);
    let centre = centroid(&unique);
    let mut cov = [[0.0_f64; 3]; 3];
    for p in &unique {
        let d = (*p - centre).to_array();
        for (i, row) in cov.iter_mut().enumerate() {
            for (j, c) in row.iter_mut().enumerate() {
                *c += d[i] * d[j];
            }
        }
    }
    (centre, smallest_axis(cov))
}

/// The two clasps' rings: each one's centre and its normal out of the cloth (toward +z), side 0 (at +x) first.
fn rings(mesh: &MeshData) -> Vec<(DVec3, DVec3)> {
    let all = positions(mesh);
    let mut found: Vec<(DVec3, DVec3)> = pieces(mesh)
        .into_iter()
        .filter(|p| p.len() == RING_VERTICES)
        .map(|p| {
            let points: Vec<DVec3> = p.iter().map(|&i| all[i]).collect();
            let (centre, mut normal) = torus_frame(&points);
            if normal.z < 0.0 {
                normal = -normal;
            }
            (centre, normal)
        })
        .collect();
    found.sort_by(|a, b| b.0.x.total_cmp(&a.0.x));
    found
}

/// The clasp's frame from its ring: out the ring's normal, and up toward the collar bead on the ring's top (the
/// prototype's up along the cloth, which the port's level frame replaces on a garment: on the robe the two differ by
/// about a degree, so the pieces are compared in the prototype's own frame).
fn frame((centre, normal): (DVec3, DVec3), mesh: &MeshData) -> Basis {
    let level = (DVec3::Y - normal * normal.y).normalize();
    let all = positions(mesh);
    let guess = centre + level * RING.0;
    let collar = pieces(mesh)
        .into_iter()
        .map(|p| centroid(&welded(&p.iter().map(|&i| all[i]).collect::<Vec<_>>())))
        .min_by(|a, b| a.distance(guess).total_cmp(&b.distance(guess)))
        .expect("the gold has pieces");
    assert!(collar.distance(guess) < 0.002, "a collar bead sits on the ring's top");
    let up = collar - centre;
    let up = (up - normal * up.dot(normal)).normalize();
    let x = up.cross(normal).normalize();
    let anchor = centre + up * (RING.0 + RING_HANG.0) - normal * RING_HANG.1;
    Basis { origin: anchor, x, y: up, z: x.cross(up) }
}

#[test]
fn the_clasps_match_the_prototypes() {
    let Some(mesh) = gold() else { return };
    let found = rings(&mesh);
    assert_eq!(found.len(), 2, "two clasps");
    for (side, ring) in found.into_iter().enumerate() {
        let at = frame(ring, &mesh);
        let mut port = MeshBuilder::new();
        clasp::pieces(&mut port, None, (&at, &at), ClaspStyle::Medallion, (1.0, 0.0), &[(JointRef::HIPS, 1.0)])
            .unwrap();
        let ours = positions(port.mesh());
        let (off, worst) = farthest(&ours, &mesh);
        let local = worst - at.origin;
        println!(
            "clasp {side}: {} port vertices, the farthest {:.4} mm from the prototype's, at ({:.2}, {:.2}, {:.2}) mm in \
             its frame",
            ours.len(),
            off * 1e3,
            local.dot(at.x) * 1e3,
            local.dot(at.y) * 1e3,
            local.dot(at.z) * 1e3
        );
        assert!(off <= CLASP, "clasp {side}: a port vertex is {off} m from the prototype's surfaces");
    }
}

/// The chain's hang: from each ring's attach point (on its inner side, toward the other ring) with the prototype's sag
/// and bow.
fn hang(found: &[(DVec3, DVec3)]) -> Hang {
    let attach = |(centre, normal): (DVec3, DVec3), sign: f64| {
        let to_middle = DVec3::new(-sign, 0.0, 0.0);
        let to_middle = (to_middle - normal * to_middle.dot(normal)).normalize();
        centre + to_middle * (RING.0 + 0.4 * RING.1)
    };
    Hang {
        from: attach(found[0], 1.0),
        to: attach(found[1], -1.0),
        up: DVec3::Y,
        out: DVec3::Z,
        sag: SAG.0,
        bow: SAG.1,
    }
}

#[test]
fn the_chain_links_sit_where_the_prototypes_do() {
    let Some(mesh) = gold() else { return };
    let all = positions(&mesh);
    let theirs: Vec<DVec3> = pieces(&mesh)
        .into_iter()
        .filter(|p| p.len() == LINK_VERTICES)
        .map(|p| torus_frame(&p.iter().map(|&i| all[i]).collect::<Vec<_>>()).0)
        .collect();
    let hang = hang(&rings(&mesh));
    let ours = chain::links(&hang, PITCH, None, 0.002).unwrap();
    println!("{} prototype links, {} port links", theirs.len(), ours.len());
    assert_eq!(theirs.len(), ours.len(), "as many links");
    let worst = ours
        .iter()
        .map(|l| theirs.iter().map(|c| c.distance(l.centre)).fold(f64::INFINITY, f64::min))
        .fold(0.0, f64::max);
    println!("the farthest port link is {:.4} mm from a prototype link", worst * 1e3);
    assert!(worst <= LINK, "a link is {worst} m from the prototype's");
}

#[test]
fn the_pendant_cross_matches_the_prototypes() {
    let Some(mesh) = gold() else { return };
    let hang = hang(&rings(&mesh));
    let at = Basis { origin: hang.at(0.5), x: DVec3::X, y: DVec3::Y, z: DVec3::Z };
    let mut port = MeshBuilder::new();
    clasp::pendant_cross(&mut port, &at, 1.0, &[(JointRef::HIPS, 1.0)]).unwrap();
    // The cross's centre, and the box round it that hides where the arms meet.
    let (lv, _, _) = clasp::CROSS_ARMS;
    let (top, gap, out) = clasp::CROSS_HANG;
    let centre = at.place(DVec3::new(0.0, -top - gap - lv, out));
    let half = DVec3::new(CROSS_BOX.0, CROSS_BOX.0, CROSS_BOX.1) / 2.0;
    let ours: Vec<DVec3> = positions(port.mesh())
        .into_iter()
        .filter(|p| {
            let d = (*p - centre).abs();
            !(d.x < half.x && d.y < half.y && d.z < half.z)
        })
        .collect();
    let (off, worst) = farthest(&ours, &mesh);
    let local = (worst - centre) * 1e3;
    println!(
        "the pendant cross: {} port vertices, the farthest {:.4} mm from the prototype's, at ({:.2}, {:.2}, {:.2}) mm \
         from its centre",
        ours.len(),
        off * 1e3,
        local.x,
        local.y,
        local.z
    );
    assert!(off <= CROSS, "a port cross vertex is {off} m from the prototype's surfaces");
}

/// A three.js `TorusGeometry(ring, tube, radial, tubular)`: `(radial + 1) × (tubular + 1)` vertices, seams repeated,
/// in the xy plane round z.
fn three_torus(ring: f64, tube: f64, radial: usize, tubular: usize) -> Vec<DVec3> {
    let mut points = Vec::new();
    for j in 0..=radial {
        for i in 0..=tubular {
            let u = std::f64::consts::TAU * i as f64 / tubular as f64;
            let v = std::f64::consts::TAU * j as f64 / radial as f64;
            points.push(DVec3::new(
                (ring + tube * v.cos()) * u.cos(),
                (ring + tube * v.cos()) * u.sin(),
                tube * v.sin(),
            ));
        }
    }
    points
}

#[test]
fn torus_frames_are_recovered_exactly_from_three_js_tori() {
    use omuzaki_core::glam::DQuat;
    let turns = [
        DQuat::IDENTITY,
        DQuat::from_euler(omuzaki_core::glam::EulerRot::XYZ, 0.3, -0.7, 1.1),
        DQuat::from_euler(omuzaki_core::glam::EulerRot::XYZ, -1.2, 0.4, 2.5),
    ];
    let places = [DVec3::ZERO, DVec3::new(0.06, 1.31, 0.12), DVec3::new(-0.04, 1.27, 0.09)];
    // The clasp's ring, and a chain link stretched along x as the port stretches it.
    let shapes = [
        (three_torus(RING.0, RING.1, 8, 32), DVec3::ONE),
        (three_torus(chain::LINK.0, chain::LINK.1, 4, 10), DVec3::new(chain::LINK_STRETCH, 1.0, 1.0)),
    ];
    for (points, stretch) in &shapes {
        for turn in turns {
            for place in places {
                let moved: Vec<DVec3> = points.iter().map(|p| place + turn * (*p * *stretch)).collect();
                let (centre, normal) = torus_frame(&moved);
                let axis = turn * DVec3::Z;
                println!(
                    "centre off by {:.2e} m, normal off by {:.2e} rad",
                    centre.distance(place),
                    normal.dot(axis).abs().min(1.0).acos()
                );
                assert!(centre.distance(place) < 1e-4, "the centre is {} m off", centre.distance(place));
                assert!(normal.dot(axis).abs() > 1.0 - 1e-9, "the normal {normal} isn't the axis {axis}");
            }
        }
    }
}
