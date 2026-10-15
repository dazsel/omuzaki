//! `cost`: what presenting a 1080p frame at 30 fps in the transparent window costs (research page section 9, item 4).
//! Animates the picture, times the GPU with timestamp queries and reads the process's CPU time.

use std::process::ExitCode;

use clap::Parser;
use mascot_window::app::{self, Config, Mode};
use mascot_window::cli::{GpuArgs, parse_size};
use mascot_window::platform::{self, ClickThrough, HitTestBackend, Level};

#[derive(Parser)]
#[command(about = "Measure the cost of a 1080p30 transparent window (BR-25 spike)")]
struct Cli {
    #[command(flatten)]
    gpu: GpuArgs,
    /// Seconds to run.
    #[arg(long, default_value_t = 60.0)]
    seconds: f64,
    /// Window size in physical pixels.
    #[arg(long, default_value = "1920x1080", value_parser = parse_size)]
    size: (u32, u32),
    /// Target frames per second.
    #[arg(long, default_value_t = 30)]
    fps: u32,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let config = Config {
        mode: Mode::Run,
        gpu: cli.gpu.options(true, true),
        platform: platform::Options {
            level: Level::Top,
            hide_from_switcher: false,
            click_through: ClickThrough::Off,
            hit_test: HitTestBackend::Winit,
        },
        size: cli.size,
        fps: cli.fps,
        seconds: Some(cli.seconds),
        opacity: 1.0,
    };
    match app::run(config) {
        Ok(outcome) => {
            println!("{outcome}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
