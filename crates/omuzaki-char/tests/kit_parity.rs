//! Parity of the geometry kit with the prototype's `uniform-geo.js`: the fixtures in `fixtures/kit/` were produced by
//! running that file under Node with three.js (the prototype's generator script); these tests rebuild the same inputs and
//! compare. Tolerances: PCHIP 1e-9, normals 0.01 degrees, UVs and colours 1e-6, weights 1e-5, positions and indices exact.

#![allow(clippy::float_cmp)] // positions are float32 values that must match exactly
#![allow(clippy::cast_precision_loss)] // small loop counters
#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers load fixtures; a bad fixture should stop the test with its reason

use omuzaki_char::kit::curve::Pchip;
use omuzaki_char::kit::grid::{Grid, GridOptions, MeshBuilder, Skin, grid_normals};
use omuzaki_char::kit::js_names;
use omuzaki_char_api::JointRef;
use omuzaki_core::glam::{DVec3, Vec2, Vec3};
use serde::Deserialize;

type Points = Vec<Vec<[f64; 3]>>;

fn grid(points: &Points) -> Grid {
    let rows: Vec<Vec<DVec3>> = points.iter().map(|r| r.iter().map(|p| DVec3::from_array(*p)).collect()).collect();
    Grid::from_rows(&rows).expect("a fixture grid is well-formed")
}

#[derive(Deserialize)]
struct PchipFixture {
    knots: Vec<(f64, f64)>,
    xs: Vec<f64>,
    ys: Vec<f64>,
}

#[test]
fn pchip_matches_the_prototype_at_200_samples_over_20_knots() {
    let f: PchipFixture = serde_json::from_str(include_str!("fixtures/kit/pchip.json")).unwrap();
    assert_eq!((f.knots.len(), f.xs.len()), (20, 200));
    let curve = Pchip::new(&f.knots).unwrap();
    let worst = f.xs.iter().zip(&f.ys).map(|(&x, &y)| (curve.eval(x) - y).abs()).fold(0.0, f64::max);
    assert!(worst < 1e-9, "worst difference {worst:e}");
}

#[derive(Deserialize)]
struct Inside {
    point: Option<[f64; 3]>,
    per_row: Option<Vec<[f64; 3]>>,
}

#[derive(Deserialize)]
#[allow(clippy::struct_excessive_bools)] // mirrors the generator's option names
struct RunOptions {
    #[serde(default)]
    wrap_i: bool,
    #[serde(default)]
    wrap_j: bool,
    #[serde(default)]
    flip: bool,
    #[serde(default)]
    invert: bool,
    inside: Option<Inside>,
}

#[derive(Deserialize)]
struct Run {
    options: RunOptions,
    flip: bool,
    normals: Vec<[f64; 3]>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    points: Points,
    runs: Vec<Run>,
}

#[derive(Deserialize)]
struct NormalsFixture {
    cases: Vec<Case>,
}

/// The angle between two vectors in degrees, accurate for tiny angles.
fn degrees_between(a: DVec3, b: DVec3) -> f64 {
    a.cross(b).length().atan2(a.dot(b)).to_degrees()
}

#[test]
fn grid_normals_match_the_prototype_on_a_sphere_with_poles_a_torus_and_an_open_sheet() {
    let f: NormalsFixture = serde_json::from_str(include_str!("fixtures/kit/grid_normals.json")).unwrap();
    assert_eq!(f.cases.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["sphere", "torus", "sheet"]);
    for case in &f.cases {
        let g = grid(&case.points);
        for (n, run) in case.runs.iter().enumerate() {
            let o = &run.options;
            let inside = |i: usize, _: usize| -> DVec3 {
                let spec = o.inside.as_ref().expect("only called when set");
                DVec3::from_array(spec.point.unwrap_or_else(|| spec.per_row.as_ref().expect("point or per_row")[i]))
            };
            let options = GridOptions {
                wrap_i: o.wrap_i,
                wrap_j: o.wrap_j,
                flip: o.flip,
                invert: o.invert,
                inside: o.inside.is_some().then_some(&inside),
                ..GridOptions::default()
            };
            let got = grid_normals(&g, &options).unwrap();
            assert_eq!(got.flipped, run.flip, "{} run {n}: orientation", case.name);
            assert_eq!(got.normals.len(), run.normals.len());
            let worst = got
                .normals
                .iter()
                .zip(&run.normals)
                .map(|(a, b)| degrees_between(*a, DVec3::from_array(*b)))
                .fold(0.0, f64::max);
            assert!(worst < 0.01, "{} run {n}: worst normal is {worst} degrees off", case.name);
        }
    }
}

