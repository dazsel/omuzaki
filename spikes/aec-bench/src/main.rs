//! `aec-bench`: echo return loss enhancement, double-talk preservation, per-frame CPU and a delay sweep for each
//! echo-cancellation candidate, on synthetic seeded signals only (research #920).
//!
//! Usage: `cargo run --release -- [table|sweep|seeds]` (default `table`).

mod candidates;
mod count_alloc;
mod own;
mod signals;

use signals::{FRAME, RATE, ROOMS, Rng, Room, speech_like};
use std::time::Instant;

#[global_allocator]
static ALLOC: count_alloc::Counting = count_alloc::Counting;

/// A mono 48 kHz echo canceller working in 10 ms blocks.
pub trait Canceller {
    fn name(&self) -> String;
    fn process(&mut self, far: &[f32], mic: &[f32], out: &mut [f32]);
}

const SECONDS: usize = 24;
/// Far-end speech is on throughout; the second talker speaks from 14 s to 20 s.
const DT_START: usize = 14;
const DT_END: usize = 20;
/// Echo-only measuring window, after the filters have had time to converge.
const STEADY_START: usize = 8;
/// -70 dBFS room noise on the mic.
/// ERLE is shown up to this; a gate that outputs digital silence has no meaningful number above it.
const ERLE_CAP_DB: f32 = 60.0;
/// Longest added latency searched for: 20 ms.
const MAX_LAG: usize = RATE / 50;
const NOISE_RMS: f32 = 3.16e-4;
/// The far-end speech peak, and the echo gain from speaker to mic (about -12 dB).
const FAR_PEAK: f32 = 0.5;
const ECHO_GAIN: f32 = 0.25;

struct Scenario {
    far: Vec<f32>,
    echo: Vec<f32>,
    near: Vec<f32>,
    mic: Vec<f32>,
}

fn scenario(room: &Room, seed: u64) -> Scenario {
    let far = speech_like(11 + 1000 * seed, SECONDS, FAR_PEAK);
    let echo = room.echo(&far, 23 + 1000 * seed, ECHO_GAIN);
    let mut near = vec![0.0f32; SECONDS * RATE];
    let speech = speech_like(37 + 1000 * seed, DT_END - DT_START, 0.35);
    near[DT_START * RATE..DT_END * RATE].copy_from_slice(&speech);
    let mut rng = Rng::new(41 + 1000 * seed);
    let mic = (0..far.len()).map(|i| echo[i] + near[i] + NOISE_RMS * rng.gauss()).collect();
    Scenario { far, echo, near, mic }
}

fn db(ratio: f32) -> f32 {
    10.0 * ratio.max(1e-12).log10()
}

struct Run {
    out: Vec<f32>,
    frame_us: Vec<f32>,
    /// Rust heap allocations per block once past the warm-up.
    rust_allocs_per_block: f32,
    /// C and C++ (`malloc`, `new`) allocations per block; `None` where they cannot be counted.
    native_allocs_per_block: Option<f32>,
}

/// Blocks skipped before counting allocations: filters have been sized and caches filled.
const ALLOC_WARMUP_BLOCKS: usize = 300;

fn run(c: &mut dyn Canceller, s: &Scenario) -> Run {
    let mut out = vec![0.0f32; s.mic.len()];
    let mut frame_us = Vec::with_capacity(s.mic.len() / FRAME);
    let (mut rust_allocs, mut native_allocs) = (0usize, Some(0usize));
    for b in 0..s.mic.len() / FRAME {
        let r = b * FRAME..(b + 1) * FRAME;
        let before = count_alloc::snapshot();
        let t = Instant::now();
        c.process(&s.far[r.clone()], &s.mic[r.clone()], &mut out[r]);
        frame_us.push(t.elapsed().as_secs_f32() * 1e6);
        if b >= ALLOC_WARMUP_BLOCKS {
            let after = count_alloc::snapshot();
            rust_allocs += after.rust_since(before);
            native_allocs = native_allocs.zip(after.native_since(before)).map(|(a, b)| a + b);
        }
    }
    let counted = (s.mic.len() / FRAME).saturating_sub(ALLOC_WARMUP_BLOCKS).max(1);
    Run {
        out,
        frame_us,
        rust_allocs_per_block: rust_allocs as f32 / counted as f32,
        native_allocs_per_block: native_allocs.map(|n| n as f32 / counted as f32),
    }
}

