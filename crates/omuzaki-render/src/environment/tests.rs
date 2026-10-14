//! The shader against the bake: a compute kernel evaluates `environment_light` and `environment_specular` from the real
//! bind group and the results are compared with the same maths on the CPU. This is what proves the Rust uniform and the
//! WGSL struct describe the same bytes, that the cube's faces are where `bake::face_direction` says, and that rotation
//! and intensity reach the shader. It runs on the software adapter, like the rest of the renderer's tests.

#![allow(clippy::cast_precision_loss, clippy::chunks_exact_to_as_chunks)] // test arithmetic on tiny grids

use std::f32::consts::{PI, TAU};

use omuzaki_core::glam::{Mat3, UVec2, Vec3};
use omuzaki_render_api::{EnvironmentDesc, EnvironmentUse};

use super::{GpuEnvironment, layout};
use crate::gpu::{AdapterPreference, Gpu, HeadlessOptions, WAIT_LIMIT};

const KERNEL: &str = r"
@group(0) @binding(0) var<storage, read> dirs: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> out: array<vec4<f32>>;

// The material's ramp lives in toon.wgsl; a fixed one stands in so the blend is checkable.
fn terminator(x: f32) -> f32 { return clamp(x * 0.5 + 0.5, 0.0, 1.0); }

@compute @workgroup_size(1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    let d = normalize(dirs[i].xyz);
    out[4u * i] = vec4<f32>(environment_light(d), 1.0);
    out[4u * i + 1u] = vec4<f32>(environment_specular(d, 0.0), 1.0);
    out[4u * i + 2u] = vec4<f32>(environment_specular(d, 1.0), 1.0);
    out[4u * i + 3u] = vec4<f32>(sky_direction(d), 1.0);
}
";

/// What the kernel returned for one direction.
struct Probe {
    light: Vec3,
    mirror: Vec3,
    rough: Vec3,
    sky: Vec3,
}

fn gpu() -> Gpu {
    Gpu::open_headless(HeadlessOptions { adapter: AdapterPreference::Software }).expect("a software adapter")
}

/// Evaluates the shader functions of `env` for `dirs`, after writing `using` into its uniform.
fn run(gpu: &Gpu, env: &GpuEnvironment, using: Option<&EnvironmentUse>, dirs: &[Vec3]) -> Vec<Probe> {
    let device = &gpu.device;
    let storage = |read_only| wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let io = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("probe io"),
        entries: &[storage(true), wgpu::BindGroupLayoutEntry { binding: 1, ..storage(false) }],
    });
    let env_layout = layout(device);
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("environment probe"),
        source: wgpu::ShaderSource::Wgsl(format!("{}\n{KERNEL}", include_str!("../shader/environment.wgsl")).into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("environment probe"),
        bind_group_layouts: &[Some(&io), None, None, Some(&env_layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("environment probe"),
        layout: Some(&pipeline_layout),
        module: &module,
        entry_point: Some("main"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    });
    // `env` was built against its own layout object; a layout made by the same descriptor is compatible.
    let inputs: Vec<[f32; 4]> = dirs.iter().map(|d| [d.x, d.y, d.z, 0.0]).collect();
    let make = |size: u64, usage| {
        device.create_buffer(&wgpu::BufferDescriptor { label: Some("probe"), size, usage, mapped_at_creation: false })
    };
    let input = make((inputs.len() * 16) as u64, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST);
    let output = make((inputs.len() * 64) as u64, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC);
    let readback = make((inputs.len() * 64) as u64, wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ);
    gpu.queue.write_buffer(&input, 0, bytemuck::cast_slice(&inputs));
    if let Some(using) = using {
        env.apply(&gpu.queue, using);
    }
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("probe io"),
        layout: &io,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
        ],
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("probe") });
    {
        let mut pass =
            encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("probe"), timestamp_writes: None });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.set_bind_group(3, env.bind_group(), &[]);
        pass.dispatch_workgroups(u32::try_from(inputs.len()).unwrap(), 1, 1);
    }
    encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, (inputs.len() * 64) as u64);
    gpu.queue.submit([encoder.finish()]);
    readback.slice(..).map_async(wgpu::MapMode::Read, |result| result.unwrap());
    gpu.wait_idle(WAIT_LIMIT).unwrap();
    let mapped = readback.slice(..).get_mapped_range().unwrap();
    let words: &[[f32; 4]] = bytemuck::cast_slice(&mapped);
    words
        .chunks_exact(4)
        .map(|c| {
            let v = |i: usize| Vec3::new(c[i][0], c[i][1], c[i][2]);
            Probe { light: v(0), mirror: v(1), rough: v(2), sky: v(3) }
        })
        .collect()
}

