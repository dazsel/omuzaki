//! Mesh comparison on synthetic meshes with answers worked out by hand, plus one `#[ignore]` run on the real JS dump.
//!
//! The hand-computed cases use a flat 10 x 10 vertex plane with 0.05 m spacing in the XY plane (normals +Z). Vertex
//! `(i, j)` is index `10 * j + i`, at `(0.05 i, 0.05 j, 0)`. Spacing is large against every shift used, so each
//! vertex's nearest neighbour in the other mesh is its own counterpart.

// Synthetic meshes are indexed and scaled with small counts, where the casts are exact.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::time::Instant;

use omuzaki_core::glam::Vec3;
use omuzaki_core::mesh::MeshData;
use omuzaki_test::jsref::JsRef;
use omuzaki_test::meshcmp::{
    Finding, MeshReport, MeshTolerance, Metric, Side, Verdict, WORST_LIMIT, compare_meshes, write_error_obj,
};

const SPACING: f32 = 0.05;
const SIDE: usize = 10;
const T: MeshTolerance = MeshTolerance::T_GEOM;
/// f32 rounding of coordinates up to 0.5 is about 3e-8; a shift of 1e-4 is exact to well within this.
const EPS: f32 = 1e-6;

fn plane() -> MeshData {
    let mut mesh = MeshData::default();
    for j in 0..SIDE {
        for i in 0..SIDE {
            mesh.positions.push(Vec3::new(i as f32 * SPACING, j as f32 * SPACING, 0.0));
            mesh.normals.push(Vec3::Z);
        }
    }
    for j in 0..SIDE - 1 {
        for i in 0..SIDE - 1 {
            let a = (j * SIDE + i) as u32;
            let (b, c, d) = (a + 1, a + SIDE as u32, a + SIDE as u32 + 1);
            mesh.indices.extend([a, b, d, a, d, c]);
        }
    }
    mesh
}

fn moved(mesh: &MeshData, by: Vec3) -> MeshData {
    let mut out = mesh.clone();
    out.positions.iter_mut().for_each(|p| *p += by);
    out
}

/// The same mesh with its vertices in reverse order (indices follow), so every vertex is where it was, under another
/// index.
fn reversed(mesh: &MeshData) -> MeshData {
    let last = mesh.positions.len() as u32 - 1;
    let mut out = mesh.clone();
    out.positions.reverse();
    out.normals.reverse();
    out.indices.iter_mut().for_each(|i| *i = last - *i);
    out
}

fn assert_near(value: f32, expected: f32, what: &str) {
    assert!((value - expected).abs() <= EPS, "{what}: {value} vs {expected}");
}

fn in_order(report: &MeshReport) -> &omuzaki_test::meshcmp::InOrderStats {
    report.in_order.as_ref().expect("same vertex counts")
}

fn unordered(report: &MeshReport) -> &omuzaki_test::meshcmp::UnorderedStats {
    report.unordered.as_ref().expect("valid meshes")
}

#[test]
fn identical_meshes_match_exactly() {
    let report = compare_meshes(&plane(), &plane(), T);
    assert_eq!(report.verdict, Verdict::InOrder);
    assert!(report.passed() && report.findings.is_empty(), "{report}");
    let (order, free) = (in_order(&report), unordered(&report));
    assert_eq!((order.max_error, order.mean_error, order.max_normal_angle_deg), (0.0, 0.0, 0.0));
    assert_eq!((free.chamfer, free.hausdorff), (0.0, 0.0));
    assert!(order.worst_positions.is_empty() && order.worst_normals.is_empty());
    assert_eq!(report.bbox_delta.unwrap().max_abs(), 0.0);
    assert_eq!((report.vertices.actual, report.triangles.expected), (100, 162));
}

#[test]
fn a_uniform_shift_of_1e_4_is_1e_4_everywhere() {
    let shifted = moved(&plane(), Vec3::new(1e-4, 0.0, 0.0));
    let report = compare_meshes(&shifted, &plane(), T);
    // Every vertex is 1e-4 from its twin and nothing else is nearer, so all measures are 1e-4.
    assert_eq!(report.verdict, Verdict::InOrder, "{report}");
    let (order, free) = (in_order(&report), unordered(&report));
    assert_near(order.max_error, 1e-4, "max");
    assert_near(order.mean_error, 1e-4, "mean");
    assert_near(free.chamfer, 1e-4, "Chamfer");
    assert_near(free.hausdorff, 1e-4, "Hausdorff");
    assert_near(report.bbox_delta.unwrap().max_abs(), 1e-4, "bbox");
    assert_eq!(order.worst_positions.len(), WORST_LIMIT);
}

