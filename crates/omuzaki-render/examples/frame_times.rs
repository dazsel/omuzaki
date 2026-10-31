//! Per-pass frame times: `cargo run --release --example frame_times -- [--size 1920x1080] [--frames 600] [--samples 4]
//! [--scale 1.0] [--triangles 130000] [--props 20] [--paint 0] [--outlines] [--blended] [--hair] [--post none] [--offline]
//! [--shadows off|low|high] [--keys 1] [--interval 2] [--environment sky|hdri] [--toon-ramp] [--spot] [--fog] [--software]`.
//!
//! Renders a stage-like scene (one dense sphere standing in for a character, some props, a light rig) for a number of
//! frames after a warm-up and prints, per pass, the CPU time spent recording and the GPU time from timestamp queries
//! (mean, median and 95th percentile), plus the whole-frame figures. It is how the numbers in the pull request were
//! taken: on the real GPU, or with `--software` on the CI rasteriser.
//!
//! `--post bloom,dof,grain,grade` (or `all`) turns those effects on, with the prototype's bloom numbers, a focus on the
//! character, a light grain and a mild grade, and `Quality::post_effects` with them; `--offline` runs them in the
//! frame-exact mode (more depth-of-field taps). The preset names are `--post none` with `--samples 4 --scale 1` (live)
//! and `--post all --scale 1.5` (high).
//!
//! `--paint N` repaints an N x N dynamic texture (a face atlas stand-in, drawn on a prop) through the portable
//! `write_rgba` path every frame, so the `PaintTextures` pass has work.
//!
//! `--hair` draws the body with the `hair` shading program and its two attributes instead of toon: the worst case for the
//! program (every pixel of the 130 000-triangle body runs the strand and ring maths, both faces drawn). Run it with and
//! without and subtract the `opaque` GPU times to see what the program costs.
//!
//! `--blended` draws the props half transparent (`AlphaMode::Blend`), so the `Transparent` pass sorts and draws them;
//! run it with and without to see what the pass costs (the `transparent` rows).
//!
//! `--outlines` gives the body and the props an outline (3 mm at 2 m, the widest line on the sample character is 2.6 mm), so the
//! `Outlines` pass draws a second hull of every triangle; run it with and without to see what the pass costs.
//!
//! `--shadows low|high` makes the key light cast (the live and the quality preset's shadow maps, both fitted to the whole
//! scene), and `--interval N` is the live redraw interval: the `shadows` row then averages the frames that redrew, so
//! the cost per frame is that figure divided by `N`. `--keys N` adds casting keys from other sides (up to four in all),
//! each with its own map at `high`: run with `--keys 1` and `--keys 2` to see what one more map costs.
//!
//! `--environment sky|hdri` lights the scene with an environment as well (a procedural sky, or a synthesised 1024 x 512
//! HDRI with a sun) and prints what creating it cost; `--toon-ramp` sends its diffuse light through the toon ramp. Run it
//! with and without to see what image-based light costs a frame.
//!
//! `--spot` adds a spot light on the body (50 degree cone, half penumbra) and `--fog` a linear fog from 1.5 to 4 m: run
//! with and without to see what the lighting and fog cost on top of the rig.
//!
//! `WGPU_ADAPTER_NAME` and `WGPU_BACKEND` pick the GPU, as for every program built on this crate.

// A benchmark's statistics and procedural mesh use small counts and sizes in float maths; the casts are exact enough.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::time::Instant;

use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_core::time::Seconds;
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_render_api::{
    AlphaEncoding, AlphaMode, Background, Bloom, Camera, Colour, ColourGrade, DepthOfField, EnvironmentDesc,
    EnvironmentUse, ExtraAttribute, FilmGrain, Fog, Instance, Light, Mapping, Material, MeshData, Outline, Overlay,
    Pass, PassTimes, PixelRect, ProgramParams, Quality, QualityMode, Renderer, RendererExt, Scene, Shading,
    ShadowQuality, TargetDesc, TextureDesc,
};

