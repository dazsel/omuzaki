//! What the two boots parity tests share: the oracle (the sample character's boot dump and her private profile file),
//! the boots built from that profile, and the comparison of a range of one mesh against the prototype's.
//!
//! The oracle is private test material kept in the local assets folder (`goldens/js`, `characters/<private-test>/
//! boots-profile.json`); without it every test skips with a message. The profile file holds the prototype's `BOOT`
//! knots, which are hand-fitted to her body and so stay out of the repository.

// Test helpers: a panic is a clear failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(dead_code)] // each test binary uses the part of the helpers it needs
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use std::collections::BTreeMap;

use omuzaki_char::kit::js_names;
use omuzaki_char::outfit::boots::{BootProfile, BootsParams, LAYER_FOOTWEAR};
use omuzaki_char_api::{BodyShape, BuildOptions, BuildServices, JointRef, PartMesh, PartOutput, Quality};
use omuzaki_core::mesh::MeshData;
use omuzaki_test::jsref::{GeometryDump, JsMesh, JsRef};
use omuzaki_test::meshcmp::{MeshTolerance, compare_meshes};

#[path = "outfit_conformance.rs"]
mod conformance;

/// The golden variant the boots are dumped in (both variants hold the same boots).
pub const SET: &str = "game";
/// What an in-order match must reach: the port runs the prototype's arithmetic, so vertices agree to rounding in `f32`.
pub const EXACT: MeshTolerance =
    MeshTolerance { max_vertex_error: 2e-6, max_normal_angle_deg: 0.05, max_chamfer: 3e-4, max_hausdorff: 1.5e-3 };
/// The vertices of the prototype's left boot upper and cuff, strap bands, pull tab and strap tails in the `boots` mesh,
/// at the prototype's tessellation: 42 rows of 73, 21 of 73, two of 15 x 81, 6 x 11, two tails of 171 + 2 x 38.
pub const UPPER: usize = 42 * 73;
pub const CUFF: usize = 21 * 73;
pub const STRAPS: usize = 2 * 15 * 81;
pub const TAB: usize = 6 * 11;
pub const TAILS: usize = 2 * (19 * 9 + 2 * 19 * 2);

/// The dump and the profile, or `None` (after saying why) so the caller skips.
pub struct Oracle {
    pub dump: GeometryDump,
    pub profile: BootProfile,
}

pub fn open() -> Option<Oracle> {
    let js = JsRef::open()?;
    let assets = js.root().parent().and_then(std::path::Path::parent).expect("the dump root is <assets>/goldens/js");
    let file = assets.join("characters/<private-test>/boots-profile.json");
    let Ok(text) = std::fs::read_to_string(&file) else {
        eprintln!("skipped: no boot profile at {} (it holds the prototype's hand-fitted knots)", file.display());
        return None;
    };
    let profile = serde_json::from_str(&text).expect("the boot profile file parses");
    Some(Oracle { dump: js.geometry(SET).expect("the geometry dump loads"), profile })
}

/// The boots built from the oracle's profile at the prototype's tessellation.
pub fn build(oracle: &Oracle) -> PartOutput {
    let params = BootsParams { profile: Some(oracle.profile.clone()), ..BootsParams::default() };
    let def = conformance::character(
        "soft-sd",
        &BodyShape::default(),
        &[(conformance::PART, "boots", LAYER_FOOTWEAR, serde_json::to_value(&params).unwrap())],
    );
    let options = BuildOptions { quality: Quality::Offline, ..BuildOptions::default() };
    let services = BuildServices::default();
    let mut published = omuzaki_char_api::Published::default();
    let mut boots = None;
    for id in omuzaki_char_api::plan(&def).unwrap() {
        let out = omuzaki_char_api::build_part(&def, &id, &options, &services, &published).unwrap();
        published.absorb(&id, out.publishes.clone()).unwrap();
        boots = Some(out);
    }
    boots.expect("the boots were built")
}

/// The part's mesh named `<part>-<name>`.
pub fn mesh<'a>(out: &'a PartOutput, name: &str) -> &'a PartMesh {
    let full = format!("{}-{name}", conformance::PART);
    out.meshes.iter().find(|m| m.name == full).unwrap_or_else(|| panic!("the boots have no `{full}` mesh"))
}

/// The prototype's bone name as `js_names` spells it: the dump writes `foot.L` as `footL`.
fn dotted(name: &str) -> String {
    for stem in ["upper_leg", "lower_leg", "foot", "toes"] {
        for side in ["L", "R"] {
            if name == format!("{stem}{side}") {
                return format!("{stem}.{side}");
            }
        }
    }
    name.to_owned()
}

/// Items `lo..hi` of a per-vertex list, or none when the mesh doesn't carry the list.
fn part<T: Clone>(items: &[T], lo: usize, hi: usize) -> Vec<T> {
    items.get(lo..hi).map(<[T]>::to_vec).unwrap_or_default()
}

