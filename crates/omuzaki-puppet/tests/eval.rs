//! The evaluator against formulas: keyform interpolation, combination rules, deformers, draw order, determinism and
//! the no-allocation rule (#1362).
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "test arithmetic on small numbers"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_core::glam::Vec2;
use omuzaki_puppet::{Evaluator, PuppetBuilder, grid_mesh};
use omuzaki_puppet_api::{BindTarget, DrawCmd2D, Interp, NodeIx, NodeKind, ParamIx, PuppetDef};

/// Counts allocations on the thread that turned counting on (the harness allocates on its own threads).
struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    // `const` init with no destructor: reading it never allocates, so it is safe inside the allocator.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

// SAFETY: forwards to the system allocator; only counts.
#[allow(unsafe_code, reason = "a counting allocator is the only way to prove no allocation")]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.try_with(Cell::get).unwrap_or(false) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: the caller's layout goes straight to the system allocator.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `alloc` above with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const QUAD: [[f32; 2]; 4] = [[0.0, 0.0], [10.0, 0.0], [0.0, 10.0], [10.0, 10.0]];

fn quad_puppet() -> (PuppetBuilder, NodeIx) {
    let mut b = PuppetBuilder::new("quad", [100.0, 100.0]);
    let tex = b.texture("t.png", [4, 4]);
    let root = b.root();
    let quad = b.part("quad", root, tex, grid_mesh([0.0, 0.0, 10.0, 10.0], 1, 1, [0.0, 0.0, 1.0, 1.0]));
    (b, quad)
}

fn evaluator(def: Result<PuppetDef, omuzaki_puppet_api::PuppetError>) -> Evaluator {
    match def.and_then(Evaluator::new) {
        Ok(e) => e,
        Err(e) => panic!("a valid test puppet: {e}"),
    }
}

fn close(a: Vec2, b: [f32; 2]) -> bool {
    (a - Vec2::from(b)).length() < 1e-5
}

#[test]
fn a_one_axis_deform_hits_its_keys_blends_between_and_clamps_outside() {
    let (mut b, quad) = quad_puppet();
    let p = b.param("p", -1.0, 1.0, 0.0, &[-1.0, 0.0, 1.0]);
    b.bind_deform(p, quad, Interp::Linear, vec![vec![[-2.0, 0.0]; 4], vec![[0.0, 0.0]; 4], vec![[4.0, 2.0]; 4]]);
    let mut e = evaluator(b.finish());
    for (v, want) in
        [(-1.0, [-2.0, 0.0]), (0.0, [0.0, 0.0]), (1.0, [4.0, 2.0]), (0.5, [2.0, 1.0]), (-0.25, [-0.5, 0.0])]
    {
        e.set(p, [v, 0.0]);
        e.evaluate();
        for (k, q) in e.points(quad).iter().enumerate() {
            let rest = QUAD[k];
            assert!(close(*q, [rest[0] + want[0], rest[1] + want[1]]), "at {v}: vertex {k} is {q}");
        }
    }
    e.set(p, [5.0, 0.0]);
    assert_eq!(e.value(p), [1.0, 0.0], "values clamp to the range");
}

#[test]
fn a_two_by_two_grid_is_bilinear() {
    let (mut b, quad) = quad_puppet();
    let p = b.param2("xy", [0.0, 0.0], [1.0, 1.0], [0.0, 0.0], [&[0.0, 1.0], &[0.0, 1.0]]);
    // Key (ix, iy) offsets x by 1 * ix and y by 3 * iy, plus 5 at the far corner, x-major.
    let corner = |ix: f32, iy: f32| vec![[ix + 5.0 * ix * iy, 3.0 * iy]; 4];
    b.bind_deform(
        p,
        quad,
        Interp::Linear,
        vec![corner(0.0, 0.0), corner(0.0, 1.0), corner(1.0, 0.0), corner(1.0, 1.0)],
    );
    let mut e = evaluator(b.finish());
    for (x, y) in [(0.0, 0.0), (1.0, 1.0), (0.5, 0.5), (0.25, 0.75), (0.9, 0.1)] {
        e.set(p, [x, y]);
        e.evaluate();
        let want = [x + 5.0 * x * y, 3.0 * y];
        let got = e.points(quad)[0];
        assert!(close(got, want), "at ({x}, {y}): {got} against {want:?}");
    }
}

#[test]
fn offsets_add_and_scales_multiply_across_parameters() {
    let (mut b, quad) = quad_puppet();
    let a = b.param("a", 0.0, 1.0, 0.0, &[0.0, 1.0]);
    let c = b.param("c", 0.0, 1.0, 0.0, &[0.0, 1.0]);
    b.bind_deform(a, quad, Interp::Linear, vec![vec![[0.0; 2]; 4], vec![[1.0, 0.0]; 4]]);
    b.bind_deform(c, quad, Interp::Linear, vec![vec![[0.0; 2]; 4], vec![[0.0, 2.0]; 4]]);
    b.bind(a, quad, BindTarget::ScaleX, Interp::Linear, vec![1.0, 2.0]);
    b.bind(c, quad, BindTarget::ScaleX, Interp::Linear, vec![1.0, 3.0]);
    b.bind(a, quad, BindTarget::TranslateY, Interp::Linear, vec![0.0, 10.0]);
    b.bind(c, quad, BindTarget::TranslateY, Interp::Linear, vec![0.0, 5.0]);
    let mut e = evaluator(b.finish());
    e.set(a, [1.0, 0.0]);
    e.set(c, [1.0, 0.0]);
    e.evaluate();
    // Vertex (10, 0) deformed to (11, 2), scaled x by 2 * 3, translated y by 15.
    assert!(close(e.points(quad)[1], [66.0, 17.0]), "{}", e.points(quad)[1]);
}

