//! `wp-spike`: `host` runs the experiment, `worker` is the process the host kills, `adapter` names the GPU a worker gets.

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use worker_process::Error;

/// Spike: a renderer in a killable worker process.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the experiment: own the output, kill the worker at seeded moments, report what the output saw.
    Host {
        /// Kills to perform.
        #[arg(long, default_value_t = 200)]
        kills: u32,
        /// Seed of the kill schedule.
        #[arg(long, default_value_t = 1022)]
        seed: u64,
        /// Frame size, `WIDTHxHEIGHT`.
        #[arg(long, default_value = "1920x1080")]
        size: String,
        /// Output frame rate.
        #[arg(long, default_value_t = 30.0)]
        fps: f64,
        /// Seconds the first worker runs undisturbed, for the per-frame cost.
        #[arg(long, default_value_t = 30)]
        baseline_s: u64,
        /// Workers started and stopped one at a time, for uncensored cold-start timings.
        #[arg(long, default_value_t = 30)]
        cold_starts: u32,
        /// Earliest kill, ms after a worker is spawned.
        #[arg(long, default_value_t = 20)]
        kill_min_ms: u64,
        /// Latest kill, ms after a worker is spawned.
        #[arg(long, default_value_t = 1500)]
        kill_max_ms: u64,
        /// Environment for the worker, KEY=VALUE (repeatable).
        #[arg(long = "worker-env")]
        worker_env: Vec<String>,
        /// Write the full report as JSON here.
        #[arg(long)]
        json: Option<std::path::PathBuf>,
    },
    /// The worker process. Started by the host; not for people.
    Worker {
        /// The slot ring file (Linux).
        #[arg(long)]
        shm: Option<std::path::PathBuf>,
        /// The host's process id, which names the shared objects (Windows).
        #[arg(long)]
        host: Option<u32>,
        /// This worker's number, which names the shared objects (Windows).
        #[arg(long)]
        worker_number: Option<u32>,
        /// Frame size, `WIDTHxHEIGHT`.
        #[arg(long)]
        size: String,
        /// Frame-rate cap.
        #[arg(long)]
        fps: f64,
    },
    /// Print the GPU adapter a worker would use.
    Adapter,
}

fn parse_size(text: &str) -> Result<(u32, u32), Error> {
    let bad = || Error::Config(format!("size must be WIDTHxHEIGHT, got {text:?}"));
    let (w, h) = text.split_once('x').ok_or_else(bad)?;
    Ok((w.parse().map_err(|_| bad())?, h.parse().map_err(|_| bad())?))
}

/// The host command line as options, and where the JSON goes.
struct HostArgs {
    options: worker_process::options::HostOptions,
    json: Option<std::path::PathBuf>,
}

fn host_args(command: Command) -> Result<HostArgs, Error> {
    let Command::Host { kills, seed, size, fps, baseline_s, cold_starts, kill_min_ms, kill_max_ms, worker_env, json } =
        command
    else {
        return Err(Error::Config("not a host command".into()));
    };
    let worker_env = worker_env
        .iter()
        .map(|pair| {
            pair.split_once('=')
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .ok_or_else(|| Error::Config(format!("--worker-env wants KEY=VALUE, got {pair:?}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    worker_process::options::period_for(fps)?;
    let options = worker_process::options::HostOptions {
        kills,
        seed,
        size: parse_size(&size)?,
        fps,
        baseline_s,
        cold_starts,
        kill_window_ms: (kill_min_ms, kill_max_ms),
        worker_env,
    };
    Ok(HostArgs { options, json })
}

/// Prints the report as JSON, or writes it where `--json` says.
fn emit(report: &impl serde::Serialize, json: Option<std::path::PathBuf>) -> Result<(), Error> {
    let text = serde_json::to_string_pretty(report).map_err(|e| Error::Config(e.to_string()))?;
    if let Some(path) = json {
        return std::fs::write(&path, &text).map_err(Error::io(format!("writing {}", path.display())));
    }
    println!("{text}");
    Ok(())
}

fn adapter_line() -> Result<String, Error> {
    let backends = if cfg!(windows) { wgpu::Backends::DX12 } else { wgpu::Backends::VULKAN };
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        .map_err(|e| Error::Config(format!("no adapter: {e}")))?;
    let info = adapter.get_info();
    Ok(format!("{} | {:?} | driver {} {}", info.name, info.device_type, info.driver, info.driver_info))
}

#[cfg(target_os = "linux")]
fn run(cli: Cli) -> Result<(), Error> {
    use worker_process::{cpu_host, cpu_worker};
    match cli.command {
        command @ Command::Host { .. } => {
            let args = host_args(command)?;
            emit(&cpu_host::run(&args.options)?, args.json)
        }
        Command::Worker { shm, size, fps, .. } => {
            let shm = shm.ok_or_else(|| Error::Config("the Linux worker needs --shm".into()))?;
            cpu_worker::run(&cpu_worker::WorkerArgs { shm, size: parse_size(&size)?, fps })
        }
        Command::Adapter => {
            println!("{}", adapter_line()?);
            Ok(())
        }
    }
}

#[cfg(windows)]
fn run(cli: Cli) -> Result<(), Error> {
    use worker_process::win;
    match cli.command {
        command @ Command::Host { .. } => {
            let args = host_args(command)?;
            emit(&win::host::run(&args.options)?, args.json)
        }
        Command::Worker { host, worker_number, size, fps, .. } => {
            let (Some(host), Some(worker)) = (host, worker_number) else {
                return Err(Error::Config("the Windows worker needs --host and --worker-number".into()));
            };
            win::worker::run(&win::worker::WorkerArgs { host, worker, size: parse_size(&size)?, fps })
        }
        Command::Adapter => {
            println!("{}", adapter_line()?);
            Ok(())
        }
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn run(_cli: Cli) -> Result<(), Error> {
    Err(Error::Config("this platform has no lane in the spike yet (Linux CPU lane, Windows DX12 lane)".into()))
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("wp-spike: {e}");
            ExitCode::FAILURE
        }
    }
}