struct Metrics {
    erle_db: f32,
    first_s: Option<f32>,
    converge_s: Option<f32>,
    lag_ms: f32,
    leak_pct: f32,
    dt_sdr_db: f32,
    dt_fit_sdr_db: f32,
    /// Near-end gain in double talk: the projection gain of the output on the true near end, in dB. Residual echo and
    /// noise in the output are orthogonal to the near end and do not count; `dt_fit_sdr_db` measures them.
    dt_level_db: f32,
}

fn metrics(s: &Scenario, out: &[f32]) -> Metrics {
    let blk = |x: &[f32], b: usize| -> f32 { x[b * FRAME..(b + 1) * FRAME].iter().map(|v| v * v).sum() };
    let echo_active = |b: usize| blk(&s.echo, b) / FRAME as f32 > 3.16e-6; // above -55 dBFS rms
    // ERLE over the steady echo-only window, on blocks where there is echo to remove.
    let (mut e_in, mut e_out) = (0.0f32, 0.0f32);
    let (mut active, mut leaked) = (0usize, 0usize);
    for b in STEADY_START * 100..DT_START * 100 {
        if echo_active(b) {
            e_in += blk(&s.echo, b);
            e_out += blk(out, b);
            active += 1;
            if blk(out, b) / FRAME as f32 > 1e-5 {
                leaked += 1; // output above -50 dBFS rms
            }
        }
    }
    // Convergence: the first 0.5 s window after which every later 0.5 s window of the echo-only part keeps the ERLE at
    // 25 dB or more (the audio-api.md target), judged on blocks that have echo.
    let windows: Vec<Option<f32>> = (0..DT_START * 100 / 50)
        .map(|w| {
            let (mut i, mut o, mut n) = (0.0f32, 0.0f32, 0);
            for k in w * 50..(w + 1) * 50 {
                if echo_active(k) {
                    i += blk(&s.echo, k);
                    o += blk(out, k);
                    n += 1;
                }
            }
            (n >= 10).then(|| db(i / o.max(1e-12)))
        })
        .collect();
    let first_s = windows.iter().position(|w| w.is_some_and(|e| e >= 25.0)).map(|w| (w + 1) as f32 / 2.0);
    let first_good = (0..windows.len()).find(|&w| windows[w..].iter().flatten().all(|&e| e >= 25.0));
    let converge_s = first_good.filter(|&w| windows[w..].iter().any(Option::is_some)).map(|w| (w + 1) as f32 / 2.0);
    // The latency the canceller adds to the near-end path: the lag that best aligns its output with the true near-end
    // talker over the first 2 s of double talk (a block-processing canceller with a filter bank delays what it passes).
    let lag = best_lag(&s.near[DT_START * RATE..(DT_START + 2) * RATE], &out[DT_START * RATE..], MAX_LAG);
    // Double talk: blocks with both a near-end talker and echo. `dt_sdr` is the signal-to-error ratio against the true
    // near-end after removing that lag; `dt_fit` also fits one gain to the output, so it measures leftover echo and
    // distortion apart from how much a suppressor turned the talker down (`dt_level`, from that same gain `alpha`).
    let (mut ne, mut err, mut outp, mut cross) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for b in DT_START * 100..DT_END * 100 {
        if blk(&s.near, b) / FRAME as f32 > 1e-4 && echo_active(b) {
            for i in b * FRAME..(b + 1) * FRAME {
                let o = out.get(i + lag).copied().unwrap_or(0.0);
                ne += s.near[i] * s.near[i];
                outp += o * o;
                cross += o * s.near[i];
                err += (o - s.near[i]).powi(2);
            }
        }
    }
    let alpha = cross / ne.max(1e-12);
    let fit_err = (outp - alpha * cross).max(1e-12);
    Metrics {
        erle_db: db(e_in / e_out.max(1e-12)).min(ERLE_CAP_DB),
        first_s,
        converge_s,
        lag_ms: lag as f32 * 1000.0 / RATE as f32,
        leak_pct: 100.0 * leaked as f32 / active.max(1) as f32,
        dt_sdr_db: db(ne / err.max(1e-12)),
        dt_fit_sdr_db: db(alpha * alpha * ne / fit_err),
        dt_level_db: db(alpha * alpha),
    }
}

