//! The `cap` (scalp shell) and `part_sweep` hair generators on synthetic heads: counts, attributes, the gap the cap keeps
//! from the skull, the clearance of the locks' roots, watertight tubes at the parameter extremes, determinism, and
//! zero allocation per call once the buffers are warm. Everything is synthetic.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::float_cmp)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_char::hair::compile::{
    Dispatch, Forked, GroupGeometry, HairGenCtx, clumps_in, compile_groups, is_bilateral,
};
use omuzaki_char::hair::r#gen::cap::{self, MIN_CLEARANCE, RINGS, SEGMENTS};
use omuzaki_char::hair::r#gen::part_sweep::{self, Lock, REST_POINTS};
use omuzaki_char::hair::tube::{RING, TubeLook, TubeMesh};
use omuzaki_char_api::{
    CapGen, Dynamics, Generator, GroupId, HAIR_INFO, HAIR_UV, HairGroup, HairParams, HeadSurface, PartSweepGen, Quality,
};
use omuzaki_core::glam::Vec3;
use omuzaki_core::mesh::MeshData;
use omuzaki_core::rng::Rng;
use omuzaki_face_api::Palette;

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    // `const` init with no destructor: reading it never allocates, so it is safe inside the allocator.
    static MEASURING: Cell<bool> = const { Cell::new(false) };
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// An ellipsoid head: centre `(0, 0.2, 0)`, semi-axes `(0.08, 0.11, 0.095)` times `scale`.
#[derive(Debug)]
struct Egg {
    scale: f32,
}

impl HeadSurface for Egg {
    fn centre(&self) -> Vec3 {
        Vec3::new(0.0, 0.2, 0.0)
    }
    fn radius(&self, dir: Vec3) -> f32 {
        let s = Vec3::new(0.08, 0.11, 0.095) * self.scale;
        let q = dir / s;
        1.0 / q.length()
    }
}

/// How far `p` stands outside the head, along the ray from its centre.
fn gap(head: &dyn HeadSurface, p: Vec3) -> f32 {
    let d = p - head.centre();
    d.length() - head.radius(d.normalize())
}

fn shell(g: &CapGen, head: &dyn HeadSurface) -> MeshData {
    let mut m = MeshData::default();
    cap::fill_shell(g, head, &mut m);
    m
}

/// The cap parameters at the corners and middle of the contract's ranges.
fn cap_extremes() -> Vec<CapGen> {
    let mut all = Vec::new();
    for front in [20.0, 58.0, 120.0] {
        for back in [60.0, 118.0, 150.0] {
            for notch in [0.0, 16.0, 40.0] {
                for thickness in [0.002, 0.016, 0.06] {
                    all.push(CapGen {
                        hairline_front_deg: front,
                        hairline_back_deg: back,
                        ear_notch_deg: notch,
                        thickness,
                    });
                }
            }
        }
    }
    all
}

// ------------------------------------------------------------------------------------------------ the cap

#[test]
fn the_cap_is_a_96_by_48_grid_with_its_two_attributes() {
    let m = shell(&CapGen::default(), &Egg { scale: 1.0 });
    let n = (SEGMENTS + 1) * (RINGS + 1);
    assert_eq!(cap::vertex_count(), n);
    assert_eq!((m.positions.len(), m.normals.len(), m.indices.len()), (n, n, SEGMENTS * RINGS * 6));
    assert!(m.indices.iter().all(|i| (*i as usize) < n));
    let uv = m.extra_attribute(HAIR_UV).expect("hair-uv");
    let info = m.extra_attribute(HAIR_INFO).expect("hair-info");
    assert_eq!((uv.components, uv.data.len()), (2, 2 * n));
    assert_eq!((info.components, info.data.len()), (3, 3 * n));
    assert!(info.data.chunks(3).all(|c| c[0] == 0.37 && c[1] == 0.93));
    assert!(
        uv.data.chunks(2).all(|c| (-1.0..=1.0).contains(&c[0]) && (0.0..=1.0).contains(&c[1])),
        "hair-uv in contract"
    );
    assert!(m.normals.iter().all(|v| (v.length() - 1.0).abs() < 1e-4), "unit normals");
    assert_eq!(cap::clump_count(&CapGen::default()), 0);
}

