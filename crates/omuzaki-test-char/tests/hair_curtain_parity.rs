//! Parity of the hair `curtain` strip mesh (`omuzaki_char::hair::r#gen::curtain`) with the prototype's `shellFill`
//! (`parts/hair.js` 421 to 494), against the sample character's dumps.
//!
//! The oracle is the `game` variant's `hair-curtain` mesh (`geometry/game`) and the clumps of `state/game/hair.json`,
//! private test material kept in the local assets folder: without it (always, in CI and in the cloud) every test here
//! skips with a message. Nothing of the character is committed.
//!
//! **What is compared, and why not the rest pose.** The dump holds the hair *after the settle*: the mesh is the strips
//! filled from the simulated particles, not from the rest guides, and the head had dropped 16 mm. So positions are
//! compared by rebuilding the dump's own drawn guides (the settled particles joined linearly and lightly smoothed, as
//! the prototype's `rebuild` does) and filling the strips from them:
//!
//! - the two outer columns (`a = +-1`), where the prototype's bulge is zero, agree with the dump to the f32 noise;
//! - every other vertex lies exactly the bulge `0.007 (1 - a^2) ss(t, 0.04, 0.25)` from where the port puts it, which
//!   checks the column offsets, the neighbour pull and its mirror at the ends, the `0.15 |a|^1.5` shortening and the
//!   bulge's size, without needing the posed outward direction (that is the runtime's, #322).
//!
//! Everything static is compared exactly from the rest guides: the vertex and triangle counts (5 720 and 8 800), the
//! index list (bands inside a guide, the flip test), `hair-uv`, the strip seed, shade and polar angle of `hair-info`,
//! and the vertex normals the indices give the dump's own positions (within 1 degree).

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers load local files; a bad one should stop the test with its reason
#![allow(clippy::needless_range_loop, clippy::float_cmp)] // rest-point indices; the outer columns' offset is exactly 1
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::chunks_exact_to_as_chunks)]

use omuzaki_char::hair::r#gen::curtain::{
    Spacing, StripLook, strip_indices, strip_mesh, strip_normals, strip_positions, vertex_count, winding_flips,
};
use omuzaki_char_api::{HAIR_INFO, HAIR_UV};
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_test::jsref::{HairClump, HairDump, JsRef};

/// Position tolerance, metres (T-GEOM).
const POSITION: f32 = 2e-4;
/// Normal tolerance, degrees (T-GEOM).
const NORMAL_DEG: f32 = 1.0;
/// Attribute tolerance (`hair-uv`, `hair-info`).
const ATTRIBUTE: f32 = 1e-5;
/// The prototype's head centre `HEAD.C`, which `hair-info`'s polar angle is measured from: the sample character's, so
/// kept here as a placeholder of the same order as the public cranium default.
const HEAD_CENTRE: Vec3 = Vec3::new(0.0, 0.778, -0.022);
/// Columns, points per guide and guides of the dump.
const COLUMNS: u8 = 5;
const POINTS: usize = 26;
/// The prototype's smoothing of the drawn points between particles, and how far each point moves toward its neighbours'
/// mean.
const SMOOTH: f32 = 0.35;

/// The dumps, or `None` (after saying why) so the caller skips.
fn dumps() -> Option<(HairDump, omuzaki_test::jsref::JsMesh, Mat4)> {
    let js = JsRef::open()?;
    let hair = js.hair("game").unwrap();
    let geometry = js.geometry("game").unwrap();
    let mesh = geometry.mesh("hair-curtain").expect("the dump has the curtain mesh").clone();
    Some((hair, mesh, geometry.group_matrix_world))
}

fn guides_of(dump: &HairDump) -> Vec<&HairClump> {
    let mut guides: Vec<&HairClump> = dump.clumps.iter().filter(|c| c.shell).collect();
    guides.sort_by(|a, b| a.u.unwrap().total_cmp(&b.u.unwrap()));
    guides
}

