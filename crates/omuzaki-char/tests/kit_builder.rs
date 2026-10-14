//! Behaviour of the kit's grid and mesh-builder API that the prototype never exercised: mirroring, validation, weight
//! normalisation, merging, rollback on failure and the edge cases that must be errors rather than panics.

#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers build fixtures; a bad one should stop the test with its reason
#![allow(clippy::float_cmp, clippy::cast_precision_loss)] // exact values that were copied or computed exactly; small loop counters

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char::kit::KitError;
use omuzaki_char::kit::curve::{TAU, range};
use omuzaki_char::kit::grid::{Grid, GridOptions, MeshBuilder, Skin, grid_normals, grid_offset, mirror_joint};
use omuzaki_char::kit::loft::loft_y;
use omuzaki_char_api::{BoneName, JointRef, MaterialName};
use omuzaki_core::glam::{DVec2, DVec3, Vec2, Vec3};
use omuzaki_core::mesh::MeshData;
use proptest::prelude::*;

fn skin_material() -> MaterialName {
    MaterialName::new("skin").unwrap()
}

fn joint(name: &str) -> JointRef {
    JointRef::Named(BoneName::new(name).unwrap())
}

/// A sphere of radius `r` centred at `(cx, 0, 0)`, poles on the Y axis, as a closed lathe with collapsed end rings.
fn sphere(cx: f64, r: f64, rows: usize, seg: usize) -> Grid {
    let thetas: Vec<f64> = range(0.0, std::f64::consts::PI, rows - 1);
    loft_y(&thetas, seg, |theta, phi, _, _| {
        DVec3::new(cx + r * theta.sin() * phi.sin(), -r * theta.cos(), r * theta.sin() * phi.cos())
    })
    .unwrap()
}

fn outward(cx: f64) -> impl Fn(usize, usize) -> DVec3 {
    move |_, _| DVec3::new(cx, 0.0, 0.0)
}

/// The signed volume enclosed by a mesh (positive when triangles wind counter-clockwise seen from outside).
fn signed_volume(m: &MeshData) -> f64 {
    m.indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|&[a, b, c]| {
            let p = |k: u32| m.positions[k as usize].as_dvec3();
            p(a).dot(p(b).cross(p(c))) / 6.0
        })
        .sum()
}

fn flat_grid(rows: usize, cols: usize) -> Grid {
    Grid::from_fn(rows, cols, |i, j| DVec3::new(j as f64, i as f64, 0.0)).unwrap()
}

#[test]
fn grids_below_two_by_two_or_ragged_or_not_finite_are_errors() {
    assert!(matches!(Grid::new(0, 0, vec![]), Err(KitError::TooFew { .. })));
    assert!(matches!(Grid::new(1, 5, vec![DVec3::ZERO; 5]), Err(KitError::TooFew { .. })), "a single row");
    assert!(matches!(Grid::new(5, 1, vec![DVec3::ZERO; 5]), Err(KitError::TooFew { .. })), "a single column");
    assert!(matches!(Grid::new(2, 2, vec![DVec3::ZERO; 3]), Err(KitError::TooFew { .. })), "a missing point");
    assert!(matches!(Grid::from_rows(&[]), Err(KitError::TooFew { .. })));
    let ragged = vec![vec![DVec3::ZERO; 3], vec![DVec3::ZERO; 2]];
    assert_eq!(Grid::from_rows(&ragged), Err(KitError::Ragged { row: 1, expected: 3, got: 2 }));
    let nan = vec![DVec3::ZERO, DVec3::X, DVec3::Y, DVec3::new(f64::NAN, 0.0, 0.0)];
    assert!(matches!(Grid::new(2, 2, nan), Err(KitError::NonFinite { .. })));
    assert!(Grid::new(2, 2, vec![DVec3::ZERO; 4]).is_ok(), "the smallest grid is one quad");
    let mut called = false;
    assert!(
        Grid::from_fn(1, 4, |_, _| {
            called = true;
            DVec3::ZERO
        })
        .is_err()
            && !called
    );
}