#[test]
fn a_shift_past_every_limit_fails_and_names_what_it_broke() {
    let report = compare_meshes(&moved(&plane(), Vec3::new(5e-4, 0.0, 0.0)), &plane(), T);
    assert!(!report.passed());
    // 5e-4 breaks the vertex limit (2e-4) and the Chamfer limit (3e-4), but not Hausdorff (1.5e-3).
    let metrics: Vec<Metric> = report
        .findings
        .iter()
        .map(|f| match f {
            Finding::Exceeded { metric, .. } => *metric,
            other => panic!("unexpected {other}"),
        })
        .collect();
    assert_eq!(metrics, [Metric::VertexError, Metric::Chamfer]);
}

#[test]
fn one_vertex_moved_1e_2_is_one_hundredth_of_a_centimetre_chamfer_but_a_full_centimetre_hausdorff() {
    let mut bent = plane();
    bent.positions[44].z = 1e-2;
    let report = compare_meshes(&bent, &plane(), T);
    let (order, free) = (in_order(&report), unordered(&report));
    assert_near(order.max_error, 1e-2, "max");
    assert_near(order.mean_error, 1e-2 / 100.0, "mean");
    assert_eq!(order.worst_positions.len(), 1);
    assert_eq!(order.worst_positions[0].index, 44);
    // Moved vertex and its twin are each other's nearest neighbour at 1e-2; the other 99 + 99 have a zero-distance twin.
    assert_near(free.chamfer, 1e-2 / 100.0, "Chamfer");
    assert_near(free.hausdorff, 1e-2, "Hausdorff");
    // The vertex limit and the Hausdorff limit break; Chamfer (1e-4) is inside its 3e-4.
    assert_eq!(report.verdict, Verdict::Failed);
    let broken: Vec<String> = report.findings.iter().map(ToString::to_string).collect();
    assert_eq!(broken.len(), 2, "{report}");
    assert!(broken[0].starts_with("vertex error 1.000e-2 m exceeds 2.000e-4 m"), "{broken:?}");
    assert!(broken[1].starts_with("Hausdorff 1.000e-2 m exceeds 1.500e-3 m"), "{broken:?}");
}

#[test]
fn a_reordered_mesh_passes_unordered_with_zero_chamfer() {
    let report = compare_meshes(&reversed(&plane()), &plane(), T);
    assert_eq!(report.verdict, Verdict::Unordered, "{report}");
    assert!(report.passed() && report.findings.is_empty());
    assert_eq!((unordered(&report).chamfer, unordered(&report).hausdorff), (0.0, 0.0));
    // Vertex 0 (0, 0) faces vertex 99 (0.45, 0.45): the plane's diagonal, 0.45 * sqrt(2).
    assert_near(in_order(&report).max_error, 0.45 * 2.0_f32.sqrt(), "in-order max");
}

#[test]
fn an_extra_vertex_is_a_finding_but_the_shape_still_matches() {
    let mut extra = plane();
    extra.positions.push(extra.positions[7]); // a duplicate, e.g. a seam the port splits and the prototype welds
    extra.normals.push(Vec3::Z);
    let report = compare_meshes(&extra, &plane(), T);
    assert_eq!(report.verdict, Verdict::Unordered);
    assert!(report.in_order.is_none());
    assert_eq!(report.findings, [Finding::VertexCount { actual: 101, expected: 100 }]);
    assert_eq!(report.triangles.actual, report.triangles.expected);
}

#[test]
fn a_missing_corner_fails_by_one_edge_length() {
    let mut cut = plane();
    cut.positions.pop();
    cut.normals.pop();
    let corner = cut.positions.len() as u32; // index 99 is gone, with the two triangles that used it
    let kept: Vec<u32> = cut.indices.chunks(3).filter(|t| !t.contains(&corner)).flatten().copied().collect();
    cut.indices = kept;
    let report = compare_meshes(&cut, &plane(), T);
    assert_eq!(report.verdict, Verdict::Failed);
    // The expected corner (0.45, 0.45) has its nearest actual vertex one spacing away.
    assert_near(unordered(&report).hausdorff, SPACING, "Hausdorff");
    assert!(report.findings.contains(&Finding::VertexCount { actual: 99, expected: 100 }));
    assert!(report.findings.contains(&Finding::TriangleCount { actual: 160, expected: 162 }));
}

