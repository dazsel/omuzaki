//! The curtain's strip geometry on synthetic guides and on the generator's own: vertex counts and order, the hand-worked
//! column positions, the winding, the inward-facing share of each fold treatment (#841), and the fold sheet (rendered when
//! asked). Everything is synthetic.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]
#![allow(clippy::needless_range_loop, clippy::manual_midpoint)] // indices mirror the strip's (guide, column, point) layout
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::chunks_exact_to_as_chunks)]

use omuzaki_char::hair::compile::{Forked, GroupGeometry, HairGenCtx};
use omuzaki_char::hair::r#gen::curtain::{
    self, Folds, GUIDE_POINTS, MIN_GAP, Spacing, StripLook, strip_mesh, strip_normals, strip_positions, vertex_count,
};
use omuzaki_char::hair::strands::StrandMap;
use omuzaki_char::hair::tube::OutwardFrame;
use omuzaki_char_api::{CurtainGen, Dynamics, GroupId, HAIR_INFO, HAIR_UV, HairClump, HeadSurface, Quality, Wave};
use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_core::rng::Rng;
use omuzaki_face_api::Palette;

/// An ellipsoid head: centre `(0, 1.6, 0)`, semi-axes `(0.08, 0.11, 0.095)`.
#[derive(Debug)]
struct Egg;

impl HeadSurface for Egg {
    fn centre(&self) -> Vec3 {
        Vec3::new(0.0, 1.6, 0.0)
    }
    fn radius(&self, dir: Vec3) -> f32 {
        1.0 / (dir / Vec3::new(0.08, 0.11, 0.095)).length()
    }
}

/// The outward frame the part uses: the head, and a body axis straight below it.
fn frame() -> OutwardFrame {
    OutwardFrame { head: Egg.centre(), body_axis: Vec2::new(0.0, -0.03) }
}

fn curtain(count: u16, columns: u8) -> CurtainGen {
    CurtainGen {
        count,
        columns,
        envelope: vec![[1.6, 0.11], [1.4, 0.17], [1.2, 0.22], [0.9, 0.26], [0.5, 0.3]],
        root_tuck_deg: 0.0, // the prototype's own guides; the tuck has its own tests
        ..CurtainGen::default()
    }
}

fn compile(g: &CurtainGen, seed: u64, folds: Folds) -> (Vec<HairClump>, Vec<f32>) {
    let (palette, id, dynamics) = (Palette::default(), GroupId::new("curtain").unwrap(), Dynamics::default());
    let mut forked = Rng::new(seed);
    let mut stream = Forked(&mut forked);
    let mut ctx = HairGenCtx::new(&Egg, &palette, Quality::Live, &id, &dynamics, &mut stream);
    let group = curtain::compile_folds(g, &mut ctx, folds);
    let GroupGeometry::Strip { u, .. } = group.geometry else { panic!("the curtain is a strip") };
    (group.clumps, u)
}

fn mesh_of(g: &CurtainGen, seed: u64, folds: Folds) -> MeshData {
    let (clumps, u) = compile(g, seed, folds);
    curtain::group_mesh(&clumps, &u, g.columns, &frame(), None, &StrandMap::default()).unwrap()
}

/// Straight parallel guides 2 cm apart along x, hanging from y = 0 to -0.5 in the plane z = 0, outward +Z.
fn flat_guides(n: usize) -> Vec<Vec<Vec3>> {
    (0..n)
        .map(|j| (0..GUIDE_POINTS).map(|k| Vec3::new(0.02 * j as f32, -0.5 * k as f32 / 25.0, 0.0)).collect())
        .collect()
}

fn flat_mesh(guides: &[Vec<Vec3>], columns: u8, spacing: Spacing) -> MeshData {
    let refs: Vec<&[Vec3]> = guides.iter().map(Vec::as_slice).collect();
    let looks = vec![StripLook { shade: 1.0, colour: [1.0; 4] }; guides.len()];
    strip_mesh(&refs, &looks, columns, spacing, &StrandMap::default(), Vec3::new(0.0, 0.1, -0.5), |_| Vec3::Z, |p| p)
        .unwrap()
}