#[test]
fn a_wrapped_axis_needs_three_points_and_an_empty_builder_cannot_finish() {
    let two = flat_grid(2, 2);
    for options in
        [GridOptions { wrap_i: true, ..GridOptions::default() }, GridOptions { wrap_j: true, ..GridOptions::default() }]
    {
        assert!(matches!(grid_normals(&two, &options), Err(KitError::TooFew { need: 3, .. })));
        assert!(matches!(MeshBuilder::new().add_grid(&two, &options), Err(KitError::TooFew { .. })));
        assert!(matches!(grid_offset(&two, 0.1, &options), Err(KitError::TooFew { .. })));
    }
    assert!(
        grid_normals(&flat_grid(3, 3), &GridOptions { wrap_i: true, wrap_j: true, ..GridOptions::default() }).is_ok()
    );
    assert!(matches!(MeshBuilder::new().finish("empty", skin_material()), Err(KitError::Mesh(_))));
}

#[test]
fn a_flat_sheet_has_the_natural_normal_and_flip_or_invert_reverse_it() {
    let g = flat_grid(3, 4);
    // d/di = +y, d/dj = +x, so the natural normal is y x x = -z.
    let natural = grid_normals(&g, &GridOptions::default()).unwrap();
    assert!(!natural.flipped && natural.normals.iter().all(|n| (*n - DVec3::NEG_Z).length() < 1e-12));
    for options in
        [GridOptions { flip: true, ..GridOptions::default() }, GridOptions { invert: true, ..GridOptions::default() }]
    {
        let reversed = grid_normals(&g, &options).unwrap();
        assert!(reversed.flipped && reversed.normals.iter().all(|n| (*n - DVec3::Z).length() < 1e-12));
    }
    let both = GridOptions { flip: true, invert: true, ..GridOptions::default() };
    assert!(!grid_normals(&g, &both).unwrap().flipped, "invert undoes flip");
    let below = |_: usize, _: usize| DVec3::new(1.0, 1.0, -5.0);
    let toward_viewer = GridOptions { inside: Some(&below), ..GridOptions::default() };
    assert!(grid_normals(&g, &toward_viewer).unwrap().flipped, "an interior behind the sheet puts normals toward +z");
}

#[test]
fn sphere_normals_point_out_with_one_shared_near_axial_normal_at_each_pole() {
    let g = sphere(0.0, 1.0, 13, 24);
    let o = GridOptions { wrap_j: true, inside: Some(&outward(0.0)), ..GridOptions::default() };
    let n = grid_normals(&g, &o).unwrap().normals;
    let cols = g.cols();
    for i in 1..g.rows() - 1 {
        for j in 0..cols {
            assert!(n[i * cols + j].dot(g.at(i, j)) > 0.99, "({i}, {j}) points out");
        }
    }
    // The shared pole normal averages every copy of the ring, including the repeated seam column (as the prototype
    // does), so it leans slightly toward the seam: within 20 degrees of the axis at 24 segments.
    for (row, axis) in [(0, DVec3::NEG_Y), (g.rows() - 1, DVec3::Y)] {
        let shared = n[row * cols];
        assert!((0..cols).all(|j| n[row * cols + j] == shared), "pole row {row} shares one normal");
        assert!(shared.dot(axis) > 0.94, "pole row {row} is nearly axial: {shared:?}");
    }
    for i in 0..g.rows() {
        assert!((n[i * cols] - n[i * cols + cols - 1]).length() < 1e-9, "the seam of row {i} is smooth");
    }
}

#[test]
fn a_wrapped_torus_has_no_seam_in_either_direction() {
    let (major, minor) = (0.4, 0.12);
    let g = Grid::from_fn(17, 13, |i, j| {
        let (th, ph) = (TAU * i as f64 / 16.0, TAU * j as f64 / 12.0);
        let ring = major + minor * ph.cos();
        DVec3::new(ring * th.cos(), minor * ph.sin(), ring * th.sin())
    })
    .unwrap();
    let centre = |i: usize, _: usize| {
        let th = TAU * i as f64 / 16.0;
        DVec3::new(major * th.cos(), 0.0, major * th.sin())
    };
    let o = GridOptions { wrap_i: true, wrap_j: true, inside: Some(&centre), ..GridOptions::default() };
    let n = grid_normals(&g, &o).unwrap().normals;
    for j in 0..13 {
        assert!((n[j] - n[16 * 13 + j]).length() < 1e-9, "first and last rows agree at column {j}");
    }
    for i in 0..17 {
        assert!((n[i * 13] - n[i * 13 + 12]).length() < 1e-9, "first and last columns agree at row {i}");
    }
    assert!(
        n.iter().enumerate().all(|(k, n)| n.dot(g.points()[k] - centre(k / 13, 0)) > 0.0),
        "all point away from the tube"
    );
}