#[test]
fn the_cap_ends_higher_at_the_forehead_than_at_the_nape_and_lifts_over_the_ears() {
    let g = CapGen::default();
    let m = shell(&g, &Egg { scale: 1.0 });
    let info = m.extra_attribute(HAIR_INFO).unwrap();
    // The last ring's polar angle (hair-info.z) is the hairline at that azimuth.
    let end = |j: usize| info.data[3 * (RINGS * (SEGMENTS + 1) + j) + 2].to_degrees();
    let (front, back, ear) = (end(0), end(SEGMENTS / 2), end(SEGMENTS / 4));
    assert!((front - g.hairline_front_deg as f32).abs() < 0.01, "front {front}");
    assert!((back - g.hairline_back_deg as f32).abs() < 0.01, "back {back}");
    assert!(ear < back && ear < 118.0 - 1.0, "ear {ear}");
    // The seam columns are the same place.
    let row = RINGS * (SEGMENTS + 1);
    assert!((m.positions[row] - m.positions[row + SEGMENTS]).length() < 1e-6);
}

#[test]
fn the_cap_keeps_a_gap_of_two_millimetres_at_every_vertex_triangle_and_edge_middle() {
    for scale in [0.6, 1.0, 1.6] {
        let head = Egg { scale };
        for g in cap_extremes() {
            let m = shell(&g, &head);
            let floor = MIN_CLEARANCE - 1e-5;
            for p in &m.positions {
                assert!(gap(&head, *p) >= floor, "vertex {} below the floor on {g:?}", gap(&head, *p));
            }
            for t in m.indices.chunks(3) {
                let [a, b, c] = [0, 1, 2].map(|k| m.positions[t[k] as usize]);
                for s in [(a + b + c) / 3.0, (a + b) / 2.0, (b + c) / 2.0, (c + a) / 2.0] {
                    assert!(gap(&head, s) >= floor, "face point {} below the floor on {g:?}", gap(&head, s));
                }
            }
        }
    }
}

#[test]
fn the_cap_at_the_default_offsets_needs_no_lift() {
    // At the prototype's thickness the shell stands at its own offset: crown 0.026, hairline 0.016.
    let head = Egg { scale: 1.0 };
    let m = shell(&CapGen::default(), &head);
    let crown = m.positions[0];
    assert!((gap(&head, crown) - 0.026).abs() < 1e-4, "{}", gap(&head, crown));
}

#[test]
fn the_cap_is_finite_and_deterministic_and_a_refill_equals_a_fresh_build() {
    let head = Egg { scale: 1.0 };
    let mut reused = shell(&CapGen { thickness: 0.06, ..CapGen::default() }, &head);
    for g in cap_extremes() {
        let fresh = shell(&g, &head);
        assert!(fresh.positions.iter().chain(&fresh.normals).all(|v| v.is_finite()));
        let uv = fresh.extra_attribute(HAIR_UV).expect("hair-uv");
        assert!(uv.data.chunks(2).all(|c| (-1.0..=1.0).contains(&c[0]) && (0.0..=1.0).contains(&c[1])), "{g:?}");
        cap::fill_shell(&g, &head, &mut reused);
        assert_eq!(reused.positions, fresh.positions);
        assert_eq!(reused.normals, fresh.normals);
        assert_eq!(reused.indices, fresh.indices);
        assert_eq!(reused.extra, fresh.extra);
    }
}

#[test]
fn compiling_the_cap_gives_a_shell_and_no_clumps() {
    let (id, dynamics) = (GroupId::new("cap").unwrap(), Dynamics::default());
    let mut rng = Rng::new(1);
    let mut forked = Forked(&mut rng);
    let head = Egg { scale: 1.0 };
    let palette = Palette::default();
    let mut ctx = HairGenCtx::new(&head, &palette, Quality::Live, &id, &dynamics, &mut forked);
    let compiled = cap::compile(&CapGen::default(), &mut ctx);
    assert_eq!(compiled.clumps.len(), 0);
    let GroupGeometry::Shell(m) = compiled.geometry else { panic!("not a shell") };
    assert_eq!(m.positions.len(), cap::vertex_count());
}

// ------------------------------------------------------------------------------------------------ the part sweep

fn locks(g: &PartSweepGen, head: &dyn HeadSurface) -> Vec<Lock> {
    let mut l = Vec::new();
    part_sweep::rebuild(g, head, &mut l);
    l
}

