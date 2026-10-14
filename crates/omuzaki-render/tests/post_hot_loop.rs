//! What a frame allocates after warm-up with every post effect on (spec §8, "no allocation in the hot loop").
//!
//! The same measurement as `hot_loop.rs`, on a scene that runs Bloom (12 render passes), `DepthOfField` (3), the grade, the
//! grain and the dither, in the live-quality and the frame-exact modes: every allocation is attributed to the first
//! frame of its backtrace that is not std plumbing, and none may come from `omuzaki-render`. The effects build their
//! images, bind groups and pipelines when a size or a mode is first seen, and nothing after that.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;
// The counting allocator is installed only in this test binary, so the other tests run on the plain allocator.
#[path = "support/counting.rs"]
mod counting;

use std::collections::BTreeMap;

use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_render::WgpuRenderer;
use omuzaki_render_api::{
    AlphaEncoding, Background, Bloom, Camera, Colour, ColourGrade, DepthOfField, FilmGrain, Instance, Material,
    MaterialId, MeshId, Projection, Quality, Renderer, Scene, TargetDesc, TargetId,
};
use omuzaki_render_mock::shapes;

struct Fixture {
    renderer: WgpuRenderer,
    scene: Scene,
    target: TargetId,
}

fn fixture(size: UVec2) -> Fixture {
    let mut renderer = support::renderer();
    let mesh: MeshId = renderer.create_mesh(&shapes::quad(Vec2::new(0.4, 0.5))).unwrap();
    let material: MaterialId = renderer.create_material(&Material::unlit(Colour::hex(0x00c9_b8f0))).unwrap();
    let mut scene = Scene::new(Camera::look_at(
        Vec3::new(0.0, 0.0, 3.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.1, far: 50.0 },
    ));
    scene.background = Background::Transparent;
    scene.instances.push(Instance::new(mesh, material, Mat4::from_translation(Vec3::new(0.0, 0.0, 1.0))));
    scene.instances.push(Instance::new(mesh, material, Mat4::from_translation(Vec3::new(0.5, 0.2, -2.0))));
    scene.post.bloom = Some(Bloom { strength: 0.35, radius: 0.5, threshold: 0.5 });
    scene.post.depth_of_field = Some(DepthOfField { focus_distance: 2.0, focus_range: 0.3, max_blur: 0.03 });
    scene.post.grain = Some(FilmGrain { strength: 0.04 });
    scene.post.grade = Some(ColourGrade { saturation: 1.1, contrast: 1.1 });
    let target = renderer.create_target(&TargetDesc { size, alpha: AlphaEncoding::Premultiplied }).unwrap();
    Fixture { renderer, scene, target }
}

/// Allocations per frame by owner, for `frames` frames of `quality` after a warm-up.
fn allocations_per_frame(quality: Quality, frames: u64) -> BTreeMap<&'static str, usize> {
    let mut f = fixture(UVec2::new(96, 64));
    for i in 0..30 {
        f.scene.frame.index = i;
        f.renderer.render(f.target, &f.scene, &quality).unwrap();
        f.renderer.wait_idle().unwrap();
    }
    let totals = counting::measure(|| {
        for i in 30..30 + frames {
            f.scene.frame.index = i;
            f.renderer.render(f.target, &f.scene, &quality).unwrap();
        }
    });
    totals.into_iter().map(|(owner, n)| (owner, n / usize::try_from(frames).unwrap())).collect()
}

fn qualities() -> [(&'static str, Quality); 2] {
    [
        ("high", Quality { samples: 4, render_scale: 1.5, ..Quality::high() }),
        ("offline", Quality { samples: 4, ..Quality::offline() }),
    ]
}

#[test]
fn the_renderers_own_code_allocates_nothing_per_frame_with_every_effect_on() {
    if !counting::can_attribute() {
        println!("no source paths in backtraces (a build without debug info): skipping");
        return;
    }
    for (name, quality) in qualities() {
        let per_frame = allocations_per_frame(quality, 20);
        println!("{name}: allocations per frame by owner: {per_frame:?}");
        for owner in ["omuzaki-render", "render-api", "test and mock", "unattributed"] {
            assert_eq!(
                per_frame.get(owner).copied().unwrap_or(0),
                0,
                "{name}: {owner} allocated per frame: {per_frame:?}"
            );
        }
    }
}

/// wgpu's share grows with the passes recorded (about 17 per render pass), so the effects cost what their 15 passes cost (measured 365 for this scene)
/// there; a bind group or buffer made per frame by mistake would push the total far past this ceiling.
#[test]
fn wgpus_share_stays_bounded_with_every_effect_on() {
    if !counting::can_attribute() {
        println!("no source paths in backtraces (a build without debug info): skipping");
        return;
    }
    for (name, quality) in qualities() {
        let per_frame = allocations_per_frame(quality, 20);
        let wgpu = per_frame.get("wgpu and other dependencies").copied().unwrap_or(0);
        println!("{name}: wgpu allocated {wgpu} times per frame");
        assert!(wgpu <= 450, "{name}: wgpu allocated {wgpu} times per frame: {per_frame:?}");
    }
}