fn smoothstep(x: f32, lo: f32, hi: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[test]
fn columns_one_to_nine_all_build_with_columns_times_points_vertices_per_strip() {
    for columns in 1..=9u8 {
        let g = curtain(12, columns);
        let mesh = mesh_of(&g, 1, Folds::Prototype);
        let per_strip = usize::from(columns) * GUIDE_POINTS;
        assert_eq!(mesh.positions.len(), 12 * per_strip, "columns {columns}");
        assert_eq!(mesh.normals.len(), mesh.positions.len());
        assert_eq!(mesh.colours.len(), mesh.positions.len());
        assert_eq!(mesh.uvs.len(), mesh.positions.len());
        assert_eq!(mesh.indices.len(), 12 * (usize::from(columns) - 1) * (GUIDE_POINTS - 1) * 6);
        assert_eq!(vertex_count(12, GUIDE_POINTS, columns), mesh.positions.len());
        // No strip shares a vertex with another: every triangle's three vertices lie in one strip.
        for tri in mesh.indices.chunks(3) {
            let strips: Vec<u32> = tri.iter().map(|i| *i / per_strip as u32).collect();
            assert!(strips.iter().all(|s| *s == strips[0]), "columns {columns}: triangle {tri:?} spans strips");
        }
        // ... and every vertex of a strip with width is used by its own strip only.
        if columns > 1 {
            let mut used = vec![false; mesh.positions.len()];
            mesh.indices.iter().for_each(|i| used[*i as usize] = true);
            assert!(used.iter().all(|u| *u), "columns {columns}: a vertex is unused");
        }
    }
}

#[test]
fn the_five_prototype_columns_sit_where_the_construction_puts_them() {
    let guides = flat_guides(4);
    let mesh = flat_mesh(&guides, 5, Spacing::Prototype);
    let vertex = |j: usize, c: usize, k: usize| mesh.positions[(j * 5 + c) * GUIDE_POINTS + k];
    for j in 0..4 {
        for (c, a) in [-1.0_f32, -0.5, 0.0, 0.5, 1.0].into_iter().enumerate() {
            let sc = 1.0 - 0.15 * a.abs().powf(1.5);
            for k in 0..GUIDE_POINTS {
                let t = k as f32 / 25.0 * sc;
                // The neighbour is 2 cm away on the side `a` points to; the missing one is mirrored.
                let want = Vec3::new(
                    0.02 * j as f32 + a * 0.5 * 0.02,
                    -0.5 * t.min(0.9999),
                    0.0015 * (1.0 - a * a) * smoothstep(t, 0.04, 0.25),
                );
                let got = vertex(j, c, k);
                assert!((got - want).abs().max_element() < 2e-5, "guide {j} column {c} point {k}: {got} want {want}");
            }
        }
    }
    // The outer columns of neighbours meet at the same line (halfway between them) but are separate vertices.
    assert!((vertex(0, 4, 10) - vertex(1, 0, 10)).length() < 1e-5);
    // The outer columns end 15 % short, the half columns 5.3 % short (the prototype shortens every column but the centre).
    // (The last point reads the guide at 0.9999 of its length, the prototype's sample clamp.)
    assert!((vertex(0, 4, 25).y - -0.5 * 0.85).abs() < 1e-4);
    assert!((vertex(0, 3, 25).y - -0.5 * (1.0 - 0.15 * 0.5_f32.powf(1.5))).abs() < 1e-4);
    assert!((vertex(0, 2, 25).y - -0.5).abs() < 1e-4);
}

#[test]
fn hair_uv_and_info_carry_the_column_the_position_and_a_seed_per_strip() {
    let mesh = mesh_of(&curtain(6, 5), 2, Folds::Prototype);
    let uv = &mesh.extra_attribute(HAIR_UV).unwrap().data;
    let info = &mesh.extra_attribute(HAIR_INFO).unwrap().data;
    assert_eq!((uv.len(), info.len()), (2 * mesh.positions.len(), 3 * mesh.positions.len()));
    for (v, a) in [(0, -1.0), (GUIDE_POINTS, -0.5), (2 * GUIDE_POINTS + 7, 0.0), (4 * GUIDE_POINTS, 1.0)] {
        assert_eq!(uv[2 * v], a, "vertex {v}");
    }
    assert_eq!(uv[2 * 7 + 1], 7.0 / 25.0);
    assert_eq!(uv[2 * (GUIDE_POINTS - 1) + 1], 1.0, "the last point of a column is exactly 1");
    let per_strip = 5 * GUIDE_POINTS;
    for j in 0..6 {
        let want_seed = (j as f32 * 0.371 + 0.13).fract();
        let want_shade = 0.97 + 0.03 * (j as f32 * 2.1).sin();
        for v in [j * per_strip, j * per_strip + per_strip - 1] {
            assert!((info[3 * v] - want_seed).abs() < 1e-6 && (info[3 * v + 1] - want_shade).abs() < 1e-6);
            assert!((0.0..=std::f32::consts::PI).contains(&info[3 * v + 2]));
        }
    }
}

/// The geometric normal of every triangle, with its outward direction at the centre.
fn faces(mesh: &MeshData, frame: &OutwardFrame) -> Vec<(Vec3, Vec3)> {
    mesh.indices
        .chunks(3)
        .map(|t| {
            let [a, b, c] = [t[0], t[1], t[2]].map(|i| mesh.positions[i as usize]);
            ((b - a).cross(c - a), frame.rest((a + b + c) / 3.0))
        })
        .collect()
}

/// The share of non-degenerate triangles whose normal is more than 90 degrees from the outward direction.
fn inward_share(mesh: &MeshData, frame: &OutwardFrame) -> f32 {
    let faces: Vec<_> = faces(mesh, frame).into_iter().filter(|(n, _)| n.length_squared() > 1e-18).collect();
    faces.iter().filter(|(n, out)| n.dot(*out) < 0.0).count() as f32 / faces.len() as f32
}

#[test]
fn a_flat_curtain_faces_outward_whichever_way_the_guides_run() {
    let mut guides = flat_guides(5);
    for reversed in [false, true] {
        if reversed {
            guides.reverse();
        }
        let mesh = flat_mesh(&guides, 5, Spacing::Prototype);
        // Reversing the guides turns the sheet's across axis round, so the winding must flip to keep facing +Z.
        assert!(
            mesh.indices.chunks(3).all(|t| {
                let [a, b, c] = [t[0], t[1], t[2]].map(|i| mesh.positions[i as usize]);
                (b - a).cross(c - a).z > 0.0
            }),
            "reversed {reversed}: a triangle faces away"
        );
        assert!(mesh.normals.iter().all(|n| n.z > 0.5), "reversed {reversed}: vertex normals follow the faces");
    }
}

#[test]
fn vertex_normals_are_the_area_weighted_face_normals() {
    let positions = [Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::new(1.0, 1.0, 0.0)];
    let normals = strip_normals(&positions, &[0, 1, 2, 1, 3, 2]);
    assert!(normals.iter().all(|n| (*n - Vec3::Z).length() < 1e-6));
    assert_eq!(strip_normals(&positions, &[0, 1, 2])[3], Vec3::Z, "a vertex of no triangle gets +Z");
}

#[test]
fn with_wave_and_curl_off_no_treatment_folds() {
    for folds in [Folds::Prototype, Folds::Coherent] {
        let mut g = curtain(44, 5);
        g.wave = Wave { amplitude: 0.0, curl: 0.0, ..g.wave };
        for seed in 1..=6 {
            let mesh = mesh_of(&g, seed, folds);
            let share = inward_share(&mesh, &frame());
            assert_eq!(share, 0.0, "{folds:?} seed {seed}");
        }
    }
}

/// The inward share of `folds` over six seeds, on the default curtain (44 guides, 5 columns).
fn shares(folds: Folds) -> Vec<f32> {
    (1..=6).map(|seed| inward_share(&mesh_of(&curtain(44, 5), seed, folds), &frame())).collect()
}

#[test]
fn the_prototypes_folds_exist_and_the_coherent_treatment_has_fewer() {
    let (prototype, coherent) = (shares(Folds::Prototype), shares(Folds::Coherent));
    eprintln!("inward-facing share over six seeds: prototype {prototype:.4?}, coherent {coherent:.4?}");
    let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
    assert!(mean(&prototype) > 0.02, "the prototype's guides cross: {prototype:?}");
    // Crossing guides are only part of the folds: the columns are sampled at different heights (the pointed clumps), so a
    // waved band skews even where no guide crosses. The coherent treatment removes the crossing and no more.
    assert!(mean(&coherent) < mean(&prototype), "coherent {coherent:?} vs prototype {prototype:?}");
}

#[test]
fn the_coherent_treatment_draws_the_same_numbers_and_only_smooths_the_jitter() {
    let g = curtain(44, 5);
    let (proto, _) = compile(&g, 3, Folds::Prototype);
    let (coherent, _) = compile(&g, 3, Folds::Coherent);
    // Same roots (the length and curl draws are untouched; the root does not depend on any draw).
    for (p, c) in proto.iter().zip(&coherent) {
        assert_eq!(p.rest[0], c.rest[0]);
    }
    assert_ne!(proto, coherent);
    // Neighbours differ less than in the prototype's: the sideways step between neighbouring guides at the tips.
    let step = |clumps: &[HairClump]| {
        clumps.windows(2).map(|w| (w[0].rest[20] - w[1].rest[20]).length()).fold(0.0_f32, f32::max)
    };
    assert!(step(&coherent) < step(&proto), "{} vs {}", step(&coherent), step(&proto));
}

#[test]
fn a_clamped_column_never_passes_its_guide() {
    // Two guides that cross: the right one swings left of the left one from the middle down.
    let mut guides = flat_guides(3);
    for k in 0..GUIDE_POINTS {
        guides[1][k].x -= 0.05 * k as f32 / 25.0;
    }
    let refs: Vec<&[Vec3]> = guides.iter().map(Vec::as_slice).collect();
    let mut out = vec![Vec3::ZERO; vertex_count(3, GUIDE_POINTS, 5)];
    strip_positions(&refs, 5, Spacing::Clamped, |_| Vec3::Z, &mut out).unwrap();
    let mut prototype = vec![Vec3::ZERO; out.len()];
    strip_positions(&refs, 5, Spacing::Prototype, |_| Vec3::Z, &mut prototype).unwrap();
    let across = |v: &[Vec3], j: usize, c: usize, k: usize| v[(j * 5 + c) * GUIDE_POINTS + k].x;
    // A column samples its guide at `t`, shortened by its offset; the guide's own x there is the reference.
    let guide_x = |j: usize, t: f32| guides[j][0].x + (guides[j][GUIDE_POINTS - 1].x - guides[j][0].x) * t;
    for j in 0..3 {
        for (c, a) in [-1.0_f32, -0.5, 0.0, 0.5, 1.0].into_iter().enumerate() {
            for k in 0..GUIDE_POINTS {
                let t = k as f32 / 25.0 * (1.0 - 0.15 * a.abs().powf(1.5));
                let off = across(&out, j, c, k) - guide_x(j, t);
                // Pulled toward the neighbour on its side, by at least |a| / 2 of MIN_GAP (the pull is |a| / 2 of the way).
                assert!(
                    off * a.signum() >= a.abs() * 0.5 * MIN_GAP - 1e-5 || a == 0.0,
                    "guide {j} column {c} point {k}: {off}"
                );
            }
        }
    }
    let unordered = (0..GUIDE_POINTS).any(|k| {
        let t = k as f32 / 25.0;
        (across(&prototype, 1, 0, k) - guide_x(1, t * 0.85)) > 1e-4
    });
    assert!(unordered, "the crossing guides do fold the prototype's columns, so the clamp has something to do");
}

#[test]
fn a_lone_guide_and_a_non_finite_guide_still_build() {
    let lone = flat_guides(1);
    let mesh = flat_mesh(&lone, 5, Spacing::Prototype);
    assert_eq!(mesh.positions.len(), 5 * GUIDE_POINTS);
    assert!(mesh.positions.iter().all(|p| p.is_finite()), "a lone guide has no neighbour to pull toward");
    let mut bad = flat_guides(3);
    bad[1][4] = Vec3::NAN;
    let mesh = flat_mesh(&bad, 5, Spacing::Clamped);
    assert_eq!(mesh.positions.len(), 3 * 5 * GUIDE_POINTS, "a NaN point keeps the count");
}

#[test]
fn the_strips_follow_u_not_the_order_of_the_clumps() {
    let g = curtain(8, 5);
    let (clumps, u) = compile(&g, 1, Folds::Prototype);
    let straight = curtain::group_mesh(&clumps, &u, 5, &frame(), None, &StrandMap::default()).unwrap();
    let reversed_clumps: Vec<_> = clumps.iter().rev().cloned().collect();
    let reversed_u: Vec<f32> = u.iter().rev().copied().collect();
    let again = curtain::group_mesh(&reversed_clumps, &reversed_u, 5, &frame(), None, &StrandMap::default()).unwrap();
    assert_eq!(straight, again);
    assert!(curtain::group_mesh(&clumps, &u[1..], 5, &frame(), None, &StrandMap::default()).is_err());
}

#[test]
fn the_streak_tint_and_shade_of_a_clump_reach_its_strip() {
    let g = curtain(4, 3);
    let (mut clumps, u) = compile(&g, 1, Folds::Prototype);
    clumps[2].tint = Some(omuzaki_core::colour::Colour::hex(0x00ff_0000));
    clumps[1].shade = 0.5;
    let mesh = curtain::group_mesh(&clumps, &u, 3, &frame(), None, &StrandMap::default()).unwrap();
    let per = 3 * GUIDE_POINTS;
    // Untinted, and whole to its side: the curtain is one sheet, so a strip's sides are not feathered.
    assert_eq!(mesh.colours[0], [1.0; 4]);
    assert!(mesh.colours[2 * per..3 * per].iter().all(|c| c[0] > 0.9 && c[1] < 0.1));
    let info = &mesh.extra_attribute(HAIR_INFO).unwrap().data;
    assert!((info[3 * per + 1] - 0.5 * (0.97 + 0.03 * 2.1_f32.sin())).abs() < 1e-6);
}

#[test]
fn the_same_seed_gives_the_same_bytes() {
    let g = curtain(44, 5);
    assert_eq!(mesh_of(&g, 5, Folds::Prototype), mesh_of(&g, 5, Folds::Prototype));
    assert_ne!(mesh_of(&g, 5, Folds::Prototype), mesh_of(&g, 6, Folds::Prototype));
}

#[test]
fn non_finite_parameters_keep_the_clump_count_and_do_not_panic() {
    let g = CurtainGen { count: 10, spread_deg: f64::NAN, ..curtain(10, 5) };
    for folds in [Folds::Prototype, Folds::Coherent] {
        let mesh = mesh_of(&g, 1, folds);
        assert_eq!(mesh.positions.len(), 10 * 5 * GUIDE_POINTS);
    }
}

#[test]
fn the_default_curtain_fits_the_hair_budget() {
    let mesh = mesh_of(&curtain(44, 5), 1, Folds::Prototype);
    assert_eq!((mesh.positions.len(), mesh.triangle_count()), (5720, 8800), "the prototype's counts");
}
