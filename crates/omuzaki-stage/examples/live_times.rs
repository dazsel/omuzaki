//! The live loop's frame times (spec v3 §10): `cargo run -p omuzaki-stage --features wgpu --release --example
//! live_times -- [--character kit|vrm|both] [--size 1920x1080] [--fps 30] [--warmup 20] [--frames 3000] [--unpaced]
//! [--steady MS [--steady-wait]] [--camera bust|full|face] [--software] [--gpu-face] [--out DIR]`.
//!
//! It runs the studio's live path from start to finish, with nothing stood in:
//! - a project with the kit's default character, made the way `character.create` makes it (template `standard`, seed
//!   7: the real body, head and hair generators), and the same character exported to VRM 1.0 and imported back, which is
//!   the bring-your-own-model path without a third-party file;
//! - the default scene, created by `stage.create_scene` (the studio light rig, its ground and backdrop), resolved on the
//!   body's landmarks and opened on its `bust` camera (`--camera` picks another): the face and upper body, as §10's
//!   budget scene frames them;
//! - the body staged with its face (the production `FaceRig::standard`), and the one live loop (`LiveStage`) fed by a
//!   scripted webcam through `LiveInput`: the real retarget, life layer and animator, the rig and spring bones, the face
//!   painter, the frame-budget scheduler and the renderer;
//! - paced at `--fps` (§10 measures at 30), warmed up for `--warmup` seconds, then `--frames` recorded.
//!
//! Printed per character, as mean, p50, p95, p99 and maximum:
//! - `gpu.frame`: the GPU time of every pass but face paint, from timestamp queries, against `budget::LIVE_GPU`;
//! - `gpu.face_paint`: the `paint_textures` pass on the frames that ran it (the painter's own commands and the
//!   renderer's mip rebuild of what it painted), against `budget::FACE_PAINT_GPU`, split into the painters' share (the
//!   face rasteriser's raster and resolve, which is what the `gpu_face_timing` probe times) and the renderer's own, and
//!   `gpu.frame_with_face`, the two added on the frames that repainted;
//! - the frame thread: the wall time of `LiveStage::frame` (performer, stage, face and the renderer's recording), split
//!   into the renderer's recording (with a CPU face raster, when the GPU one can't run) and the rest (the performer,
//!   the stage and any wait for the GPU), against the frame period;
//! - the cold half (`LiveStage::service`) the loop runs after each frame, and every step the scheduler took.
//!
//! **Clock state.** Paced at 30 fps, the live frame is a millisecond or two of GPU work in every 33 ms, so a desktop GPU
//! drops to its idle clock between frames and the first work of a frame (the face paint) runs at it. Spec §10 reads
//! `gpu.face_paint` at a steady clock (the `gpu_face_timing` probe keeps the GPU busy in front of each repaint) and
//! reports the paced reading beside it. `--steady MS` does the same here: before each frame it submits a compute load
//! that keeps the GPU busy for about `MS` milliseconds (2 is the probe's), so the live loop's numbers are the work's and
//! not the clock state's. Without it, the readings are the paced ones. The load is queued ahead of the frame, as the
//! probe queues it ahead of a repaint; `--steady-wait` waits for it to finish first, so no pass of the frame can share
//! the GPU with it (a GPU may run one submission's work beside the next one's), at the cost of the GPU idling while
//! the frame is recorded. A frame thread over the frame period is listed with its frame and the scheduler's last
//! change.
//!
//! The frame thread is timed with a stopwatch, which §10 does not accept for a gating CPU number (it asks for the OS's
//! CPU-time counters): read it as the frame's deadline, not as `cpu.total`. Tracking (`track.face`) and the
//! camera-to-photon latency need a real camera and are not measured here. `--software` runs on lavapipe or WARP, which
//! proves the path runs and is never a timing machine; with `--gpu-face` it paints the face with the GPU rasteriser as a
//! hardware adapter does, so the face path's work can be compared with the `gpu_face_timing` probe's on the same
//! adapter. `--out DIR` writes each character's last frame as a PNG.
//!
//! `WGPU_ADAPTER_NAME` and `WGPU_BACKEND` pick the GPU, as for every program built on the renderer.

// A benchmark's statistics use small counts in float maths; the casts are exact enough.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

#[path = "../tests/avatar/kit.rs"]
mod kit;
#[path = "../tests/live/support.rs"]
mod support;