#[test]
fn offsetting_a_sphere_grows_its_radius() {
    let g = sphere(0.0, 1.0, 13, 24);
    let o = GridOptions { wrap_j: true, inside: Some(&outward(0.0)), ..GridOptions::default() };
    let bigger = grid_offset(&g, 0.25, &o).unwrap();
    assert_eq!((bigger.rows(), bigger.cols()), (g.rows(), g.cols()));
    for (a, b) in g.points().iter().zip(bigger.points()) {
        assert!((b.length() - 1.25).abs() < 0.02, "radius {}", b.length());
        assert!(a.length() > 0.99);
    }
    assert!(matches!(grid_offset(&g, f64::NAN, &o), Err(KitError::NonFinite { .. })));
}

#[test]
fn a_built_sphere_is_a_valid_closed_shell() {
    let mut mb = MeshBuilder::new();
    let o = GridOptions { wrap_j: true, inside: Some(&outward(0.5)), ..GridOptions::default() };
    mb.add_grid(&sphere(0.5, 0.4, 13, 24), &o).unwrap();
    let part = mb.finish("ball", skin_material()).unwrap();
    part.validate().unwrap();
    part.geometry.validate().unwrap();
    assert_eq!(part.joint_bones, [JointRef::HIPS], "the default skin is the hips");
    assert!(part.casts_shadow && !part.dynamic && part.render_order == 0 && part.name == "ball");
    let expected = 4.0 / 3.0 * std::f64::consts::PI * 0.4_f64.powi(3);
    let volume = signed_volume(&part.geometry);
    assert!(volume > 0.0 && (volume - expected).abs() / expected < 0.05, "volume {volume} vs {expected}");
}

#[test]
fn mirroring_keeps_the_volume_positive_negates_x_and_swaps_the_sides_of_the_skin() {
    let left_arm = JointRef::Humanoid(HumanoidBone::LeftUpperArm);
    let o = GridOptions {
        wrap_j: true,
        inside: Some(&outward(0.5)),
        skin: Skin::Blend(vec![(left_arm.clone(), 0.75), (joint("ear_l_1"), 0.25)]),
        ..GridOptions::default()
    };
    let mut mb = MeshBuilder::new();
    mb.add_grid(&sphere(0.5, 0.4, 13, 24), &o).unwrap();
    let mirror = mb.mirrored();
    let (a, b) = (mb.mesh(), mirror.mesh());
    assert!(signed_volume(a) > 0.0 && signed_volume(b) > 0.0);
    assert!((signed_volume(a) - signed_volume(b)).abs() < 1e-6);
    for (p, q) in a.positions.iter().zip(&b.positions) {
        assert_eq!(*q, Vec3::new(-p.x, p.y, p.z));
    }
    // Outward normals stay outward: they point away from the mirrored centre.
    assert!(b.positions.iter().zip(&b.normals).all(|(p, n)| n.dot(*p - Vec3::new(-0.5, 0.0, 0.0)) > 0.0));
    assert_eq!(mirror.bones(), [JointRef::Humanoid(HumanoidBone::RightUpperArm), joint("ear_r_1")]);
    assert_eq!(
        (b.joints.clone(), b.weights.clone()),
        (a.joints.clone(), a.weights.clone()),
        "same slots, other side's bones"
    );
    assert_eq!(mirror.mirrored().mesh(), mb.mesh(), "mirroring twice is the identity");
    mirror.finish("ball-r", skin_material()).unwrap();
}