/// Tilts every normal about the X axis by `degrees`.
fn tilted(mesh: &MeshData, degrees: f32) -> MeshData {
    let (sin, cos) = degrees.to_radians().sin_cos();
    let mut out = mesh.clone();
    out.normals.iter_mut().for_each(|n| *n = Vec3::new(0.0, -sin, cos));
    out
}

#[test]
fn normals_are_judged_in_degrees() {
    let half = compare_meshes(&tilted(&plane(), 0.5), &plane(), T);
    assert_eq!(half.verdict, Verdict::InOrder, "{half}");
    assert!((in_order(&half).max_normal_angle_deg - 0.5).abs() < 1e-3);

    let two = compare_meshes(&tilted(&plane(), 2.0), &plane(), T);
    // The positions are identical in order, so the bad normals cannot fall through to the unordered rule.
    assert_eq!(two.verdict, Verdict::Failed);
    assert_eq!((unordered(&two).chamfer, unordered(&two).hausdorff), (0.0, 0.0));
    let [Finding::Exceeded { metric: Metric::NormalAngle, value, limit }] = two.findings[..] else {
        panic!("{two}");
    };
    assert!((value - 2.0).abs() < 1e-3 && (limit - 1.0).abs() < f32::EPSILON);
}

#[test]
fn a_tiny_normal_angle_is_not_lost_to_rounding() {
    // acos(dot) of unit vectors 0.01 degrees apart is off by a few hundredths of a degree in f32; atan2(|cross|, dot) is not.
    let report = compare_meshes(&tilted(&plane(), 0.01), &plane(), T);
    assert!((in_order(&report).max_normal_angle_deg - 0.01).abs() < 1e-4, "{report}");
}

#[test]
fn a_zero_normal_against_a_real_one_is_a_total_mismatch() {
    let mut flat = plane();
    flat.normals[3] = Vec3::ZERO;
    let report = compare_meshes(&flat, &plane(), T);
    assert_eq!(in_order(&report).max_normal_angle_deg, 180.0);
    assert_eq!(report.verdict, Verdict::Failed);
}

#[test]
fn invalid_meshes_fail_without_numbers() {
    let mut broken = plane();
    broken.positions[5].x = f32::NAN;
    let report = compare_meshes(&broken, &MeshData::default(), T);
    assert_eq!(report.verdict, Verdict::Failed);
    assert!(report.in_order.is_none() && report.unordered.is_none() && report.bbox_delta.is_none());
    let sides: Vec<Side> = report
        .findings
        .iter()
        .map(|f| if let Finding::Invalid { side, .. } = f { *side } else { panic!("{f}") })
        .collect();
    assert_eq!(sides, [Side::Actual, Side::Expected]);
}

#[test]
fn a_far_away_mesh_is_found_ten_metres_off() {
    // Both meshes are 10 m apart along X. For each actual vertex at x the nearest expected vertex is the one at the
    // expected plane's edge facing it: 10 - x away; for each expected vertex at 10 + x: 10 + x - 0.45. Means over
    // x = 0, 0.05 ... 0.45 (mean 0.225): 9.775 both ways. Worst: 10 (actual x = 0 and expected x = 0.45).
    let report = compare_meshes(&moved(&plane(), Vec3::new(10.0, 0.0, 0.0)), &plane(), T);
    assert!((unordered(&report).chamfer - 9.775).abs() < 1e-4, "{report}");
    assert!((unordered(&report).hausdorff - 10.0).abs() < 1e-4, "{report}");
    assert_eq!(report.verdict, Verdict::Failed);
}

