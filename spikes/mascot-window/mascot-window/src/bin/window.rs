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
    /// Which event loop runs the window (ignored with `--probe`, which never starts the eframe window).
    #[arg(long, value_enum, default_value = "winit")]
    host: HostArg,
    /// Apply the platform features, print what the OS reports back and exit. Draws nothing, except that
    /// `--passthrough pixel` draws the picture once on a GPU device to build its click-through mask.
    #[arg(long)]
    probe: bool,
}

/// Whether the run goes through eframe's event loop. A probe only asks the OS about the mascot window, so it never
/// starts the eframe window next to it, whatever `--host` says.
fn shares_eframe_loop(host: HostArg, probe: bool) -> bool {
    matches!(host, HostArg::Eframe) && !probe
}

fn run(cli: &Cli) -> Result<Outcome, RunError> {
    let config = cli.window.config(if cli.probe { Mode::Probe } else { Mode::Run });
    if shares_eframe_loop(cli.host, cli.probe) {
        host::run_shared(config, cli.window.gpu.dx12_visual())
    } else {
        app::run(config)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_run_on_the_eframe_host_shares_eframes_loop() {
        assert!(shares_eframe_loop(HostArg::Eframe, false));
        assert!(!shares_eframe_loop(HostArg::Eframe, true), "a probe never starts the eframe window");
        assert!(!shares_eframe_loop(HostArg::Winit, false));
        assert!(!shares_eframe_loop(HostArg::Winit, true));
    }
}
