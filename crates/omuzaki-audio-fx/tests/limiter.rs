//! `fx.limiter` (#921): the true peak never passes the ceiling (measured by an independent 16x windowed-sinc
//! reconstruction), the limiter is transparent under it, a limited sine stays clean, and the reported gain reduction is
//! the gain applied.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::many_single_char_names
)]

mod common;

use std::f64::consts::PI;

use common::{db, dtft, hann, node_with, run, sine, true_peak_16x};
use omuzaki_audio_api::graph::Channels;
use omuzaki_audio_api::node::{BlockMut, Node, NodeConfig};
use omuzaki_audio_fx::limiter::{LATENCY, LimiterKind};
use omuzaki_audio_mock::signals::{VoiceSpec, voice};

fn limiter(params: &[(&str, f32)]) -> Box<dyn Node> {
    node_with(&LimiterKind, &NodeConfig::default(), Channels::Mono, params)
}

/// `x` with 10 ms raised-cosine fades at both ends and 200 samples of silence after, so the reference measures the
/// signal and not the edges of a test buffer (an abrupt start or end is a step whose reconstruction rings).
fn faded(x: &[f32]) -> Vec<f32> {
    let ramp = 480;
    let n = x.len();
    let mut out: Vec<f32> = x
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let edge = i.min(n - 1 - i);
            let w = if edge < ramp { 0.5 - 0.5 * (PI * edge as f64 / ramp as f64).cos() } else { 1.0 };
            (f64::from(*s) * w) as f32
        })
        .collect();
    out.extend(vec![0.0; 200]);
    out
}

/// `x` scaled so its true peak (by the reference) is `dbtp`.
fn at_true_peak(x: &[f32], dbtp: f64) -> Vec<f32> {
    let scale = 10f64.powf(dbtp / 20.0) / true_peak_16x(x);
    x.iter().map(|s| (f64::from(*s) * scale) as f32).collect()
}

/// The EBU Tech 3341 true-peak shapes (cases 15 to 18: sines at fs/4, fs/6 and fs/8 whose samples miss their peaks,
/// at 0°, 45°, 60° and 67.5°), plus the fs/4 sine at 45° the issue names, each half a second long.
fn ebu_shapes() -> Vec<(String, Vec<f32>)> {
    [(12_000.0, 0.0), (12_000.0, 45.0), (8000.0, 60.0), (6000.0, 67.5)]
        .into_iter()
        .map(|(hz, deg)| (format!("{hz} Hz at {deg}°"), faded(&sine(24_000, hz, 1.0, deg * PI / 180.0))))
        .collect()
}

#[test]
fn the_true_peak_never_passes_the_ceiling() {
    let mut cases = Vec::new();
    for (name, shape) in ebu_shapes() {
        for over in [0.0, 6.0, 12.0] {
            cases.push((format!("{name}, {over:+} dBTP"), at_true_peak(&shape, over)));
        }
    }
    let mut spec = VoiceSpec::steady(17, 0.5);
    spec.f0 = omuzaki_audio_mock::signals::Trajectory::new(vec![(0.0, 110.0), (0.5, 240.0)]);
    cases.push(("synthetic voice, +12 dB over full scale".into(), at_true_peak(&faded(&voice(&spec)), 12.0)));
    for ceiling in [-1.0f32, -3.0] {
        let mut worst = f64::MIN;
        for (name, x) in &cases {
            let mut node = limiter(&[("ceiling_dbtp", ceiling)]);
            let y = run(&mut *node, x);
            let tp = db(true_peak_16x(&y));
            worst = worst.max(tp - f64::from(ceiling));
            assert!(tp <= f64::from(ceiling) + 0.1, "{name}: {tp:.3} dBTP out against a {ceiling} dBTP ceiling");
        }
        eprintln!("fx.limiter: ceiling {ceiling} dBTP, worst true peak out over {} cases: {worst:+.3} dB", cases.len());
    }
}

#[test]
fn under_the_ceiling_it_is_the_input_delayed() {
    let mut spec = VoiceSpec::steady(23, 1.0);
    spec.f0 = omuzaki_audio_mock::signals::Trajectory::new(vec![(0.0, 90.0), (1.0, 300.0)]);
    let x = at_true_peak(&voice(&spec), -1.5);
    let mut node = limiter(&[]);
    let y = run(&mut *node, &x);
    let d = LATENCY as usize;
    let worst = (d..x.len()).map(|n| (y[n] - x[n - d]).abs()).fold(0.0f32, f32::max);
    assert!(y[..d].iter().all(|s| *s == 0.0));
    eprintln!("fx.limiter: a voice 0.5 dB under the ceiling comes out {d} samples late, largest difference {worst:e}");
    assert!(worst <= 1e-7, "{worst}");
}

#[test]
fn a_sine_3_db_over_the_ceiling_stays_under_1_percent_thd() {
    let x = sine(96_000, 1000.0, 10f64.powf(2.0 / 20.0), 0.0);
    let mut node = limiter(&[]);
    let y = run(&mut *node, &x);
    let tail = hann(&y[48_000..]);
    let fundamental = dtft(&tail, 1000.0);
    let harmonics = (2..=10).map(|k| dtft(&tail, 1000.0 * f64::from(k)).powi(2)).sum::<f64>().sqrt();
    let thd = harmonics / fundamental;
    eprintln!("fx.limiter: 1 kHz at +2 dBFS (3 dB over a -1 dBTP ceiling): THD {:.4}%", 100.0 * thd);
    assert!(thd < 0.01, "{thd}");
}

#[test]
fn the_reported_gain_reduction_is_the_gain_applied() {
    let x = at_true_peak(&sine(24_000, 220.0, 1.0, 0.0), 4.0);
    let mut node = limiter(&[]);
    let d = LATENCY as usize;
    let mut worst = 0.0f64;
    let mut seen = 0;
    for n in 0..x.len() {
        let mut one = [x[n]];
        node.process(&mut BlockMut::mono(&mut one, n as u64));
        if n >= d && x[n - d].abs() > 0.1 {
            let applied = -db(f64::from(one[0]) / f64::from(x[n - d]));
            worst = worst.max((applied - f64::from(node.report().gain_reduction_db)).abs());
            seen += 1;
        }
    }
    eprintln!("fx.limiter: reported against applied gain reduction over {seen} samples: worst {worst:.5} dB");
    assert!(seen > 10_000 && worst < 0.05, "{seen} {worst}");
}

#[test]
fn switched_off_it_passes_the_input_delayed_by_the_same_latency() {
    let x = at_true_peak(&sine(9600, 1000.0, 1.0, 0.3), 6.0);
    let mut node = limiter(&[("on", 0.0)]);
    let y = run(&mut *node, &x);
    let d = LATENCY as usize;
    assert!((d..x.len()).all(|n| y[n] == x[n - d]));
    assert_eq!(node.latency(), LATENCY);
}
