//! The streaming analysis the shifter synthesises from (issue #917): pre-emphasis, an LPC predictor per hop, the
//! residual, the F0 track on a 16 kHz copy, and the pitch marks.
//!
//! Every hop of [`HOP`] samples a predictor is fitted to the last [`LPC_WINDOW`] pre-emphasised samples (a Hann window,
//! two periods of a 120 Hz voice) and used to inverse-filter the samples of that hop; the same hop runs the F0 tracker
//! on the last 24 ms of the decimated signal. Marks follow the residual as it arrives. Nothing allocates after
//! [`Analysis::new`], and every decision is taken at a sample index, so the block size never matters.

use std::f64::consts::PI;

use super::f0::{DECIMATION, Estimate, SPAN, Yin};
use super::lpc::{IDENTITY, Lsf, ORDER, Poly, levinson, to_lsf};
use super::marks::Marks;
use crate::biquad::{Biquad, Coefs, Shape};
use crate::dsp::{FS, dot};

/// The analysis hop, in samples (4 ms).
pub const HOP: i64 = 192;

/// The LPC window, in samples (16 ms).
pub const LPC_WINDOW: usize = 768;

/// The pre-emphasis coefficient.
pub const PRE_EMPHASIS: f64 = 0.97;

/// The length of the sample rings (a power of two).
pub const RING: usize = 4096;

/// Predictors kept for the synthesis, by hop.
pub const FRAMES: usize = 16;

const DEC_RING: usize = 512;

/// The lag window's Gaussian width, in Hz: it keeps a predictor of a pure tone stable.
const LAG_WINDOW_HZ: f64 = 60.0;

/// One hop's predictor.
#[derive(Copy, Clone, Debug)]
pub struct Frame {
    /// The predictor the hop was inverse-filtered with.
    pub a: Poly,
    /// Its line spectral frequencies, when they were wanted and found.
    pub lsf: Option<Lsf>,
}

/// A ring of `f64` samples indexed by absolute sample.
#[derive(Clone, Debug)]
pub struct Ring(Box<[f64]>);

impl Ring {
    /// A ring of `len` (a power of two) zeros.
    #[must_use]
    pub fn new(len: usize) -> Self {
        Self(vec![0.0; len].into_boxed_slice())
    }

    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)] // masked to the ring
    fn slot(&self, at: i64) -> usize {
        (at as usize) & (self.0.len() - 1)
    }

    /// The sample at `at` (zero before the first).
    #[must_use]
    pub fn get(&self, at: i64) -> f64 {
        if at < 0 { 0.0 } else { self.0[self.slot(at)] }
    }

    /// Stores `v` at `at`.
    pub fn set(&mut self, at: i64, v: f64) {
        let slot = self.slot(at);
        self.0[slot] = v;
    }

    /// Adds `v` at `at`.
    pub fn add(&mut self, at: i64, v: f64) {
        let slot = self.slot(at);
        self.0[slot] += v;
    }

    /// Back to zeros.
    pub fn clear(&mut self) {
        self.0.fill(0.0);
    }
}

/// The analysis state.
#[derive(Clone, Debug)]
pub struct Analysis {
    n: i64,
    /// The input.
    pub x: Ring,
    xp: Ring,
    /// The LPC residual.
    pub e: Ring,
    previous: f64,
    window: Box<[f64]>,
    grain: Box<[f64]>,
    lag: [f64; ORDER + 1],
    frames: [Frame; FRAMES],
    a: Poly,
    last_lsf: Option<Lsf>,
    /// Whether the hop before this one had a fit (not the identity).
    fitted: bool,
    decimator: [Biquad; 2],
    dec: Ring,
    dec_count: i64,
    span: Box<[f64; SPAN]>,
    yin: Yin,
    estimate: Estimate,
    /// The pitch marks.
    pub marks: Marks,
}

