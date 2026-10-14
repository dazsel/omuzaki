//! Benchmark and accuracy harness for the speech spike.
//!
//! `vad-synth`  synthetic scenes through Silero VAD: false alarms on non-speech, per-frame latency.
//! `vad`        a speech recording through Silero VAD: agreement with energy labels, noise robustness, latency.
//! `pipeline`   VAD segments each transcribed by Whisper: word error rate, per-utterance latency.

// Same reasoning as the library (see `speech_spike`): audio and timing arithmetic, far from any precision limit.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::{Args, Parser, Subcommand};
use speech_spike::asr::{AsrOptions, Decoding, Whisper};
use speech_spike::metrics::{Edit, LatencyStats, wer};
use speech_spike::pipeline::{boundary_lag, detect, frame_scores};
use speech_spike::segmenter::{Event, FRAME, Segment, SegmenterConfig};
use speech_spike::vad::{SileroVad, load_runtime};
use speech_spike::{SAMPLE_RATE, synth, wav};

type Failure = Box<dyn Error>;

#[derive(Parser)]
#[command(about = "Silero VAD + Whisper benchmark for the omuzaki speech spike")]
struct Cli {
    /// The ONNX Runtime shared library (`libonnxruntime.so` or `onnxruntime.dll`).
    #[arg(long, env = "ORT_DYLIB_PATH", global = true)]
    runtime: Option<PathBuf>,
    /// The Silero VAD model (`silero_vad.onnx`).
    #[arg(long, global = true)]
    vad_model: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Synthetic scenes: false alarms on non-speech and per-frame latency.
    VadSynth,
    /// One speech recording: label agreement, noise robustness, boundary latency.
    Vad(VadArgs),
    /// VAD then Whisper on each segment, per model.
    Pipeline(PipelineArgs),
}

#[derive(Args)]
struct VadArgs {
    /// A 16 kHz mono WAV of clean speech.
    audio: PathBuf,
}

#[derive(Args)]
struct PipelineArgs {
    /// `audio.wav:reference.txt` pairs (the reference may be omitted: `audio.wav`).
    #[arg(long = "clip", required = true)]
    clips: Vec<String>,
    /// ggml Whisper models to compare.
    #[arg(long = "model", required = true)]
    models: Vec<PathBuf>,
    /// Whisper threads.
    #[arg(long, default_value_t = 4)]
    threads: u32,
    /// Beam width; 0 means greedy.
    #[arg(long, default_value_t = 0)]
    beam: u8,
    /// Language code, or `auto` to detect.
    #[arg(long, default_value = "en")]
    language: String,
    /// Shrink Whisper's encoder window to the segment length.
    #[arg(long)]
    fit_audio_ctx: bool,
    /// Timed repetitions per segment (the median is reported).
    #[arg(long, default_value_t = 3)]
    runs: usize,
    /// Minimum silence that ends a segment, in milliseconds.
    #[arg(long, default_value_t = 200)]
    min_silence_ms: u32,
    /// Padding kept around each segment, in milliseconds.
    #[arg(long, default_value_t = 100)]
    pad_ms: u32,
    /// Print every segment's text and the word diff against the reference.
    #[arg(long)]
    show: bool,
    /// Add pink noise at this speech-to-noise ratio (dB) before anything else, for noise robustness.
    #[arg(long)]
    snr: Option<f32>,
    /// Skip the VAD and cut the audio into fixed windows of this many seconds instead (the no-VAD baseline).
    #[arg(long)]
    fixed_window_s: Option<f32>,
}

fn main() -> Result<(), Failure> {
    let cli = Cli::parse();
    let runtime = cli.runtime.ok_or("--runtime (or ORT_DYLIB_PATH) is required")?;
    let vad_model = cli.vad_model.ok_or("--vad-model is required")?;
    load_runtime(&runtime)?;
    let mut vad = SileroVad::new(&vad_model)?;
    match cli.command {
        Command::VadSynth => vad_synth(&mut vad),
        Command::Vad(args) => vad_speech(&mut vad, &args),
        Command::Pipeline(args) => pipeline(&mut vad, &args),
    }
}