#[test]
fn single_point_meshes_have_distance_equal_to_their_gap() {
    // Chamfer and Hausdorff of two one-vertex meshes 0.01 apart are both 0.01 (all vertices identical: a zero-size grid).
    let point = |x: f32| MeshData {
        positions: vec![Vec3::new(x, 0.0, 0.0); 3],
        normals: vec![Vec3::Z; 3],
        indices: vec![0, 1, 2],
        ..MeshData::default()
    };
    let report = compare_meshes(&point(0.01), &point(0.0), T);
    assert_near(unordered(&report).chamfer, 0.01, "Chamfer");
    assert_near(unordered(&report).hausdorff, 0.01, "Hausdorff");
}

/// A small deterministic generator (an LCG), so "random" meshes are the same on every machine.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 40) as f32) / (1u64 << 24) as f32
    }
}

/// `n` vertices scattered in a `size`-metre box (some meshes squashed flat), with a triangle strip through them.
fn scatter(n: usize, size: Vec3, seed: u64) -> MeshData {
    let mut rng = Lcg(seed);
    let positions = (0..n).map(|_| Vec3::new(rng.next(), rng.next(), rng.next()) * size).collect();
    let indices = (0..n as u32 - 2).flat_map(|i| [i, i + 1, i + 2]).collect();
    MeshData { positions, normals: vec![Vec3::Y; n], indices, ..MeshData::default() }
}

fn nearest_by_brute_force(p: Vec3, set: &[Vec3]) -> f32 {
    set.iter().map(|q| p.distance(*q)).fold(f32::INFINITY, f32::min)
}

#[test]
fn the_grid_finds_the_same_nearest_vertices_as_checking_every_pair() {
    // Box shapes that stress the grid: a cube, a long thin rod, a flat sheet, and one mesh far outside the other's box.
    let shapes = [Vec3::ONE, Vec3::new(2.0, 0.01, 0.01), Vec3::new(1.0, 1.0, 0.0), Vec3::new(0.0, 0.0, 0.0)];
    for (seed, size) in (1_u64..).zip(shapes) {
        for offset in [Vec3::ZERO, Vec3::new(0.3, -0.2, 0.1), Vec3::new(5.0, 3.0, -4.0)] {
            let (a, b) = (scatter(400, size, seed), moved(&scatter(300, size, seed + 100), offset));
            let report = compare_meshes(&a, &b, T);
            let free = unordered(&report);
            for (i, p) in a.positions.iter().enumerate() {
                let brute = nearest_by_brute_force(*p, &b.positions);
                assert!((free.actual_to_expected[i] - brute).abs() < 1e-5, "{size} {offset} a[{i}]");
            }
            for (i, p) in b.positions.iter().enumerate() {
                let brute = nearest_by_brute_force(*p, &a.positions);
                assert!((free.expected_to_actual[i] - brute).abs() < 1e-5, "{size} {offset} b[{i}]");
            }
        }
    }
}

#[test]
fn worst_vertices_are_listed_worst_first_and_capped() {
    let mut bent = plane();
    for k in 0..15 {
        bent.positions[k * 6].z = 1e-3 * (k + 1) as f32; // vertex 6k is off by 1e-3 (k + 1)
    }
    let report = compare_meshes(&bent, &plane(), T);
    let worst = &in_order(&report).worst_positions;
    assert_eq!(worst.len(), WORST_LIMIT);
    assert_eq!(worst[0].index, 14 * 6);
    assert_eq!(worst.iter().map(|w| w.index).collect::<Vec<_>>(), (5..15).rev().map(|k| k * 6).collect::<Vec<_>>());
    assert!(worst.windows(2).all(|w| w[0].value > w[1].value));
}

#[test]
fn reports_are_deterministic_and_debug_prints_stay_short() {
    let bent = moved(&plane(), Vec3::new(1e-4, 2e-4, 0.0));
    let first = compare_meshes(&bent, &plane(), T);
    assert_eq!(compare_meshes(&bent, &plane(), T), first);
    assert_eq!(first.to_string(), compare_meshes(&bent, &plane(), T).to_string());
    assert!(format!("{first:?}").contains("VertexErrors(100 values, max 2.236e-4)"), "{first:?}");
}

#[test]
fn display_names_the_verdict_the_counts_and_the_worst_vertex() {
    let mut bent = plane();
    bent.positions[44].z = 1e-2;
    let text = compare_meshes(&bent, &plane(), T).to_string();
    assert!(text.starts_with("FAIL: 100 vs 100 vertices, 162 vs 162 triangles\n"), "{text}");
    assert!(text.contains("worst vertex 44 (1.000e-2 m)"), "{text}");
    assert!(text.contains("finding: Hausdorff 1.000e-2 m exceeds 1.500e-3 m"), "{text}");
}