use std::collections::BTreeMap;
use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use omuzaki_avatar_api::def::{AvatarDef, AvatarSource, SourceFormat};
use omuzaki_char::ops::character::create::CharacterCreate;
use omuzaki_char_api::{BuildServices, CharId, Quality as BuildQuality, RegistryBuilder};
use omuzaki_core::glam::{Mat4, UVec2};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::DocWrite;
use omuzaki_doc_mock::MemDoc;
use omuzaki_face::raster::gpu::VelloRasterizer;
use omuzaki_face_api::FaceLayerId;
use omuzaki_ops_api::{Actor, Mode, Op, OpCtx};
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_render_api::{AlphaEncoding, Camera, Pass, Quality, Renderer, Scene, TargetDesc, budget};
use omuzaki_stage::UploadOptions;
use omuzaki_stage::body::{Body, BodyServices, load_body};
use omuzaki_stage::frame::StageClock;
use omuzaki_stage::frame::face::FaceRig;
use omuzaki_stage::ops::scene::create::CreateScene;
use omuzaki_stage::perform::STEP_HZ;
use omuzaki_stage::perform::host::PerformHost;
use omuzaki_stage::perform::input::{InputHost, LiveInput, NoPrompt, SourceKind, SourceSlot};
use omuzaki_stage::perform::live::LiveStage;
use omuzaki_stage::scene::default::scene_for;
use omuzaki_stage::scene::resolve::{CheckedScene, Figure, ResolvedScene};
use omuzaki_stage::stages::{AnyStage, FrameLoop};
use omuzaki_store_api::Store;
use omuzaki_store_mock::MemStore;
use omuzaki_track_api::camera::CameraId;
use omuzaki_track_api::tracker::TrackerConfig;
use serde_json::json;

use support::{Cameras, face_for, performer};

/// The kit character's id in the project, and its imported copy's.
const KIT: &str = "kit";
const VRM: &str = "kit_vrm";

// One flag per switch of the command line.
#[allow(clippy::struct_excessive_bools)]
struct Args {
    characters: Vec<&'static str>,
    size: UVec2,
    fps: f64,
    warmup: f64,
    frames: usize,
    paced: bool,
    /// Milliseconds of GPU load submitted before each frame, to hold the GPU at a steady clock; `None` for none.
    steady: Option<f64>,
    /// Wait for the steady-clock load to finish before each frame, so no pass of the frame shares the GPU with it.
    steady_wait: bool,
    camera: String,
    software: bool,
    /// Paint the face with the GPU rasteriser on any adapter, software ones included, so `--software` runs the same
    /// face path as a hardware adapter (the live stage leaves a software adapter to the CPU raster).
    gpu_face: bool,
    out: Option<PathBuf>,
}

fn parse() -> Result<Args, Box<dyn Error>> {
    let mut args = Args {
        characters: vec![KIT, VRM],
        size: UVec2::new(1920, 1080),
        fps: 30.0,
        warmup: 20.0,
        frames: 3000,
        paced: true,
        steady: None,
        steady_wait: false,
        camera: "bust".to_owned(),
        software: false,
        gpu_face: false,
        out: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--character" => {
                args.characters = match value()?.as_str() {
                    "kit" => vec![KIT],
                    "vrm" => vec![VRM],
                    "both" => vec![KIT, VRM],
                    other => return Err(format!("--character is kit, vrm or both, not {other}").into()),
                }
            }
            "--size" => {
                let v = value()?;
                let (w, h) = v.split_once('x').ok_or("--size is WIDTHxHEIGHT")?;
                args.size = UVec2::new(w.parse()?, h.parse()?);
            }
            "--fps" => args.fps = value()?.parse()?,
            "--warmup" => args.warmup = value()?.parse()?,
            "--frames" => args.frames = value()?.parse()?,
            "--unpaced" => args.paced = false,
            "--steady" => args.steady = Some(value()?.parse()?),
            "--steady-wait" => args.steady_wait = true,
            "--camera" => args.camera = value()?,
            "--software" => args.software = true,
            "--gpu-face" => args.gpu_face = true,
            "--out" => args.out = Some(PathBuf::from(value()?)),
            other => return Err(format!("unknown flag {other}").into()),
        }
    }
    if !(args.fps > 0.0 && args.fps <= f64::from(STEP_HZ)) {
        return Err(format!("--fps is above 0 and at most the performer's {STEP_HZ} steps a second").into());
    }
    Ok(args)
}