#[test]
fn unset_key_points_are_filled_from_their_neighbours() {
    let (mut b, quad) = quad_puppet();
    let p = b.param("p", 0.0, 2.0, 0.0, &[0.0, 1.0, 2.0]);
    b.bind(p, quad, BindTarget::TranslateX, Interp::Linear, vec![0.0, 999.0, 8.0]);
    if let Some(param) = b.param_mut(p) {
        param.bindings[0].set = vec![true, false, true];
    }
    let mut e = evaluator(b.finish());
    e.set(p, [1.0, 0.0]);
    e.evaluate();
    assert!(close(e.points(quad)[0], [4.0, 0.0]), "the middle key interpolates 0 and 8: {}", e.points(quad)[0]);
}

#[test]
fn nearest_swaps_and_cubic_passes_through_keys() {
    let (mut b, quad) = quad_puppet();
    let p = b.param("p", 0.0, 2.0, 0.0, &[0.0, 1.0, 2.0]);
    b.bind(p, quad, BindTarget::Opacity, Interp::Nearest, vec![1.0, 0.0, 1.0]);
    b.bind(p, quad, BindTarget::TranslateX, Interp::Cubic, vec![0.0, 10.0, 0.0]);
    let mut e = evaluator(b.finish());
    let opacity = |e: &Evaluator| {
        e.draw_list().cmds.iter().find_map(|c| match c {
            DrawCmd2D::Mesh { state, .. } => Some(state.opacity),
            _ => None,
        })
    };
    e.set(p, [0.4, 0.0]);
    e.evaluate();
    assert_eq!(opacity(&e), Some(1.0));
    e.set(p, [0.6, 0.0]);
    e.evaluate();
    assert_eq!(opacity(&e), Some(0.0));
    e.set(p, [1.0, 0.0]);
    e.evaluate();
    assert!(close(e.points(quad)[0], [10.0, 0.0]));
    e.set(p, [0.5, 0.0]);
    e.evaluate();
    let x = e.points(quad)[0].x;
    assert!(x > 5.0 && x < 10.0, "Catmull-Rom bulges above the straight line toward the peak: {x}");
}

#[test]
fn children_of_a_lattice_follow_its_points() {
    let mut b = PuppetBuilder::new("lattice", [100.0, 100.0]);
    let tex = b.texture("t.png", [4, 4]);
    let root = b.root();
    let lat = b.lattice("warp", root, 3, 3, [0.0, 0.0], [20.0, 20.0]);
    let quad = b.part("quad", lat, tex, grid_mesh([5.0, 5.0, 10.0, 10.0], 1, 1, [0.0, 0.0, 1.0, 1.0]));
    let p = b.param("p", 0.0, 1.0, 0.0, &[0.0, 1.0]);
    // Shift the whole grid by (3, -1), and the centre point a further (4, 0).
    let mut moved = vec![[3.0, -1.0]; 9];
    moved[4] = [7.0, -1.0];
    b.bind_deform(p, lat, Interp::Linear, vec![vec![[0.0; 2]; 9], moved]);
    let mut e = evaluator(b.finish());
    assert!(close(e.points(quad)[0], [5.0, 5.0]), "at rest a lattice changes nothing");
    e.set(p, [1.0, 0.0]);
    e.evaluate();
    // Vertex (5, 5) sits at u = v = 0.5 in the top-left cell: corners move by (3,-1) except the centre's (7,-1),
    // so the bilinear blend adds a quarter of the centre's extra 4.
    assert!(close(e.points(quad)[0], [9.0, 4.0]), "{}", e.points(quad)[0]);
    // Vertex (15, 15) sits at the middle of the bottom-right cell: the same.
    assert!(close(e.points(quad)[3], [19.0, 14.0]), "{}", e.points(quad)[3]);
    let outside = e.to_model(lat, Vec2::new(30.0, 10.0));
    assert!(outside.is_finite(), "points beyond the grid extrapolate");
}