fn obj_for(actual: &MeshData, expected: &MeshData) -> Vec<String> {
    let report = compare_meshes(actual, expected, T);
    let mut bytes = Vec::new();
    write_error_obj(&mut bytes, actual, expected, &report).unwrap();
    String::from_utf8(bytes).unwrap().lines().map(str::to_owned).collect()
}

#[test]
fn the_obj_has_both_meshes_coloured_by_error() {
    let mut bent = plane();
    bent.positions[44].z = 1e-2; // 1e-2 against a vertex limit of 2e-4 is far past "twice the tolerance"
    let lines = obj_for(&bent, &plane());
    let objects: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| l.starts_with("o ")).map(|(i, _)| i).collect();
    assert_eq!(lines[objects[0]], "o actual");
    assert_eq!(lines[objects[1]], "o expected");
    // Coloured by nearest-vertex distance (this comparison failed): scale is twice Hausdorff's 1.5e-3, so 1e-2 is red
    // and a vertex with a zero-distance twin is blue.
    assert_eq!(lines[objects[0] + 1], "v 0 0 0 0.000 0.000 1.000");
    let moved_vertex = lines.iter().find(|l| l.starts_with("v 0.2 0.2 0.01 ")).expect("the moved vertex");
    assert!(moved_vertex.ends_with(" 1.000 0.000 0.000"), "{moved_vertex}");
    // The second object's faces are numbered after the first's 100 vertices.
    assert_eq!(lines.iter().filter(|l| l.starts_with("v ")).count(), 200);
    assert_eq!(lines.iter().filter(|l| l.starts_with("f ")).count(), 324);
    assert_eq!(lines[objects[1] + 101], "f 101 102 112");
    assert_eq!(lines[objects[0] + 101], "f 1 2 12");
}

#[test]
fn a_matching_mesh_is_colour_coded_by_its_vertex_error() {
    // 1e-4 against the vertex limit 2e-4: error / (2 x limit) = 0.25, a blue-green halfway between "none" and "at the limit".
    let lines = obj_for(&moved(&plane(), Vec3::new(1e-4, 0.0, 0.0)), &plane());
    assert!(lines[2].ends_with(" 0.000 0.500 0.500"), "{}", lines[2]);
}