/// The project: the kit character as `character.create` makes it, the same character as an imported VRM, and the
/// default scene as `stage.create_scene` writes it, casting the kit.
fn project(clock: &FakeClock) -> Result<(MemDoc, MemStore), Box<dyn Error>> {
    let mut doc = MemDoc::from_value(json!({}));
    let actor = Actor::Person { name: "budget".to_owned() };
    let create: CharacterCreate =
        serde_json::from_value(json!({ "id": KIT, "name": "Kit", "template": "standard", "seed": 7 }))?;
    let patch = create.plan(&OpCtx::new(&doc, &actor, clock))?.patch;
    doc.apply(&patch)?;

    let store = MemStore::new();
    let build = BuildServices::default();
    let services = BodyServices::new(&RegistryBuilder, &build, &store, omuzaki_render_api::MtoonSupport::NONE, clock);
    let Body::Kit(character) = load_body(&doc, &services, &CharId::new(KIT)?, None, BuildQuality::Live)? else {
        return Err("the kit character loaded as an import".into());
    };
    let hash = store.put(&kit::vrm_bytes(&character), kit::provenance())?;
    let def = AvatarDef::new(AvatarSource::new(hash, SourceFormat::Vrm1, "kit.vrm"));
    let mut root = doc.root().clone();
    root["characters"][VRM] = json!({ "avatar": serde_json::to_value(&def)? });
    root["project"]["default_character"] = json!(KIT);
    let mut doc = MemDoc::from_value(root);

    let scene: CreateScene = serde_json::from_value(json!({ "id": "main" }))?;
    let patch = scene.plan(&OpCtx::new(&doc, &actor, clock))?.patch;
    doc.apply(&patch)?;
    Ok((doc, store))
}

/// What one character's run measured, in milliseconds per frame.
#[derive(Default)]
struct Samples {
    gpu_frame: Vec<f64>,
    gpu_paint: Vec<f64>,
    gpu_painters: Vec<f64>,
    gpu_paint_own: Vec<f64>,
    gpu_with_face: Vec<f64>,
    gpu_passes: Vec<(Pass, f64)>,
    frame: Vec<f64>,
    recording: Vec<f64>,
    service: Vec<f64>,
    late: usize,
    /// Each frame over the frame period, described.
    late_frames: Vec<String>,
    changes: Vec<String>,
    draw_calls: u32,
    triangles: u64,
    /// The face layers the body paints, with their sizes in pixels and how often each was painted.
    face_layers: Vec<String>,
}

/// How many times the stage has painted each face layer (zeros for an imported avatar, which has no painted face).
fn face_paints(stage: &AnyStage) -> [u64; FaceLayerId::COUNT] {
    match stage {
        AnyStage::Kit(kit) => kit.face_paints(),
        AnyStage::Avatar(_) => [0; FaceLayerId::COUNT],
    }
}

fn ms(s: Seconds) -> f64 {
    s.0 * 1000.0
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    sorted.get(((sorted.len() as f64 - 1.0) * p).round() as usize).copied().unwrap_or(0.0)
}

fn summary(label: &str, mut values: Vec<f64>, budget: Option<f64>) {
    if values.is_empty() {
        println!("  {label:<22} no samples");
        return;
    }
    values.sort_by(f64::total_cmp);
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let p95 = percentile(&values, 0.95);
    let verdict =
        budget.map_or(String::new(), |b| format!("   budget {b:.2} ms: {}", if p95 <= b { "within" } else { "OVER" }));
    println!(
        "  {label:<22} mean {mean:>7.3}  p50 {:>7.3}  p95 {p95:>7.3}  p99 {:>7.3}  max {:>7.3} ms  ({} samples){verdict}",
        percentile(&values, 0.5),
        percentile(&values, 0.99),
        values.last().copied().unwrap_or(0.0),
        values.len(),
    );
}