/// The lag in `0..=max` samples at which `y[i + lag]` best matches `x[i]` (largest normalised cross-correlation).
fn best_lag(x: &[f32], y: &[f32], max: usize) -> usize {
    let ex: f32 = x.iter().map(|v| v * v).sum();
    let (mut best, mut best_c) = (0, f32::MIN);
    for lag in 0..=max {
        let (mut c, mut ey) = (0.0f32, 0.0f32);
        for (i, &xv) in x.iter().enumerate() {
            let yv = y.get(i + lag).copied().unwrap_or(0.0);
            c += xv * yv;
            ey += yv * yv;
        }
        let c = c / (ex * ey).sqrt().max(1e-12);
        if c > best_c {
            (best, best_c) = (lag, c);
        }
    }
    best
}

fn percentile(sorted: &[f32], p: f32) -> f32 {
    sorted[((sorted.len() - 1) as f32 * p) as usize]
}

fn make(name: &str) -> Option<Box<dyn Canceller>> {
    Some(match name {
        "sonora" => Box::new(candidates::Sonora::new()),
        #[cfg(feature = "webrtc-cpp")]
        "webrtc-cpp" => Box::new(candidates::WebrtcCpp::new().ok()?),
        "aec3-port" => Box::new(candidates::Aec3Port::new().ok()?),
        "speex" => Box::new(candidates::Speex::new(500, false)),
        "speex+pp" => Box::new(candidates::Speex::new(500, true)),
        "nlms" => Box::new(own::Nlms::new(RATE / 5, 0.5)),
        // 30 partitions of 10 ms: 300 ms, covering the delay and the tail of the first three rooms.
        "pbfdaf" => Box::new(own::Pbfdaf::new(30, 0.5)),
        "none" => Box::new(own::Passthrough),
        "gate" => Box::new(own::Gate::new(own::GateMode::Gate, 500)),
        "duck20" => Box::new(own::Gate::new(own::GateMode::Duck(20.0), 500)),
        _ => return None,
    })
}

const NAMES: [&str; 10] =
    ["none", "webrtc-cpp", "sonora", "aec3-port", "speex", "speex+pp", "nlms", "pbfdaf", "gate", "duck20"];

fn table() {
    println!(
        "| room | candidate | ERLE dB | 25 dB first / stable (s) | leak % | lag ms | DT SDR dB | DT fit SDR dB | DT level dB | mean us | p99 us | max us | core % | Rust allocs/block | native allocs/block |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    for room in &ROOMS {
        let s = scenario(room, 0);
        for name in NAMES {
            let Some(mut c) = make(name) else {
                println!("| {} | {name} | could not be built |", room.name);
                continue;
            };
            let r = run(c.as_mut(), &s);
            let m = metrics(&s, &r.out);
            let mut us = r.frame_us.clone();
            us.sort_by(f32::total_cmp);
            let mean = us.iter().sum::<f32>() / us.len() as f32;
            println!(
                "| {} | {} | {:.1} | {} / {} | {:.1} | {:.1} | {:.1} | {:.1} | {:+.1} | {:.0} | {:.0} | {:.0} | {:.2} | {:.1} | {} |",
                room.name,
                c.name(),
                m.erle_db,
                m.first_s.map_or("never".to_string(), |v| format!("{v:.1}")),
                m.converge_s.map_or("never".to_string(), |v| format!("{v:.1}")),
                m.leak_pct,
                m.lag_ms,
                m.dt_sdr_db,
                m.dt_fit_sdr_db,
                m.dt_level_db,
                mean,
                percentile(&us, 0.99),
                us[us.len() - 1],
                mean / 100.0,
                r.rust_allocs_per_block,
                r.native_allocs_per_block.map_or("not measured".to_string(), |n| format!("{n:.1}")),
            );
        }
    }
}

/// ERLE against the bulk delay, with a fixed medium tail, to find each estimator's range.
fn sweep() {
    let delays = [10.0f32, 100.0, 300.0, 500.0, 550.0, 600.0, 650.0, 700.0];
    println!("| candidate | {} |", delays.map(|d| format!("{d:.0} ms")).join(" | "));
    println!("|---|{}|", delays.map(|_| "---").join("|"));
    let scenarios: Vec<Scenario> = delays
        .iter()
        .map(|&d| scenario(&Room { name: "sweep", delay_ms: d, rt60_ms: 300.0, tail_ms: 250.0, nonlinear: false }, 0))
        .collect();
    for name in ["webrtc-cpp", "sonora", "aec3-port", "speex", "speex+pp", "pbfdaf"] {
        let mut row = Vec::new();
        for s in &scenarios {
            let cell = match make(name) {
                Some(mut c) => {
                    let r = run(c.as_mut(), s);
                    format!("{:.1}", metrics(s, &r.out).erle_db)
                }
                None => "n/a".to_string(),
            };
            row.push(cell);
        }
        println!("| {name} | {} |", row.join(" | "));
    }
}