fn ms(stats: Option<LatencyStats>) -> String {
    stats.map_or_else(
        || "n/a".into(),
        |s| {
            format!(
                "mean {:.3} p50 {:.3} p95 {:.3} p99 {:.3} max {:.3} (n={})",
                s.mean, s.p50, s.p95, s.p99, s.max, s.n
            )
        },
    )
}

fn dbfs(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

fn vad_synth(vad: &mut SileroVad) -> Result<(), Failure> {
    let scenes: Vec<(&str, Vec<f32>)> = vec![
        ("silence", synth::silence(10.0)),
        ("white noise -40 dBFS", synth::white_noise(10.0, dbfs(-40.0), 1)),
        ("white noise -20 dBFS", synth::white_noise(10.0, dbfs(-20.0), 2)),
        ("pink noise -30 dBFS", synth::pink_noise(10.0, dbfs(-30.0), 3)),
        ("pink noise -12 dBFS", synth::pink_noise(10.0, dbfs(-12.0), 4)),
        ("50 Hz hum -20 dBFS", synth::hum(10.0, 50.0, dbfs(-20.0))),
        ("220 Hz tone stack -15 dBFS", synth::hum(10.0, 220.0, dbfs(-15.0))),
        ("keyboard clicks -10 dBFS", synth::clicks(10.0, 0.25, dbfs(-10.0), 5)),
        ("vowel-like syllables (not speech)", synth::voiced_syllables(25, 6)),
    ];
    println!("{:<36} {:>7} {:>9} {:>9} {:>9}", "scene (10 s each)", "frames", "max p", ">=0.5 %", "segments");
    let mut all_frame_ms = Vec::new();
    for (name, audio) in &scenes {
        let d = detect(vad, audio, SegmenterConfig::default())?;
        let above = d.probabilities.iter().filter(|&&p| p >= 0.5).count();
        let max = d.probabilities.iter().fold(0.0f32, |m, &p| m.max(p));
        let segments = d.events.iter().filter(|(_, e)| matches!(e, Event::End(_))).count();
        println!(
            "{:<36} {:>7} {:>9.3} {:>9.1} {:>9}",
            name,
            d.probabilities.len(),
            max,
            100.0 * above as f64 / d.probabilities.len() as f64,
            segments
        );
        all_frame_ms.extend(d.frame_ms);
    }
    println!("per-frame inference ms: {}", ms(LatencyStats::from_samples(&all_frame_ms)));
    Ok(())
}

/// Silence, then the clip, then silence: the padding gives the first onset and the last end a quiet surround.
fn framed(audio: &[f32]) -> Vec<f32> {
    let mut out = synth::silence(2.0);
    out.extend_from_slice(audio);
    out.extend(synth::silence(2.0));
    out
}

fn vad_speech(vad: &mut SileroVad, args: &VadArgs) -> Result<(), Failure> {
    let clip = framed(&wav::read_mono_16k(&args.audio)?);
    let seconds = clip.len() as f64 / f64::from(SAMPLE_RATE);
    println!("{}: {seconds:.1} s, {} frames", args.audio.display(), clip.len().div_ceil(FRAME));

    // Warm up the session (first run allocates), then run the clean recording.
    detect(vad, &clip[..FRAME * 20], SegmenterConfig::default())?;
    let started = Instant::now();
    let clean = detect(vad, &clip, SegmenterConfig::default())?;
    let wall = started.elapsed().as_secs_f64();
    println!("real-time factor {:.5} (wall {wall:.3} s for {seconds:.1} s of audio)", wall / seconds);
    println!("per-frame inference ms: {}", ms(LatencyStats::from_samples(&clean.frame_ms)));
    let segments = clean.kept_segments(clip.len());
    let rejected = clean.events.iter().filter(|(_, e)| matches!(e, Event::Rejected(_))).count();
    println!(
        "segments kept: {} covering {:.1} s of {seconds:.1} s ({rejected} rejected as too short)",
        segments.len(),
        segments.iter().map(Segment::len).sum::<usize>() as f64 / f64::from(SAMPLE_RATE)
    );
    let lag = boundary_lag(&clip, &clean.events);
    println!("onset: sound -> Start event ms: {}", ms(LatencyStats::from_samples(&lag.onset_ms)));
    println!("end:   last speech energy -> End event ms: {}", ms(LatencyStats::from_samples(&lag.end_ms)));

    // Robustness without hand labels: how many frame decisions change when noise is added to the clean recording.
    let reference: Vec<bool> = clean.probabilities.iter().map(|&p| p >= 0.5).collect();
    for (noise_name, noise) in
        [("white", synth::white_noise(10.0, 1.0, 11)), ("pink", synth::pink_noise(10.0, 1.0, 12))]
    {
        for snr in [20.0, 10.0, 5.0, 0.0] {
            let noisy = synth::mix_at_snr(&clip, &noise, snr);
            let d = detect(vad, &noisy, SegmenterConfig::default())?;
            let s = frame_scores(&reference, &d.probabilities, 0.5);
            println!(
                "{noise_name} noise {snr:>4.0} dB SNR vs clean decisions: precision {:.3} recall {:.3} f1 {:.3}; {} segments",
                s.precision,
                s.recall,
                s.f1,
                d.kept_segments(noisy.len()).len()
            );
        }
    }
    Ok(())
}

struct Clip {
    name: String,
    audio: Vec<f32>,
    reference: Option<String>,
}

fn load_clips(specs: &[String]) -> Result<Vec<Clip>, Failure> {
    specs
        .iter()
        .map(|spec| {
            let (audio_path, reference_path) =
                spec.split_once(':').map_or((spec.as_str(), None), |(a, r)| (a, Some(r)));
            let reference = reference_path.map(std::fs::read_to_string).transpose()?;
            let name = PathBuf::from(audio_path)
                .file_stem()
                .map_or_else(|| audio_path.to_owned(), |s| s.to_string_lossy().into_owned());
            Ok(Clip { name, audio: wav::read_mono_16k(&PathBuf::from(audio_path))?, reference })
        })
        .collect()
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

/// A clip as the ASR will see it (noise added if asked) and the pieces to transcribe.
struct Plan {
    clip: Clip,
    segments: Vec<Segment>,
}

/// Decides, per clip, which stretches go to Whisper: VAD segments, or fixed windows for the no-VAD baseline.
fn plan(vad: &mut SileroVad, clips: Vec<Clip>, args: &PipelineArgs) -> Result<Vec<Plan>, Failure> {
    let config =
        SegmenterConfig { min_silence_ms: args.min_silence_ms, pad_ms: args.pad_ms, ..SegmenterConfig::default() };
    clips
        .into_iter()
        .map(|mut clip| {
            if let Some(snr) = args.snr {
                clip.audio = synth::mix_at_snr(&clip.audio, &synth::pink_noise(10.0, 1.0, 12), snr);
            }
            let segments = match args.fixed_window_s {
                Some(seconds) => {
                    let step = (seconds * SAMPLE_RATE as f32) as usize;
                    (0..clip.audio.len())
                        .step_by(step)
                        .map(|start| Segment { start, end: (start + step).min(clip.audio.len()) })
                        .collect()
                }
                None => detect(vad, &clip.audio, config)?.kept_segments(clip.audio.len()),
            };
            Ok(Plan { clip, segments })
        })
        .collect()
}

/// Running totals over all clips for one model.
#[derive(Default)]
struct Totals {
    errors: usize,
    reference_words: usize,
    segment_ms: Vec<f64>,
    audio_s: f64,
    elapsed_s: f64,
}

/// Transcribes `audio` `runs` times and returns the (identical, greedy) text and the median wall time in ms.
fn transcribe_timed(
    whisper: &mut Whisper,
    audio: &[f32],
    options: &AsrOptions,
    runs: usize,
) -> Result<(String, f64), Failure> {
    let mut times = Vec::new();
    let mut text = String::new();
    for _ in 0..runs.max(1) {
        let t = whisper.transcribe(audio, options)?;
        times.push(t.elapsed.as_secs_f64() * 1e3);
        text = t.text;
    }
    Ok((text, median(&mut times)))
}

fn report_clip(plan: &Plan, hypothesis: &str, show: bool, totals: &mut Totals) {
    let Some(reference) = &plan.clip.reference else {
        println!("  {:<30} {} segments: {hypothesis}", plan.clip.name, plan.segments.len());
        return;
    };
    let w = wer(reference, hypothesis);
    totals.errors += w.errors();
    totals.reference_words += w.reference_words;
    println!(
        "  {:<30} {:>2} segments, WER {:5.1} % ({} S, {} D, {} I of {} words)",
        plan.clip.name,
        plan.segments.len(),
        100.0 * w.rate(),
        w.substitutions,
        w.deletions,
        w.insertions,
        w.reference_words
    );
    if show {
        for (edit, r, h) in w.alignment.iter().filter(|(e, _, _)| *e != Edit::Match) {
            println!("      {edit:?}: ref {:?} -> hyp {:?}", r.as_deref().unwrap_or("-"), h.as_deref().unwrap_or("-"));
        }
    }
}

fn run_model(model_path: &Path, plans: &[Plan], args: &PipelineArgs, options: &AsrOptions) -> Result<(), Failure> {
    let loading = Instant::now();
    let mut whisper = Whisper::load(model_path)?;
    let load_s = loading.elapsed().as_secs_f64();
    // The first call allocates the compute buffers; keep that out of the timings.
    whisper.transcribe(&synth::white_noise(1.0, 0.001, 1), options)?;
    let name = model_path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    println!(
        "== {name}  (load {load_s:.2} s, threads {}, beam {}, fit-audio-ctx {})",
        args.threads, args.beam, args.fit_audio_ctx
    );
    let mut totals = Totals::default();
    for plan in plans {
        let mut texts = Vec::new();
        for seg in &plan.segments {
            let audio = &plan.clip.audio[seg.start..seg.end];
            let (text, elapsed_ms) = transcribe_timed(&mut whisper, audio, options, args.runs)?;
            totals.segment_ms.push(elapsed_ms);
            totals.audio_s += audio.len() as f64 / f64::from(SAMPLE_RATE);
            totals.elapsed_s += elapsed_ms / 1e3;
            if args.show {
                println!(
                    "  [{:6.2} - {:6.2} s] {elapsed_ms:>6.0} ms  {text}",
                    seg.start as f64 / 16e3,
                    seg.end as f64 / 16e3
                );
            }
            texts.push(text);
        }
        report_clip(plan, &texts.join(" "), args.show, &mut totals);
    }
    if totals.reference_words > 0 {
        println!(
            "  TOTAL WER {:.2} % ({} errors / {} words)",
            100.0 * totals.errors as f64 / totals.reference_words as f64,
            totals.errors,
            totals.reference_words
        );
    }
    println!("  per-segment ASR ms: {}", ms(LatencyStats::from_samples(&totals.segment_ms)));
    println!("  real-time factor (ASR time / segment audio): {:.3}", totals.elapsed_s / totals.audio_s);
    Ok(())
}

fn pipeline(vad: &mut SileroVad, args: &PipelineArgs) -> Result<(), Failure> {
    let plans = plan(vad, load_clips(&args.clips)?, args)?;
    let options = AsrOptions {
        decoding: if args.beam == 0 { Decoding::Greedy } else { Decoding::Beam(args.beam) },
        threads: args.threads,
        language: (args.language != "auto").then(|| args.language.clone()),
        fit_audio_context: args.fit_audio_ctx,
    };
    for model in &args.models {
        run_model(model, &plans, args, &options)?;
    }
    Ok(())
}