/// Runs character `id` live and records its frames.
// A benchmark reads top to bottom: setup, warm-up and measuring loop.
#[allow(clippy::too_many_lines)]
fn measure(
    renderer: &mut WgpuRenderer,
    doc: &MemDoc,
    store: &MemStore,
    clock: &FakeClock,
    id: &str,
    args: &Args,
    load: Option<&Load>,
) -> Result<Samples, Box<dyn Error>> {
    let id = CharId::new(id)?;
    let (_, mut scene) = scene_for(doc, None)?;
    let member = scene.cast.values_mut().next().ok_or("the default scene casts nobody")?;
    member.character = id.clone();
    let build = BuildServices::default();
    let mtoon = renderer.capabilities().mtoon;
    let services = BodyServices::new(&RegistryBuilder, &build, store, mtoon, clock);
    let body = load_body(doc, &services, &id, None, BuildQuality::Live)?;
    let (member, cast) = scene.cast.iter().next().ok_or("the default scene casts nobody")?;
    let figure = Figure::new(cast, body.landmarks()?);
    let figures = BTreeMap::from([(member.clone(), figure)]);
    let mut resolved = ResolvedScene::new();
    let aspect = args.size.x as f32 / args.size.y as f32;
    resolved.resolve_into(CheckedScene::new(&scene)?, &figures, aspect)?;
    let camera = resolved.cameras.iter().position(|(camera, _)| camera.as_str() == args.camera);
    resolved.active = Some(camera.ok_or_else(|| format!("the default scene has no `{}` camera", args.camera))?);

    let base = body.base_pose();
    let has_face = matches!(&body, Body::Kit(character) if character.face.is_some());
    let face_layers: Vec<(FaceLayerId, String)> = match &body {
        Body::Kit(character) => character
            .face_textures(UploadOptions::default().face_px_per_m)
            .iter()
            .map(|(spec, desc)| (spec.id, format!("{:?} {}x{}", spec.id, desc.size.x, desc.size.y)))
            .collect(),
        Body::Avatar(_) => Vec::new(),
    };
    let target = renderer.create_target(&TargetDesc { size: args.size, alpha: AlphaEncoding::Straight })?;
    let mut stage = body.stage(
        renderer,
        UploadOptions::default(),
        Scene::new(Camera::default()),
        target,
        Quality::live(),
        StageClock::live(),
    )?;
    if let (AnyStage::Kit(kit), true) = (&mut stage, has_face) {
        let rig = FaceRig::standard();
        let rig = if args.gpu_face { rig.with_gpu_rasterizer(Box::new(VelloRasterizer::new())) } else { rig };
        kit.set_face(renderer, rig)?;
    }
    let sources = Arc::new(SourceSlot::new());
    let mut live = LiveStage::new(performer(&base), stage, Arc::clone(&sources), Quality::live(), budget::LIVE_GPU)?;
    live.scenes().post(resolved);
    live.set_subject_root(Mat4::from(figure.root));
    let perform: Arc<dyn PerformHost> = Arc::new(live.handle().clone());
    let total = (args.warmup * args.fps).ceil() as usize + args.frames;
    let seconds = total as f64 / args.fps + 1.0;
    let (tracker, _log) = Cameras::new(vec![("webcam", face_for(seconds as f32, 7))]);
    let input = LiveInput::new(Box::new(tracker), Box::new(NoPrompt), TrackerConfig::default(), sources, perform);
    input.select(SourceKind::Webcam, Mode::Apply)?;
    input.bind_camera(Some(CameraId("webcam".to_owned())), Mode::Apply)?;
    input.preview(true, Mode::Apply)?;

    let mut samples = Samples::default();
    let mut per_pass: BTreeMap<usize, Vec<f64>> = BTreeMap::new();
    let mut paints_at_warm = [0; FaceLayerId::COUNT];
    let mut last_change: Option<usize> = None;
    let period = 1.0 / args.fps;
    let deadline = period * 1000.0;
    let warm = total - args.frames;
    let mut last_gpu = None;
    let start = Instant::now();
    for i in 0..total {
        // The loop's clock: the middle of the frame's first performer step, so each frame runs its steps exactly.
        let at = i as f64 * period;
        if args.paced {
            let due = Duration::from_secs_f64(at);
            if let Some(wait) = due.checked_sub(start.elapsed()) {
                std::thread::sleep(wait);
            }
        }
        let now = Seconds(at + 0.5 / f64::from(STEP_HZ));
        if let Some(load) = load {
            let (device, queue) = renderer.device_and_queue();
            load.submit(device, queue);
            if args.steady_wait {
                device.poll(wgpu::PollType::wait_indefinitely())?;
            }
        }
        let begun = Instant::now();
        let frame = live.frame(renderer, now)?;
        let frame_ms = begun.elapsed().as_secs_f64() * 1000.0;
        let begun = Instant::now();
        live.service(renderer)?;
        let service_ms = begun.elapsed().as_secs_f64() * 1000.0;
        let Some(frame) = frame else { continue };
        if let Some(change) = frame.change {
            samples.changes.push(format!("frame {i}: {change:?}"));
            last_change = Some(i);
        }
        if i + 1 == warm {
            paints_at_warm = face_paints(live.stage());
        }
        if i < warm {
            continue;
        }
        let report = frame.output.report;
        samples.frame.push(frame_ms);
        samples.recording.push(ms(report.cpu.total()));
        samples.service.push(service_ms);
        samples.late += usize::from(frame_ms > deadline);
        if frame_ms > deadline {
            let recording = ms(report.cpu.total());
            let last = last_change.map_or_else(|| "none yet".to_owned(), |at| format!("at frame {at}"));
            samples.late_frames.push(format!(
                "frame {i}: {frame_ms:.1} ms ({recording:.1} recording, {:.1} the rest); the scheduler's last change was {last}",
                frame_ms - recording
            ));
        }
        samples.draw_calls = report.draw_calls;
        samples.triangles = report.triangles;
        // Timestamps resolve a frame or two late and repeat until the next arrive; each frame's are counted once.
        if let Some(gpu) = report.gpu
            && last_gpu != Some(gpu.frame)
        {
            last_gpu = Some(gpu.frame);
            let paint = gpu.passes.get(Pass::PaintTextures);
            samples.gpu_frame.push(ms(gpu.passes.total()) - paint.map_or(0.0, ms));
            if let Some(paint) = paint {
                samples.gpu_paint.push(ms(paint));
                let painters = gpu.painters.map_or(0.0, ms);
                samples.gpu_painters.push(painters);
                samples.gpu_paint_own.push(ms(paint) - painters);
                samples.gpu_with_face.push(ms(gpu.passes.total()));
            }
            for (pass, time) in gpu.passes.iter() {
                per_pass.entry(pass.index()).or_default().push(ms(time));
            }
        }
    }
    let paints = face_paints(live.stage());
    let measured = samples.frame.len().max(1) as f64;
    samples.face_layers = face_layers
        .iter()
        .map(|(id, layer)| {
            let painted = paints[*id as usize] - paints_at_warm[*id as usize];
            format!("{layer} ({:.2} paints a frame)", painted as f64 / measured)
        })
        .collect();
    for pass in Pass::ALL {
        if let Some(mut times) = per_pass.remove(&pass.index()) {
            times.sort_by(f64::total_cmp);
            samples.gpu_passes.push((pass, percentile(&times, 0.95)));
        }
    }

    if let Some(dir) = &args.out {
        let image = renderer.read_target(target)?;
        let png = omuzaki_stage::ops::render::still::encode_png(&image)?;
        #[allow(clippy::disallowed_methods, reason = "the evidence image is written by the benchmark, not by an op")]
        std::fs::write(dir.join(format!("live-{id}.png")), png)?;
    }
    drop(input);
    live.into_stage().release(renderer)?;
    renderer.release(target.into())?;
    Ok(samples)
}

