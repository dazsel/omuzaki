//! `mascot-winit`: the winit + wgpu transparent window spike. See the research page `docs/research/2026-10-mascot-window.md`.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use mascot_window::alpha::Output;
use mascot_window::app::{self, Config, Mode};
use mascot_window::gpu;
use mascot_window::platform::{self, ClickThrough, HitTestBackend};
use mascot_window::scene::Scene;

#[derive(Parser)]
#[command(about = "Transparent, always-on-top, click-through window with wgpu (BR-25 spike)")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Open the window and draw the test image.
    Run(WindowArgs),
    /// Open the window, apply the platform features and read the OS state back; touches no GPU.
    Probe(WindowArgs),
    /// List the composite alpha modes each wgpu backend offers for a window.
    Caps(WindowArgs),
    /// Write the test image composited over white and over black to a folder (the reference for the fringe check).
    Png {
        /// Folder to write `scene-over-white.png` and `scene-over-black.png` into.
        #[arg(long)]
        out: PathBuf,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum BackendArg {
    Auto,
    Vulkan,
    Dx12,
    Metal,
    Gl,
}

#[derive(Clone, Copy, ValueEnum)]
enum AlphaArg {
    Auto,
    Premultiplied,
    Postmultiplied,
    Inherit,
    Opaque,
}

#[derive(Clone, Copy, ValueEnum)]
enum OutputArg {
    Auto,
    Premultiplied,
    Straight,
}

#[derive(Clone, Copy, ValueEnum)]
enum ClickArg {
    Off,
    Window,
    Pixel,
}

#[derive(Clone, Copy, ValueEnum)]
enum HitArg {
    Winit,
    Native,
}

// One bool per command line flag: a struct of switches is what a CLI is.
#[allow(clippy::struct_excessive_bools)]
#[derive(Args)]
struct WindowArgs {
    /// wgpu backend to use.
    #[arg(long, value_enum, default_value = "auto")]
    backend: BackendArg,
    /// Composite alpha mode to configure the surface with (auto picks the best offered).
    #[arg(long, value_enum, default_value = "auto")]
    alpha: AlphaArg,
    /// What the shader writes: premultiplied or straight colour (auto follows the alpha mode).
    #[arg(long, value_enum, default_value = "auto")]
    output: OutputArg,
    /// Click-through: off, the whole window, or per pixel by the image's alpha.
    #[arg(long, value_enum, default_value = "off")]
    click_through: ClickArg,
    /// Windows only: who toggles click-through.
    #[arg(long, value_enum, default_value = "native")]
    hit_test: HitArg,
    /// Do not ask for always-on-top.
    #[arg(long)]
    no_always_on_top: bool,
    /// Hide from the taskbar or Dock and from the window switcher.
    #[arg(long)]
    hide_from_switcher: bool,
    /// Windows DX12 only: use a plain HWND swap chain instead of a `DirectComposition` visual (opaque only).
    #[arg(long)]
    dx12_hwnd: bool,
    /// Use the CPU adapter where the platform has one.
    #[arg(long)]
    software: bool,
    /// Window size in physical pixels, `WIDTHxHEIGHT`. The 1080p cost measurement uses 1920x1080.
    #[arg(long, default_value = "512x512", value_parser = parse_size)]
    size: (u32, u32),
    /// Target frames per second.
    #[arg(long, default_value_t = 30)]
    fps: u32,
    /// Stop after this many seconds (default: run until closed or Esc).
    #[arg(long)]
    seconds: Option<f64>,
    /// Starting opacity, 0 to 1 (Up and Down arrows change it while running).
    #[arg(long, default_value_t = 1.0)]
    opacity: f32,
}

fn parse_size(text: &str) -> Result<(u32, u32), String> {
    let (w, h) = text.split_once('x').ok_or("expected WIDTHxHEIGHT, for example 1920x1080")?;
    let parse = |s: &str| s.parse::<u32>().ok().filter(|n| *n > 0).ok_or(format!("{s:?} is not a positive number"));
    Ok((parse(w)?, parse(h)?))
}

impl WindowArgs {
    fn config(&self, mode: Mode) -> Config {
        let backends = match self.backend {
            BackendArg::Auto => wgpu::Backends::all(),
            BackendArg::Vulkan => wgpu::Backends::VULKAN,
            BackendArg::Dx12 => wgpu::Backends::DX12,
            BackendArg::Metal => wgpu::Backends::METAL,
            BackendArg::Gl => wgpu::Backends::GL,
        };
        let alpha = match self.alpha {
            AlphaArg::Auto => None,
            AlphaArg::Premultiplied => Some(wgpu::CompositeAlphaMode::PreMultiplied),
            AlphaArg::Postmultiplied => Some(wgpu::CompositeAlphaMode::PostMultiplied),
            AlphaArg::Inherit => Some(wgpu::CompositeAlphaMode::Inherit),
            AlphaArg::Opaque => Some(wgpu::CompositeAlphaMode::Opaque),
        };
        let output = match self.output {
            OutputArg::Auto => None,
            OutputArg::Premultiplied => Some(Output::Premultiplied),
            OutputArg::Straight => Some(Output::Straight),
        };
        Config {
            mode,
            gpu: gpu::Options {
                backends,
                alpha,
                output,
                fallback_adapter: self.software,
                dx12_visual: !self.dx12_hwnd,
            },
            platform: platform::Options {
                always_on_top: !self.no_always_on_top,
                hide_from_switcher: self.hide_from_switcher,
                click_through: match self.click_through {
                    ClickArg::Off => ClickThrough::Off,
                    ClickArg::Window => ClickThrough::Window,
                    ClickArg::Pixel => ClickThrough::Pixel,
                },
                hit_test: match self.hit_test {
                    HitArg::Winit => HitTestBackend::Winit,
                    HitArg::Native => HitTestBackend::Native,
                },
            },
            size: self.size,
            fps: self.fps,
            seconds: self.seconds,
            opacity: self.opacity,
        }
    }
}

fn write_pngs(out: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(out)?;
    let scene = Scene::generate();
    for (name, bg) in [("white", [255_u8; 3]), ("black", [0_u8; 3])] {
        let path = out.join(format!("scene-over-{name}.png"));
        image::save_buffer(&path, &scene.over_background(bg), scene.width, scene.height, image::ColorType::Rgba8)?;
        println!("wrote {}", path.display());
    }
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result: Result<(), Box<dyn std::error::Error>> = match cli.command {
        Command::Run(args) => app::run(args.config(Mode::Run)).map_err(Into::into),
        Command::Probe(args) => app::run(args.config(Mode::Probe)).map_err(Into::into),
        Command::Caps(args) => app::run(args.config(Mode::Caps)).map_err(Into::into),
        Command::Png { out } => write_pngs(&out),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