fn create(gpu: &Gpu, desc: &EnvironmentDesc) -> GpuEnvironment {
    GpuEnvironment::create(gpu, &layout(&gpu.device), desc).unwrap()
}

/// An equirectangular map whose colour is `0.5 + 0.5 * direction`: smooth, and different towards every direction, so a
/// swapped, mirrored or flipped face shows at once.
fn direction_map() -> EnvironmentDesc {
    let size = UVec2::new(256, 128);
    let mut rgb = Vec::new();
    for y in 0..size.y {
        for x in 0..size.x {
            let phi = ((x as f32 + 0.5) / size.x as f32 - 0.5) * TAU;
            let elevation = (0.5 - (y as f32 + 0.5) / size.y as f32) * PI;
            let d = Vec3::new(elevation.cos() * phi.cos(), elevation.sin(), elevation.cos() * phi.sin());
            rgb.extend((0.5 + 0.5 * d).to_array());
        }
    }
    EnvironmentDesc::Hdri { size, rgb }
}

/// A grey sky with a bright patch towards +Z, 45 degrees up.
fn sun_map() -> EnvironmentDesc {
    let size = UVec2::new(128, 64);
    let rgb = (0..size.y)
        .flat_map(|y| (0..size.x).map(move |x| (x, y)))
        .flat_map(|(x, y)| if (95..98).contains(&x) && (15..18).contains(&y) { [50.0; 3] } else { [0.2; 3] })
        .collect();
    EnvironmentDesc::Hdri { size, rgb }
}

const DIRECTIONS: [Vec3; 14] = [
    Vec3::X,
    Vec3::NEG_X,
    Vec3::Y,
    Vec3::NEG_Y,
    Vec3::Z,
    Vec3::NEG_Z,
    Vec3::new(1.0, 1.0, 0.0),
    Vec3::new(-1.0, 0.5, 1.0),
    Vec3::new(0.3, -1.0, 0.7),
    Vec3::new(-1.0, -1.0, -1.0),
    Vec3::new(1.0, 0.2, -1.0),
    Vec3::new(0.0, 1.0, 1.0),
    Vec3::new(0.0, 1.0, -1.0),
    Vec3::new(-0.4, 0.1, 0.9),
];

/// The nine-harmonic irradiance from the coefficients the bake packed (the maths of `sh_irradiance`).
fn cpu_irradiance(packed: &[[f32; 4]; 9], n: Vec3) -> Vec3 {
    let basis = [
        0.282_095,
        0.488_603 * n.y,
        0.488_603 * n.z,
        0.488_603 * n.x,
        1.092_548 * n.x * n.y,
        1.092_548 * n.y * n.z,
        0.315_392 * (3.0 * n.z * n.z - 1.0),
        1.092_548 * n.x * n.z,
        0.546_274 * (n.x * n.x - n.y * n.y),
    ];
    let sum: Vec3 = packed.iter().zip(basis).map(|(c, b)| Vec3::new(c[0], c[1], c[2]) * b).sum();
    sum.max(Vec3::ZERO)
}

fn assert_near(got: Vec3, want: Vec3, tolerance: f32, what: &str) {
    assert!((got - want).abs().max_element() <= tolerance, "{what}: got {got}, wanted {want}");
}

