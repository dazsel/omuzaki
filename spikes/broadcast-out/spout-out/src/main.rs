#![allow(linker_messages)] // old binutils warns about rustc's .drectve sections on every Windows link; nothing to act on

//! Command line for the spike. `render` works on any platform; `send`, `receive` and `loopback` need Windows.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use spout_out::analysis::{Blend, blend_over, quantise};
use spout_out::frame_tag::FrameTag;
use spout_out::gpu;
use spout_out::report::{Path, ReceiverKind, SyncMode};
use spout_out::scene::{SceneParams, SceneRenderer, TRUTH_BACKGROUND};

#[derive(Parser)]
#[command(about = "Spike: premultiplied-alpha frames from wgpu to Spout2, with a receiver to measure them")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Render the test scene to PNGs (premultiplied as stored, and composited over a checkerboard) on any backend.
    Render {
        /// Output prefix: writes `<prefix>_premultiplied.png`, `<prefix>_over_checker.png`, `<prefix>_truth.png`.
        #[arg(long)]
        out: PathBuf,
        /// Frame size.
        #[arg(long, default_value = "1920x1080", value_parser = parse_size)]
        size: (u32, u32),
        /// Frame number to draw (moves the animated disc).
        #[arg(long, default_value_t = 25)]
        frame: u32,
    },
    /// Send frames to Spout until done (Windows).
    Send(SendArgs),
    /// Receive frames from a Spout sender and print a JSON report (Windows).
    Receive {
        /// Sender name.
        #[arg(long)]
        name: String,
        /// Stop after this many frames.
        #[arg(long, default_value_t = 600)]
        frames: u32,
        /// Hash every n-th frame.
        #[arg(long, default_value_t = 100)]
        hash_every: u32,
        /// How to receive.
        #[arg(long, value_enum, default_value_t = ReceiverKind::Sdk)]
        receiver: ReceiverKind,
        /// Polling rate of the raw receiver, in reads per second.
        #[arg(long, default_value_t = 60.0)]
        poll_hz: f64,
    },
    /// Send from this process and receive in a child process, then compare (Windows).
    Loopback(SendArgs),
}

#[derive(clap::Args)]
struct SendArgs {
    /// Spout sender name.
    #[arg(long, default_value = "omuzaki-spike")]
    name: String,
    /// Frame size.
    #[arg(long, default_value = "1920x1080", value_parser = parse_size)]
    size: (u32, u32),
    /// Frames to send.
    #[arg(long, default_value_t = 600)]
    frames: u32,
    /// Frames per second; 0 means as fast as possible.
    #[arg(long, default_value_t = 60.0)]
    fps: f64,
    /// Hand-off path.
    #[arg(long, value_enum, default_value_t = Path::Gpu)]
    path: Path,
    /// Synchronisation before signalling a frame (GPU path).
    #[arg(long, value_enum, default_value_t = SyncMode::None)]
    sync: SyncMode,
    /// Read back and hash every n-th frame.
    #[arg(long, default_value_t = 100)]
    hash_every: u32,
    /// Print one JSON line instead of the readable summary.
    #[arg(long)]
    json: bool,
    /// How the receiving process reads (loopback only).
    #[arg(long, value_enum, default_value_t = ReceiverKind::Sdk)]
    receiver: ReceiverKind,
    /// Polling rate of the raw receiver, in reads per second (loopback only).
    #[arg(long, default_value_t = 60.0)]
    poll_hz: f64,
}

fn parse_size(text: &str) -> Result<(u32, u32), String> {
    let (w, h) = text.split_once('x').ok_or("expected WIDTHxHEIGHT")?;
    Ok((w.parse().map_err(|e| format!("width: {e}"))?, h.parse().map_err(|e| format!("height: {e}"))?))
}

