//! What the effect tests share: running a node over a signal, and measuring the result with code that does not share
//! anything with the effects (a direct DTFT, RMS, levels).
#![allow(
    dead_code,
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::many_single_char_names
)]

use std::f64::consts::PI;

use omuzaki_audio_api::graph::Channels;
use omuzaki_audio_api::node::{BlockMut, Node, NodeConfig, NodeKind, ParamIndex, SAMPLE_RATE};

pub const FS: f64 = SAMPLE_RATE as f64;

/// A node of `kind` on a mono bus with `params` set by name, then reset so every smoothed value is at its target.
pub fn node(kind: &dyn NodeKind, params: &[(&str, f32)]) -> Box<dyn Node> {
    node_with(kind, &NodeConfig::default(), Channels::Mono, params)
}

/// The same, with a config and a channel count.
pub fn node_with(
    kind: &dyn NodeKind,
    config: &NodeConfig,
    channels: Channels,
    params: &[(&str, f32)],
) -> Box<dyn Node> {
    let mut node = kind.create(config, channels).expect("the node is created");
    set(&mut *node, kind, params);
    node.reset();
    node
}

/// Sets parameters by name.
pub fn set(node: &mut dyn Node, kind: &dyn NodeKind, params: &[(&str, f32)]) {
    let info = kind.info();
    for (name, value) in params {
        let at = info.param_index(name).unwrap_or_else(|| panic!("{} has no parameter {name}", info.id));
        node.set_param(at, *value);
    }
}

/// Runs a mono signal through in blocks of `block` frames, starting at graph sample `first`.
pub fn run_from(node: &mut dyn Node, input: &[f32], block: usize, first: u64) -> Vec<f32> {
    let mut out = input.to_vec();
    let mut at = 0;
    for chunk in out.chunks_mut(block) {
        let n = chunk.len();
        node.process(&mut BlockMut::mono(chunk, first + at as u64));
        at += n;
    }
    out
}

/// Runs a mono signal through in blocks of 256 frames from graph sample 0.
pub fn run(node: &mut dyn Node, input: &[f32]) -> Vec<f32> {
    run_from(node, input, 256, 0)
}

/// The magnitude of the DTFT of `x` at `hz`, computed directly (no FFT).
pub fn dtft(x: &[f32], hz: f64) -> f64 {
    let w = 2.0 * PI * hz / FS;
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (n, s) in x.iter().enumerate() {
        let (sin, cos) = (w * n as f64).sin_cos();
        re += f64::from(*s) * cos;
        im -= f64::from(*s) * sin;
    }
    re.hypot(im)
}

/// `x` times a Hann window, for spectra of steady tones.
pub fn hann(x: &[f32]) -> Vec<f32> {
    let n = x.len() as f64;
    x.iter().enumerate().map(|(i, s)| s * (0.5 - 0.5 * (2.0 * PI * i as f64 / n).cos()) as f32).collect()
}

/// Root mean square.
pub fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>() / x.len().max(1) as f64).sqrt()
}

/// `20 log10`.
pub fn db(x: f64) -> f64 {
    20.0 * x.max(1e-30).log10()
}

/// A sine of `len` samples at `hz` with peak `amp` and starting phase `phase` (radians).
pub fn sine(len: usize, hz: f64, amp: f64, phase: f64) -> Vec<f32> {
    (0..len).map(|n| (amp * (2.0 * PI * hz * n as f64 / FS + phase).sin()) as f32).collect()
}

/// The gain in dB of a steady sine through `node`, measured on the last half of a second-long run.
pub fn sine_gain_db(node: &mut dyn Node, hz: f64, amp: f64) -> f64 {
    let x = sine(48_000, hz, amp, 0.0);
    let y = run(node, &x);
    db(rms(&y[24_000..]) / rms(&x[24_000..]))
}

/// The true peak of `x` by a 16x windowed-sinc reconstruction (Blackman window, 48 zero crossings each side): an
/// independent reference for the limiter, sharing nothing with its BS.1770 filter.
pub fn true_peak_16x(x: &[f32]) -> f64 {
    const HALF: i64 = 48;
    const UP: i64 = 16;
    // kernel[k][j - n + HALF - 1]: the weight of x[j] at time n + k / UP.
    let kernel: Vec<Vec<f64>> = (1..UP)
        .map(|k| {
            (-HALF + 1..=HALF)
                .map(|m| {
                    let d = k as f64 / UP as f64 - m as f64;
                    let sinc = (PI * d).sin() / (PI * d);
                    let u = (d + HALF as f64) / (2.0 * HALF as f64);
                    sinc * (0.42 - 0.5 * (2.0 * PI * u).cos() + 0.08 * (4.0 * PI * u).cos())
                })
                .collect()
        })
        .collect();
    let mut peak = x.iter().fold(0.0f64, |m, s| m.max(f64::from(*s).abs()));
    let len = x.len() as i64;
    for n in 0..len {
        for taps in &kernel {
            let mut sum = 0.0;
            for (i, w) in taps.iter().enumerate() {
                let j = n - HALF + 1 + i as i64;
                if (0..len).contains(&j) {
                    sum += f64::from(x[j as usize]) * w;
                }
            }
            peak = peak.max(sum.abs());
        }
    }
    peak
}

