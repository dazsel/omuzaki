//! Allocation across a scripted run of changing scenes: instance counts that grow and shrink, zero and non-zero morph
//! weights at the end and in the middle, two mesh sizes, outlined and plain materials. After one warm-up pass a second
//! pass allocates nothing in this crate and leaves the GPU memory where it was; once the instances stop morphing, the
//! pool gives everything back.
//!
//! As in `hot_loop.rs`, wgpu allocates inside its own calls; what this crate controls, and what is asserted, is that
//! none of its own code does.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]

mod support;
// The counting allocator is installed only in this test binary, so the other tests run on the plain allocator.
use omuzaki_render_mock::counting;

#[global_allocator]
static ALLOCATOR: counting::Counting = counting::Counting::new(env!("CARGO_PKG_NAME"));
#[path = "support/grid.rs"]
mod grid;

use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_render::WgpuRenderer;
use omuzaki_render_api::{
    AlphaEncoding, Camera, Colour, Instance, Material, MaterialId, MeshData, MeshId, MeshOptions, MorphRange, MorphSet,
    Outline, Quality, Renderer, Scene, TargetDesc,
};
use omuzaki_render_mock::shapes;

/// How many frames with nothing morphing release a slot (`IDLE_FRAMES` in `src/morph.rs`), plus a margin.
const EMPTY_FRAMES: u32 = 130;
const SCENES: usize = 20;

fn morphing_mesh(r: &mut WgpuRenderer, mesh: &MeshData) -> MeshId {
    let n = mesh.positions.len();
    let ramp: Vec<Vec3> = (0..n).map(|i| Vec3::new(0.0, 0.0, (i % 5) as f32 * 0.01)).collect();
    let set = MorphSet::from_dense(n, &[(Slug::new("t").unwrap(), &ramp, &[])], 0.0).unwrap();
    r.create_mesh_with(mesh, &MeshOptions { morphs: Some(set), ..MeshOptions::default() }).unwrap()
}

/// Scene `k` of the script: `1..=12` instances, each on the large or the small mesh, with the plain or the outlined
/// material, and a weight that is zero for some (at the end of the list and in the middle) and moves with `k`.
fn scene(k: usize, meshes: [MeshId; 2], materials: [MaterialId; 2]) -> Scene {
    let count = [1, 8, 3, 12, 2, 6, 12, 1, 5, 9][k % 10];
    let mut scene =
        Scene::new(Camera::look_at(Vec3::new(0.0, 0.0, 6.0), Vec3::ZERO, Vec3::Y, Camera::default().projection));
    for i in 0..count {
        let mut instance = Instance::new(
            meshes[(i + k) % 2],
            materials[(i * 3 + k) % 2],
            Mat4::from_translation(Vec3::new(i as f32 * 0.3 - 1.5, 0.0, 0.0)),
        );
        instance.morph = Some(MorphRange { start: i as u32, len: 1 });
        scene.instances.push(instance);
        let zero = (i * 7 + k).is_multiple_of(3) || i + 1 == count && k.is_multiple_of(2);
        scene.morph_weights.push(if zero { 0.0 } else { 0.1 + 0.05 * ((i + k) % 7) as f32 });
    }
    scene
}

/// Runs on the GPU path, or on the CPU path forced on the same adapter.
fn a_scripted_run_of_changing_scenes_allocates_nothing_after_one_pass_and_gives_everything_back(cpu: bool) {
    if !counting::can_attribute() {
        println!("no source paths in backtraces (a build without debug info): skipping");
        return;
    }
    let mut r = support::renderer();
    if cpu {
        r.force_cpu_morph().unwrap();
    }
    assert_eq!(r.morph_path(), if cpu { "cpu" } else { "gpu" });
    let meshes = [
        morphing_mesh(&mut r, &grid::grid(40, 40)),
        morphing_mesh(&mut r, &shapes::quad(omuzaki_core::glam::Vec2::ONE)),
    ];
    let plain = r.create_material(&Material::toon(Colour::hex(0x00c9_b8f0))).unwrap();
    let mut outlined = Material::toon(Colour::hex(0x00c9_b8f0));
    outlined.outline = Some(Outline { colour: Colour::BLACK, width: 0.01 });
    let outlined = r.create_material(&outlined).unwrap();
    let target = r.create_target(&TargetDesc { size: UVec2::splat(16), alpha: AlphaEncoding::Straight }).unwrap();
    let quality = Quality::live();
    let scenes: Vec<Scene> = (0..SCENES).map(|k| scene(k, meshes, [plain, outlined])).collect();

    let empty = Scene::new(Camera::default());
    r.render(target, &empty, &quality).unwrap();
    let baseline = r.stats().gpu_bytes;

    for scene in &scenes {
        r.render(target, scene, &quality).unwrap();
    }
    let warmed = r.stats().gpu_bytes;
    assert!(warmed > baseline, "the run pooled something: {baseline} then {warmed}");

    let mut bytes = Vec::new();
    let totals = counting::measure(|| {
        for scene in &scenes {
            r.render(target, scene, &quality).unwrap();
            bytes.push(r.stats().gpu_bytes);
        }
    });
    println!("allocations over {SCENES} scenes by owner: {totals:?}");
    for owner in [counting::OWN, counting::WORKSPACE, counting::UNATTRIBUTED] {
        assert_eq!(totals.get(owner).copied().unwrap_or(0), 0, "{owner} allocated: {totals:?}");
    }
    assert!(bytes.iter().all(|b| *b == warmed), "a pooled buffer was created or dropped in the second pass: {bytes:?}");

    for _ in 0..EMPTY_FRAMES {
        r.render(target, &empty, &quality).unwrap();
    }
    assert_eq!(r.stats().gpu_bytes, baseline, "the pool gives everything back once nothing morphs");

    // With the pool empty, instances whose weights are all zero need no slot, and a frame of them allocates nothing.
    let zeroed: Vec<Scene> = (1..=5)
        .map(|n| {
            let mut scene = scene(n, meshes, [plain, outlined]);
            scene.morph_weights.iter_mut().for_each(|w| *w = 0.0);
            scene
        })
        .collect();
    let totals = counting::measure(|| {
        for _ in 0..4 {
            for scene in &zeroed {
                r.render(target, scene, &quality).unwrap();
            }
        }
    });
    println!("allocations over zero-weight scenes by owner: {totals:?}");
    for owner in [counting::OWN, counting::WORKSPACE, counting::UNATTRIBUTED] {
        assert_eq!(totals.get(owner).copied().unwrap_or(0), 0, "{owner} allocated: {totals:?}");
    }
    assert_eq!(r.stats().gpu_bytes, baseline, "zero weights pool nothing");
}

#[test]
fn on_the_gpu_path() {
    a_scripted_run_of_changing_scenes_allocates_nothing_after_one_pass_and_gives_everything_back(false);
}

#[test]
fn on_the_cpu_path() {
    a_scripted_run_of_changing_scenes_allocates_nothing_after_one_pass_and_gives_everything_back(true);
}
