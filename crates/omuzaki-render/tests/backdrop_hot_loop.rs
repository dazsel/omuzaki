//! What a frame over a gradient or an image backdrop allocates after warm-up (spec §8, "no allocation in the hot loop";
//! #930). The method and its limits are those of `hot_loop.rs`: wgpu allocates inside its own calls, so the strict check
//! is that none of the allocations come from `omuzaki-render` itself. The resolve pass binds the backdrop texture
//! through a group it makes once per texture, so a frame only writes its uniform; this proves it, with the fit and the
//! bars changing every frame and a gradient frame between texture frames.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)]

mod support;
// The counting allocator is installed only in this test binary, so the other tests run on the plain allocator.
use omuzaki_render_mock::counting;

#[global_allocator]
static ALLOCATOR: counting::Counting = counting::Counting::new(env!("CARGO_PKG_NAME"));

use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_render_api::{
    AlphaEncoding, BackdropFit, Background, Camera, Colour, Instance, Material, Quality, Renderer, Scene, TargetDesc,
    TextureDesc,
};
use omuzaki_render_mock::shapes;

#[test]
fn a_frame_over_an_image_backdrop_allocates_nothing_in_the_renderers_own_code() {
    if !counting::can_attribute() {
        println!("no source paths in backtraces (a build without debug info): skipping");
        return;
    }
    let mut renderer = support::renderer();
    let mesh = renderer.create_mesh(&shapes::cuboid(Vec3::splat(0.3))).unwrap();
    let material = renderer.create_material(&Material::toon(Colour::hex(0x00c9_b8f0))).unwrap();
    let target =
        renderer.create_target(&TargetDesc { size: UVec2::new(64, 36), alpha: AlphaEncoding::Straight }).unwrap();
    #[allow(clippy::cast_possible_truncation)] // both are below 256
    let rgba: Vec<u8> = (0..48 * 64_u32).flat_map(|i| [(i % 251) as u8, (i % 13 * 19) as u8, 200, 255]).collect();
    let texture = renderer.create_texture(&TextureDesc::colour(UVec2::new(48, 64)), Some(&rgba)).unwrap();
    let mut scene =
        Scene::new(Camera::look_at(Vec3::new(1.5, 1.0, 2.5), Vec3::ZERO, Vec3::Y, Camera::default().projection));
    let quality = Quality::live();
    let mut frame = |renderer: &mut omuzaki_render::WgpuRenderer, index: u64| {
        scene.clear();
        scene.frame.index = index;
        let fit = if index.is_multiple_of(2) { BackdropFit::Cover } else { BackdropFit::Contain };
        let shade = (index % 200) as u8;
        let (top, bottom) = (Colour::rgb(shade, 20, 60), Colour::BLACK);
        scene.background = if index.is_multiple_of(5) {
            Background::Gradient { top, bottom }
        } else {
            Background::Texture { texture, fit, top, bottom }
        };
        scene.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
        renderer.render(target, &scene, &quality).unwrap();
    };
    for i in 0..30 {
        frame(&mut renderer, i);
        renderer.wait_idle().unwrap();
    }
    // The gradient frames let the texture's group go and the next texture frame makes it again: steady state is texture
    // frames only.
    let totals = counting::measure(|| (31..51).filter(|i| i % 5 != 0).for_each(|i| frame(&mut renderer, i)));
    println!("allocations in 16 frames by owner: {totals:?}");
    for owner in [counting::OWN, counting::WORKSPACE, counting::TESTS, counting::UNATTRIBUTED] {
        assert_eq!(totals.get(owner).copied().unwrap_or(0), 0, "{owner} allocated in 16 frames: {totals:?}");
    }
}