#[test]
fn diffuse_light_is_the_harmonics_turned_and_scaled() {
    let gpu = gpu();
    let env = create(&gpu, &sun_map());
    let using = EnvironmentUse {
        intensity: 0.7,
        rotation_deg: 30.0,
        ..EnvironmentUse::new(omuzaki_render_api::EnvironmentId::from_parts(0, 1))
    };
    let to_sky = Mat3::from_rotation_y(-30.0_f32.to_radians());
    for (d, probe) in DIRECTIONS.iter().zip(run(&gpu, &env, Some(&using), &DIRECTIONS)) {
        let n = d.normalize();
        assert_near(probe.sky, to_sky * n, 1e-5, "the sky direction");
        let want = cpu_irradiance(&env.base.sh, to_sky * n) * 0.7;
        assert_near(probe.light, want, 1e-3 * want.max_element().max(1.0), "diffuse light");
    }
}

#[test]
fn the_toon_ramp_blends_the_two_sides_of_the_sky() {
    let gpu = gpu();
    let env = create(&gpu, &sun_map());
    let using = EnvironmentUse {
        intensity: 2.0,
        toon_ramp: true,
        ..EnvironmentUse::new(omuzaki_render_api::EnvironmentId::from_parts(0, 1))
    };
    let (toward, dark, bright) = (
        Vec3::new(env.base.toward[0], env.base.toward[1], env.base.toward[2]),
        Vec3::new(env.base.dark[0], env.base.dark[1], env.base.dark[2]),
        Vec3::new(env.base.bright[0], env.base.bright[1], env.base.bright[2]),
    );
    for (d, probe) in DIRECTIONS.iter().zip(run(&gpu, &env, Some(&using), &DIRECTIONS)) {
        let ramp = (0.5 + 0.5 * d.normalize().dot(toward)).clamp(0.0, 1.0);
        assert_near(probe.light, dark.lerp(bright, ramp) * 2.0, 1e-3 * bright.max_element() * 2.0, "banded light");
    }
}

#[test]
fn the_specular_cube_has_its_faces_where_the_bake_put_them() {
    let gpu = gpu();
    let env = create(&gpu, &direction_map());
    // The map is 0.5 + 0.5 * direction, so a mirror lookup towards d reads that colour from whichever face d falls on.
    for (d, probe) in DIRECTIONS.iter().zip(run(
        &gpu,
        &env,
        Some(&EnvironmentUse {
            intensity: 1.0,
            ..EnvironmentUse::new(omuzaki_render_api::EnvironmentId::from_parts(0, 1))
        }),
        &DIRECTIONS,
    )) {
        assert_near(probe.mirror, 0.5 + 0.5 * d.normalize(), 0.06, "the mirror level");
        assert!(probe.rough.is_finite() && probe.rough.min_element() >= 0.0, "the roughest level is a plain colour");
    }
    // Turning the sky by 90 degrees about +Y moves what each direction sees: a direction reads the sky at to_sky * d.
    let turned = EnvironmentUse {
        rotation_deg: 90.0,
        ..EnvironmentUse::new(omuzaki_render_api::EnvironmentId::from_parts(0, 1))
    };
    let to_sky = Mat3::from_rotation_y(-90.0_f32.to_radians());
    for (d, probe) in DIRECTIONS.iter().zip(run(&gpu, &env, Some(&turned), &DIRECTIONS)) {
        assert_near(probe.mirror, 0.5 + 0.5 * (to_sky * d.normalize()), 0.06, "the turned mirror level");
    }
}

#[test]
fn the_stand_in_environment_lights_nothing() {
    let gpu = gpu();
    let none = GpuEnvironment::none(&gpu, &layout(&gpu.device));
    for probe in run(&gpu, &none, None, &DIRECTIONS) {
        assert_eq!((probe.light, probe.mirror, probe.rough), (Vec3::ZERO, Vec3::ZERO, Vec3::ZERO));
    }
}

#[test]
fn sizes_are_checked_and_counted() {
    // 6 faces of 8-byte texels over 64, 32, 16, 8 and 4 texels a side.
    assert_eq!(super::cube_bytes(64, 5).unwrap(), 6 * 8 * (4096 + 1024 + 256 + 64 + 16));
    assert!(super::cube_bytes(u32::MAX, 1).is_err(), "a four-billion-texel side overflows 64 bits and says so");
}
