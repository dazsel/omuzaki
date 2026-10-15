//! Our own candidates: a time-domain NLMS and a partitioned-block frequency-domain adaptive filter (PBFDAF), plus the
//! cheap fallback of gating or ducking the signal while the far end speaks. All three are deliberately plain: what a
//! first implementation inside `omuzaki-audio-fx` would look like, to price building it ourselves.

use crate::Canceller;
use crate::signals::FRAME;
use rustfft::{Fft, FftPlanner, num_complex::Complex32};
use std::sync::Arc;

/// Time-domain NLMS over the last `taps` reference samples. One filter must cover the bulk delay and the tail, so at
/// 48 kHz it costs two multiply-adds per tap per sample.
pub struct Nlms {
    w: Vec<f32>,
    hist: Vec<f32>, // reference history, newest at the end; length taps + FRAME
    taps: usize,
    mu: f32,
    energy: f32,
}

impl Nlms {
    pub fn new(taps: usize, mu: f32) -> Self {
        Self { w: vec![0.0; taps], hist: vec![0.0; taps + FRAME], taps, mu, energy: 0.0 }
    }
}

impl Canceller for Nlms {
    fn name(&self) -> String {
        format!("own-nlms-{}ms", self.taps * 1000 / crate::signals::RATE)
    }

    fn process(&mut self, far: &[f32], mic: &[f32], out: &mut [f32]) {
        let n = self.taps;
        // Slide the history by one block, then append the new reference block.
        self.hist.copy_within(FRAME.., 0);
        self.hist[n..].copy_from_slice(far);
        for i in 0..FRAME {
            // The window for sample i ends with the sample just appended (index n + i).
            let win = &self.hist[i + 1..=i + n];
            let y: f32 = self.w.iter().zip(win).map(|(w, x)| w * x).sum();
            let e = mic[i] - y;
            out[i] = e;
            // Running ||x||^2: add the newest sample, drop the one that left the window.
            let entering = self.hist[n + i];
            let leaving = self.hist[i];
            self.energy = (self.energy + entering * entering - leaving * leaving).max(0.0);
            let g = self.mu * e / (self.energy + 1e-6 * n as f32);
            for (w, x) in self.w.iter_mut().zip(win) {
                *w += g * x;
            }
        }
    }
}

/// PBFDAF: `parts` partitions of one `FRAME` block each, an FFT of size `2 * FRAME`, a per-bin normalised update and
/// a double-talk guard that slows the step when the error is large against the echo estimate. One partition's gradient
/// is constrained per block, round robin (the usual saving, as in Speex's MDF).
pub struct Pbfdaf {
    parts: usize,
    fft: Arc<dyn Fft<f32>>,
    ifft: Arc<dyn Fft<f32>>,
    x_hist: Vec<Vec<Complex32>>, // newest first
    w: Vec<Vec<Complex32>>,
    prev_far: Vec<f32>,
    power: Vec<f32>,
    blocks: usize,
    next_constrained: usize,
    mu: f32,
}

const BINS: usize = FRAME + 1;

impl Pbfdaf {
    pub fn new(parts: usize, mu: f32) -> Self {
        let mut planner = FftPlanner::new();
        Self {
            parts,
            fft: planner.plan_fft_forward(2 * FRAME),
            ifft: planner.plan_fft_inverse(2 * FRAME),
            x_hist: vec![vec![Complex32::new(0.0, 0.0); BINS]; parts],
            w: vec![vec![Complex32::new(0.0, 0.0); BINS]; parts],
            prev_far: vec![0.0; FRAME],
            power: vec![1e-6; BINS],
            blocks: 0,
            next_constrained: 0,
            mu,
        }
    }

    fn forward(&self, block: &[f32]) -> Vec<Complex32> {
        let mut buf: Vec<Complex32> = block.iter().map(|&v| Complex32::new(v, 0.0)).collect();
        self.fft.process(&mut buf);
        buf.truncate(BINS);
        buf
    }

    fn inverse(&self, spec: &[Complex32]) -> Vec<f32> {
        let mut buf = vec![Complex32::new(0.0, 0.0); 2 * FRAME];
        buf[..BINS].copy_from_slice(spec);
        for k in 1..FRAME {
            buf[2 * FRAME - k] = spec[k].conj();
        }
        self.ifft.process(&mut buf);
        buf.iter().map(|c| c.re / (2 * FRAME) as f32).collect()
    }
}

impl Canceller for Pbfdaf {
    fn name(&self) -> String {
        format!("own-pbfdaf-{}ms", self.parts * 10)
    }

