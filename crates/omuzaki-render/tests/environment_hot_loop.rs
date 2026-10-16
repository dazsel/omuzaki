//! What a frame lit by an environment allocates after warm-up (spec §8, "no allocation in the hot loop"). The method and
//! its limits are those of `hot_loop.rs`: wgpu allocates inside its own calls, so the strict check is that none of the
//! allocations come from `omuzaki-render` itself. The environment is baked at creation, so a frame only writes one small
//! uniform; this proves it, with the toon ramp on and off and with the sky turning every frame.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)]

mod support;
// The counting allocator is installed only in this test binary, so the other tests run on the plain allocator.
use omuzaki_render_mock::counting;

#[global_allocator]
static ALLOCATOR: counting::Counting = counting::Counting::new(env!("CARGO_PKG_NAME"));

use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_render_api::{
    AlphaEncoding, Camera, Colour, EnvironmentDesc, EnvironmentUse, Instance, Material, Quality, Renderer, Scene,
    TargetDesc,
};
use omuzaki_render_mock::shapes;

#[test]
fn a_frame_lit_by_an_environment_allocates_nothing_in_the_renderers_own_code() {
    if !counting::can_attribute() {
        println!("no source paths in backtraces (a build without debug info): skipping");
        return;
    }
    let mut renderer = support::renderer();
    let mesh = renderer.create_mesh(&shapes::cuboid(Vec3::splat(0.3))).unwrap();
    let material = renderer.create_material(&Material::toon(Colour::hex(0x00c9_b8f0))).unwrap();
    let target =
        renderer.create_target(&TargetDesc { size: UVec2::new(64, 64), alpha: AlphaEncoding::Straight }).unwrap();
    let sky = EnvironmentDesc::Sky {
        zenith: Colour::hex(0x0060_90ff),
        horizon: Colour::hex(0x00ff_e0c0),
        ground: Colour::hex(0x0030_2820),
    };
    let environment = renderer.create_environment(&sky).unwrap();
    let mut scene =
        Scene::new(Camera::look_at(Vec3::new(1.5, 1.0, 2.5), Vec3::ZERO, Vec3::Y, Camera::default().projection));
    let quality = Quality::live();
    let mut frame = |renderer: &mut omuzaki_render::WgpuRenderer, index: u64| {
        scene.clear();
        scene.frame.index = index;
        let mut using = EnvironmentUse::new(environment);
        using.rotation_deg = index as f32;
        using.toon_ramp = index.is_multiple_of(2);
        scene.environment = Some(using);
        scene.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
        renderer.render(target, &scene, &quality).unwrap();
    };
    for i in 0..30 {
        frame(&mut renderer, i);
        renderer.wait_idle().unwrap();
    }
    let totals = counting::measure(|| (30..50).for_each(|i| frame(&mut renderer, i)));
    let per_frame: std::collections::BTreeMap<_, _> = totals.into_iter().map(|(owner, n)| (owner, n / 20)).collect();
    println!("allocations per frame by owner: {per_frame:?}");
    for owner in [counting::OWN, counting::WORKSPACE, counting::TESTS, counting::UNATTRIBUTED] {
        assert_eq!(per_frame.get(owner).copied().unwrap_or(0), 0, "{owner} allocated per frame: {per_frame:?}");
    }
}