// One flag per switch of the command line.
#[allow(clippy::struct_excessive_bools)]
struct Args {
    size: UVec2,
    frames: usize,
    samples: u8,
    scale: f32,
    triangles: u32,
    props: u32,
    paint: u32,
    outlines: bool,
    blended: bool,
    hair: bool,
    /// Which post effects to ask for: any of `bloom`, `dof`, `grain`, `grade`.
    post: Vec<String>,
    offline: bool,
    shadows: ShadowQuality,
    interval: u8,
    /// Casting key lights, 1 to 4.
    keys: usize,
    /// `sky` or `hdri`: light the scene with an environment too.
    environment: Option<String>,
    toon_ramp: bool,
    spot: bool,
    fog: bool,
    software: bool,
}

fn parse() -> Result<Args, String> {
    let mut args = Args {
        size: UVec2::new(1920, 1080),
        frames: 600,
        samples: 4,
        scale: 1.0,
        triangles: 130_000,
        props: 20,
        paint: 0,
        outlines: false,
        blended: false,
        hair: false,
        post: Vec::new(),
        offline: false,
        shadows: ShadowQuality::Off,
        interval: 2,
        keys: 1,
        environment: None,
        toon_ramp: false,
        spot: false,
        fog: false,
        software: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--size" => {
                let v = value()?;
                let (w, h) = v.split_once('x').ok_or("--size is WxH")?;
                args.size = UVec2::new(w.parse().map_err(|e| format!("{e}"))?, h.parse().map_err(|e| format!("{e}"))?);
            }
            "--frames" => args.frames = value()?.parse().map_err(|e| format!("{e}"))?,
            "--samples" => args.samples = value()?.parse().map_err(|e| format!("{e}"))?,
            "--scale" => args.scale = value()?.parse().map_err(|e| format!("{e}"))?,
            "--triangles" => args.triangles = value()?.parse().map_err(|e| format!("{e}"))?,
            "--props" => args.props = value()?.parse().map_err(|e| format!("{e}"))?,
            "--paint" => args.paint = value()?.parse().map_err(|e| format!("{e}"))?,
            "--outlines" => args.outlines = true,
            "--blended" => args.blended = true,
            "--hair" => args.hair = true,
            "--post" => {
                args.post = match value()?.as_str() {
                    "none" => Vec::new(),
                    "all" => ["bloom", "dof", "grain", "grade"].map(String::from).to_vec(),
                    list => list.split(',').map(String::from).collect(),
                };
                if let Some(unknown) =
                    args.post.iter().find(|e| !["bloom", "dof", "grain", "grade"].contains(&e.as_str()))
                {
                    return Err(format!("--post knows bloom, dof, grain and grade, not {unknown}"));
                }
            }
            "--offline" => args.offline = true,
            "--shadows" => {
                args.shadows = match value()?.as_str() {
                    "off" => ShadowQuality::Off,
                    "low" => ShadowQuality::Low,
                    "high" => ShadowQuality::High,
                    other => return Err(format!("--shadows is off, low or high, not {other}")),
                }
            }
            "--interval" => args.interval = value()?.parse().map_err(|e| format!("{e}"))?,
            "--keys" => {
                args.keys = value()?.parse().map_err(|e| format!("{e}"))?;
                if !(1..=4).contains(&args.keys) {
                    return Err(format!("--keys is 1 to 4, not {}", args.keys));
                }
            }
            "--environment" => {
                let kind = value()?;
                if !["sky", "hdri"].contains(&kind.as_str()) {
                    return Err(format!("--environment is sky or hdri, not {kind}"));
                }
                args.environment = Some(kind);
            }
            "--toon-ramp" => args.toon_ramp = true,
            "--spot" => args.spot = true,
            "--fog" => args.fog = true,
            "--software" => args.software = true,
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(args)
}