#[test]
fn joints_mirror_by_role_or_by_their_l_and_r_segments() {
    assert_eq!(mirror_joint(&JointRef::Humanoid(HumanoidBone::LeftHand)), JointRef::Humanoid(HumanoidBone::RightHand));
    assert_eq!(mirror_joint(&JointRef::HIPS), JointRef::HIPS);
    assert_eq!(mirror_joint(&joint("ear_l")), joint("ear_r"));
    assert_eq!(mirror_joint(&joint("hair_r_3")), joint("hair_l_3"));
    assert_eq!(mirror_joint(&joint("hair_back_2")), joint("hair_back_2"));
    assert_eq!(mirror_joint(&joint("lower")), joint("lower"), "only whole `l` and `r` segments swap");
}

#[test]
fn skin_weights_keep_the_four_largest_merge_repeats_drop_junk_and_sum_to_one() {
    let bones: Vec<JointRef> = (0..6).map(|k| joint(&format!("b{k}"))).collect();
    let blend = vec![
        (bones[0].clone(), 0.1),
        (bones[1].clone(), 0.4),
        (bones[2].clone(), 0.2),
        (bones[3].clone(), 0.3),
        (bones[4].clone(), 0.05),
        (bones[1].clone(), 0.1),      // repeated: merges into 0.5
        (bones[5].clone(), -1.0),     // negative: dropped
        (bones[5].clone(), f32::NAN), // not a number: dropped
    ];
    let mut mb = MeshBuilder::new();
    mb.add_grid(&flat_grid(2, 2), &GridOptions { skin: Skin::Blend(blend), ..GridOptions::default() }).unwrap();
    let m = mb.mesh();
    let ranked: Vec<&JointRef> = m.joints[0].iter().map(|&j| &mb.bones()[usize::from(j)]).collect();
    assert_eq!(ranked, [&bones[1], &bones[3], &bones[2], &bones[0]], "largest first, the 0.05 loses");
    let total = 0.5 + 0.3 + 0.2 + 0.1;
    let expected = [0.5 / total, 0.3 / total, 0.2 / total, 0.1 / total];
    for (got, want) in m.weights[0].iter().zip(expected) {
        assert!((f64::from(*got) - want).abs() < 1e-6);
    }
    assert!((m.weights[0].iter().sum::<f32>() - 1.0).abs() < 1e-6);
}

#[test]
fn a_vertex_with_no_usable_weight_falls_back_to_the_hips() {
    for blend in [vec![], vec![(joint("x"), 0.0)], vec![(joint("x"), f32::INFINITY)]] {
        let mut mb = MeshBuilder::new();
        mb.add_grid(&flat_grid(2, 2), &GridOptions { skin: Skin::Blend(blend), ..GridOptions::default() }).unwrap();
        assert_eq!(mb.bones(), [JointRef::HIPS]);
        assert_eq!(mb.mesh().weights[0], [1.0, 0.0, 0.0, 0.0]);
    }
}

#[test]
fn a_per_vertex_skin_sees_the_position_and_indices() {
    let seen = std::cell::RefCell::new(Vec::new());
    let skin = |p: DVec3, i: usize, j: usize| {
        seen.borrow_mut().push((p, i, j));
        vec![(JointRef::CHEST, 1.0)]
    };
    let mut mb = MeshBuilder::new();
    mb.add_grid(&flat_grid(2, 3), &GridOptions { skin: Skin::Per(&skin), ..GridOptions::default() }).unwrap();
    assert_eq!(seen.borrow()[4], (DVec3::new(1.0, 1.0, 0.0), 1, 1));
    assert_eq!(seen.borrow().len(), 6);
}

#[test]
fn merging_remaps_joints_and_follows_the_colour_mode() {
    let mut a = MeshBuilder::with_colours();
    a.add_grid(&flat_grid(2, 2), &GridOptions { skin: Skin::Bone(joint("a")), ..GridOptions::default() }).unwrap();
    let mut b = MeshBuilder::new();
    let two_bones = Skin::Blend(vec![(joint("b"), 0.5), (joint("a"), 0.5)]);
    b.add_grid(&flat_grid(2, 2), &GridOptions { skin: two_bones, ..GridOptions::default() }).unwrap();
    a.merge(&b).unwrap();
    assert_eq!(a.bones(), [joint("a"), joint("b")]);
    let m = a.mesh();
    assert_eq!((m.positions.len(), m.indices.len(), m.colours.len()), (8, 12, 8));
    assert_eq!(m.colours[7], [1.0; 4], "white for the colourless builder's vertices");
    assert_eq!(m.indices[6..9], [4, 6, 5], "indices are offset by the existing vertices");
    // b listed `b` first, so its slot 0 is `b` (index 1 here) and its slot 1 is `a` (index 0).
    assert_eq!((m.joints[4][0], m.joints[4][1]), (1, 0));
    m.validate().unwrap();
    // A colourless builder drops a coloured one's colours.
    let mut plain = MeshBuilder::new();
    plain.merge(&a).unwrap();
    assert!(plain.mesh().colours.is_empty() && plain.vertex_count() == 8);
    plain.mesh().validate().unwrap();
}

