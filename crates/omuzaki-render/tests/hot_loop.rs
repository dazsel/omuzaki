//! What a frame allocates after warm-up (spec §8, "no allocation in the hot loop").
//!
//! wgpu allocates inside its own calls (about 20 per submit and 17 per render pass, measured on wgpu 30), so the total
//! can't be zero. What the renderer controls is its *own* code, and that is what the strict test checks: every
//! allocation is attributed to the first frame of its backtrace that is not std plumbing, and none may come from
//! `omuzaki-render`. The second test bounds the total, so a new per-frame wgpu object (a bind group built every frame,
//! say) shows up as a regression.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;
// The counting allocator is installed only in this test binary, so the other tests run on the plain allocator.
#[path = "support/counting.rs"]
mod counting;

use std::collections::BTreeMap;
use std::time::Duration;

use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_render::WgpuRenderer;
use omuzaki_render_api::{
    AlphaEncoding, Camera, Colour, Instance, JointRange, Light, Mapping, Material, MaterialId, MeshId, Overlay,
    PixelRect, Quality, Renderer, RendererExt, Scene, TargetDesc, TargetId, TextureDesc, TextureId,
};
use omuzaki_render_mock::shapes;

/// A busy frame: a skinned mesh with a 16-joint palette, 20 props, a painted face overlay repainted every frame, a full
/// light rig and a tone-mapped, scaled render.
struct Fixture {
    renderer: WgpuRenderer,
    scene: Scene,
    quality: Quality,
    target: TargetId,
    face: TextureId,
    meshes: [MeshId; 2],
    material: MaterialId,
    /// The face's pixels, kept between frames: the test must not allocate what it measures.
    pixels: Vec<u8>,
}

fn fixture() -> Fixture {
    let mut renderer = support::renderer();
    let mut skinned = shapes::cuboid(Vec3::splat(0.2));
    skinned.joints = vec![[0, 1, 0, 0]; skinned.positions.len()];
    skinned.weights = vec![[0.5, 0.5, 0.0, 0.0]; skinned.positions.len()];
    let meshes =
        [renderer.create_mesh(&shapes::cuboid(Vec3::splat(0.1))).unwrap(), renderer.create_mesh(&skinned).unwrap()];
    let face = renderer.create_texture(&TextureDesc::colour(UVec2::new(64, 64)).dynamic(), None).unwrap();
    let mut material = Material::toon(Colour::hex(0x00c9_b8f0));
    material.overlays.push(Overlay { texture: face, mapping: Mapping::Uv, strength: 1.0 });
    let material = renderer.create_material(&material).unwrap();
    let target =
        renderer.create_target(&TargetDesc { size: UVec2::new(64, 64), alpha: AlphaEncoding::Straight }).unwrap();
    let eye = Vec3::new(2.0, 1.5, 3.0);
    let scene = Scene::new(Camera::look_at(eye, Vec3::ZERO, Vec3::Y, Camera::default().projection));
    let quality = Quality { render_scale: 1.5, ..Quality::high() };
    Fixture { renderer, scene, quality, target, face, meshes, material, pixels: vec![255; 64 * 64 * 4] }
}

impl Fixture {
    /// One frame the way the stage does it: repaint the face, clear the scene, refill it, render.
    fn frame(&mut self, index: u64) {
        let Self { renderer, scene, quality, target, face, meshes, material, pixels } = self;
        let shade = (index % 200) as u8;
        pixels.iter_mut().step_by(4).for_each(|red| *red = shade);
        renderer.update_texture(*face, PixelRect::full(UVec2::new(64, 64)), pixels).unwrap();

        scene.clear();
        scene.frame.index = index;
        scene.lights.extend((0..4u8).map(|i| Light::Point {
            position: Vec3::splat(f32::from(i)),
            colour: Colour::WHITE,
            intensity: 1.0,
            range: 6.0,
        }));
        let twist = f32::from(u16::try_from(index % 1000).unwrap()) * 0.05;
        scene.joints.extend((0..16u8).map(|j| Mat4::from_rotation_y(twist * f32::from(j % 2))));
        let mut character = Instance::new(meshes[1], *material, Mat4::IDENTITY);
        character.skin = Some(JointRange { start: 0, len: 16 });
        scene.instances.push(character);
        scene.instances.extend((0..20u8).map(|i| {
            Instance::new(meshes[0], *material, Mat4::from_translation(Vec3::new(f32::from(i) * 0.1 - 1.0, -0.5, 0.0)))
        }));
        renderer.render(*target, scene, quality).unwrap();
    }
}