#[derive(Deserialize)]
struct Counts {
    vertices: usize,
    triangles: usize,
}

#[derive(Deserialize)]
struct BuilderGrids {
    vase: Points,
    strip: Points,
    quad: Points,
    patch: Points,
}

#[derive(Deserialize)]
struct BuilderFixture {
    grids: BuilderGrids,
    counts: Counts,
    positions: Vec<f32>,
    normals: Vec<f64>,
    uvs: Vec<f32>,
    colours: Vec<f32>,
    indices: Vec<u32>,
    weights: Vec<Vec<(String, f32)>>,
    mirrored_positions: Vec<f32>,
    mirrored_indices: Vec<u32>,
    mirrored_weights: Vec<Vec<(String, f32)>>,
}

const ROWS: usize = 12;
const COLS: usize = 24;
const CX: f64 = 0.3;

/// The builder the generator makes: the 12 x 24 vase, a flipped strip, a merged quad and a merged colourless patch.
fn build(f: &BuilderFixture) -> MeshBuilder {
    let hips = JointRef::HIPS;
    let arm = js_names::joint("upper_arm.L").unwrap();
    let hand = js_names::joint("hand.R").unwrap();
    let blend = move |_: DVec3, i: usize, _: usize| {
        let t = f64::from(u32::try_from(i).unwrap()) / 11.0;
        #[allow(clippy::cast_possible_truncation)] // weights are float32 in the mesh
        let w = |x: f64| x as f32;
        vec![(hips.clone(), w(1.0 - t)), (arm.clone(), w(t * 0.7)), (hand.clone(), w(t * 0.3))]
    };
    let vase = grid(&f.grids.vase);
    let ys: Vec<f64> = (0..ROWS).map(|i| vase.at(i, 0).y).collect();
    let inside = |i: usize, _: usize| DVec3::new(CX, ys[i], 0.0);
    let colour = |i: usize, j: usize, _: DVec3| {
        #[allow(clippy::cast_possible_truncation)] // colours are float32 in the mesh
        [(i as f64 / 11.0) as f32, (j as f64 / 23.0) as f32, 0.5, 1.0]
    };
    let mut mb = MeshBuilder::with_colours();
    mb.add_grid(
        &vase,
        &GridOptions {
            wrap_j: true,
            inside: Some(&inside),
            colour: Some(&colour),
            skin: Skin::Per(&blend),
            ..GridOptions::default()
        },
    )
    .unwrap();
    let spine = js_names::joint("spine").unwrap();
    mb.add_grid(&grid(&f.grids.strip), &GridOptions { flip: true, skin: Skin::Bone(spine), ..GridOptions::default() })
        .unwrap();
    let mut other = MeshBuilder::with_colours();
    let skin = Skin::Blend(vec![(js_names::joint("chest").unwrap(), 0.25), (js_names::joint("neck").unwrap(), 0.75)]);
    other.add_grid(&grid(&f.grids.quad), &GridOptions { skin, ..GridOptions::default() }).unwrap();
    mb.merge(&other).unwrap();
    let mut plain = MeshBuilder::new();
    let skin = Skin::Bone(js_names::joint("foot.L").unwrap());
    plain.add_grid(&grid(&f.grids.patch), &GridOptions { skin, ..GridOptions::default() }).unwrap();
    mb.merge(&plain).unwrap();
    assert_eq!((ROWS, COLS), (f.grids.vase.len(), f.grids.vase[0].len()));
    mb
}