fn main() -> ExitCode {
    match run(Cli::parse().command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> Result<(), String> {
    match command {
        Command::Render { out, size, frame } => render(&out, size, frame),
        #[cfg(windows)]
        Command::Send(args) => windows_run::send(&args),
        #[cfg(windows)]
        Command::Receive { name, frames, hash_every, receiver, poll_hz } => {
            windows_run::receive(name, frames, hash_every, receiver, poll_hz)
        }
        #[cfg(windows)]
        Command::Loopback(args) => windows_run::loopback(&args),
        #[cfg(not(windows))]
        Command::Send(_) | Command::Receive { .. } | Command::Loopback(_) => {
            Err("Spout is Windows-only; build with --target x86_64-pc-windows-gnu".to_owned())
        }
    }
}

/// Writes the scene three ways so the premultiplied encoding can be looked at, not just measured.
fn render(prefix: &std::path::Path, size: (u32, u32), frame: u32) -> Result<(), String> {
    let gpu = gpu::open(wgpu::Backends::all()).map_err(|e| e.to_string())?;
    let scene = SceneRenderer::new(&gpu.device, &gpu.queue, size).map_err(|e| e.to_string())?;
    #[allow(clippy::cast_precision_loss)] // an animation phase
    let t = frame as f32 * 0.02;
    let params = SceneParams { tag: FrameTag { frame, ticks: 0x0123_4567_89AB_CDEF }, t, truth: false };
    scene.render(&params);
    let premultiplied = scene.read_back().map_err(|e| e.to_string())?;
    scene.render(&SceneParams { truth: true, ..params });
    let truth = scene.read_back().map_err(|e| e.to_string())?;

    let stem = prefix.file_name().and_then(|n| n.to_str()).unwrap_or("scene");
    let save = |suffix: &str, bgra: &[u8]| save_png(&prefix.with_file_name(format!("{stem}_{suffix}.png")), bgra, size);
    save("premultiplied", &premultiplied)?;
    save("truth", &truth)?;
    save("over_checker", &over_checkerboard(&premultiplied, size.0))
}

/// Composites a premultiplied BGRA frame the way a correct "premultiplied" receiver would, over a checkerboard, so edges
/// and any fringe are visible.
fn over_checkerboard(premultiplied: &[u8], width: u32) -> Vec<u8> {
    let dark = TRUTH_BACKGROUND;
    let light = dark.map(|c| c + 60.0);
    let mut out = Vec::with_capacity(premultiplied.len());
    for (i, &px) in premultiplied.as_chunks::<4>().0.iter().enumerate() {
        let (column, row) = (i % width as usize, i / width as usize);
        let background = if (column / 32 + row / 32) % 2 == 0 { dark } else { light };
        let rgb = blend_over(px, background, Blend::Premultiplied);
        out.extend_from_slice(&[quantise(rgb[2]), quantise(rgb[1]), quantise(rgb[0]), 255]);
    }
    out
}

/// Writes a BGRA frame as an RGBA PNG.
fn save_png(path: &std::path::Path, bgra: &[u8], size: (u32, u32)) -> Result<(), String> {
    let mut rgba = bgra.to_vec();
    for px in rgba.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    let image = image::RgbaImage::from_raw(size.0, size.1, rgba).ok_or("pixel buffer size mismatch")?;
    image.save(path).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

#[cfg(windows)]
mod windows_run {
    use spout_out::loopback;
    use spout_out::raw_receiver;
    use spout_out::receiver::{self, ReceiveConfig};
    use spout_out::report::{LoopbackReport, REPORT_PREFIX, ReceiverKind, ReceiverReport, SenderReport};
    use spout_out::sender::{self, SendConfig};

    use super::SendArgs;

    fn config(args: &SendArgs, name: String) -> SendConfig {
        SendConfig {
            name,
            size: args.size,
            frames: args.frames,
            fps: args.fps,
            path: args.path,
            sync: args.sync,
            hash_every: args.hash_every,
        }
    }

    pub fn send(args: &SendArgs) -> Result<(), String> {
        let report = sender::run(&config(args, args.name.clone())).map_err(|e| e.to_string())?;
        if args.json {
            println!("{}", serde_json::to_string(&report).map_err(|e| e.to_string())?);
        } else {
            print_sender(&report);
        }
        Ok(())
    }

    pub fn receive(name: String, frames: u32, hash_every: u32, kind: ReceiverKind, poll_hz: f64) -> Result<(), String> {
        let config = ReceiveConfig { name, frames, hash_every };
        let report = match kind {
            ReceiverKind::Sdk => receiver::run(&config),
            ReceiverKind::Raw => raw_receiver::run(&config, poll_hz),
        }
        .map_err(|e| e.to_string())?;
        let json = serde_json::to_string(&report).map_err(|e| e.to_string())?;
        println!("{REPORT_PREFIX}{json}");
        Ok(())
    }

    pub fn loopback(args: &SendArgs) -> Result<(), String> {
        // A name no stale registry entry can collide with.
        let name = format!("{}-{}", args.name, std::process::id());
        let (sent, received) =
            loopback::run(&config(args, name), args.receiver, args.poll_hz).map_err(|e| e.to_string())?;
        if args.json {
            let both = LoopbackReport { sender: sent, receiver: received };
            println!("{}", serde_json::to_string(&both).map_err(|e| e.to_string())?);
        } else {
            print_sender(&sent);
            print_receiver(&sent, &received);
        }
        Ok(())
    }

    fn print_sender(r: &SenderReport) {
        println!(
            "sender {:?} on {} ({}), {}x{}, path {:?}, sync {:?}, target {} fps",
            r.name, r.adapter, r.backend, r.size.0, r.size.1, r.path, r.sync, r.fps
        );
        println!("  sent {} frames", r.sent);
        println!("  render submit (CPU):   {}", r.submit_ms);
        println!("  Spout hand-off (CPU):  {}", r.handoff_ms);
        println!("  frame period:          {}", r.period_ms);
        println!("  test patches hold the expected premultiplied bytes: {}", r.patches_ok);
        println!("  receiver blend modes against the GPU ground truth (8-bit levels):");
        for c in &r.composite {
            println!(
                "    sent {:?} -> blended as {:?}: max error {:.1}, mean {:.3}, pixels over 8 levels: {}",
                c.sent, c.blend, c.max_error, c.mean_error, c.visible_pixels
            );
        }
    }

    fn print_receiver(sent: &SenderReport, r: &ReceiverReport) {
        println!("receiver (separate process)");
        println!(
            "  received {} of {} frames (skipped {}, repeated {}, torn {}, patch failures {})",
            r.received, sent.sent, r.skipped, r.repeats, r.torn, r.patch_failures
        );
        println!("  sender start -> receiver notices frame:  {}", r.seen_ms);
        println!("  sender start -> pixels in receiver RAM:  {}", r.read_ms);
        println!("  interval between received frames:        {}", r.interval_ms);
        println!(
            "  reads {}, torn reads {} ({:.3}%), tear lines at rows {:?}",
            r.polls,
            r.torn_polls,
            100.0 * f64::from(r.torn_polls) / f64::from(r.polls.max(1)),
            r.tear_rows
        );
        let mut compared = 0;
        let mut mismatched = 0;
        for (frame, hash) in &sent.hashes {
            if let Some((_, theirs)) = r.hashes.iter().find(|(f, _)| f == frame) {
                compared += 1;
                mismatched += usize::from(theirs != hash);
            }
        }
        println!(
            "  sampled frames compared bit for bit with the sender's own readback: {compared}, mismatches: {mismatched}"
        );
    }
}
