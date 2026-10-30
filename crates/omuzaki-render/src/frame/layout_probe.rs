//! Proves the Rust uniform structs and the WGSL structs in `shader/common.wgsl` describe the same bytes.
//!
//! A sized-and-aligned mismatch would not fail loudly: a field would quietly read its neighbour's value. So this test
//! binds the real frame uniform, instance buffer and material uniform, runs a fragment shader that returns every
//! vec4-sized chunk of each WGSL struct as raw bits (one pixel per chunk, into an `Rgba32Uint` target), reads them
//! back, and compares them with the bytes the Rust side wrote. It runs on the software adapter in CI.

use bytemuck::{bytes_of, cast_slice};
use omuzaki_core::glam::{Mat4, Quat, UVec2, Vec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{
    AlphaEncoding, AlphaMode, Camera, Colour, Fog, Instance, JointRange, Light, Mapping, Material, MaterialId, MeshId,
    Mtoon, Outline, OutlineStyle, OutlineWidthMode, Overlay, ParamValue, Rim, Scene, Shading, Skin, TextureDesc,
    TextureId, Toon, UvTransform,
};

use super::FrameState;
use super::uniform::{FrameUniform, InstanceUniform, frame_uniform, instance_uniform};
use crate::gpu::{AdapterPreference, Gpu, HeadlessOptions};
use crate::resource::Resources;
use crate::resource::material::{GpuMaterial, MaterialUniform};
use crate::resource::mesh::GpuMesh;
use crate::resource::texture::GpuTexture;
use crate::shared::Shared;

/// Chunks of each struct: vec4s in declaration order (matrices as four columns, arrays element by element).
const FRAME_CHUNKS: usize = 12 + 3 + 32 + 4;
/// Chunks before the program parameters: the nine leading vec4s and the four overlays' two each.
const PROGRAM_START: usize = 9 + 8;
/// Chunks of the program parameters (`program: array<vec4<f32>, 12>`).
const PROGRAM_CHUNKS: usize = 12;
// Then the UV transform, the outline mix and the six of MToon.
const MATERIAL_CHUNKS: usize = PROGRAM_START + PROGRAM_CHUNKS + 2 + 6;
const INSTANCE_CHUNKS: usize = 4 + 3 + 1;

const PROBE: &str = r"
fn bits(v: vec4<f32>) -> vec4<u32> { return bitcast<vec4<u32>>(v); }

fn frame_chunk(i: u32) -> vec4<u32> {
    if (i < 4u) { return bits(frame.view[i]); }
    if (i < 8u) { return bits(frame.proj[i - 4u]); }
    if (i < 12u) { return bits(frame.view_proj[i - 8u]); }
    if (i == 12u) { return bits(frame.camera_position); }
    if (i == 13u) { return bits(frame.camera_back); }
    if (i == 14u) { return bits(frame.target_size); }
    if (i < 47u) {
        let light = frame.lights[(i - 15u) / 4u];
        let part = (i - 15u) % 4u;
        if (part == 0u) { return bits(light.vector); }
        if (part == 1u) { return bits(light.colour); }
        if (part == 2u) { return bits(light.ground); }
        return bits(light.axis);
    }
    if (i == 47u) { return frame.counts; }
    if (i == 48u) { return bits(frame.look); }
    if (i == 49u) { return bits(frame.fog_colour); }
    return bits(frame.fog_far);
}

fn material_chunk(i: u32) -> vec4<u32> {
    switch (i) {
        case 0u: { return bits(material.base); }
        case 1u: { return bits(material.shade); }
        case 2u: { return bits(material.rim); }
        case 3u: { return bits(material.emissive); }
        case 4u: { return bits(material.outline); }
        case 5u: { return bits(material.toon); }
        case 6u: { return material.flags; }
        case 7u: { return bits(material.alpha); }
        case 8u: { return material.textures; }
        default: {}
    }
    if (i < 17u) {
        let overlay = material.overlays[(i - 9u) / 2u];
        if ((i - 9u) % 2u == 0u) { return bits(overlay.rect); }
        return bits(overlay.params);
    }
    if (i < 29u) { return bits(material.program[i - 17u]); }
    if (i == 29u) { return bits(material.uv_transform); }
    if (i == 30u) { return bits(material.outline_mix); }
    if (i == 31u) { return material.mtoon_flags; }
    if (i == 32u) { return bits(material.mtoon_shade); }
    if (i == 33u) { return bits(material.mtoon_shading); }
    if (i == 34u) { return bits(material.mtoon_rim); }
    if (i == 35u) { return bits(material.mtoon_matcap); }
    return bits(material.mtoon_uv);
}

fn instance_chunk(i: u32) -> vec4<u32> {
    let inst = instances[0];
    if (i < 4u) { return bits(inst.model[i]); }
    switch (i) {
        case 4u: { return bits(inst.normal_x); }
        case 5u: { return bits(inst.normal_y); }
        case 6u: { return bits(inst.normal_z); }
        default: { return inst.skin; }
    }
}

@vertex
fn vs_probe(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_probe(@builtin(position) position: vec4<f32>) -> @location(0) vec4<u32> {
    let x = u32(position.x);
    if (x < FRAME_CHUNKS) { return frame_chunk(x); }
    if (x < FRAME_CHUNKS + MATERIAL_CHUNKS) { return material_chunk(x - FRAME_CHUNKS); }
    return instance_chunk(x - FRAME_CHUNKS - MATERIAL_CHUNKS);
}
";

fn chunks<T: bytemuck::Pod>(value: &T) -> Vec<[u32; 4]> {
    cast_slice::<u8, [u32; 4]>(bytes_of(value)).to_vec()
}

/// A material that sets every field to something distinguishable, reading `face` as its base texture and overlays.
fn probe_material(face: TextureId) -> Material {
    Material {
        base: Colour::rgba(200, 100, 50, 180),
        base_texture: Some(face),
        shading: Shading::Toon(Toon {
            shade: Colour::rgb(150, 140, 160),
            shift: -0.25,
            softness: 0.3,
            rim: Rim { colour: Colour::rgb(100, 110, 170), power: 2.5, strength: 0.4 },
            emissive: Colour::rgb(20, 10, 30),
            skin: Some(Skin { floor: 0.35, hue_keep: 0.65 }),
        }),
        overlays: vec![
            Overlay { texture: face, mapping: Mapping::Uv, strength: 0.5 },
            Overlay {
                texture: face,
                mapping: Mapping::Planar {
                    min: Vec2::new(-0.2, -0.3),
                    max: Vec2::new(0.4, 0.5),
                    fade_from: 0.01,
                    fade_to: 0.07,
                },
                strength: 0.85,
            },
        ],
        alpha: AlphaMode::Mask { cutoff: 0.45 },
        order_independent: false,
        double_sided: true,
        vertex_colours: true,
        outline: Some(Outline { colour: Colour::rgb(40, 30, 60), width: 0.0123 }),
        outline_style: OutlineStyle {
            width_mode: OutlineWidthMode::Screen,
            width_texture: Some(face),
            lighting_mix: 0.375,
        },
        receives_shadows: false,
        uv_transform: UvTransform { scale: Vec2::new(0.5, 0.25), offset: Vec2::new(0.125, 0.75) },
        mtoon: None,
    }
}

/// The toon probe lit by `MToon`, every `MToon` field off its default and every `MToon` texture `face`.
fn mtoon_probe_material(face: TextureId) -> Material {
    let mtoon = Mtoon {
        shade: Colour::rgb(30, 60, 90),
        shade_texture: Some(face),
        shading_shift: -0.15,
        shading_shift_texture: Some(face),
        shading_shift_scale: 0.7,
        shading_toony: 0.35,
        gi_equalisation: 0.45,
        matcap: Colour::rgb(210, 190, 170),
        matcap_texture: Some(face),
        rim: Colour::rgb(90, 80, 70),
        rim_texture: Some(face),
        rim_fresnel_power: 3.5,
        rim_lift: 0.15,
        rim_lighting_mix: 0.65,
        emissive_texture: Some(face),
        normal_texture: Some(face),
        normal_scale: 0.8,
        uv_scroll: Vec2::new(0.11, -0.22),
        uv_rotation: 0.33,
        uv_animation_mask: Some(face),
        blend_writes_depth: true,
    };
    Material { mtoon: Some(Box::new(mtoon)), ..probe_material(face) }
}

/// A scene with one light of each kind and an instance whose normal matrix is not its model matrix.
fn probe_scene(mesh: MeshId, material: MaterialId) -> Scene {
    let eye = Vec3::new(1.0, 2.0, 3.0);
    let mut scene = Scene::new(Camera::look_at(eye, Vec3::ZERO, Vec3::Y, Camera::default().projection));
    scene.frame.index = 0x0000_0002_0000_0005;
    scene.frame.time = Seconds(1.5);
    scene.outline_light = 0.7;
    scene.lights = vec![
        Light::Directional {
            to_light: Vec3::new(0.2, 1.0, 0.4),
            colour: Colour::rgb(255, 240, 220),
            intensity: 1.7,
            casts_shadow: true,
        },
        Light::Hemisphere { sky: Colour::rgb(200, 220, 255), ground: Colour::rgb(60, 40, 30), intensity: 0.8 },
        Light::Point { position: eye, colour: Colour::rgb(255, 200, 100), intensity: 2.2, range: 4.5 },
        Light::Spot {
            position: Vec3::new(0.5, 2.0, 1.0),
            direction: Vec3::new(-0.2, -1.0, -0.4),
            colour: Colour::rgb(255, 230, 200),
            intensity: 3.1,
            range: 6.5,
            cone_outer_deg: 40.0,
            penumbra: 0.35,
            casts_shadow: false,
        },
    ];
    scene.fog = Some(Fog { colour: Colour::rgb(90, 110, 140), near: 1.5, far: 8.0 });
    let transform = Mat4::from_scale_rotation_translation(
        Vec3::new(2.0, 1.0, 0.5),
        Quat::from_rotation_y(0.7),
        Vec3::new(0.3, -0.2, 0.1),
    );
    let mut instance = Instance::new(mesh, material, transform);
    instance.skin = Some(JointRange { start: 3, len: 2 });
    scene.instances.push(instance);
    scene.joints = vec![Mat4::IDENTITY; 5];
    scene
}

/// Which struct a chunk belongs to, and its position within it.
fn locate(chunk: usize) -> (&'static str, usize) {
    if chunk < FRAME_CHUNKS {
        ("frame", chunk)
    } else if chunk < FRAME_CHUNKS + MATERIAL_CHUNKS {
        ("material", chunk - FRAME_CHUNKS)
    } else {
        ("instance", chunk - FRAME_CHUNKS - MATERIAL_CHUNKS)
    }
}

/// A material that names the `hair` program with parameters of every type it declares set off their defaults, so
/// `program[]` (all zeros for a toon material) carries distinguishable values.
fn hair_probe_material() -> Material {
    let params = [
        ("root", ParamValue::Colour(Colour::rgb(10, 60, 110))),
        ("ring", ParamValue::Colour(Colour::rgb(200, 150, 100))),
        ("ring-strength", ParamValue::Float(0.31)),
        ("keep-identity", ParamValue::Float(0.57)),
    ];
    Material {
        shading: Shading::Program {
            name: Slug::new("hair").unwrap(),
            params: params.into_iter().map(|(k, v)| (Slug::new(k).unwrap(), v)).collect(),
        },
        ..Material::toon(Colour::WHITE)
    }
}

#[test]
fn the_wgsl_structs_read_the_bytes_the_rust_structs_write() {
    let gpu = Gpu::open_headless(HeadlessOptions { adapter: AdapterPreference::Software }).unwrap();
    let shared = Shared::new(gpu);
    let mut resources = Resources::default();
    let desc = TextureDesc { alpha: AlphaEncoding::Premultiplied, ..TextureDesc::colour(UVec2::splat(2)) };
    let (index, generation) = resources.textures.insert(GpuTexture::create(&shared.gpu, &shared.mips, &desc, None));
    let face = TextureId::from_parts(index, generation);

    for (name, material) in
        [("toon", probe_material(face)), ("mtoon", mtoon_probe_material(face)), ("hair program", hair_probe_material())]
    {
        let material_uniform = MaterialUniform::new(&material, &resources.textures);
        if name == "hair program" {
            assert!(
                chunks(&material_uniform)[PROGRAM_START..PROGRAM_START + PROGRAM_CHUNKS]
                    .iter()
                    .flatten()
                    .any(|bits| *bits != 0),
                "the hair material's `program[]` must carry values for this check to mean anything"
            );
        }
        let (material_slot, material_generation) =
            resources.materials.insert(GpuMaterial::create(&shared, &resources.textures, &material));
        let quad = omuzaki_render_mock::shapes::quad(Vec2::ONE);
        let (mesh_slot, mesh_generation) = resources
            .meshes
            .insert(GpuMesh::upload(&shared.gpu, &quad, &omuzaki_render_api::MeshOptions::default(), false).unwrap());
        let scene = probe_scene(
            MeshId::from_parts(mesh_slot, mesh_generation),
            MaterialId::from_parts(material_slot, material_generation),
        );

        let internal = UVec2::new(160, 90);
        let (scene_resources, _targets) = resources.split();
        let mut frame = FrameState::new(&shared.gpu.device, &shared.layouts);
        frame.prepare(&shared, &scene, scene_resources, internal, 1, 1).unwrap();

        // The mesh is rigid, so the instance's skin flag is off: the expected record is built the way `prepare` builds it.
        let mut expected = chunks(&frame_uniform(&scene, internal, omuzaki_core::glam::Vec2::ZERO));
        expected.extend(chunks(&material_uniform));
        expected.extend(chunks(&instance_uniform(&scene.instances[0], false)));
        assert_eq!(size_of::<FrameUniform>(), FRAME_CHUNKS * 16);
        assert_eq!(size_of::<MaterialUniform>(), MATERIAL_CHUNKS * 16);
        assert_eq!(size_of::<InstanceUniform>(), INSTANCE_CHUNKS * 16);
        assert_eq!(expected.len(), FRAME_CHUNKS + MATERIAL_CHUNKS + INSTANCE_CHUNKS);

        let got = run_probe(&shared, &frame, resources.materials.at(material_slot).unwrap(), expected.len());
        for (i, (g, e)) in got.iter().zip(&expected).enumerate() {
            let (what, at) = locate(i);
            assert_eq!(
                g,
                e,
                "{name}: {what} chunk {at} (vec4 at byte {}): the shader read {g:08x?}, Rust wrote {e:08x?}",
                at * 16
            );
        }
    }
}

/// Draws one pixel per chunk into an `Rgba32Uint` target and reads the pixels back as raw words.
fn run_probe(shared: &Shared, frame: &FrameState, material: &GpuMaterial, chunks: usize) -> Vec<[u32; 4]> {
    let device = &shared.gpu.device;
    let source = format!(
        "const FRAME_CHUNKS: u32 = {FRAME_CHUNKS}u;\nconst MATERIAL_CHUNKS: u32 = {MATERIAL_CHUNKS}u;\n{}\n{PROBE}",
        include_str!("../shader/common.wgsl")
    );
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("layout probe"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("layout probe"),
        bind_group_layouts: &[Some(&shared.layouts.frame), Some(&shared.layouts.material)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("layout probe"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_probe"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_probe"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba32Uint,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    let width = u32::try_from(chunks).unwrap();
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("probe"),
        size: wgpu::Extent3d { width, height: 1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba32Uint,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let row = (chunks * 16).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize);
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("probe readback"),
        size: row as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("probe") });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("probe"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &frame.bind_groups(1)[0], &[]);
        pass.set_bind_group(1, &material.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(u32::try_from(row).unwrap()),
                rows_per_image: Some(1),
            },
        },
        wgpu::Extent3d { width, height: 1, depth_or_array_layers: 1 },
    );
    shared.gpu.queue.submit([encoder.finish()]);
    readback.slice(..).map_async(wgpu::MapMode::Read, |result| result.unwrap());
    shared.gpu.wait_idle(crate::gpu::WAIT_LIMIT).unwrap();
    let mapped = readback.slice(..).get_mapped_range().unwrap();
    cast_slice::<u8, [u32; 4]>(&mapped[..chunks * 16]).to_vec()
}
