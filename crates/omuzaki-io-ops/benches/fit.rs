//! The fit stages against their budgets (`docs/design/platform-export.md` §4). Run with
//! `cargo bench -p omuzaki-io-ops --bench fit`.
//!
//! - `decimate` on a 100,000-triangle skinned sphere with 52 sparse morph targets, down to 20,000: 5 s and 400 MB peak
//!   (issue #1075).
//! - `merge_meshes` on a 100,000-triangle, 24-piece skinned avatar (about 53,000 vertices): 200 ms and about 40 MiB
//!   beyond the scene itself.
//!
//! The decimate peak is the resident size from `/proc/self/status` after its first run; the merge's is the most a
//! counting allocator saw it hold beyond the scene. Each is printed with its time.

#![allow(clippy::cast_precision_loss)]
// A benchmark may panic: a panic is a clear failure of the fixture.
#![allow(clippy::expect_used)]
// The counting allocator wraps `System` and is the only `unsafe` here: the benchmark measures with it, and nothing in
// the library depends on it.
#![allow(unsafe_code, reason = "a counting global allocator wraps System, for the merge's memory figure only")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::f32::consts::PI;
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use omuzaki_core::glam::{Mat4, Vec2, Vec3};
use omuzaki_io_api::{IoScene, Mesh, MeshData, MorphTarget, Node, Primitive, Skin};
use omuzaki_io_ops::fit_decimate::{DecimateOptions, decimate};
use omuzaki_io_ops::fit_meshes::merge_meshes;

const STACKS: u32 = 201;
const SLICES: u32 = 250;
const TARGETS: usize = 52;
/// The pieces of the avatar `merge_meshes` joins, and each piece's sphere: 24 x 4,136 = 99,264 triangles.
const PIECES: usize = 24;
const PIECE_STACKS: u32 = 45;
const PIECE_SLICES: u32 = 47;

/// A UV sphere of `stacks` x `slices` with two joints, no morph targets.
fn sphere(stacks: u32, slices: u32) -> Primitive {
    let mut mesh = MeshData::default();
    for i in 0..=stacks {
        for j in 0..=slices {
            let (phi, theta) = (PI * i as f32 / stacks as f32, 2.0 * PI * j as f32 / slices as f32);
            let n = Vec3::new(phi.sin() * theta.cos(), phi.cos(), phi.sin() * theta.sin());
            mesh.positions.push(n * 0.1);
            mesh.normals.push(n);
            mesh.uvs.push(Vec2::new(j as f32 / slices as f32, i as f32 / stacks as f32));
            let w = 0.5 + 0.5 * n.y;
            mesh.joints.push([0, 1, 0, 0]);
            mesh.weights.push([w, 1.0 - w, 0.0, 0.0]);
        }
    }
    let at = |i: u32, j: u32| i * (slices + 1) + j;
    for i in 0..stacks {
        for j in 0..slices {
            let (a, b, c, d) = (at(i, j), at(i + 1, j), at(i, j + 1), at(i + 1, j + 1));
            if i > 0 {
                mesh.indices.extend([a, c, b]);
            }
            if i + 1 < stacks {
                mesh.indices.extend([b, c, d]);
            }
        }
    }
    Primitive::new(mesh)
}

/// A UV sphere (99,500 triangles) with two joints and sparse targets: target `k` moves the vertices in one band.
fn fixture() -> Primitive {
    let mut p = sphere(STACKS, SLICES);
    p.morph_targets = (0..TARGETS)
        .map(|k| {
            let band = (k as f32 + 0.5) / TARGETS as f32;
            let deltas = p
                .mesh
                .normals
                .iter()
                .map(|n| if ((0.5 - 0.5 * n.y) - band).abs() < 0.01 { *n * 0.002 } else { Vec3::ZERO });
            MorphTarget::new(deltas.collect())
        })
        .collect();
    p
}