/// Whether the tube along `lock` is closed along its length: every edge belongs to two triangles, except the open ring at
/// each end.
fn watertight_but_for_the_ends(lock: &Lock, head: &dyn HeadSurface, g: &PartSweepGen) -> bool {
    let mut mesh = TubeMesh::new();
    let polar = vec![0.5; REST_POINTS];
    let tube_look = TubeLook { seed: 0.5, shade: 1.0, polar: &polar, tint: None, flip: 1.0 };
    let centre = head.centre();
    if mesh.push(lock, &g.width, &g.thickness, |p| (p - centre).normalize_or_zero(), &tube_look).is_err() {
        return false;
    }
    let Ok(m) = mesh.finish() else { return false };
    if m.positions.iter().any(|p| !p.is_finite()) {
        return false;
    }
    let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
    for t in m.indices.chunks(3) {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            *edges.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    let open = edges.values().filter(|n| **n == 1).count();
    edges.values().all(|n| *n <= 2) && open == 2 * RING
}

#[test]
fn the_default_sweep_makes_count_locks_per_side_with_roots_clear_of_the_head() {
    let g = PartSweepGen::default();
    let head = Egg { scale: 1.0 };
    let l = locks(&g, &head);
    assert_eq!(l.len(), part_sweep::clump_count(&g));
    assert_eq!(l.len(), 2 * usize::from(g.count));
    for lock in &l {
        assert!(gap(&head, lock[0]) >= 0.008, "root {}", gap(&head, lock[0]));
        for p in lock {
            assert!(gap(&head, *p) >= 0.008, "point {}", gap(&head, *p));
        }
    }
    // Roots of the two sides stand apart (the nudge is 0.02 of a unit direction, so a few millimetres on the head).
    assert!((l[0][0] - l[usize::from(g.count)][0]).length() > 0.003);
}

#[test]
fn the_sides_mirror_each_other_on_a_symmetric_head() {
    let g = PartSweepGen::default();
    let all = locks(&g, &Egg { scale: 1.0 });
    let (plus, minus) = all.split_at(usize::from(g.count));
    for (left, right) in plus.iter().zip(minus) {
        for (a, b) in left.iter().zip(right) {
            assert!((a.x + b.x).abs() < 1e-5 && (a.y - b.y).abs() < 1e-5 && (a.z - b.z).abs() < 1e-5);
        }
    }
}

#[test]
fn the_sweep_is_watertight_at_the_parameter_extremes() {
    let head = Egg { scale: 1.0 };
    let base = PartSweepGen::default();
    let cases = [
        PartSweepGen { count: 1, ..base.clone() },
        PartSweepGen { count: 80, ..base.clone() },
        PartSweepGen { end_from_deg: 0.0, end_to_deg: 0.0, ..base.clone() },
        PartSweepGen { end_from_deg: 180.0, end_to_deg: 180.0, ..base.clone() },
        PartSweepGen { end_from_deg: 178.0, end_to_deg: 62.0, ..base.clone() },
        PartSweepGen { part_front_deg: -90.0, part_back_deg: 90.0, ..base.clone() },
        PartSweepGen { part_front_deg: 42.0, part_back_deg: 42.0, ..base.clone() },
        PartSweepGen { end_height_front: 1.0, end_height_back: -1.0, ..base.clone() },
        PartSweepGen { end_height_front: 0.0, end_height_back: 0.0, ..base.clone() },
        PartSweepGen { end_height_front: 0.11, end_height_back: -0.11, ..base.clone() },
    ];
    for g in cases {
        let l = locks(&g, &head);
        assert_eq!(l.len(), part_sweep::clump_count(&g), "{g:?}");
        for lock in &l {
            assert!(lock.iter().all(|p| p.is_finite()), "{g:?}");
            assert!(watertight_but_for_the_ends(lock, &head, &g), "{g:?}");
        }
    }
}

#[test]
fn a_larger_head_carries_the_same_style_proportionally() {
    let g = PartSweepGen::default();
    let small = locks(&g, &Egg { scale: 1.0 });
    let big = locks(&g, &Egg { scale: 1.2 });
    // The ends sit at the same azimuths on both, and each is outside its own head by the same lift.
    let (a, b) = (&small[3][REST_POINTS - 1], &big[3][REST_POINTS - 1]);
    assert!(a.z.signum() == b.z.signum() && a.x.signum() == b.x.signum());
    assert!(gap(&Egg { scale: 1.2 }, *b) >= 0.008);
}

#[test]
fn compiling_the_sweep_makes_the_clumps_with_alternating_shade_and_the_groups_widths() {
    let (id, dynamics) = (GroupId::new("sweep").unwrap(), Dynamics::default());
    let mut rng = Rng::new(1);
    let mut forked = Forked(&mut rng);
    let head = Egg { scale: 1.0 };
    let palette = Palette::default();
    let g = PartSweepGen { count: 4, ..PartSweepGen::default() };
    let mut ctx = HairGenCtx::new(&head, &palette, Quality::Live, &id, &dynamics, &mut forked);
    let compiled = part_sweep::compile(&g, &mut ctx);
    assert_eq!(compiled.clumps.len(), 8);
    let shades: Vec<f32> = compiled.clumps.iter().map(|c| c.shade).collect();
    assert_eq!(shades[..4], shades[4..], "each side restarts the alternation");
    assert!(shades[0] < shades[1]);
    assert!(compiled.clumps.iter().all(|c| c.rest.len() == REST_POINTS && c.width == g.width));
}

// ------------------------------------------------------------------------------------------------ mirroring

#[test]
fn a_mirrored_cap_or_sweep_group_has_no_coincident_duplicates() {
    let head = Egg { scale: 1.0 };
    let sweep = PartSweepGen { count: 5, ..PartSweepGen::default() };
    let make = |mirror: bool| {
        let group = |generator| HairGroup { enabled: true, mirror, dynamics: Dynamics::default(), generator };
        let mut params = HairParams::default();
        params.groups.clear();
        params.groups.insert(GroupId::new("a-cap").unwrap(), group(Generator::Cap(CapGen::default())));
        params.groups.insert(GroupId::new("b-sweep").unwrap(), group(Generator::PartSweep(sweep.clone())));
        params
    };
    let compile = |params: &HairParams| {
        let mut forked = Rng::new(1);
        let mut rng = Forked(&mut forked);
        compile_groups(params, &Dispatch, &head, &Palette::default(), Quality::Live, &mut rng).unwrap()
    };
    let (plain, mirrored) = (compile(&make(false)), compile(&make(true)));
    assert!(is_bilateral(&Generator::Cap(CapGen::default())) && is_bilateral(&Generator::PartSweep(sweep.clone())));
    for (a, b) in plain.iter().zip(&mirrored) {
        assert_eq!(a.group, b.group, "{}: the mirror flag changes nothing for a two-sided generator", a.id);
    }
    for (id, group) in &make(true).groups {
        assert_eq!(clumps_in(group, &Dispatch), plain.iter().find(|e| &e.id == id).unwrap().group.clumps.len());
    }
    // And no two locks sit on top of each other.
    let locks = &mirrored[1].group.clumps;
    assert_eq!(locks.len(), 10);
    for (i, a) in locks.iter().enumerate() {
        for b in &locks[i + 1..] {
            assert!(a.rest.iter().zip(&b.rest).any(|(p, q)| p.distance(*q) > 1e-4), "coincident locks");
        }
    }
}

// ------------------------------------------------------------------------------------------------ allocation

#[test]
fn the_cap_and_the_sweep_allocate_nothing_per_call_once_warm() {
    let head = Egg { scale: 1.0 };
    let (cap_g, sweep_g) = (CapGen::default(), PartSweepGen::default());
    let mut m = MeshData::default();
    let mut l: Vec<Lock> = Vec::with_capacity(part_sweep::clump_count(&sweep_g));
    cap::fill_shell(&cap_g, &head, &mut m); // warm-up: the buffers grow to their working size
    part_sweep::rebuild(&sweep_g, &head, &mut l);

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|c| c.set(true));
    for i in 0..200 {
        let g = CapGen { thickness: 0.01 + 0.0001 * f64::from(i), ..cap_g.clone() };
        cap::fill_shell(&g, &head, &mut m);
        part_sweep::rebuild(&sweep_g, &head, &mut l);
    }
    MEASURING.with(|c| c.set(false));
    let n = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(n, 0, "200 rebuilds made {n} allocations");
}
