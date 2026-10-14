//! The toon viewport spike: opens an egui window on a character GLB, or renders headless.
//!
//! ```text
//! toon-viewport character.glb                              # window
//! toon-viewport character.glb --out shot.png               # one frame, no window
//! toon-viewport character.glb --bench 300 --width 1920 --height 1080
//! toon-viewport character.glb --smoke 120 --smoke-out shot.png   # window for 120 frames, then exit
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use toon_viewport::app::{Smoke, ViewportApp};
use toon_viewport::camera::OrbitCamera;
use toon_viewport::gpu::{device_descriptor, open_headless};
use toon_viewport::headless;
use toon_viewport::lighting::Lighting;
use toon_viewport::model::Model;
use toon_viewport::renderer::{FrameParams, RenderOptions};

#[derive(Parser)]
#[command(about = "Toon-shaded character viewport (spike)")]
struct Args {
    /// The character GLB (exported by the prototype's GLB export script).
    glb: PathBuf,
    /// Lighting preset: studio, night or warm.
    #[arg(long, default_value = "studio")]
    light: String,
    /// Camera yaw in degrees (0 = front).
    #[arg(long, default_value_t = 0.0)]
    yaw: f32,
    /// Horizontal camera distance in metres.
    #[arg(long, default_value_t = 2.4)]
    dist: f32,
    /// Camera height above the look-at point, in metres.
    #[arg(long, default_value_t = 0.12)]
    cam_height: f32,
    /// Height of the look-at point, in metres.
    #[arg(long, default_value_t = 0.5)]
    target_y: f32,
    #[arg(long)]
    no_outlines: bool,
    #[arg(long)]
    no_shadows: bool,
    /// Render width in pixels (headless modes).
    #[arg(long, default_value_t = 1280)]
    width: u32,
    /// Render height in pixels (headless modes).
    #[arg(long, default_value_t = 720)]
    height: u32,
    /// Headless: render one frame to this PNG and exit.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Headless: benchmark this many frames and print the timings.
    #[arg(long)]
    bench: Option<u32>,
    /// Window: render this many frames, then exit (for smoke tests).
    #[arg(long)]
    smoke: Option<u32>,
    /// Window with --smoke: save a screenshot of the whole window to this PNG.
    #[arg(long)]
    smoke_out: Option<PathBuf>,
    /// Window: present without waiting for vertical sync, so the frame interval shows what the GPU can sustain.
    #[arg(long)]
    no_vsync: bool,
}

fn run(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let model = Model::load(&args.glb)?;
    let lighting = Lighting::preset(&args.light)
        .ok_or_else(|| format!("unknown light preset `{}` (studio, night, warm)", args.light))?;
    let camera = OrbitCamera::js_view(args.yaw, args.dist, args.cam_height, args.target_y);
    let options = RenderOptions { outlines: !args.no_outlines, shadows: !args.no_shadows, ground: true };

    if args.out.is_some() || args.bench.is_some() {
        let gpu = open_headless()?;
        println!("adapter: {} ({:?})", gpu.info.name, gpu.info.backend);
        let params = FrameParams { camera: &camera, lighting: &lighting, options };
        let size = (args.width, args.height);
        if let Some(path) = &args.out {
            headless::screenshot(&gpu, &model, &params, size, path)?;
            println!("wrote {}", path.display());
        }
        if let Some(frames) = args.bench {
            let report = headless::bench(&gpu, &model, &params, size, frames);
            println!("bench {}x{}: serial   {}", size.0, size.1, report.serial);
            match report.gpu {
                Some(gpu_time) => println!("bench {}x{}: gpu      {gpu_time}", size.0, size.1),
                None => println!("bench: no GPU timestamp queries on this adapter"),
            }
            println!("bench {}x{}: pipelined {:.3} ms/frame", size.0, size.1, report.pipelined_ms);
        }
        return Ok(());
    }

    let mut native = eframe::NativeOptions::default();
    native.viewport = native.viewport.with_inner_size([1280.0, 760.0]).with_title("toon viewport");
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut native.wgpu_options.wgpu_setup {
        setup.device_descriptor = std::sync::Arc::new(device_descriptor_for_window);
    }
    if args.no_vsync {
        native.wgpu_options.surface.present_mode = eframe::wgpu::PresentMode::AutoNoVsync;
    }
    let smoke = args.smoke.map(|frames| Smoke { frames, screenshot: args.smoke_out.clone() });
    eframe::run_native(
        "toon viewport",
        native,
        Box::new(move |cc| {
            ViewportApp::new(cc, &model, camera, lighting, options, smoke)
                .map(|app| Box::new(app) as Box<dyn eframe::App>)
                .ok_or_else(|| "eframe did not start the wgpu renderer".into())
        }),
    )?;
    Ok(())
}

fn device_descriptor_for_window(adapter: &eframe::wgpu::Adapter) -> eframe::wgpu::DeviceDescriptor<'static> {
    device_descriptor(adapter)
}

fn main() -> ExitCode {
    match run(&Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
