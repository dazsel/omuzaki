//! The voice-effect budgets of spec §10 in the budget harness's report (#1159): `voice.fx` and `voice.fx.pitch`, the
//! added latency of the BR-24 chain without and with the pitch and formant shifter (declared latency plus one
//! 256-sample block), and `voice.fx.cpu`, the chain's CPU with every effect on as a share of 8 cores, the mean over
//! `--seconds` of the synthetic voice (300 by default: the §10 "mean over 5 min").
//!
//! ```text
//! cargo run --release -p omuzaki-audio-fx --example voice_fx_budget -- [--seconds N] [--report DIR]
//!     [--class CLASS] [--previous FILE] [--strict]
//! ```
//!
//! The CPU is timed per 256-sample block on the thread that runs the chain; a block is 5.33 ms of audio, so a block
//! that takes `t` uses `t / 5.33 ms` of one core, and `/ 8` of eight. §10 measures it on `Dev` pinned to one E-core;
//! anywhere else the number is `reported` and never gates.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::float_cmp,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::many_single_char_names
)]

use std::path::PathBuf;
use std::time::Instant;

use omuzaki_audio_api::graph::NodeSpec;
use omuzaki_audio_fx::chain::Chain;
use omuzaki_audio_fx::presets::{budget_chain, budget_chain_with_pitch};
use omuzaki_audio_mock::signals::{VoiceSpec, voice};
use omuzaki_test::budget::report::{Class, Machine, Metric, Provenance, Report};
use omuzaki_test::budget::table::{Unit, parse_spec};

const SPEC: &str = include_str!("../../../docs/product/02-studio-spec-v3.md");
const BLOCK: usize = 256;
const CORES: f64 = 8.0;

struct Arguments {
    seconds: usize,
    report: Option<PathBuf>,
    class: Class,
    previous: Option<PathBuf>,
    strict: bool,
}

fn arguments() -> Result<Arguments, String> {
    let mut parsed = Arguments { seconds: 300, report: None, class: Class::Reported, previous: None, strict: false };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--seconds" => {
                let v = value()?;
                parsed.seconds = v.parse().map_err(|_| format!("--seconds {v:?} is not a number"))?;
                if parsed.seconds == 0 {
                    return Err("--seconds must be at least 1".to_owned());
                }
            }
            "--report" => parsed.report = Some(PathBuf::from(value()?)),
            "--previous" => parsed.previous = Some(PathBuf::from(value()?)),
            "--class" => parsed.class = Class::parse(&value()?).map_err(|e| e.to_string())?,
            "--strict" => parsed.strict = true,
            "--bench" => {} // `cargo bench` passes it to every target
            other => {
                return Err(format!(
                    "unknown argument {other:?}; use --seconds N, --report DIR, --class CLASS, --previous FILE, --strict"
                ));
            }
        }
    }
    Ok(parsed)
}

/// The added latency of `chain` in milliseconds: its declared latency plus one block.
fn added_ms(chain: &[NodeSpec]) -> Result<f64, String> {
    let latency = Chain::build(chain).map_err(|e| e.to_string())?.latency() as usize + BLOCK;
    Ok(latency as f64 / 48.0)
}

/// The CPU share of 8 cores of each block of `seconds` of voice through the full chain, in percent.
fn cpu(seconds: usize) -> Result<Vec<f64>, String> {
    // One second of voice, looped: the chain's cost doesn't depend on which second it is, and rendering five minutes
    // of the voice would take longer than the chain does.
    let second = voice(&VoiceSpec::steady(1, 1.0));
    let mut chain = Chain::build(&budget_chain_with_pitch()).map_err(|e| e.to_string())?;
    let mut buffer = vec![0.0f32; BLOCK];
    let mut shares = Vec::with_capacity(seconds * second.len() / BLOCK + 1);
    for _ in 0..seconds {
        for block in second.chunks(BLOCK) {
            let buffer = &mut buffer[..block.len()];
            buffer.copy_from_slice(block);
            let start = Instant::now();
            chain.process(buffer);
            let took = start.elapsed().as_secs_f64();
            std::hint::black_box(&buffer);
            shares.push(100.0 * took / (block.len() as f64 / 48_000.0) / CORES);
        }
    }
    Ok(shares)
}

fn run() -> Result<(), String> {
    let args = arguments()?;
    let budgets = parse_spec(SPEC).map_err(|e| format!("the built-in spec's §10 table: {e}"))?;
    let provenance = Provenance::from_env();
    Provenance::require(provenance.as_ref(), args.class).map_err(|e| e.to_string())?;
    let previous = match &args.previous {
        Some(path) => {
            let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            Some(Report::from_json(&text).map_err(|e| e.to_string())?)
        }
        None => None,
    };
    let fx = added_ms(&budget_chain())?;
    let pitch = added_ms(&budget_chain_with_pitch())?;
    let mut shares = cpu(args.seconds)?;
    let mean = shares.iter().sum::<f64>() / shares.len() as f64;
    shares.sort_by(f64::total_cmp);
    let p95 = shares[shares.len() * 95 / 100];
    println!("voice.fx: {fx:.2} ms; voice.fx.pitch: {pitch:.2} ms");
    println!(
        "voice.fx.cpu: mean {mean:.3}% of 8 cores (p95 of a block {p95:.3}%) over {} s, {} blocks",
        args.seconds,
        shares.len()
    );
    let metrics = vec![
        Metric::single("voice.fx", Unit::Ms, fx).ok_or("no voice.fx number")?,
        Metric::single("voice.fx.pitch", Unit::Ms, pitch).ok_or("no voice.fx.pitch number")?,
        Metric::single("voice.fx.cpu", Unit::Percent, mean).ok_or("no voice.fx.cpu number")?,
    ];
    let notes = vec![format!(
        "the BR-24 chain (gate, EQ, compressor, robot, radio; pitch and formant for `voice.fx.pitch` and `voice.fx.cpu`) \
         on the synthetic voice in {BLOCK}-sample blocks; `voice.fx.cpu` is the mean over {} s on the thread that ran it",
        args.seconds
    )];
    let report = Report::build(Machine::current(), metrics, &budgets, previous.as_ref(), args.class, notes)
        .with_provenance(provenance);
    if let Some(dir) = &args.report {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let write = |name: &str, text: String| {
            let path = dir.join(name);
            std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
        };
        write("report.md", report.to_markdown())?;
        write("report.json", report.to_json().map_err(|e| e.to_string())?)?;
    }
    println!();
    println!("{}", report.to_markdown());
    if args.strict {
        let failures = report.failures();
        if !failures.is_empty() {
            return Err(format!("over budget: {}", failures.join("; ")));
        }
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("voice_fx_budget: {error}");
        std::process::exit(1);
    }
}