/// A 1024 x 512 HDRI: a blue-to-warm gradient with a small, very bright sun.
fn sun_hdri() -> EnvironmentDesc {
    let size = UVec2::new(1024, 512);
    let sun = Vec3::new(0.5, 0.6, 0.62).normalize();
    let mut rgb = Vec::with_capacity((size.x * size.y * 3) as usize);
    for y in 0..size.y {
        for x in 0..size.x {
            let phi = ((x as f32 + 0.5) / size.x as f32 - 0.5) * std::f32::consts::TAU;
            let elevation = (0.5 - (y as f32 + 0.5) / size.y as f32) * std::f32::consts::PI;
            let d = Vec3::new(elevation.cos() * phi.cos(), elevation.sin(), elevation.cos() * phi.sin());
            let sky = Vec3::new(0.4, 0.5, 0.8).lerp(Vec3::new(1.0, 0.8, 0.6), 1.0 - d.y.clamp(0.0, 1.0));
            rgb.extend(if d.dot(sun) > 0.999 { Vec3::new(120.0, 90.0, 50.0) } else { sky }.to_array());
        }
    }
    EnvironmentDesc::Hdri { size, rgb }
}

/// A UV sphere with about `triangles` triangles.
fn sphere(triangles: u32) -> MeshData {
    let rings = ((f64::from(triangles) / 2.0).sqrt() as u32).max(3);
    let segments = rings * 2;
    let mut mesh = MeshData::default();
    for ring in 0..=rings {
        let v = ring as f32 / rings as f32;
        let (sin_t, cos_t) = (v * std::f32::consts::PI).sin_cos();
        for segment in 0..=segments {
            let u = segment as f32 / segments as f32;
            let (sin_p, cos_p) = (u * std::f32::consts::TAU).sin_cos();
            let n = Vec3::new(sin_t * cos_p, cos_t, sin_t * sin_p);
            mesh.positions.push(n * 0.5);
            mesh.normals.push(n);
            mesh.uvs.push(Vec2::new(u, v));
        }
    }
    let stride = segments + 1;
    for ring in 0..rings {
        for segment in 0..segments {
            let a = ring * stride + segment;
            mesh.indices.extend([a, a + 1, a + stride, a + 1, a + stride + 1, a + stride]);
        }
    }
    mesh
}

/// Gives a sphere the hair program's attributes: `hair-uv` from its texture coordinates, and `hair-info` with a fixed
/// seed and shade and the angle from the top as the polar angle, so the ring crosses it.
fn with_hair_attributes(mut mesh: MeshData) -> Result<MeshData, Box<dyn std::error::Error>> {
    let uv: Vec<f32> = mesh.uvs.iter().flat_map(|t| [t.x * 2.0 - 1.0, t.y]).collect();
    let info: Vec<f32> = mesh.normals.iter().flat_map(|n| [0.37, 0.9, n.y.clamp(-1.0, 1.0).acos()]).collect();
    mesh.extra = vec![
        ExtraAttribute { name: Slug::new("hair-uv")?, components: 2, data: uv },
        ExtraAttribute { name: Slug::new("hair-info")?, components: 3, data: info },
    ];
    Ok(mesh)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    sorted.get(((sorted.len() as f64 - 1.0) * p).round() as usize).copied().unwrap_or(0.0)
}

fn summary(label: &str, mut ms: Vec<f64>) {
    if ms.is_empty() {
        return;
    }
    ms.sort_by(f64::total_cmp);
    let mean = ms.iter().sum::<f64>() / ms.len() as f64;
    println!(
        "  {label:<16} mean {mean:>7.3} ms   p50 {:>7.3}   p95 {:>7.3}   ({} samples)",
        percentile(&ms, 0.5),
        percentile(&ms, 0.95),
        ms.len()
    );
}