/// ERLE and double-talk level over several seeds (a different speech, room response and noise each), for the AEC3 family:
/// whether the ranking in the table survives a change of signals.
fn seeds() {
    const SEEDS: u64 = 5;
    println!("| room | candidate | ERLE dB min / median / max over {SEEDS} seeds | DT level dB median |");
    println!("|---|---|---|---|");
    for room in &ROOMS {
        let scenarios: Vec<Scenario> = (0..SEEDS).map(|seed| scenario(room, seed)).collect();
        for name in ["webrtc-cpp", "sonora", "aec3-port"] {
            let mut erle = Vec::new();
            let mut level = Vec::new();
            for s in &scenarios {
                let Some(mut c) = make(name) else { break };
                let m = metrics(s, &run(c.as_mut(), s).out);
                erle.push(m.erle_db);
                level.push(m.dt_level_db);
            }
            if erle.is_empty() {
                println!("| {} | {name} | could not be built | |", room.name);
                continue;
            }
            erle.sort_by(f32::total_cmp);
            level.sort_by(f32::total_cmp);
            println!(
                "| {} | {name} | {:.1} / {:.1} / {:.1} | {:+.1} |",
                room.name,
                erle[0],
                erle[erle.len() / 2],
                erle[erle.len() - 1],
                level[level.len() / 2]
            );
        }
    }
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        None | Some("table") => table(),
        Some("sweep") => sweep(),
        Some("seeds") => seeds(),
        Some(other) => {
            eprintln!("unknown mode {other}; use table, sweep or seeds");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(name: &str) -> &'static Room {
        ROOMS.iter().find(|r| r.name == name).expect("room exists in ROOMS")
    }

    /// A scenario whose output is built from its own parts, so each metric has a known answer.
    fn out_from(s: &Scenario, f: impl Fn(usize) -> f32) -> Vec<f32> {
        (0..s.mic.len()).map(f).collect()
    }

    #[test]
    fn pass_through_has_no_erle_and_leaks_every_echo_frame() {
        let s = scenario(room("medium"), 0);
        let m = metrics(&s, &s.mic);
        assert!(m.erle_db.abs() < 0.5, "pass-through ERLE {} dB should be about 0", m.erle_db);
        assert!(m.leak_pct > 90.0, "pass-through leaks {} %", m.leak_pct);
        assert_eq!(m.first_s, None);
        assert_eq!(m.converge_s, None);
        assert_eq!(m.lag_ms, 0.0);
        // The talker is untouched: unit near-end gain, and the echo shows up as error, not as speech.
        assert!(m.dt_level_db.abs() < 0.5, "pass-through near-end gain {} dB", m.dt_level_db);
        assert!(m.dt_fit_sdr_db < 10.0, "echo left in the output must lower the fitted SDR, got {}", m.dt_fit_sdr_db);
    }

    #[test]
    fn best_lag_finds_a_known_delay_exactly() {
        let mut rng = Rng::new(7);
        let x: Vec<f32> = (0..4800).map(|_| rng.gauss()).collect();
        for lag in [0usize, 1, 240, MAX_LAG] {
            let mut y = vec![0.0f32; lag];
            y.extend_from_slice(&x);
            assert_eq!(best_lag(&x, &y, MAX_LAG), lag);
        }
    }

    #[test]
    fn metrics_report_an_added_latency_of_five_ms() {
        let s = scenario(room("small"), 0);
        let lag = RATE / 200; // 5 ms
        let out = out_from(&s, |i| if i >= lag { s.near[i - lag] } else { 0.0 });
        let m = metrics(&s, &out);
        assert_eq!(m.lag_ms, 5.0);
        // The lag is removed before comparing, so a delayed clean talker is perfect.
        assert!(m.dt_level_db.abs() < 0.1, "{}", m.dt_level_db);
        assert!(m.dt_sdr_db > 40.0, "{}", m.dt_sdr_db);
        assert!(m.erle_db >= ERLE_CAP_DB - 0.1, "no echo in the output reaches the cap, got {}", m.erle_db);
    }

    #[test]
    fn double_talk_metrics_on_a_constructed_output() {
        let s = scenario(room("medium"), 0);
        // Half-amplitude talker and nothing else: 6.02 dB down, and exactly one gain explains it.
        let half = out_from(&s, |i| 0.5 * s.near[i]);
        let m = metrics(&s, &half);
        assert!((m.dt_level_db + 6.02).abs() < 0.05, "level {}", m.dt_level_db);
        assert!((m.dt_sdr_db - 6.02).abs() < 0.05, "sdr {}", m.dt_sdr_db);
        assert!(m.dt_fit_sdr_db > 30.0, "fit sdr {}", m.dt_fit_sdr_db);
        // Full talker plus the echo: the level stays at 0 dB (echo is not near-end speech) but the fitted SDR drops.
        let leaky = out_from(&s, |i| s.near[i] + s.echo[i]);
        let m = metrics(&s, &leaky);
        assert!(m.dt_level_db.abs() < 0.5, "level {}", m.dt_level_db);
        assert!(m.dt_fit_sdr_db < 10.0, "fit sdr {}", m.dt_fit_sdr_db);
        // A gate: silence in, the talker is gone.
        let m = metrics(&s, &vec![0.0; s.mic.len()]);
        assert!(m.dt_level_db < -100.0, "level {}", m.dt_level_db);
    }

    #[test]
    fn a_perfect_canceller_converges_at_once_and_a_real_filter_converges_on_a_synthetic_room() {
        let s = scenario(room("small"), 0);
        let oracle = out_from(&s, |i| s.mic[i] - s.echo[i]);
        let m = metrics(&s, &oracle);
        assert!(m.erle_db > 25.0, "{}", m.erle_db);
        assert_eq!(m.first_s, Some(0.5));
        assert_eq!(m.converge_s, Some(0.5));
        assert_eq!(m.leak_pct, 0.0);

        // Our own partitioned filter, 300 ms, in the 12 ms room: repeatable, so a fixed bound is a regression test.
        let mut c = make("pbfdaf").expect("pbfdaf builds");
        let r = run(c.as_mut(), &s);
        let m = metrics(&s, &r.out);
        assert!(m.erle_db > 25.0, "PBFDAF ERLE {} dB", m.erle_db);
        assert!(m.converge_s.is_some(), "PBFDAF never stayed above 25 dB");
        assert!(m.dt_level_db > -1.0, "a linear filter should keep the talker: {} dB", m.dt_level_db);
    }

    /// The native counter must see allocations that Rust's allocator does not, or "0 native allocations" proves nothing.
    #[test]
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[allow(unsafe_code)]
    fn native_counter_sees_c_allocations_and_not_rust_ones() {
        unsafe extern "C" {
            fn malloc(size: usize) -> *mut std::ffi::c_void;
            fn free(ptr: *mut std::ffi::c_void);
        }
        let before = count_alloc::snapshot();
        // SAFETY: a plain malloc/free pair of 64 bytes.
        unsafe { free(std::hint::black_box(malloc(64))) }; // black_box: LLVM would delete an unused pair
        let after = count_alloc::snapshot();
        assert!(after.native_since(before).is_some_and(|n| n >= 1), "a C malloc was not counted");
        let before = count_alloc::snapshot();
        let v = std::hint::black_box(vec![0u8; 4096]);
        let after = count_alloc::snapshot();
        assert!(after.rust_since(before) >= 1, "a Vec was not counted as a Rust allocation");
        drop(v);
    }

    /// Building the C and C++ candidates allocates natively, so the counter reads above zero for them. (Steady-state
    /// zeros in the table mean something only because this holds.)
    #[test]
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    fn native_counter_sees_the_c_and_cpp_candidates_allocate() {
        let before = count_alloc::snapshot();
        let speex = make("speex").expect("speex builds");
        let n = count_alloc::snapshot().native_since(before).expect("measured");
        assert!(n > 0, "SpeexDSP setup showed no native allocations");
        drop(speex);
        #[cfg(feature = "webrtc-cpp")]
        {
            let before = count_alloc::snapshot();
            let cpp = make("webrtc-cpp").expect("webrtc-cpp builds");
            let n = count_alloc::snapshot().native_since(before).expect("measured");
            assert!(n > 0, "WebRTC C++ setup showed no native allocations");
            drop(cpp);
        }
    }
}