#[test]
fn a_polygon_is_a_flat_fan_with_one_normal() {
    let mut mb = MeshBuilder::with_colours();
    let quad = [DVec3::ZERO, DVec3::X, DVec3::new(1.0, 1.0, 0.0), DVec3::Y];
    mb.add_polygon(&quad, DVec3::Z, [0.5, 0.25, 0.0, 1.0], &[(JointRef::HEAD, 1.0)]).unwrap();
    let m = mb.mesh();
    assert_eq!((m.positions.len(), m.indices.clone()), (4, vec![0, 1, 2, 0, 2, 3]));
    assert!(m.normals.iter().all(|n| *n == Vec3::Z) && m.colours.iter().all(|c| *c == [0.5, 0.25, 0.0, 1.0]));
    assert_eq!(m.uvs, vec![Vec2::ZERO; 4]);
    assert_eq!(mb.bones(), [JointRef::HEAD]);
    assert!(matches!(mb.add_polygon(&quad[..2], DVec3::Z, [1.0; 4], &[]), Err(KitError::TooFew { .. })));
    assert!(matches!(mb.add_polygon(&quad, DVec3::NAN, [1.0; 4], &[]), Err(KitError::NonFinite { .. })));
    assert_eq!(mb.vertex_count(), 4, "failed adds change nothing");
}

#[test]
fn a_failed_add_leaves_the_builder_as_it_was() {
    let mut mb = MeshBuilder::new();
    mb.add_grid(&flat_grid(2, 2), &GridOptions { skin: Skin::Bone(joint("keep")), ..GridOptions::default() }).unwrap();
    let before = (mb.mesh().clone(), mb.bones().to_vec());
    // 66 000 vertices, each with a bone of its own: the joint list overflows the 16-bit index.
    let big = flat_grid(2, 33_000);
    let unique = |_: DVec3, i: usize, j: usize| vec![(joint(&format!("b{}", i * 33_000 + j)), 1.0)];
    let result = mb.add_grid(&big, &GridOptions { skin: Skin::Per(&unique), ..GridOptions::default() });
    assert!(matches!(result, Err(KitError::TooMany { what: "skin joints", .. })), "{result:?}");
    assert_eq!((mb.mesh().clone(), mb.bones().to_vec()), before);
}

#[test]
fn default_uvs_run_zero_to_one_across_the_grid_and_a_function_replaces_them() {
    let g = flat_grid(2, 3);
    let uv = |i: usize, j: usize| DVec2::new(i as f64 * 10.0, j as f64);
    let mut mb = MeshBuilder::new();
    mb.add_grid(&g, &GridOptions::default()).unwrap();
    mb.add_grid(&g, &GridOptions { uv: Some(&uv), ..GridOptions::default() }).unwrap();
    let uvs = &mb.mesh().uvs;
    let default = [(0.0, 0.0), (0.5, 0.0), (1.0, 0.0), (0.0, 1.0), (0.5, 1.0), (1.0, 1.0)];
    assert_eq!(uvs[..6], default.map(|(u, v)| Vec2::new(u, v)));
    assert_eq!(uvs[6..9], [Vec2::new(0.0, 0.0), Vec2::new(0.0, 1.0), Vec2::new(0.0, 2.0)]);
}