impl Analysis {
    /// Fresh analysis; `seed` drives the unvoiced marks' jitter.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        #[allow(clippy::cast_precision_loss)]
        let window = (0..LPC_WINDOW)
            .map(|i| 0.5 - 0.5 * (2.0 * PI * (i as f64 + 0.5) / LPC_WINDOW as f64).cos())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let mut lag = [1.0; ORDER + 1];
        for (k, l) in lag.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let x = 2.0 * PI * LAG_WINDOW_HZ * k as f64 / FS;
            *l = (-0.5 * x * x).exp();
        }
        // White-noise correction: a floor 80 dB under the grain's power. The usual 40 dB fills in everything but F1 of
        // a clean voice at 48 kHz (its energy sits under 4 kHz, so a floor spread to 24 kHz is high where F2 and F3
        // are).
        lag[0] = 1.000_000_01;
        let lp = Coefs::rbj(Shape::LowPass, 3600.0, std::f64::consts::FRAC_1_SQRT_2, 0.0);
        Self {
            n: 0,
            x: Ring::new(RING),
            xp: Ring::new(RING),
            e: Ring::new(RING),
            previous: 0.0,
            window,
            grain: vec![0.0; LPC_WINDOW].into_boxed_slice(),
            lag,
            frames: [Frame { a: IDENTITY, lsf: None }; FRAMES],
            a: IDENTITY,
            last_lsf: None,
            fitted: false,
            decimator: [Biquad::new(lp); 2],
            dec: Ring::new(DEC_RING),
            dec_count: 0,
            span: Box::new([0.0; SPAN]),
            yin: Yin::new(),
            estimate: Yin::new().last(),
            marks: Marks::new(seed),
        }
    }

    /// Back to silence, keeping every buffer.
    pub fn reset(&mut self) {
        self.n = 0;
        self.x.clear();
        self.xp.clear();
        self.e.clear();
        self.previous = 0.0;
        self.frames = [Frame { a: IDENTITY, lsf: None }; FRAMES];
        self.a = IDENTITY;
        self.last_lsf = None;
        self.fitted = false;
        self.decimator.iter_mut().for_each(Biquad::reset);
        self.dec.clear();
        self.dec_count = 0;
        self.yin.reset();
        self.estimate = self.yin.last();
        self.marks.reset();
    }

    /// The number of samples taken since the reset.
    #[must_use]
    pub const fn now(&self) -> i64 {
        self.n
    }

    /// The latest F0 estimate.
    #[must_use]
    pub const fn estimate(&self) -> Estimate {
        self.estimate
    }

    /// The predictor hop `t` was analysed with.
    #[must_use]
    pub fn frame_at(&self, t: i64) -> Frame {
        if t < 0 {
            return Frame { a: IDENTITY, lsf: None };
        }
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let slot = ((t / HOP) as usize) % FRAMES;
        self.frames[slot]
    }

    /// Takes the next input sample. `want_lsf` asks for the hop's line spectral frequencies (the formant warp needs
    /// them; a shifter at β = 1 does not).
    pub fn push(&mut self, x: f64, want_lsf: bool) {
        let n = self.n;
        self.x.set(n, x);
        self.xp.set(n, x - PRE_EMPHASIS * self.previous);
        self.previous = x;
        let low = self.decimator.iter_mut().fold(x, |v, f| f.run(v));
        if n % DECIMATION as i64 == 0 {
            self.dec.set(self.dec_count, low);
            self.dec_count += 1;
        }
        if n % HOP == 0 {
            self.fit(want_lsf);
            self.track();
        }
        let mut e = self.xp.get(n);
        for i in 1..=ORDER {
            e += self.a[i] * self.xp.get(n - i as i64);
        }
        self.e.set(n, e);
        let (residual, estimate) = (&self.e, self.estimate);
        self.marks.observe(n, |j| residual.get(j), estimate);
        self.n += 1;
    }

    /// Fits this hop's predictor to the window ending at the newest sample.
    fn fit(&mut self, want_lsf: bool) {
        let start = self.n - LPC_WINDOW as i64 + 1;
        for (i, g) in self.grain.iter_mut().enumerate() {
            *g = self.xp.get(start + i as i64) * self.window[i];
        }
        let mut r = [0.0; ORDER + 1];
        // Until the window has filled, the grain is mostly the zeros before the first sample: no fit, the identity.
        let filled = self.n >= LPC_WINDOW as i64;
        for (k, r) in r.iter_mut().enumerate().filter(|_| filled) {
            let sum = dot(&self.grain[k..], &self.grain);
            *r = sum * self.lag[k];
        }
        self.a = levinson(&r);
        // No fit (silence, or a window not yet full) leaves the identity, which whitens nothing: warping its evenly
        // spaced frequencies would put resonances on an unwhitened signal, so it has none. Nor does the first fit after
        // one: the grains around it still hold unwhitened samples, which a warped filter would ring on.
        let fitted = self.a != IDENTITY;
        let warpable = fitted && self.fitted;
        self.fitted = fitted;
        let lsf = if want_lsf && warpable {
            let found = to_lsf(&self.a).or(self.last_lsf);
            self.last_lsf = found;
            found
        } else {
            None
        };
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let slot = ((self.n / HOP) as usize) % FRAMES;
        self.frames[slot] = Frame { a: self.a, lsf };
    }

    /// Runs the F0 tracker on the newest decimated span.
    fn track(&mut self) {
        if self.dec_count < SPAN as i64 {
            return;
        }
        let first = self.dec_count - SPAN as i64;
        for (i, v) in self.span.iter_mut().enumerate() {
            *v = self.dec.get(first + i as i64);
        }
        self.estimate = self.yin.estimate(&self.span);
    }
}