// A benchmark's `main` reads top to bottom: setup, warm-up and measuring loop, then the report.
#[allow(clippy::too_many_lines)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse()?;
    let adapter = if args.software { AdapterPreference::Software } else { AdapterPreference::Auto };
    let mut renderer = WgpuRenderer::headless(&HeadlessOptions { adapter })?;
    println!(
        "{}  |  {}x{}  samples {}  scale {}  ~{} triangles + {} props  |  outlines {}  |  hair {}  |  post {:?}{}  |  shadows {:?} every {}  |  gpu timings: {}",
        renderer.capabilities().backend,
        args.size.x,
        args.size.y,
        args.samples,
        args.scale,
        args.triangles,
        args.props,
        if args.outlines { "on" } else { "off" },
        if args.hair { "on" } else { "off" },
        args.post,
        if args.offline { " offline" } else { "" },
        args.shadows,
        args.interval,
        renderer.capabilities().gpu_timings
    );

    let body = sphere(args.triangles);
    let body = renderer.create_mesh(&if args.hair { with_hair_attributes(body)? } else { body })?;
    let prop = renderer.create_mesh(&sphere(2000))?;
    let line = args.outlines.then_some(Outline { colour: Colour::hex(0x003b_2f36), width: 0.003 });
    let mut skin = Material::toon(Colour::hex(0x00f9_d3c5));
    skin.outline = line;
    if args.hair {
        skin.shading = Shading::Program { name: Slug::new("hair")?, params: ProgramParams::new() };
        skin.double_sided = true;
    }
    let skin = renderer.create_material(&skin)?;
    let mut prop_material = Material::toon(Colour::hex(0x00c9_b8f0));
    prop_material.outline = line;
    if args.blended {
        prop_material.base.a = 128;
        prop_material.alpha = AlphaMode::Blend;
    }
    let atlas = (args.paint > 0)
        .then(|| renderer.create_texture(&TextureDesc::colour(UVec2::splat(args.paint)).dynamic(), None))
        .transpose()?;
    if let Some(texture) = atlas {
        prop_material.overlays.push(Overlay { texture, mapping: Mapping::Uv, strength: 1.0 });
    }
    let prop_material = renderer.create_material(&prop_material)?;
    let mut pixels = vec![255u8; (args.paint * args.paint * 4) as usize];
    let target = renderer.create_target(&TargetDesc { size: args.size, alpha: AlphaEncoding::Straight })?;

    let mut scene =
        Scene::new(Camera::look_at(Vec3::new(0.0, 0.1, 2.2), Vec3::ZERO, Vec3::Y, Camera::default().projection));
    scene.background = Background::Solid { colour: Colour::hex(0x0018_1220) };
    let lights = [
        Light::Hemisphere { sky: Colour::hex(0x00f3_e8ff), ground: Colour::hex(0x003a_2a24), intensity: 0.9 },
        Light::Directional {
            to_light: Vec3::new(-0.8, 1.6, 2.2),
            colour: Colour::WHITE,
            intensity: 2.0,
            casts_shadow: args.shadows != ShadowQuality::Off,
        },
    ];
    if let Some(kind) = &args.environment {
        let desc = if kind == "sky" {
            EnvironmentDesc::Sky {
                zenith: Colour::hex(0x0060_90ff),
                horizon: Colour::hex(0x00ff_e0c0),
                ground: Colour::hex(0x0030_2820),
            }
        } else {
            sun_hdri()
        };
        let start = Instant::now();
        let id = renderer.create_environment(&desc)?;
        renderer.wait_idle()?;
        println!("environment ({kind}) created in {:.1} ms", start.elapsed().as_secs_f64() * 1000.0);
        // `clear` leaves the environment alone, so it is set once.
        scene.environment = Some(EnvironmentUse { toon_ramp: args.toon_ramp, ..EnvironmentUse::new(id) });
    }
    let mut lights = lights.to_vec();
    // Further keys from the right, behind and above, dimmer than the first.
    for to_light in
        [Vec3::new(1.4, 1.8, 1.2), Vec3::new(0.3, 1.2, -2.0), Vec3::new(-0.2, 3.0, 0.4)].iter().take(args.keys - 1)
    {
        lights.push(Light::Directional {
            to_light: *to_light,
            colour: Colour::hex(0x00ff_e2c8),
            intensity: 1.0,
            casts_shadow: args.shadows != ShadowQuality::Off,
        });
    }
    if args.spot {
        lights.push(Light::Spot {
            position: Vec3::new(0.0, 1.6, 1.4),
            direction: Vec3::new(0.0, -1.6, -1.4),
            colour: Colour::hex(0x00ff_e0b0),
            intensity: 6.0,
            range: 6.0,
            cone_outer_deg: 50.0,
            penumbra: 0.5,
            casts_shadow: false,
        });
    }
    scene.fog = args.fog.then_some(Fog { colour: Colour::hex(0x0018_1220), near: 1.5, far: 4.0 });
    let wants = |effect: &str| args.post.iter().any(|e| e == effect);
    scene.post.bloom = wants("bloom").then_some(Bloom { strength: 0.35, radius: 0.5, threshold: 0.9 });
    scene.post.depth_of_field = wants("dof").then_some(DepthOfField::new(2.2, 0.4, 0.03));
    scene.post.grain = wants("grain").then_some(FilmGrain { strength: 0.04 });
    scene.post.grade = wants("grade").then_some(ColourGrade { saturation: 1.1, contrast: 1.1 });
    let quality = Quality {
        samples: args.samples,
        render_scale: args.scale,
        post_effects: (!args.post.is_empty()).into(),
        shadows: args.shadows,
        shadow_interval: args.interval,
        mode: if args.offline { QualityMode::Offline } else { QualityMode::Live },
        particle_density: 1.0,
    };

    let (mut cpu, mut gpu_passes, mut wall): (Vec<PassTimes>, Vec<PassTimes>, Vec<f64>) =
        (Vec::new(), Vec::new(), Vec::new());
    let mut last_gpu_frame = None;
    let mut draw_calls = 0;
    for frame in 0..args.frames + 60 {
        // `clear` empties the lights too, so a stage refills them with the instances every frame.
        scene.clear();
        scene.lights.extend_from_slice(&lights);
        scene.frame = omuzaki_render_api::FrameInfo { index: frame as u64, time: Seconds(frame as f64 / 60.0) };
        scene.instances.push(Instance::new(body, skin, Mat4::from_rotation_y(frame as f32 * 0.02)));
        for i in 0..args.props {
            let angle = i as f32 / args.props.max(1) as f32 * std::f32::consts::TAU;
            let at = Vec3::new(angle.cos() * 0.9, angle.sin() * 0.5, -0.3);
            scene.instances.push(Instance::new(
                prop,
                prop_material,
                Mat4::from_translation(at) * Mat4::from_scale(Vec3::splat(0.15)),
            ));
        }
        if let Some(texture) = atlas {
            pixels.iter_mut().step_by(4).for_each(|red| *red = (frame % 200) as u8);
            renderer.update_texture(texture, PixelRect::full(UVec2::splat(args.paint)), &pixels)?;
        }
        let start = Instant::now();
        let report = renderer.render(target, &scene, &quality)?;
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        draw_calls = report.draw_calls;
        if frame >= 60 {
            cpu.push(report.cpu);
            wall.push(elapsed);
            // A new measurement arrives every few frames; count each once.
            if let Some(gpu) = report.gpu
                && last_gpu_frame != Some(gpu.frame)
            {
                last_gpu_frame = Some(gpu.frame);
                gpu_passes.push(gpu.passes);
            }
        }
        // Keep a couple of frames in flight, the way a real loop runs; a full wait each frame would measure latency.
        if frame % 2 == 1 {
            renderer.wait_idle()?;
        }
    }

    println!("{draw_calls} draw calls per frame\n\nrender() call, wall clock:");
    summary("whole call", wall);
    println!("\nCPU time recording each pass:");
    for pass in Pass::ALL {
        summary(pass.name(), cpu.iter().filter_map(|t| t.get(pass)).map(|s| s.0 * 1000.0).collect());
    }
    summary("all passes", cpu.iter().map(|t| t.total().0 * 1000.0).collect());
    println!("\nGPU time per pass (timestamp queries):");
    for pass in Pass::ALL {
        summary(pass.name(), gpu_passes.iter().filter_map(|t| t.get(pass)).map(|s| s.0 * 1000.0).collect());
    }
    summary("all passes", gpu_passes.iter().map(|t| t.total().0 * 1000.0).collect());
    Ok(())
}