/// The prototype's drawn points of a settled guide: the particles joined linearly between the rest points they sit at,
/// then each inner point moved 35 % toward the mean of its neighbours, in the group's local space.
fn drawn(clump: &HairClump, to_local: Mat4) -> Vec<Vec3> {
    let particles = clump.particles.as_ref().expect("a curtain guide is simulated");
    let settled: Vec<Vec3> = particles.settled.iter().map(|p| to_local.transform_point3(*p)).collect();
    let mut points = vec![Vec3::ZERO; clump.rest.len()];
    for (k, pair) in particles.rest_index.windows(2).enumerate() {
        let (i0, i1) = (pair[0] as usize, pair[1] as usize);
        for i in i0..=i1 {
            points[i] = settled[k].lerp(settled[k + 1], (i - i0) as f32 / (i1 - i0).max(1) as f32);
        }
    }
    let raw = points.clone();
    for i in 1..points.len() - 1 {
        // The prototype smooths in place, so point i sees the already smoothed i - 1.
        points[i] = raw[i].lerp((points[i - 1] + raw[i + 1]) * 0.5, SMOOTH);
    }
    points
}

fn smoothstep(x: f32, lo: f32, hi: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[test]
fn the_strip_mesh_has_the_prototypes_counts_order_and_static_attributes() {
    let Some((hair, js, _)) = dumps() else { return };
    let guides = guides_of(&hair);
    assert_eq!((guides.len(), hair.curtain_columns), (44, u32::from(COLUMNS)));
    let rest: Vec<&[Vec3]> = guides.iter().map(|g| g.rest.as_slice()).collect();
    assert!(rest.iter().all(|g| g.len() == POINTS));
    let looks = vec![StripLook { shade: 1.0, colour: [1.0; 4] }; rest.len()];
    // The flip test needs the outward direction at one point; the dump's own bulge there gives it (see below).
    let mesh = strip_mesh(&rest, &looks, COLUMNS, Spacing::Prototype, HEAD_CENTRE, |_| Vec3::Z).unwrap();
    assert_eq!((mesh.positions.len(), mesh.triangle_count()), (js.mesh.positions.len(), js.mesh.triangle_count()));
    assert_eq!((mesh.positions.len(), mesh.triangle_count()), (5720, 8800));

    let (mine_uv, theirs_uv) =
        (&mesh.extra_attribute(HAIR_UV).unwrap().data, &js.mesh.extra_attribute(HAIR_UV).unwrap().data);
    let (mine_info, theirs_info) =
        (&mesh.extra_attribute(HAIR_INFO).unwrap().data, &js.mesh.extra_attribute(HAIR_INFO).unwrap().data);
    let worst = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0_f32, f32::max);
    let (uv_error, info_error) = (worst(mine_uv, theirs_uv), worst(mine_info, theirs_info));
    eprintln!("curtain strips: hair-uv worst error {uv_error:.2e}, hair-info worst error {info_error:.2e}");
    assert_eq!((mine_uv.len(), mine_info.len()), (theirs_uv.len(), theirs_info.len()));
    assert!(uv_error <= ATTRIBUTE && info_error <= ATTRIBUTE);
}