/// Renders `frames` frames after a warm-up and returns each owner's allocations per frame.
fn allocations_per_frame(frames: u64) -> BTreeMap<&'static str, usize> {
    let mut f = fixture();
    // Warm-up: pipelines compile, buffers grow, and the GPU timing slots cycle through their states.
    for i in 0..30 {
        f.frame(i);
        f.renderer.wait_idle().unwrap();
    }
    let totals = counting::measure(|| {
        for i in 30..30 + frames {
            f.frame(i);
        }
    });
    totals.into_iter().map(|(owner, n)| (owner, n / usize::try_from(frames).unwrap())).collect()
}

#[test]
fn the_renderers_own_code_allocates_nothing_per_frame() {
    if !counting::can_attribute() {
        println!("no source paths in backtraces (a build without debug info): skipping");
        return;
    }
    let per_frame = allocations_per_frame(20);
    println!("allocations per frame by owner: {per_frame:?}");
    for owner in [counting::OWN, counting::WORKSPACE, counting::TESTS, counting::UNATTRIBUTED] {
        assert_eq!(per_frame.get(owner).copied().unwrap_or(0), 0, "{owner} allocated per frame: {per_frame:?}");
    }
}

/// wgpu's own share, which this crate cannot reduce below the number of encoders, passes and queue writes it records.
/// A bind group, buffer or encoder created per frame by mistake would push the total well past this ceiling.
#[test]
fn wgpus_share_stays_bounded() {
    if !counting::can_attribute() {
        println!("no source paths in backtraces (a build without debug info): skipping");
        return;
    }
    let per_frame = allocations_per_frame(20);
    let wgpu = [counting::WGPU, counting::DEPENDENCIES]
        .iter()
        .map(|owner| per_frame.get(owner).copied().unwrap_or(0))
        .sum::<usize>();
    // Measured on wgpu 30: 212 for this scene, of which about 100 are the six mip-level passes a repaint triggers.
    assert!(wgpu <= 260, "wgpu allocated {wgpu} times per frame (measured 212): {per_frame:?}");
}

// ---- the counting allocator itself ----

/// `Backtrace::force_capture` allocates while it holds std's backtrace lock, so a hook that takes a backtrace of its own
/// from inside such an allocation waits on itself forever. wgpu-core does exactly this on every snatch-lock acquisition
/// in a debug build when `RUST_BACKTRACE` is set, which CI does: both hot-loop tests hung there for 15 minutes, each on
/// its own thread (zero CPU, one futex wait apiece). Without the guard this test hangs until the deadline aborts it.
#[test]
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn a_backtrace_taken_by_the_code_under_measurement_does_not_deadlock_the_counter() {
    let totals = counting::measure_within(Duration::from_secs(60), || {
        let backtrace = std::backtrace::Backtrace::force_capture();
        std::hint::black_box(&backtrace);
    });
    let set_aside = totals.get(counting::IN_DEPENDENCY_BACKTRACE).copied().unwrap_or(0);
    assert!(set_aside > 0, "the capture's own allocations are noticed and set aside: {totals:?}");
}

/// The same failure end to end: the two hot-loop tests, side by side as the harness runs them, in a child process of
/// their own with backtraces switched on, three times over. A hang is a failure after the timeout, not a stuck run, and
/// the child is always reaped.
#[test]
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn the_hot_loop_tests_finish_together_with_backtraces_enabled() {
    use std::process::{Command, Stdio};
    use std::time::Instant;

    const RUNS: usize = 3;
    const TIMEOUT: Duration = Duration::from_secs(240);
    let exe = std::env::current_exe().expect("the test binary's own path");
    for run in 1..=RUNS {
        let mut child = Command::new(&exe)
            .args(["--test-threads=2", "--exact"])
            .args(["the_renderers_own_code_allocates_nothing_per_frame", "wgpus_share_stays_bounded"])
            .env("RUST_BACKTRACE", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start the test binary again");
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().expect("poll the child") {
                break Some(status);
            }
            if started.elapsed() > TIMEOUT {
                child.kill().expect("stop the hung child");
                child.wait().expect("reap the child");
                break None;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let output = child.wait_with_output().expect("read the child's output");
        let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        let status =
            status.unwrap_or_else(|| panic!("run {run}: no result after {TIMEOUT:?}, a hang. Output:\n{text}"));
        assert!(status.success(), "run {run} failed ({status}):\n{text}");
        println!("run {run}: both tests passed in {:.1} s with RUST_BACKTRACE=1", started.elapsed().as_secs_f64());
    }
}