    fn process(&mut self, far: &[f32], mic: &[f32], out: &mut [f32]) {
        let mut block = Vec::with_capacity(2 * FRAME);
        block.extend_from_slice(&self.prev_far);
        block.extend_from_slice(far);
        self.prev_far.copy_from_slice(far);
        let x = self.forward(&block);
        self.x_hist.rotate_right(1);
        self.x_hist[0] = x;

        // Echo estimate: the sum over partitions, keeping the last half of the circular convolution.
        let mut y_spec = vec![Complex32::new(0.0, 0.0); BINS];
        for (w, xh) in self.w.iter().zip(&self.x_hist) {
            for k in 0..BINS {
                y_spec[k] += w[k] * xh[k];
            }
        }
        let y_time = self.inverse(&y_spec);
        let y = &y_time[FRAME..];
        let mut y_pow = 0.0f32;
        let mut e_pow = 0.0f32;
        for i in 0..FRAME {
            let e = mic[i] - y[i];
            out[i] = e;
            y_pow += y[i] * y[i];
            e_pow += e * e;
        }

        // Step size: full for the first second while the filter is empty; afterwards scaled down when the error
        // outweighs the echo estimate, which is what a second talker looks like.
        self.blocks += 1;
        let guard = if self.blocks < 100 { 1.0 } else { (y_pow / (e_pow + 1e-9)).clamp(0.05, 1.0) };

        let mut e_block = vec![0.0f32; 2 * FRAME];
        e_block[FRAME..].copy_from_slice(out);
        let e_spec = self.forward(&e_block);

        for k in 0..BINS {
            let p: f32 = self.x_hist.iter().map(|xh| xh[k].norm_sqr()).sum();
            self.power[k] = 0.9 * self.power[k] + 0.1 * p;
        }
        for (w, xh) in self.w.iter_mut().zip(&self.x_hist) {
            for k in 0..BINS {
                let step = self.mu * guard / (self.power[k] + 1e-4 * self.parts as f32);
                w[k] += xh[k].conj() * e_spec[k] * step;
            }
        }
        // Constrain one partition's gradient to the causal block: back to time, zero the second half, forward.
        let c = self.next_constrained;
        self.next_constrained = (c + 1) % self.parts;
        let mut t = self.inverse(&self.w[c]);
        t[FRAME..].iter_mut().for_each(|v| *v = 0.0);
        self.w[c] = self.forward(&t);
    }
}

/// How the fallback treats the signal while the far end is active.
#[derive(Clone, Copy)]
pub enum GateMode {
    /// Hard gate: silence.
    Gate,
    /// Attenuate by this many dB.
    Duck(f32),
}

/// The cheaper mitigation from the issue: close or duck the signal while the reference is active, with a hangover that
/// covers the speaker-to-mic delay and the tail.
pub struct Gate {
    mode: GateMode,
    hangover_blocks: usize,
    remaining: usize,
    gain: f32,
}

impl Gate {
    pub fn new(mode: GateMode, hangover_ms: usize) -> Self {
        Self { mode, hangover_blocks: hangover_ms / 10, remaining: 0, gain: 1.0 }
    }
}

impl Canceller for Gate {
    fn name(&self) -> String {
        match self.mode {
            GateMode::Gate => "gate".to_string(),
            GateMode::Duck(db) => format!("duck-{db:.0}dB"),
        }
    }

    fn process(&mut self, far: &[f32], mic: &[f32], out: &mut [f32]) {
        let rms = (far.iter().map(|v| v * v).sum::<f32>() / FRAME as f32).sqrt();
        // -50 dBFS: the far end is "speaking".
        if rms > 0.003_16 {
            self.remaining = self.hangover_blocks;
        }
        let target = if self.remaining > 0 {
            self.remaining -= 1;
            match self.mode {
                GateMode::Gate => 0.0,
                GateMode::Duck(db) => 10f32.powf(-db / 20.0),
            }
        } else {
            1.0
        };
        // A 10 ms linear ramp, so the gate itself doesn't click.
        for (i, (o, m)) in out.iter_mut().zip(mic).enumerate() {
            let t = (i + 1) as f32 / FRAME as f32;
            *o = m * (self.gain + (target - self.gain) * t);
        }
        self.gain = target;
    }
}

/// No cancellation: the calibration row. It shows what the metrics read when nothing is done.
pub struct Passthrough;

impl Canceller for Passthrough {
    fn name(&self) -> String {
        "none".to_string()
    }

    fn process(&mut self, _far: &[f32], mic: &[f32], out: &mut [f32]) {
        out.copy_from_slice(mic);
    }
}