/// The index of a parameter.
pub fn param(kind: &dyn NodeKind, name: &str) -> ParamIndex {
    kind.info().param_index(name).expect("a parameter")
}

/// F0 per frame by an offline YIN (2,048-sample frames every 480 samples, 60 to 800 Hz, voiced under 0.15), written
/// here so the shifter is measured by code it does not share: `None` for an unvoiced frame. Frame `i` is centred on
/// sample `i * 480 + 1024`. It takes the first dip of the CMND within 0.05 of the deepest rather than the first under
/// the threshold: the first-dip rule reads /a/ at 360 Hz an octave up (its second harmonic sits on F1), which made it
/// wrong on 18% of a directly rendered 150 to 450 Hz glide.
pub fn yin_track(x: &[f32]) -> Vec<Option<f64>> {
    const FRAME: usize = 2048;
    const HOP: usize = 480;
    const WINDOW: usize = 1024;
    let (tau_min, tau_max) = (60usize, 800usize);
    let mut out = Vec::new();
    let mut d = vec![0.0f64; tau_max + 2];
    let mut start = 0;
    while start + FRAME <= x.len() {
        let f: Vec<f64> = x[start..start + FRAME].iter().map(|v| f64::from(*v)).collect();
        let energy = f[..WINDOW].iter().map(|v| v * v).sum::<f64>() / WINDOW as f64;
        let mut running = 0.0;
        d[0] = 1.0;
        for tau in 1..=tau_max {
            let s: f64 = (0..WINDOW).map(|j| (f[j] - f[j + tau]).powi(2)).sum();
            running += s;
            d[tau] = if running > 0.0 { s * tau as f64 / running } else { 1.0 };
        }
        d[tau_max + 1] = f64::MAX;
        let deepest = (tau_min..=tau_max).map(|t| d[t]).fold(f64::MAX, f64::min);
        let mut t = (tau_min..=tau_max).find(|t| d[*t] <= deepest + 0.05).unwrap_or(tau_min);
        while t < tau_max && d[t + 1] < d[t] {
            t += 1;
        }
        let found = (d[t] < 0.15 && t > tau_min && t < tau_max).then_some(t);
        out.push(found.filter(|_| energy > 1e-7).map(|t| {
            let (a, b, c) = (d[t - 1], d[t], d[t + 1]);
            let den = a - 2.0 * b + c;
            let lag = t as f64 + if den > 0.0 { (0.5 * (a - c) / den).clamp(-0.5, 0.5) } else { 0.0 };
            FS / lag
        }));
        start += HOP;
    }
    out
}

/// Cents from `b` to `a`.
pub fn cents(a: f64, b: f64) -> f64 {
    1200.0 * (a / b).log2()
}

/// The first three formants of `x`, from the average LPC envelope (order 18 on 2,048-sample Hann frames of the
/// pre-emphasised signal, 512 apart): its first three peaks above 200 Hz.
pub fn formants(x: &[f32]) -> [f64; 3] {
    const ORDER: usize = 18;
    const FRAME: usize = 2048;
    let grid: Vec<f64> = (0..=860).map(|i| 200.0 + 5.0 * f64::from(i)).collect();
    let mut envelope = vec![0.0f64; grid.len()];
    let mut start = 0;
    while start + FRAME <= x.len() {
        let w: Vec<f64> = (0..FRAME)
            .map(|i| {
                let pre =
                    f64::from(x[start + i]) - if start + i > 0 { 0.97 * f64::from(x[start + i - 1]) } else { 0.0 };
                pre * (0.5 - 0.5 * (2.0 * PI * i as f64 / FRAME as f64).cos())
            })
            .collect();
        let r: Vec<f64> = (0..=ORDER).map(|k| (k..FRAME).map(|i| w[i] * w[i - k]).sum()).collect();
        // Levinson-Durbin.
        let mut a = vec![0.0f64; ORDER + 1];
        a[0] = 1.0;
        let mut err = r[0];
        for i in 1..=ORDER {
            let acc: f64 = r[i] + (1..i).map(|j| a[j] * r[i - j]).sum::<f64>();
            let k = -acc / err;
            let prev = a.clone();
            a[i] = k;
            for j in 1..i {
                a[j] = prev[j] + k * prev[i - j];
            }
            err *= 1.0 - k * k;
        }
        for (e, hz) in envelope.iter_mut().zip(&grid) {
            let w = 2.0 * PI * hz / FS;
            let (mut re, mut im) = (0.0, 0.0);
            for (k, c) in a.iter().enumerate() {
                re += c * (w * k as f64).cos();
                im -= c * (w * k as f64).sin();
            }
            *e += 1.0 / (re * re + im * im);
        }
        start += 512;
    }
    let mut peaks = [0.0; 3];
    let mut found = 0;
    for i in 1..grid.len() - 1 {
        if found < 3 && envelope[i] > envelope[i - 1] && envelope[i] >= envelope[i + 1] {
            peaks[found] = grid[i];
            found += 1;
        }
    }
    peaks
}
