//! Decodes a file and reports what came out: `cargo run -p omuzaki-audio-decode --release --example decode_check --
//! <file> [tone Hz]`. With a tone, each channel is compared with the best-fitting sine at that frequency (amplitude
//! and phase fitted, so an encoder's delay doesn't count), which measures a lossy codec's residual.
#![allow(clippy::print_stdout, clippy::cast_precision_loss)]

use std::f64::consts::TAU;
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next().map(PathBuf::from) else {
        println!("usage: decode_check <file> [tone Hz]");
        return;
    };
    let tone: Option<f64> = args.next().and_then(|t| t.parse().ok());
    let started = Instant::now();
    let sound = match omuzaki_audio_decode::decode_file(&path) {
        Ok(sound) => sound,
        Err(error) => {
            println!("{}: {error}", path.display());
            return;
        }
    };
    let took = started.elapsed();
    let seconds = sound.frames() as f64 / 48_000.0;
    print!(
        "{}: {} ch, {} frames ({seconds:.2} s) at 48 kHz, decoded in {:.1} ms ({:.0}x real time)",
        path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
        sound.channel_count(),
        sound.frames(),
        took.as_secs_f64() * 1e3,
        seconds / took.as_secs_f64(),
    );
    if let Some(frequency) = tone {
        for c in 0..sound.channel_count() {
            print!(", ch {c} residual {:.1} dB", residual_db(sound.channel(c), frequency));
        }
    }
    println!();
}

/// The level of what's left after removing the best-fitting sine at `frequency`, relative to that sine, over the
/// middle of the signal.
fn residual_db(samples: &[f32], frequency: f64) -> f64 {
    let offset = samples.len() / 10;
    let middle = &samples[offset..samples.len() * 9 / 10];
    let (mut ss, mut sc, mut cc, mut xs, mut xc) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (i, x) in middle.iter().enumerate() {
        let t = TAU * frequency * (offset + i) as f64 / 48_000.0;
        let (s, c, x) = (t.sin(), t.cos(), f64::from(*x));
        ss += s * s;
        sc += s * c;
        cc += c * c;
        xs += x * s;
        xc += x * c;
    }
    let det = ss * cc - sc * sc;
    let (a, b) = ((xs * cc - xc * sc) / det, (xc * ss - xs * sc) / det);
    let (mut residual, mut signal) = (0.0, 0.0);
    for (i, x) in middle.iter().enumerate() {
        let t = TAU * frequency * (offset + i) as f64 / 48_000.0;
        let fit = a * t.sin() + b * t.cos();
        residual += (f64::from(*x) - fit).powi(2);
        signal += fit * fit;
    }
    10.0 * (residual / signal).log10()
}