fn main() -> Result<(), Box<dyn Error>> {
    omuzaki_stage::link();
    omuzaki_io_vrm::link();
    let args = parse()?;
    let adapter = if args.software { AdapterPreference::Software } else { AdapterPreference::Auto };
    let mut renderer = WgpuRenderer::headless(&HeadlessOptions { adapter })?;
    let load = match args.steady {
        Some(ms) => {
            let (device, queue) = renderer.device_and_queue();
            Some(Load::new(device, queue, ms)?)
        }
        None => None,
    };
    let capabilities = renderer.capabilities();
    let waited = if args.steady_wait { ", waited for" } else { "" };
    let steady =
        load.as_ref().map_or(String::new(), |l| format!(", steady clock ({:.1} ms load a frame{waited})", l.ms));
    println!(
        "{}  |  {}x{} at {} fps{}{steady}  |  warm-up {} s, {} frames  |  live preset  |  gpu timings: {}",
        capabilities.backend,
        args.size.x,
        args.size.y,
        args.fps,
        if args.paced { ", paced" } else { ", unpaced" },
        args.warmup,
        args.frames,
        capabilities.gpu_timings,
    );
    let clock = FakeClock::new(Seconds(0.0));
    let (doc, store) = project(&clock)?;
    for id in &args.characters {
        let name = if *id == KIT { "the kit character" } else { "the kit character imported from its VRM" };
        let samples = measure(&mut renderer, &doc, &store, &clock, id, &args, load.as_ref())?;
        println!("\n{name} ({id}): {} draw calls, {} triangles a frame", samples.draw_calls, samples.triangles);
        if !samples.face_layers.is_empty() {
            println!("  face layers painted: {}", samples.face_layers.join(", "));
        }
        summary("gpu.frame", samples.gpu_frame, Some(ms(budget::LIVE_GPU)));
        summary("gpu.face_paint", samples.gpu_paint, Some(ms(budget::FACE_PAINT_GPU)));
        summary("  the painters", samples.gpu_painters, None);
        summary("  premultiply, mips", samples.gpu_paint_own, None);
        summary("gpu.frame_with_face", samples.gpu_with_face, None);
        let rest: Vec<f64> = samples.frame.iter().zip(&samples.recording).map(|(f, r)| f - r).collect();
        let deadline = 1000.0 / args.fps;
        summary("frame thread (wall)", samples.frame, Some(deadline));
        summary("  renderer recording", samples.recording, None);
        summary("  the rest", rest, None);
        summary("service (cold half)", samples.service, None);
        println!("  frames over {deadline:.1} ms: {}", samples.late);
        for late in samples.late_frames.iter().take(12) {
            println!("    {late}");
        }
        let passes: Vec<String> = samples.gpu_passes.iter().map(|(p, t)| format!("{} {t:.3}", p.name())).collect();
        println!("  gpu p95 per pass (ms): {}", passes.join(", "));
        if samples.changes.is_empty() {
            println!("  the scheduler kept the live preset throughout");
        } else {
            println!("  the scheduler changed quality {} times:", samples.changes.len());
            for change in samples.changes.iter().take(12) {
                println!("    {change}");
            }
        }
    }
    Ok(())
}

