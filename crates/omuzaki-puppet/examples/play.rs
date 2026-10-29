//! Plays a track on a puppet bundle and writes the frames as raw straight RGBA8 to standard output (or a file), for
//! `ffmpeg -f rawvideo` or a script to read.
//!
//! ```text
//! cargo run --release -p omuzaki-puppet --example play -- <bundle dir> <track.json> [out.rgba]
//! ```

use std::io::Write as _;
use std::path::Path;

use omuzaki_puppet::track::PuppetTrack;
use omuzaki_puppet::{CpuRenderer, Evaluator, bundle};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [dir, track, rest @ ..] = args.as_slice() else {
        return Err("usage: play <bundle dir> <track.json> [out.rgba]".into());
    };
    let bundle = bundle::load(Path::new(dir))?;
    let track: PuppetTrack = serde_json::from_slice(&std::fs::read(track)?)?;
    let mut eval = Evaluator::new(bundle.def)?;
    let mut out: Box<dyn std::io::Write> = match rest.first() {
        Some(path) => Box::new(std::io::BufWriter::new(std::fs::File::create(path)?)),
        None => Box::new(std::io::BufWriter::new(std::io::stdout().lock())),
    };
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let started = std::time::Instant::now();
    track.play(&mut eval, &bundle.textures, &mut CpuRenderer::new(threads), |_, rgba| {
        out.write_all(rgba).map_err(|e| e.to_string())
    })?;
    out.flush()?;
    eprintln!("{} frames in {:.1} s", track.frames.len(), started.elapsed().as_secs_f32());
    Ok(())
}