/// A size in MB from `/proc/self/status` (`VmHWM:` is the peak resident size, `VmRSS:` the current one), or 0 where
/// `/proc` is not there.
#[allow(clippy::disallowed_methods)] // a benchmark reading its own process status, not an op reaching a file
fn status_mb(field: &str) -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kb = status
        .lines()
        .find_map(|l| l.strip_prefix(field))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok());
    kb.unwrap_or(0) / 1024
}

fn peak_mb() -> u64 {
    status_mb("VmHWM:")
}

/// Counts the bytes live and their peak, for the merge's memory figure: the resident size cannot say what one call
/// held, because the process has already held more. It wraps `System` and nothing in the library depends on it.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call is `System`'s, with the same layout; the counters are atomics.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract for `alloc` is passed on unchanged.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: the caller's contract for `dealloc` is passed on unchanged.
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: the caller's contract for `realloc` is passed on unchanged.
        let moved = unsafe { System.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            let live = if new_size >= layout.size() {
                LIVE.fetch_add(new_size - layout.size(), Ordering::Relaxed) + (new_size - layout.size())
            } else {
                LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed) - (layout.size() - new_size)
            };
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Runs `f` and returns its result with the most bytes it held beyond what was live when it started.
fn extra_peak<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let start = LIVE.load(Ordering::Relaxed);
    PEAK.store(start, Ordering::Relaxed);
    let result = f();
    (result, PEAK.load(Ordering::Relaxed).saturating_sub(start))
}

/// A skinned avatar of [`PIECES`] pieces, each with its own skin of the same two joints, which `merge_meshes` joins.
fn avatar() -> IoScene {
    let mut scene = IoScene::new();
    let mut joints = Vec::new();
    for name in ["hips", "spine"] {
        let joint = scene.add_node(Node::new(name));
        scene.roots.push(joint);
        joints.push(joint);
    }
    for k in 0..PIECES {
        let skin = scene.add_skin(Skin::new("rig", joints.clone(), vec![Mat4::IDENTITY; 2]));
        let mut mesh = Mesh::new(format!("piece {k}"));
        mesh.primitives.push(sphere(PIECE_STACKS, PIECE_SLICES));
        let mesh = scene.add_mesh(mesh);
        let mut node = Node::new(format!("piece {k}"));
        node.mesh = Some(mesh);
        node.skin = Some(skin);
        let node = scene.add_node(node);
        scene.roots.push(node);
    }
    scene
}

fn decimate_100k(c: &mut Criterion) {
    let base = fixture();
    let options = DecimateOptions { max_error: 1.0, ..DecimateOptions::new(20_000) };
    let mut once = base.clone();
    let start = Instant::now();
    let report = decimate(&mut once, &options);
    println!("decimate 100k -> 20k, 52 targets: {:?}, peak {} MB, {report:?}", start.elapsed(), peak_mb());
    let mut group = c.benchmark_group("fit_decimate");
    group.sample_size(10);
    group.bench_function("100k to 20k triangles, 52 sparse morph targets", |b| {
        b.iter_batched(|| base.clone(), |mut p| black_box(decimate(&mut p, &options)), BatchSize::LargeInput);
    });
    group.finish();
}

fn merge_24_pieces(c: &mut Criterion) {
    let base = avatar();
    let mut once = base.clone();
    let start = Instant::now();
    let (report, extra) = extra_peak(|| merge_meshes(&mut once).expect("the avatar passes its own check"));
    println!(
        "merge 24 pieces, 99k triangles: {:?}, {} KiB beyond the scene, skinned {:?}",
        start.elapsed(),
        extra / 1024,
        report.skinned
    );
    assert_eq!(report.skinned, (PIECES, 1), "the pieces all join");
    let mut group = c.benchmark_group("fit_meshes");
    group.sample_size(10);
    group.bench_function("24 skinned pieces, 99k triangles", |b| {
        b.iter_batched(|| base.clone(), |mut scene| black_box(merge_meshes(&mut scene)), BatchSize::LargeInput);
    });
    group.finish();
}

criterion_group!(benches, merge_24_pieces, decimate_100k);
criterion_main!(benches);