/// A vertex's non-zero influences as joint and weight, sorted by joint for comparison.
fn influences(joints: [u16; 4], weights: [f32; 4], bones: &[JointRef]) -> Vec<(JointRef, f32)> {
    let mut v: Vec<(JointRef, f32)> = joints
        .iter()
        .zip(weights)
        .filter(|(_, w)| *w > 0.0)
        .map(|(j, w)| (bones[usize::from(*j)].clone(), w))
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

/// The prototype's `[bone name, weight]` lists as sorted joint/weight pairs, with zero weights dropped.
fn expected_influences(list: &[(String, f32)]) -> Vec<(JointRef, f32)> {
    let mut v: Vec<(JointRef, f32)> =
        list.iter().filter(|(_, w)| *w > 0.0).map(|(b, w)| (js_names::joint(b).unwrap(), *w)).collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

fn assert_weights_match(mb: &MeshBuilder, expected: &[Vec<(String, f32)>], what: &str) {
    let m = mb.mesh();
    assert_eq!(m.joints.len(), expected.len());
    for (v, list) in expected.iter().enumerate() {
        let got = influences(m.joints[v], m.weights[v], mb.bones());
        let want = expected_influences(list);
        assert_eq!(
            got.iter().map(|g| &g.0).collect::<Vec<_>>(),
            want.iter().map(|w| &w.0).collect::<Vec<_>>(),
            "{what}: vertex {v} joints"
        );
        for ((_, g), (_, w)) in got.iter().zip(&want) {
            assert!((g - w).abs() < 1e-5, "{what}: vertex {v} weight {g} vs {w}");
        }
    }
}

fn vec3s(flat: &[f32]) -> Vec<Vec3> {
    flat.as_chunks::<3>().0.iter().map(|&[x, y, z]| Vec3::new(x, y, z)).collect()
}

#[test]
fn a_mesh_builder_of_grids_matches_the_prototype_in_counts_indices_positions_and_attributes() {
    let f: BuilderFixture = serde_json::from_str(include_str!("fixtures/kit/mesh_builder.json")).unwrap();
    let mb = build(&f);
    let m = mb.mesh();
    assert_eq!((m.positions.len(), m.triangle_count()), (f.counts.vertices, f.counts.triangles));
    assert_eq!(
        (m.positions.len(), m.triangle_count()),
        (12 * 24 + 4 * 6 + 3 * 3 + 2 * 2, 2 * (11 * 23 + 3 * 5 + 2 * 2 + 1))
    );
    assert_eq!(m.indices, f.indices, "triangle indices");
    assert_eq!(m.positions, vec3s(&f.positions), "positions");
    for (v, (got, &[x, y, z])) in m.normals.iter().zip(f.normals.as_chunks::<3>().0).enumerate() {
        let want = DVec3::new(x, y, z);
        assert!(degrees_between(got.as_dvec3(), want) < 0.01, "normal {v}");
    }
    for (got, &[u, v]) in m.uvs.iter().zip(f.uvs.as_chunks::<2>().0) {
        assert!((*got - Vec2::new(u, v)).length() < 1e-6);
    }
    for (got, want) in m.colours.iter().zip(f.colours.as_chunks::<3>().0) {
        assert!((0..3).all(|k| (got[k] - want[k]).abs() < 1e-6), "{got:?} vs {want:?}");
        assert_eq!(got[3], 1.0, "opaque");
    }
    assert_eq!(m.colours.len(), m.positions.len(), "the colourless patch is filled with white");
    assert_weights_match(&mb, &f.weights, "weights");
    m.validate().unwrap();
}

#[test]
fn the_mirrored_builder_matches_the_prototypes_positions_winding_and_swapped_sides() {
    let f: BuilderFixture = serde_json::from_str(include_str!("fixtures/kit/mesh_builder.json")).unwrap();
    let mirror = build(&f).mirrored();
    let m = mirror.mesh();
    assert_eq!(m.positions, vec3s(&f.mirrored_positions));
    assert_eq!(m.indices, f.mirrored_indices);
    assert_weights_match(&mirror, &f.mirrored_weights, "mirrored weights");
    let original = build(&f);
    for (a, b) in original.mesh().normals.iter().zip(&m.normals) {
        assert_eq!(*b, Vec3::new(-a.x, a.y, a.z));
    }
    m.validate().unwrap();
}