#[test]
fn children_of_a_mesh_deformer_follow_its_triangles() {
    let mut b = PuppetBuilder::new("meshgroup", [100.0, 100.0]);
    let tex = b.texture("t.png", [4, 4]);
    let root = b.root();
    let mut mesh = grid_mesh([0.0, 0.0, 20.0, 20.0], 2, 2, [0.0, 0.0, 1.0, 1.0]);
    mesh.uvs.clear();
    let def = b.node("group", root, [0.0, 0.0], NodeKind::MeshDeformer(mesh));
    let quad = b.part("quad", def, tex, grid_mesh([2.0, 2.0, 6.0, 6.0], 1, 1, [0.0, 0.0, 1.0, 1.0]));
    let p = b.param("p", 0.0, 1.0, 0.0, &[0.0, 1.0]);
    b.bind_deform(p, def, Interp::Linear, vec![vec![[0.0; 2]; 9], vec![[5.0, 5.0]; 9]]);
    let mut e = evaluator(b.finish());
    e.set(p, [1.0, 0.0]);
    e.evaluate();
    assert!(close(e.points(quad)[0], [7.0, 7.0]), "a uniform move carries the child: {}", e.points(quad)[0]);
}

#[test]
fn transforms_compose_down_the_tree() {
    let mut b = PuppetBuilder::new("tree", [100.0, 100.0]);
    let tex = b.texture("t.png", [4, 4]);
    let root = b.root();
    let pivot = b.group("pivot", root, [50.0, 50.0]);
    let quad = b.part("quad", pivot, tex, grid_mesh([10.0, 0.0, 1.0, 1.0], 1, 1, [0.0, 0.0, 1.0, 1.0]));
    let turn = b.param("turn", -1.0, 1.0, 0.0, &[-1.0, 1.0]);
    b.bind(
        turn,
        pivot,
        BindTarget::Rotate,
        Interp::Linear,
        vec![-std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2],
    );
    let mut e = evaluator(b.finish());
    assert!(close(e.points(quad)[0], [60.0, 50.0]));
    e.set(turn, [1.0, 0.0]);
    e.evaluate();
    // A quarter turn clockwise on screen (y down) carries +x to +y.
    assert!(close(e.points(quad)[0], [50.0, 60.0]), "{}", e.points(quad)[0]);
}

fn first_drawn_x(e: &Evaluator) -> Option<f32> {
    let list = e.draw_list();
    list.cmds.iter().find_map(|c| match c {
        DrawCmd2D::Mesh { first, .. } => Some(list.vertices[list.indices[*first as usize] as usize].pos[0]),
        _ => None,
    })
}

#[test]
fn larger_zsort_draws_first_and_a_binding_can_reorder() {
    let mut b = PuppetBuilder::new("z", [100.0, 100.0]);
    let tex = b.texture("t.png", [4, 4]);
    let root = b.root();
    let back = b.part("back", root, tex, grid_mesh([0.0, 0.0, 1.0, 1.0], 1, 1, [0.0, 0.0, 1.0, 1.0]));
    let front = b.part("front", root, tex, grid_mesh([50.0, 0.0, 1.0, 1.0], 1, 1, [0.0, 0.0, 1.0, 1.0]));
    if let Some(n) = b.node_mut(back) {
        n.zsort = 0.5;
    }
    let p = b.param("swap", 0.0, 1.0, 0.0, &[0.0, 1.0]);
    b.bind(p, front, BindTarget::ZSort, Interp::Linear, vec![0.0, 1.0]);
    let mut e = evaluator(b.finish());
    assert_eq!(first_drawn_x(&e), Some(0.0), "back (zsort 0.5) draws before front (0)");
    e.set(p, [1.0, 0.0]);
    e.evaluate();
    assert_eq!(first_drawn_x(&e), Some(50.0), "front moved to zsort 1, behind back");
}

#[test]
fn the_same_values_give_the_same_draw_list_and_evaluation_allocates_nothing() {
    let (mut b, quad) = quad_puppet();
    let p = b.param2("xy", [-1.0, -1.0], [1.0, 1.0], [0.0, 0.0], [&[-1.0, 0.0, 1.0], &[-1.0, 0.0, 1.0]]);
    b.bind_deform(p, quad, Interp::Cubic, (0..9).map(|k| vec![[k as f32, -(k as f32)]; 4]).collect());
    b.bind(p, quad, BindTarget::Rotate, Interp::Linear, (0..9).map(|k| k as f32 * 0.1).collect());
    let def = b.finish();
    let mut a = evaluator(def.clone());
    let mut c = evaluator(def);
    for e in [&mut a, &mut c] {
        e.set(p, [0.3, -0.7]);
        e.evaluate();
    }
    assert_eq!(a.draw_list(), c.draw_list());
    COUNTING.with(|c| c.set(true));
    for i in 0..1000 {
        a.set(p, [(i as f32 * 0.01).sin(), (i as f32 * 0.013).cos()]);
        a.evaluate();
    }
    COUNTING.with(|c| c.set(false));
    assert_eq!(ALLOCS.load(Ordering::Relaxed), 0, "evaluate allocated");
}

#[test]
fn unknown_params_and_nodes_are_ignored_safely() {
    let (b, quad) = quad_puppet();
    let mut e = evaluator(b.finish());
    e.set(ParamIx(99), [1.0, 1.0]);
    e.evaluate();
    assert_eq!(e.points(NodeIx(99)), &[] as &[Vec2]);
    assert_eq!(e.points(quad).len(), 4);
}
