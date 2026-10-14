//! What a frame with hair allocates after warm-up (spec §8, "no allocation in the hot loop"). The method and its
//! limits are those of `hot_loop.rs`: wgpu allocates inside its own calls, so the strict check is that none of the
//! allocations come from `omuzaki-render` itself. The hair program binds extra vertex streams and picks a pipeline per
//! program and per set of attributes the mesh has, so the scene mixes a skinned hair mesh with all its attributes, one
//! with none, and plain toon props.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)]

mod support;
// The counting allocator is installed only in this test binary, so the other tests run on the plain allocator.
#[path = "support/counting.rs"]
mod counting;

use std::collections::BTreeMap;

use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_render::WgpuRenderer;
use omuzaki_render_api::{
    AlphaEncoding, Camera, Colour, ExtraAttribute, Instance, JointRange, Material, MaterialId, MeshData, MeshId,
    ParamValue, Pass, Quality, Renderer, Scene, Shading, TargetDesc, TargetId,
};
use omuzaki_render_mock::shapes;

struct Fixture {
    renderer: WgpuRenderer,
    scene: Scene,
    quality: Quality,
    target: TargetId,
    /// Skinned hair with both attributes, hair with none, a plain prop.
    meshes: [MeshId; 3],
    hair: MaterialId,
    toon: MaterialId,
}

/// A cuboid carrying the hair program's two attributes.
fn with_hair_attributes(mut mesh: MeshData) -> MeshData {
    let n = mesh.positions.len();
    let attribute = |name: &str, components: u8| ExtraAttribute {
        name: Slug::new(name).unwrap(),
        components,
        data: (0..n * usize::from(components)).map(|i| (i % 7) as f32 / 7.0).collect(),
    };
    mesh.extra = vec![attribute("hair-uv", 2), attribute("hair-info", 3)];
    mesh
}

fn fixture() -> Fixture {
    let mut renderer = support::renderer();
    let mut skinned = with_hair_attributes(shapes::cuboid(Vec3::splat(0.2)));
    skinned.joints = vec![[0, 1, 0, 0]; skinned.positions.len()];
    skinned.weights = vec![[0.5, 0.5, 0.0, 0.0]; skinned.positions.len()];
    let meshes = [
        renderer.create_mesh(&skinned).unwrap(),
        renderer.create_mesh(&shapes::cuboid(Vec3::splat(0.1))).unwrap(),
        renderer.create_mesh(&shapes::cuboid(Vec3::splat(0.1))).unwrap(),
    ];
    let hair = Material {
        shading: Shading::Program {
            name: Slug::new("hair").unwrap(),
            params: [(Slug::new("ring-strength").unwrap(), ParamValue::Float(0.4))].into(),
        },
        double_sided: true,
        ..Material::toon(Colour::WHITE)
    };
    let hair = renderer.create_material(&hair).unwrap();
    let toon = renderer.create_material(&Material::toon(Colour::hex(0x00c9_b8f0))).unwrap();
    let target =
        renderer.create_target(&TargetDesc { size: UVec2::new(64, 64), alpha: AlphaEncoding::Straight }).unwrap();
    let scene =
        Scene::new(Camera::look_at(Vec3::new(2.0, 1.5, 3.0), Vec3::ZERO, Vec3::Y, Camera::default().projection));
    let quality = Quality { render_scale: 1.5, ..Quality::high() };
    Fixture { renderer, scene, quality, target, meshes, hair, toon }
}

impl Fixture {
    /// One frame the way the stage does it: clear the scene, refill it, render.
    fn frame(&mut self, index: u64) -> bool {
        let Self { renderer, scene, quality, target, meshes, hair, toon } = self;
        scene.clear();
        scene.frame.index = index;
        let twist = f32::from(u16::try_from(index % 1000).unwrap()) * 0.05;
        scene.joints.extend((0..16u8).map(|j| Mat4::from_rotation_y(twist * f32::from(j % 2))));
        let mut character = Instance::new(meshes[0], *hair, Mat4::IDENTITY);
        character.skin = Some(JointRange { start: 0, len: 16 });
        scene.instances.push(character);
        scene.instances.extend((0..20u8).map(|i| {
            let (mesh, material) = if i % 2 == 0 { (meshes[1], *hair) } else { (meshes[2], *toon) };
            Instance::new(mesh, material, Mat4::from_translation(Vec3::new(f32::from(i) * 0.1 - 1.0, -0.5, 0.0)))
        }));
        renderer.render(*target, scene, quality).unwrap().cpu.get(Pass::Opaque).is_some()
    }
}

/// Renders `frames` frames after a warm-up and returns each owner's allocations per frame.
fn allocations_per_frame(frames: u64) -> BTreeMap<&'static str, usize> {
    let mut f = fixture();
    // Warm-up: pipelines compile, buffers grow, and the GPU timing slots cycle through their states.
    for i in 0..30 {
        assert!(f.frame(i), "the Opaque pass must run in this scene");
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
fn a_frame_with_hair_allocates_nothing_in_the_renderers_own_code() {
    if !counting::can_attribute() {
        println!("no source paths in backtraces (a build without debug info): skipping");
        return;
    }
    let per_frame = allocations_per_frame(20);
    println!("allocations per frame by owner: {per_frame:?}");
    for owner in ["omuzaki-render", "render-api", "test and mock", "unattributed"] {
        assert_eq!(per_frame.get(owner).copied().unwrap_or(0), 0, "{owner} allocated per frame: {per_frame:?}");
    }
}