#[test]
fn a_colour_function_is_ignored_unless_the_builder_records_colours() {
    let red = |_: usize, _: usize, _: DVec3| [1.0, 0.0, 0.0, 1.0];
    let o = GridOptions { colour: Some(&red), ..GridOptions::default() };
    let mut plain = MeshBuilder::new();
    plain.add_grid(&flat_grid(2, 2), &o).unwrap();
    assert_eq!(plain.mesh().colours, Vec::<[f32; 4]>::new());
    let mut coloured = MeshBuilder::with_colours();
    coloured.add_grid(&flat_grid(2, 2), &o).unwrap();
    assert_eq!(coloured.mesh().colours, vec![[1.0, 0.0, 0.0, 1.0]; 4]);
    coloured.add_grid(&flat_grid(2, 2), &GridOptions::default()).unwrap();
    assert_eq!(coloured.mesh().colours[4], [1.0; 4], "white by default");
}

#[test]
fn a_grid_too_large_to_count_is_an_overflow_error_and_its_points_are_never_computed() {
    let never = |_: usize, _: usize| -> DVec3 { panic!("the point function ran for an impossible grid") };
    assert!(matches!(Grid::new(usize::MAX, 2, vec![]), Err(KitError::Overflow { .. })));
    assert!(matches!(Grid::new(2, usize::MAX, vec![]), Err(KitError::Overflow { .. })));
    assert!(matches!(Grid::new(usize::MAX, usize::MAX, vec![]), Err(KitError::Overflow { .. })));
    assert!(matches!(Grid::from_fn(usize::MAX, 2, never), Err(KitError::Overflow { .. })));
    assert!(matches!(Grid::from_fn(2, usize::MAX, never), Err(KitError::Overflow { .. })));
}

#[test]
fn finish_rejects_a_non_finite_uv() {
    let nan_uv = |_: usize, _: usize| DVec2::new(f64::NAN, 0.0);
    let mut mb = MeshBuilder::new();
    mb.add_grid(&flat_grid(2, 2), &GridOptions { uv: Some(&nan_uv), ..GridOptions::default() }).unwrap();
    assert!(matches!(mb.finish("uv", skin_material()), Err(KitError::NonFinite { what: "a mesh's UVs" })));
}

#[test]
fn finish_rejects_a_non_finite_colour() {
    let inf_colour = |_: usize, _: usize, _: DVec3| [1.0, f32::INFINITY, 1.0, 1.0];
    let mut mb = MeshBuilder::with_colours();
    mb.add_grid(&flat_grid(2, 2), &GridOptions { colour: Some(&inf_colour), ..GridOptions::default() }).unwrap();
    assert!(matches!(mb.finish("tint", skin_material()), Err(KitError::NonFinite { what: "a mesh's colours" })));
}

proptest! {
    #[test]
    fn any_smooth_grid_builds_a_valid_part_whose_mirror_is_valid_and_involutive(
        rows in 2_usize..10, cols in 2_usize..10,
        amp in 0.0_f64..0.5, freq in 0.5_f64..4.0, flip in any::<bool>(), invert in any::<bool>(),
        weights in proptest::collection::vec((0_u8..6, 0.0_f32..2.0), 0..8),
    ) {
        let g = Grid::from_fn(rows, cols, |i, j| {
            let (x, y) = (j as f64 / (cols - 1) as f64, i as f64 / (rows - 1) as f64);
            DVec3::new(x, y, amp * (freq * x).sin() * (freq * y).cos())
        }).unwrap();
        let blend: Vec<(JointRef, f32)> = weights.iter().map(|&(b, w)| (joint(&format!("b{b}_l")), w)).collect();
        let mut mb = MeshBuilder::new();
        mb.add_grid(&g, &GridOptions { flip, invert, skin: Skin::Blend(blend), ..GridOptions::default() }).unwrap();
        let part = mb.clone().finish("grid", skin_material()).unwrap();
        part.validate().unwrap();
        prop_assert_eq!(part.geometry.positions.len(), rows * cols);
        prop_assert_eq!(part.geometry.triangle_count(), 2 * (rows - 1) * (cols - 1));
        prop_assert!(part.geometry.weights.iter().all(|w| (w.iter().sum::<f32>() - 1.0).abs() < 1e-5));
        let mirror = mb.mirrored();
        mirror.clone().finish("grid-r", skin_material()).unwrap();
        let twice = mirror.mirrored();
        prop_assert_eq!(twice.mesh(), mb.mesh());
    }
}
