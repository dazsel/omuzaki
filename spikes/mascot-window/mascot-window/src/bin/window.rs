//! `window`: the transparent, always-on-top, click-through window with a wgpu surface (research page section 9, item 2).
//! Run alone on its own winit loop (`--host winit`) or on eframe's loop next to an eframe window (`--host eframe`).

use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use mascot_window::app::{self, Mode, Outcome, RunError};
use mascot_window::cli::WindowArgs;
use mascot_window::host;

#[derive(Clone, Copy, ValueEnum)]
enum HostArg {
    /// The window owns the event loop.
    Winit,
    /// The window shares eframe's event loop with an eframe control window.
    Eframe,
}

#[derive(Parser)]
#[command(about = "Transparent, always-on-top, click-through window with wgpu (BR-25 spike)")]
struct Cli {
    #[command(flatten)]
    window: WindowArgs,
    /// Which event loop runs the window.
    #[arg(long, value_enum, default_value = "winit")]
    host: HostArg,
    /// Apply the platform features, print what the OS reports back and exit; draws nothing.
    #[arg(long)]
    probe: bool,
}

fn run(cli: &Cli) -> Result<Outcome, RunError> {
    let config = cli.window.config(if cli.probe { Mode::Probe } else { Mode::Run });
    match cli.host {
        HostArg::Winit => app::run(config),
        HostArg::Eframe => host::run_shared(config, cli.window.gpu.dx12_visual()),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(outcome) => {
            if !cli.probe {
                println!("{outcome}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