/// Vertices `lo..hi` of `mesh` and the triangles that lie wholly inside them, renumbered from 0.
pub fn range(mesh: &MeshData, lo: usize, hi: usize) -> MeshData {
    let keep = |i: &u32| (lo..hi).contains(&(*i as usize));
    let indices =
        mesh.indices.chunks_exact(3).filter(|t| t.iter().all(keep)).flatten().map(|i| *i - lo as u32).collect();
    MeshData {
        positions: mesh.positions[lo..hi].to_vec(),
        normals: mesh.normals[lo..hi].to_vec(),
        uvs: part(&mesh.uvs, lo, hi),
        colours: part(&mesh.colours, lo, hi),
        joints: part(&mesh.joints, lo, hi),
        weights: part(&mesh.weights, lo, hi),
        extra: Vec::new(),
        indices,
    }
}

/// The largest difference of any vertex's colour channel in `lo..hi`, ours against the prototype's.
pub fn colour_error(ours: &MeshData, theirs: &MeshData, lo: usize, hi: usize) -> f32 {
    if theirs.colours.is_empty() {
        return 0.0;
    }
    assert_eq!(ours.colours.len(), ours.positions.len(), "our mesh has vertex colours");
    (lo..hi).flat_map(|v| (0..3).map(move |c| (ours.colours[v][c] - theirs.colours[v][c]).abs())).fold(0.0, f32::max)
}

/// The largest difference of any vertex's weight on any joint in `lo..hi`, ours against the prototype's.
pub fn weight_error(ours: &PartMesh, theirs: &JsMesh, dump: &GeometryDump, lo: usize, hi: usize) -> f32 {
    let weights_of = |joints: &[u16; 4], weights: &[f32; 4], names: &dyn Fn(u16) -> JointRef| {
        let mut out: BTreeMap<String, f32> = BTreeMap::new();
        for k in 0..4 {
            if weights[k] > 0.0 {
                *out.entry(format!("{:?}", names(joints[k]))).or_default() += weights[k];
            }
        }
        out
    };
    let mine = |j: u16| ours.joint_bones[usize::from(j)].clone();
    let theirs_joint = |j: u16| js_names::joint(&dotted(&dump.bones[usize::from(j)])).expect("a bone name");
    let (g, t) = (&ours.geometry, &theirs.mesh);
    let mut worst = (0.0_f32, lo);
    for v in lo..hi {
        let (a, b) =
            (weights_of(&g.joints[v], &g.weights[v], &mine), weights_of(&t.joints[v], &t.weights[v], &theirs_joint));
        let e = a
            .keys()
            .chain(b.keys())
            .map(|k| (a.get(k).copied().unwrap_or(0.0) - b.get(k).copied().unwrap_or(0.0)).abs())
            .fold(0.0, f32::max);
        if e > worst.0 {
            worst = (e, v);
            eprintln!("  weights at {v} ({:?}): ours {a:?} theirs {b:?}", g.positions[v]);
        }
    }
    worst.0
}

/// What a range of a mesh must match.
#[derive(Copy, Clone)]
pub struct Limits {
    /// The in-order position and normal tolerance.
    pub tolerance: MeshTolerance,
    /// The most a vertex colour channel may differ.
    pub colour: f32,
    /// The most a vertex weight may differ.
    pub weight: f32,
    /// Whether the triangle lists must be identical. When not, only the triangles with an area must agree in number
    /// (the prototype's sphere has no triangles at its poles; ours keeps the grid's zero-area ones).
    pub exact_triangles: bool,
}

impl Limits {
    /// Everything exact: the port runs the prototype's arithmetic.
    pub const EXACT: Self = Self { tolerance: EXACT, colour: 1e-6, weight: 1e-4, exact_triangles: true };
}

/// The triangles of `mesh` that have an area.
fn with_area(mesh: &MeshData) -> usize {
    mesh.indices
        .chunks_exact(3)
        .filter(|t| {
            let p = |i: u32| mesh.positions[i as usize].as_dvec3();
            (p(t[1]) - p(t[0])).cross(p(t[2]) - p(t[0])).length() > 1e-18
        })
        .count()
}

/// Compares vertices `lo..hi` of our mesh against the prototype's, prints the report, and asserts it is in `limits`.
pub fn check(
    label: &str,
    ours: &PartMesh,
    theirs: &JsMesh,
    dump: &GeometryDump,
    (lo, hi): (usize, usize),
    limits: Limits,
) {
    let (a, b) = (range(&ours.geometry, lo, hi), range(&theirs.mesh, lo, hi));
    let report = compare_meshes(&a, &b, limits.tolerance);
    let colours = colour_error(&ours.geometry, &theirs.mesh, lo, hi);
    let weights = weight_error(ours, theirs, dump, lo, hi);
    println!("{label}:\n{report}  colours: max {colours:.2e}, weights: max {weights:.2e}");
    assert!(report.passed(), "{label} does not match the prototype:\n{report}");
    assert!(report.verdict == omuzaki_test::meshcmp::Verdict::InOrder, "{label}: not in order");
    assert_eq!(report.vertices.actual, report.vertices.expected, "{label}: vertex counts");
    if limits.exact_triangles {
        assert_eq!(report.triangles.actual, report.triangles.expected, "{label}: triangle counts");
        assert_eq!(a.indices, b.indices, "{label}: the triangles differ");
    } else {
        assert_eq!(with_area(&a), with_area(&b), "{label}: triangles with an area");
    }
    assert!(colours <= limits.colour, "{label}: colours differ by {colours}");
    assert!(weights <= limits.weight, "{label}: weights differ by {weights}");
}