/// A compute pass that keeps the GPU busy for about `ms` milliseconds, submitted before each frame so the frame's work
/// runs at the clock of a busy GPU (the `gpu_face_timing` probe's load). It writes no timestamp, so it never counts in
/// the frame's passes.
struct Load {
    pipeline: wgpu::ComputePipeline,
    bind: wgpu::BindGroup,
    /// What it was sized to, in milliseconds.
    ms: f64,
}

/// A loop of integer work in one workgroup, as long as its uniform says.
const LOAD_WGSL: &str = "
@group(0) @binding(0) var<storage, read_write> sink: array<u32>;
@group(0) @binding(1) var<uniform> n: vec4<u32>;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    var x = id.x;
    for (var i = 0u; i < n.x; i++) { x = x * 1664525u + 1013904223u; }
    sink[id.x] = x;
}
";

impl Load {
    /// Builds the load and sizes it to about `ms` on this GPU, by timing a trial on the wall clock with the GPU
    /// otherwise idle (the fastest of a few runs, as the first ones also wake the GPU).
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, ms: f64) -> Result<Self, Box<dyn Error>> {
        const TRIAL: u32 = 200_000;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("steady-clock load"),
            source: wgpu::ShaderSource::Wgsl(LOAD_WGSL.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("steady-clock load"),
            layout: None,
            module: &module,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let buffer = |size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("steady-clock load"),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let sink = buffer(64 * 4, wgpu::BufferUsages::STORAGE);
        let iterations = buffer(16, wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST);
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("steady-clock load"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: sink.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: iterations.as_entire_binding() },
            ],
        });
        let set = |n: u32| {
            let mut bytes = [0_u8; 16];
            bytes[..4].copy_from_slice(&n.to_le_bytes());
            queue.write_buffer(&iterations, 0, &bytes);
        };
        let mut load = Self { pipeline, bind, ms };
        set(TRIAL);
        let mut fastest = f64::INFINITY;
        for _ in 0..8 {
            let begun = Instant::now();
            load.submit(device, queue);
            device.poll(wgpu::PollType::wait_indefinitely())?;
            fastest = fastest.min(begun.elapsed().as_secs_f64() * 1000.0);
        }
        // Thousands to millions of iterations: well inside a u32.
        set((ms / fastest * f64::from(TRIAL)).clamp(1_000.0, 1e9) as u32);
        load.ms = ms;
        Ok(load)
    }

    fn submit(&self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("steady-clock load") });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("steady-clock load"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
        queue.submit([encoder.finish()]);
    }
}