#[test]
fn the_obj_writer_refuses_a_report_without_measures() {
    let report = compare_meshes(&MeshData::default(), &plane(), T);
    let err = write_error_obj(&mut Vec::new(), &MeshData::default(), &plane(), &report).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

/// A lat-long sphere of about `target` vertices: a surface, like a head, not a filled volume.
fn sphere(target: usize, radius: f32) -> MeshData {
    let rings = (target as f32).sqrt() as usize;
    let (cols, rows) = (rings, rings + 1);
    let mut mesh = MeshData::default();
    for r in 0..rows {
        let theta = std::f32::consts::PI * r as f32 / (rows - 1) as f32;
        for c in 0..cols {
            let phi = std::f32::consts::TAU * c as f32 / cols as f32;
            let n = Vec3::new(theta.sin() * phi.cos(), theta.cos(), theta.sin() * phi.sin());
            mesh.positions.push(n * radius);
            mesh.normals.push(n);
        }
    }
    for r in 0..rows - 1 {
        for c in 0..cols {
            let a = (r * cols + c) as u32;
            let b = (r * cols + (c + 1) % cols) as u32;
            let (c2, d) = (a + cols as u32, b + cols as u32);
            mesh.indices.extend([a, b, d, a, d, c2]);
        }
    }
    mesh
}

/// `expected` and a copy of it with every vertex nudged by a deterministic amount below the vertex limit.
fn wobbled(expected: MeshData) -> (MeshData, MeshData) {
    let mut actual = expected.clone();
    let mut rng = Lcg(7);
    let nudge = |rng: &mut Lcg| (rng.next() - 0.5) * 1e-4;
    actual.positions.iter_mut().for_each(|p| *p += Vec3::new(nudge(&mut rng), nudge(&mut rng), nudge(&mut rng)));
    (actual, expected)
}

#[test]
fn a_hundred_thousand_vertices_compare_in_under_a_second() {
    // A surface (a head), a filled volume and a flat sheet: the three point distributions a grid can choke on.
    let shapes = [
        ("sphere surface", sphere(100_000, 0.5)),
        ("filled cube", scatter(100_000, Vec3::ONE, 3)),
        ("flat sheet", scatter(100_000, Vec3::new(1.0, 1.0, 0.0), 4)),
    ];
    // Release builds take about 0.1 s; the unoptimised build `cargo test` makes is allowed ten times the 1 s budget.
    let limit = if cfg!(debug_assertions) { 10.0 } else { 1.0 };
    for (name, expected) in shapes {
        assert!(expected.positions.len() >= 99_000, "{name}");
        let (actual, expected) = wobbled(expected);
        let start = Instant::now();
        let report = compare_meshes(&actual, &expected, T);
        let took = start.elapsed();
        println!("100k-vertex compare, {name}: {} vertices, {took:?}", expected.positions.len());
        assert!(unordered(&report).hausdorff < 2e-4, "{name}: {report}");
        assert!(took.as_secs_f64() < limit, "{name}: {took:?}");
    }
}

/// Reads the real dump in a local assets folder and compares what can be compared. Run by hand:
/// `cargo test --release -p omuzaki-test --test meshcmp -- --ignored --nocapture`.
/// The sample character is local-only: the numbers go to the PR, the OBJ (to a local assets folder) stays there.
#[test]
#[ignore = "reads the sample character's local JS dump from a local assets folder; run by hand"]
fn the_real_js_dump_loads_and_the_traced_head_differs_from_the_analytic_one() {
    let Some(js) = JsRef::open() else { return };
    let start = Instant::now();
    let game = js.geometry("game").unwrap();
    println!("loaded geometry/game: {} meshes in {:?}", game.meshes.len(), start.elapsed());
    for set in ["close-up", "game-trace0", "close-up-trace0"] {
        let dump = js.geometry(set).unwrap();
        println!("loaded geometry/{set}: {} meshes", dump.meshes.len());
    }
    let total: usize = game.meshes.iter().map(|m| m.mesh.positions.len()).sum();
    println!("game: {total} vertices in {} meshes; skeleton of {} bones", game.meshes.len(), game.bones.len());
    for m in &game.meshes {
        // Every position blob matched its index hash while loading; the bbox in the index must match the numbers too.
        let (lo, hi) = m
            .mesh
            .positions
            .iter()
            .fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
        assert!((lo - m.bbox.0).abs().max_element() < 1e-5 && (hi - m.bbox.1).abs().max_element() < 1e-5, "{}", m.name);
    }

    let analytic = js.geometry("game-trace0").unwrap();
    for name in ["head", "ear.L", "ear.R", "neck"] {
        let (traced, flat) = (&game.mesh(name).unwrap().mesh, &analytic.mesh(name).unwrap().mesh);
        let report = compare_meshes(flat, traced, T);
        println!("{name}: analytic vs traced head\n{report}");
        if name == "head" {
            let dir = omuzaki_core::assets::assets_root().join("goldens-out/mesh-compare");
            std::fs::create_dir_all(&dir).unwrap();
            let mut out =
                std::io::BufWriter::new(std::fs::File::create(dir.join("head-analytic-vs-traced.obj")).unwrap());
            write_error_obj(&mut out, flat, traced, &report).unwrap();
        }
    }
    // The same mesh against itself is exactly zero, and against its close-up twin: the body is unchanged, the head moved.
    let close = js.geometry("close-up").unwrap();
    for name in ["tights", "head"] {
        let report = compare_meshes(&close.mesh(name).unwrap().mesh, &game.mesh(name).unwrap().mesh, T);
        println!("{name}: close-up vs game\n{report}");
    }

    let hair = js.hair("game").unwrap();
    let simulated = hair.clumps.iter().filter(|c| c.particles.is_some()).count();
    println!(
        "hair: {} clumps ({simulated} simulated), {} motion steps, {} bones",
        hair.clumps.len(),
        hair.motion.len(),
        hair.bones_settled.len()
    );
    let faces = js.face_states("game").unwrap();
    println!("face: {} cases, e.g. {:?}", faces.cases.len(), faces.cases[0].id);
}