#[test]
fn the_index_list_and_the_flip_test_match_the_prototypes() {
    let Some((hair, js, _)) = dumps() else { return };
    let guides = guides_of(&hair);
    let rest: Vec<&[Vec3]> = guides.iter().map(|g| g.rest.as_slice()).collect();
    let mut nobulge = vec![Vec3::ZERO; vertex_count(rest.len(), POINTS, COLUMNS)];
    strip_positions(&rest, COLUMNS, Spacing::Prototype, |_| Vec3::ZERO, &mut nobulge).unwrap();
    // The dump's bulge at the vertex the flip test reads is the outward direction there.
    let base = ((guides.len() / 2) * usize::from(COLUMNS) + (usize::from(COLUMNS) - 1) / 2) * POINTS + POINTS / 2;
    let outward = (js.mesh.positions[base] - nobulge[base]).normalize_or_zero();
    assert!(outward != Vec3::ZERO, "the test vertex has a bulge to read the outward direction from");
    // Positions of the rest guides are not the dump's (settled, dropped head), but the flip only needs the winding of the
    // sheet, which is the same: so test it on the dump's positions with the dump's outward direction.
    let flip = winding_flips(&js.mesh.positions, guides.len(), POINTS, COLUMNS, |_| outward);
    let indices = strip_indices(guides.len(), POINTS, COLUMNS, flip).unwrap();
    assert!(flip, "the dump's strips are wound (a0, a0 + 1, b0, ...)");
    assert_eq!(indices, js.mesh.indices, "bands inside a guide only, wound as the flip test says");
    // The opposite decision would not match: the comparison can fail.
    assert_ne!(strip_indices(guides.len(), POINTS, COLUMNS, !flip).unwrap(), js.mesh.indices);
}

#[test]
fn strip_positions_fill_from_the_drawn_guides_to_the_prototypes_vertices() {
    let Some((hair, js, group)) = dumps() else { return };
    let guides = guides_of(&hair);
    let to_local = group.inverse();
    let draws: Vec<Vec<Vec3>> = guides.iter().map(|g| drawn(g, to_local)).collect();
    let refs: Vec<&[Vec3]> = draws.iter().map(Vec::as_slice).collect();
    let mut mine = vec![Vec3::ZERO; vertex_count(refs.len(), POINTS, COLUMNS)];
    strip_positions(&refs, COLUMNS, Spacing::Prototype, |_| Vec3::ZERO, &mut mine).unwrap();
    let columns = usize::from(COLUMNS);
    let (mut edge, mut bulge, mut edge_seen, mut bulge_seen) = (0.0_f32, 0.0_f32, 0, 0);
    for (v, (ours, theirs)) in mine.iter().zip(&js.mesh.positions).enumerate() {
        let (column, k) = (v / POINTS % columns, v % POINTS);
        let a = -1.0 + 2.0 * column as f32 / (columns - 1) as f32;
        let t = k as f32 / (POINTS - 1) as f32 * (1.0 - 0.15 * a.abs().powf(1.5));
        let want_bulge = 0.007 * (1.0 - a * a) * smoothstep(t, 0.04, 0.25);
        let moved = (*theirs - *ours).length();
        assert!(ours.is_finite() && moved.is_finite());
        if a.abs() == 1.0 {
            edge = edge.max(moved);
            edge_seen += 1;
        } else {
            bulge = bulge.max((moved - want_bulge).abs());
            bulge_seen += 1;
        }
    }
    eprintln!(
        "curtain strips: outer columns worst position error {edge:.2e} m over {edge_seen} vertices, bulge size worst error {bulge:.2e} m over {bulge_seen}"
    );
    assert!(edge_seen > 0 && bulge_seen > 0, "nothing compared, so nothing was checked");
    assert!(edge <= POSITION, "outer columns: {edge:.2e} m exceeds {POSITION:.0e} m");
    assert!(bulge <= POSITION, "bulge size: {bulge:.2e} m exceeds {POSITION:.0e} m");
}

#[test]
fn vertex_normals_from_the_dumps_positions_and_indices_match_its_normals() {
    let Some((_, js, _)) = dumps() else { return };
    let mine = strip_normals(&js.mesh.positions, &js.mesh.indices);
    let mut worst = 0.0_f32;
    for (n, theirs) in mine.iter().zip(&js.mesh.normals) {
        worst = worst.max(n.dot(theirs.normalize()).clamp(-1.0, 1.0).acos().to_degrees());
    }
    eprintln!("curtain strips: vertex normals worst error {worst:.3} degrees");
    assert!(!mine.is_empty() && worst.is_finite());
    assert!(worst <= NORMAL_DEG, "normals: {worst:.3} degrees exceeds {NORMAL_DEG}");
}
