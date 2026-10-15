//! `caps`: per wgpu backend, the adapter and the composite alpha modes a window surface offers (research page section 9,
//! item 1). Opens a small transparent window because a surface needs one; prints one line per backend and exits.

use std::process::ExitCode;

use clap::Parser;
use mascot_window::app::{self, Config, Mode};
use mascot_window::cli::GpuArgs;
use mascot_window::platform::{self, ClickThrough, HitTestBackend, Level};

#[derive(Parser)]
#[command(about = "Print each wgpu backend's adapter and composite alpha modes for a window (BR-25 spike)")]
struct Cli {
    #[command(flatten)]
    gpu: GpuArgs,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let config = Config {
        mode: Mode::Caps,
        gpu: cli.gpu.options(false, false),
        platform: platform::Options {
            level: Level::Normal,
            hide_from_switcher: false,
            click_through: ClickThrough::Off,
            hit_test: HitTestBackend::Winit,
        },
        size: (256, 256),
        fps: 30,
        seconds: None,
        opacity: 1.0,
    };
    match app::run(config) {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
