//! Real-time benchmark of the speech host: plays a 16 kHz mono WAV into the host at 1x speed, like a microphone, and
//! reports speech-end-to-final-transcript latency, Whisper's real-time factor and the CPU the process used.
//!
//!     host_bench <models-dir> <clip.wav> [whisper-file (default ggml-base.en.bin)] [threads (default 4)]
//!                [hangover-ms (default: the host's, 600)] [listen (listening only: no transcript consumer)]
//!
//! The ONNX Runtime library comes from `ORT_DYLIB_PATH` or `<models-dir>/onnxruntime-linux-x64-1.28.3/lib`.
//! CPU time is read from `/proc/self/stat` (user plus system, 100 ticks per second), so this runs on Linux only.
//! Spec v3 §10 `cpu.total` divides CPU seconds by 8 x elapsed seconds; the report prints that share.

#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::too_many_lines)] // a measurement script: readability over cast purity

use std::path::PathBuf;
use std::time::{Duration, Instant};

use omuzaki_audio::host::{AudioInput, ModelPaths, SpeechEvent};
use omuzaki_audio::{HostConfig, SpeechHost};

const BLOCK: usize = 160; // 10 ms, a typical capture callback
const SPEC_CORES: f64 = 8.0;

fn cpu_seconds() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    // Fields after the parenthesised command name; utime and stime are the 12th and 13th of those.
    let rest = stat.rsplit_once(") ").map_or("", |(_, rest)| rest);
    let ticks: f64 = rest.split(' ').skip(11).take(2).filter_map(|f| f.parse::<f64>().ok()).sum();
    ticks / 100.0
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn read_wav(path: &std::path::Path) -> Result<Vec<f32>, hound::Error> {
    let mut reader = hound::WavReader::open(path)?;
    let scale = 1.0 / (1u64 << (reader.spec().bits_per_sample - 1)) as f32;
    reader.samples::<i32>().map(|s| s.map(|v| v as f32 * scale)).collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [models, clip, rest @ ..] = args.as_slice() else {
        return Err("usage: host_bench <models-dir> <clip.wav> [whisper-file] [threads]".into());
    };
    let models = PathBuf::from(models);
    let whisper_file = rest.first().map_or("ggml-base.en.bin", String::as_str);
    let threads: u32 = rest.get(1).map_or(Ok(4), |t| t.parse())?;
    let paths = ModelPaths {
        ort_library: std::env::var_os("ORT_DYLIB_PATH")
            .map_or_else(|| models.join("onnxruntime-linux-x64-1.28.3/lib/libonnxruntime.so"), PathBuf::from),
        vad_model: models.join("silero_vad.onnx"),
        whisper_model: models.join(whisper_file),
    };
    let hangover_ms: Option<u32> = rest.get(2).map(|t| t.parse()).transpose()?;
    let listen_only = rest.get(3).is_some_and(|m| m == "listen");
    let audio = read_wav(std::path::Path::new(clip))?;
    let audio_seconds = audio.len() as f64 / 16_000.0;
    let mut config = HostConfig { threads, utterance_queue: 64, ..HostConfig::default() };
    if let Some(ms) = hangover_ms {
        config.segmenter.min_silence_ms = ms;
    }
    let pad = f64::from(config.segmenter.pad_ms) / 1000.0;
    let hangover = f64::from(config.segmenter.min_silence_ms) / 1000.0;

    let loading = Instant::now();
    let (mut host, mut input) = SpeechHost::open(&paths, &config)?;
    let load_s = loading.elapsed().as_secs_f64();
    // Whisper runs only while a consumer wants transcripts; `listen` measures the VAD alone.
    let _demand = (!listen_only).then(|| host.request_transcripts());

    // Play the clip in real time, then silence long enough for the last utterance to close.
    let mut tail = vec![0.0f32; 16_000 * 2];
    let mut stream = audio;
    stream.append(&mut tail);
    let (cpu0, t0) = (cpu_seconds(), Instant::now());
    let mut finals: Vec<(f64, Duration)> = Vec::new(); // (speech end in stream seconds, wall time received)
    let mut latencies = Vec::new(); // last speech sound to final text, wall clock
    let mut detect_to_final = Vec::new();
    let mut asr_times = Vec::new();
    let mut errors = 0;
    let mut spoken = 0.0;
    let asr_count = std::cell::Cell::new(0usize);
    let mut collect = |host: &SpeechHost, now: Duration| {
        while let Some(event) = host.try_next_event() {
            match event {
                SpeechEvent::Final { end, latency, .. } => {
                    // The last spoken sound is the utterance end minus the padding the segmenter added.
                    latencies.push(now.as_secs_f64() - (end - pad));
                    detect_to_final.push(latency.detect_to_final.as_secs_f64());
                    asr_times.push(latency.asr.as_secs_f64());
                    finals.push((end, now));
                    asr_count.set(asr_count.get() + 1);
                }
                SpeechEvent::SpeechEnded { start, end, .. } => spoken += end - start,
                SpeechEvent::Error { message, .. } => {
                    errors += 1;
                    eprintln!("error: {message}");
                }
                _ => {}
            }
        }
    };
    play(&mut input, &stream, t0, |now| collect(&host, now));
    // Keep collecting until every queued utterance has its transcript (or 60 s pass), so late finals get true arrival times.
    let give_up = Instant::now() + Duration::from_secs(60);
    loop {
        collect(&host, t0.elapsed());
        let stats = host.stats();
        let settled = stats.utterances_dropped + (asr_count.get() as u64) >= stats.utterances;
        if settled || Instant::now() > give_up {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let wall_play = t0.elapsed().as_secs_f64();
    let stats = host.stop();
    let (wall, cpu) = (wall_play, cpu_seconds() - cpu0);

    latencies.sort_by(f64::total_cmp);
    detect_to_final.sort_by(f64::total_cmp);
    asr_times.sort_by(f64::total_cmp);
    println!(
        "model {whisper_file}, {threads} threads, hangover {:.0} ms, {}, clip {audio_seconds:.1} s, {errors} errors, load {load_s:.2} s",
        hangover * 1000.0,
        if listen_only { "listening only" } else { "transcribing" },
    );
    println!(
        "whisper calls {} ({} utterances heard, {} skipped without a consumer); talk ratio (utterance audio / clip) {:.2}",
        finals.len(),
        stats.utterances + stats.utterances_skipped,
        stats.utterances_skipped,
        spoken / audio_seconds
    );
    println!(
        "speech end -> final text (wall, last spoken sound to text): p50 {:.2} s, p95 {:.2} s, max {:.2} s  [= {hangover:.2} s hangover + detect->final p50 {:.2} s, p95 {:.2} s]",
        percentile(&latencies, 0.5),
        percentile(&latencies, 0.95),
        percentile(&latencies, 1.0),
        percentile(&detect_to_final, 0.5),
        percentile(&detect_to_final, 0.95),
    );
    println!(
        "whisper per utterance: p50 {:.2} s, p95 {:.2} s; real-time factor (whisper busy / audio) {:.3}",
        percentile(&asr_times, 0.5),
        percentile(&asr_times, 0.95),
        stats.asr_busy.as_secs_f64() / audio_seconds
    );
    println!(
        "vad thread busy {:.3} s over {} frames ({:.2} ms/frame); dropped: {} utterances, {} samples",
        stats.vad_busy.as_secs_f64(),
        stats.frames,
        stats.vad_busy.as_secs_f64() * 1000.0 / stats.frames as f64,
        stats.utterances_dropped,
        stats.samples_dropped
    );
    println!(
        "process CPU {cpu:.2} s over {wall:.1} s wall = {:.2} cores = {:.2} % of {SPEC_CORES} cores (spec cpu.total basis)",
        cpu / wall,
        100.0 * cpu / (SPEC_CORES * wall)
    );
    println!(
        "listening only (VAD thread): {:.3} % of {SPEC_CORES} cores",
        100.0 * stats.vad_busy.as_secs_f64() / (SPEC_CORES * wall)
    );
    Ok(())
}

/// Pushes `stream` in 10 ms blocks on a wall-clock schedule and calls `poll` with the elapsed time after each block.
fn play(input: &mut AudioInput, stream: &[f32], t0: Instant, mut poll: impl FnMut(Duration)) {
    for (i, block) in stream.chunks(BLOCK).enumerate() {
        let due = t0 + Duration::from_secs_f64((i * BLOCK) as f64 / 16_000.0);
        std::thread::sleep(due.saturating_duration_since(Instant::now()));
        input.push(block);
        poll(t0.elapsed());
    }
}
