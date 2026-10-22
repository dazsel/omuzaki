//! The bounds of `omuzaki_render_api::memory` (#2092) against what each renderer reports: for every kind of mesh and
//! texture a character uploads, the growth of `ResourceStats::gpu_bytes` after creating it and drawing it once (which
//! sizes a morphed instance's copy) is never above its bound, and the bound is never more than twice that growth, so a
//! plan built on the bounds neither admits what does not fit nor charges far more than is used.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss)]

mod support;

use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_render_api::memory::{MATERIAL_BYTES, mesh_bytes, texture_bytes};
use omuzaki_render_api::{
    AlphaEncoding, Camera, ColourSpace, Instance, Material, MeshData, MeshOptions, MeshUsage, MorphRange, MorphSet,
    Projection, Quality, Renderer, RendererExt, Scene, TextureDesc, TextureUsage,
};
use omuzaki_render_mock::MockRenderer;

/// The renderers under test, by name.
fn renderers() -> [(&'static str, Box<dyn Renderer>); 2] {
    [("GPU", Box::new(support::renderer())), ("mock", Box::new(MockRenderer::new()))]
}

fn plain() -> Quality {
    Quality { samples: 1, render_scale: 1.0, post_effects: false, ..Quality::offline() }
}

/// What `gpu_bytes` grows by when `mesh` is created with `options` and drawn once by one instance, which blends its
/// morph targets if it has any. The material is created first, so it is not counted.
fn mesh_growth(r: &mut dyn Renderer, mesh: &MeshData, options: &MeshOptions) -> u64 {
    let material = r.create_material(&Material::toon(omuzaki_render_api::Colour::WHITE)).unwrap();
    let before = r.stats().gpu_bytes;
    let id = r.create_mesh_with(mesh, options).unwrap();
    let mut scene = Scene::new(Camera::look_at(
        Vec3::new(0.0, 0.0, 3.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 20.0 },
    ));
    let mut instance = Instance::new(id, material, Mat4::IDENTITY);
    if let Some(set) = &options.morphs {
        instance.morph = Some(MorphRange { start: 0, len: u32::try_from(set.len()).unwrap() });
        scene.morph_weights = vec![0.5; set.len()];
    }
    scene.instances = vec![instance];
    r.render_image(UVec2::splat(32), AlphaEncoding::Straight, &scene, &plain()).unwrap();
    r.stats().gpu_bytes - before
}

/// A grid of `side` by `side` vertices in the XY plane, facing +Z.
fn grid(side: u32) -> MeshData {
    let mut mesh = MeshData::default();
    for y in 0..side {
        for x in 0..side {
            let at = |v: u32| v as f32 / (side - 1) as f32 - 0.5;
            mesh.positions.push(Vec3::new(at(x), at(y), 0.0));
            mesh.normals.push(Vec3::Z);
        }
    }
    for y in 0..side - 1 {
        for x in 0..side - 1 {
            let i = y * side + x;
            mesh.indices.extend([i, i + 1, i + side, i + 1, i + side + 1, i + side]);
        }
    }
    mesh
}

/// Two targets: one moving every vertex, one moving every fourth, each with normals.
fn morphs(mesh: &MeshData) -> MorphSet {
    let n = mesh.positions.len();
    let all = vec![Vec3::Z * 0.1; n];
    let some: Vec<Vec3> = (0..n).map(|i| if i % 4 == 0 { Vec3::X * 0.1 } else { Vec3::ZERO }).collect();
    let normals = vec![Vec3::X * 0.01; n];
    MorphSet::from_dense(
        n,
        &[(Slug::new("all").unwrap(), &all, &normals), (Slug::new("some").unwrap(), &some, &[])],
        0.0,
    )
    .unwrap()
}

#[test]
fn every_kind_of_mesh_stays_within_its_bound() {
    let mesh = grid(64);
    let kinds = [
        ("static", MeshOptions::default()),
        ("dynamic", MeshOptions { usage: MeshUsage::Dynamic, morphs: None }),
        ("static, morphed", MeshOptions { usage: MeshUsage::Static, morphs: Some(morphs(&mesh)) }),
        ("dynamic, morphed", MeshOptions { usage: MeshUsage::Dynamic, morphs: Some(morphs(&mesh)) }),
    ];
    for (kind, options) in &kinds {
        // A fresh renderer for each: nothing released lingers in the figure, and no pooled copy is reused.
        for (name, mut r) in renderers() {
            let growth = mesh_growth(r.as_mut(), &mesh, options);
            let bound = mesh_bytes(&mesh, options);
            println!("{name}, {kind} mesh of 4096 vertices: {growth} bytes, bound {bound}");
            assert!(growth <= bound, "{name}, {kind}: {growth} bytes is above the bound {bound}");
            assert!(bound <= 2 * growth, "{name}, {kind}: the bound {bound} is more than twice {growth} bytes");
        }
    }
}

#[test]
fn every_kind_of_texture_and_a_material_stay_within_their_bounds() {
    let colour = TextureDesc::colour(UVec2::splat(4096));
    let mut painted = TextureDesc::colour(UVec2::new(512, 256));
    (painted.usage, painted.mips) = (TextureUsage::Dynamic, false);
    let mut painted_mipped = painted;
    painted_mipped.mips = true;
    let mut premultiplied = painted;
    premultiplied.alpha = AlphaEncoding::Premultiplied;
    let mut data = TextureDesc::colour(UVec2::new(300, 200));
    (data.colour_space, data.mips) = (ColourSpace::Linear, false);
    let kinds = [
        ("4096 colour, mipped", colour),
        ("painted straight", painted),
        ("painted straight, mipped", painted_mipped),
        ("painted premultiplied", premultiplied),
        ("linear data", data),
    ];
    for (name, mut r) in renderers() {
        for (kind, desc) in &kinds {
            let before = r.stats().gpu_bytes;
            // Kept, not released: a released texture may linger in the figure for the frames in flight.
            r.create_texture(desc, None).unwrap();
            let growth = r.stats().gpu_bytes - before;
            let bound = texture_bytes(desc);
            println!("{name}, {kind} texture: {growth} bytes, bound {bound}");
            assert!(growth <= bound, "{name}, {kind}: {growth} bytes is above the bound {bound}");
            assert!(bound <= 2 * growth, "{name}, {kind}: the bound {bound} is more than twice {growth} bytes");
        }
        let before = r.stats().gpu_bytes;
        r.create_material(&Material::toon(omuzaki_render_api::Colour::WHITE)).unwrap();
        let growth = r.stats().gpu_bytes - before;
        assert!(growth <= MATERIAL_BYTES, "{name}: a material's {growth} bytes is above {MATERIAL_BYTES}");
    }
}
