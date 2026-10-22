//! The meters (design section 3.5): peak with a 1.5 s hold and a 20 dB/s fall, and a 300 ms RMS, on every bus and
//! device input; on `program` also the short-term loudness (EBU R128, ITU-R BS.1770-4), the true peak (4x
//! oversampling) and the limiter's gain reduction.
//!
//! Everything is fixed state made on the control thread: measuring a block allocates nothing.

use omuzaki_audio_api::node::SAMPLE_RATE;
use omuzaki_audio_api::report::Meters;

use crate::dsp::{bessel_i0, to_db};

/// The RMS meter's time constant.
const RMS_SECONDS: f64 = 0.3;
/// How long a peak stays before it falls: 1.5 s.
const PEAK_HOLD_SAMPLES: usize = 72_000;
/// How fast a peak falls after its hold: 20 dB per second.
const PEAK_FALL_DB_PER_SAMPLE: f32 = 20.0 / 48_000.0;
/// A falling peak below this reads as silence.
const PEAK_FLOOR_DB: f32 = -120.0;
/// One loudness segment: 100 ms.
const SEGMENT: usize = 4_800;
/// The short-term window in segments: 3 s.
const SEGMENTS: usize = 30;
/// The short-term window in samples, as the divisor of its mean square.
const WINDOW_SAMPLES: f64 = 144_000.0;
/// Taps per phase of the true-peak interpolator.
const TRUE_PEAK_TAPS: usize = 24;
/// The true-peak oversampling factor (BS.1770-4 Annex 2).
const OVERSAMPLE: usize = 4;
/// The interpolator's Kaiser beta: about 60 dB of image rejection.
const TRUE_PEAK_BETA: f64 = 5.65;

/// A peak that holds, then falls.
#[derive(Clone, Copy, Debug)]
struct PeakHold {
    db: f32,
    hold: usize,
}

impl PeakHold {
    const fn new() -> Self {
        Self { db: f32::NEG_INFINITY, hold: 0 }
    }

    /// Takes one block's peak (dB) over `n` samples and returns the reading.
    fn update(&mut self, peak_db: f32, n: usize) -> f32 {
        if peak_db >= self.db {
            self.db = peak_db;
            self.hold = PEAK_HOLD_SAMPLES;
        } else if self.hold >= n {
            self.hold -= n;
        } else {
            let falling = u16::try_from(n - self.hold).unwrap_or(u16::MAX);
            self.hold = 0;
            self.db = (self.db - PEAK_FALL_DB_PER_SAMPLE * f32::from(falling)).max(peak_db);
            if self.db < PEAK_FLOOR_DB {
                self.db = f32::NEG_INFINITY;
            }
        }
        self.db
    }
}

/// One biquad, transposed direct form II, in `f64`.
#[derive(Clone, Copy, Debug, Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    const fn new(b: [f64; 3], a: [f64; 2]) -> Self {
        Self { b, a, z: [0.0; 2] }
    }

    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// The K-weighting of ITU-R BS.1770-4 at 48 kHz: the head's shelf, then the RLB high-pass.
#[derive(Clone, Copy, Debug)]
struct KWeighting {
    shelf: Biquad,
    high_pass: Biquad,
}

impl KWeighting {
    const fn new() -> Self {
        Self {
            shelf: Biquad::new(
                [1.535_124_859_586_97, -2.691_696_189_406_38, 1.198_392_810_852_85],
                [-1.690_659_293_182_41, 0.732_480_774_215_85],
            ),
            high_pass: Biquad::new([1.0, -2.0, 1.0], [-1.990_047_454_833_98, 0.990_072_250_366_21]),
        }
    }

    fn run(&mut self, x: f32) -> f64 {
        self.high_pass.run(self.shelf.run(f64::from(x)))
    }
}

/// The 4x interpolator for the true peak: four phases of a Kaiser-windowed sinc cut at the input's Nyquist, each
/// normalised to unity gain at DC. Section Annex 2 of BS.1770-4 gives a 48-tap example; this one is longer (96 taps),
/// so it under-reads less near 20 kHz.
#[derive(Clone, Debug)]
pub struct Interpolator {
    phases: [[f32; TRUE_PEAK_TAPS]; OVERSAMPLE],
}

impl Interpolator {
    #[allow(clippy::cast_precision_loss)] // tap counts and phases: small.
    fn new() -> Self {
        let mut phases = [[0.0f32; TRUE_PEAK_TAPS]; OVERSAMPLE];
        let centre = (TRUE_PEAK_TAPS as f64 - 1.0) / 2.0;
        let half = TRUE_PEAK_TAPS as f64 / 2.0;
        for (p, phase) in phases.iter_mut().enumerate() {
            // Phase `p` lands `p / 4` of a sample after the centre tap's sample (the centre sits between taps 11 and 12,
            // so phases are offsets from the half-sample point; the four together cover one input sample evenly).
            let offset = p as f64 / OVERSAMPLE as f64 - 0.5;
            let mut taps = [0.0f64; TRUE_PEAK_TAPS];
            for (k, tap) in taps.iter_mut().enumerate() {
                let t = k as f64 - centre - offset;
                let sinc =
                    if t.abs() < 1e-12 { 1.0 } else { (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t) };
                let r = t / half;
                let window = if r.abs() >= 1.0 { 0.0 } else { bessel_i0(TRUE_PEAK_BETA * (1.0 - r * r).sqrt()) };
                *tap = sinc * window / bessel_i0(TRUE_PEAK_BETA);
            }
            let sum: f64 = taps.iter().sum();
            for (slot, tap) in phase.iter_mut().zip(taps) {
                #[allow(clippy::cast_possible_truncation)] // filter taps: within f32.
                {
                    *slot = (tap / sum) as f32;
                }
            }
        }
        Self { phases }
    }
}

/// One channel's history for the interpolator: the last [`TRUE_PEAK_TAPS`] samples, doubled so a window is one slice.
#[derive(Clone, Copy, Debug)]
struct History {
    samples: [f32; 2 * TRUE_PEAK_TAPS],
    at: usize,
}

impl History {
    const fn new() -> Self {
        Self { samples: [0.0; 2 * TRUE_PEAK_TAPS], at: 0 }
    }

    /// Pushes one sample and returns the largest absolute value of the four interpolated points it completes.
    fn push(&mut self, x: f32, filter: &Interpolator) -> f32 {
        self.samples[self.at] = x;
        self.samples[self.at + TRUE_PEAK_TAPS] = x;
        self.at = (self.at + 1) % TRUE_PEAK_TAPS;
        let window = &self.samples[self.at..self.at + TRUE_PEAK_TAPS];
        let mut peak = 0.0f32;
        for phase in &filter.phases {
            let y: f32 = window.iter().zip(phase).map(|(s, c)| s * c).sum();
            peak = peak.max(y.abs());
        }
        peak
    }
}

/// What only `program` measures: loudness, true peak and the limiter's reduction.
#[derive(Clone, Debug)]
struct ProgramMeter {
    weighting: [KWeighting; 2],
    /// The K-weighted sum of squares of the current segment, over the channels.
    sum: f64,
    count: usize,
    /// The last [`SEGMENTS`] segments' sums.
    segments: [f64; SEGMENTS],
    next: usize,
    lufs: f32,
    interpolator: Interpolator,
    history: [History; 2],
    true_peak: PeakHold,
}

impl ProgramMeter {
    fn new() -> Self {
        Self {
            weighting: [KWeighting::new(); 2],
            sum: 0.0,
            count: 0,
            segments: [0.0; SEGMENTS],
            next: 0,
            lufs: f32::NEG_INFINITY,
            interpolator: Interpolator::new(),
            history: [History::new(); 2],
            true_peak: PeakHold::new(),
        }
    }

    /// Measures one block; returns (short-term LUFS, true peak dBTP).
    fn measure(&mut self, channels: &[&[f32]]) -> (f32, f32) {
        let frames = channels.first().map_or(0, |c| c.len());
        let mut peak = 0.0f32;
        for n in 0..frames {
            for (c, channel) in channels.iter().enumerate().take(2) {
                let k = self.weighting[c].run(channel[n]);
                self.sum += k * k;
                peak = peak.max(self.history[c].push(channel[n], &self.interpolator));
            }
            self.count += 1;
            if self.count == SEGMENT {
                self.segments[self.next] = self.sum;
                self.next = (self.next + 1) % SEGMENTS;
                self.sum = 0.0;
                self.count = 0;
                let mean: f64 = self.segments.iter().sum::<f64>() / WINDOW_SAMPLES;
                #[allow(clippy::cast_possible_truncation)] // LUFS values: small.
                {
                    self.lufs = if mean > 0.0 { (-0.691 + 10.0 * mean.log10()) as f32 } else { f32::NEG_INFINITY };
                }
            }
        }
        (self.lufs, self.true_peak.update(to_db(f64::from(peak)), frames))
    }
}

/// The meter of one bus or device input.
#[derive(Clone, Debug)]
pub struct Meter {
    peak: PeakHold,
    mean_square: f64,
    alpha: f64,
    program: Option<Box<ProgramMeter>>,
}

impl Meter {
    /// Peak and RMS only (every bus but `program`, and device inputs).
    #[must_use]
    pub fn plain() -> Self {
        Self {
            peak: PeakHold::new(),
            mean_square: 0.0,
            alpha: (-1.0 / (RMS_SECONDS * f64::from(SAMPLE_RATE))).exp(),
            program: None,
        }
    }

    /// Peak and RMS plus loudness, true peak and the limiter's reduction (the `program` bus).
    #[must_use]
    pub fn program() -> Self {
        Self { program: Some(Box::new(ProgramMeter::new())), ..Self::plain() }
    }

    /// Measures one block of up to two channels (the louder of each pair counts for peak and RMS; loudness sums the
    /// channels), with `gain_reduction_db` the chain's reported reduction, and returns the reading.
    pub fn measure(&mut self, channels: &[&[f32]], gain_reduction_db: f32) -> Meters {
        let frames = channels.first().map_or(0, |c| c.len());
        let mut peak = 0.0f32;
        for n in 0..frames {
            let x = channels.iter().map(|c| c[n].abs()).fold(0.0f32, f32::max);
            peak = peak.max(x);
            self.mean_square = self.alpha * self.mean_square + (1.0 - self.alpha) * f64::from(x) * f64::from(x);
        }
        let mut reading = Meters {
            peak_db: self.peak.update(to_db(f64::from(peak)), frames),
            rms_db: to_db(self.mean_square.sqrt()),
            ..Meters::SILENT
        };
        if let Some(program) = &mut self.program {
            (reading.short_term_lufs, reading.true_peak_dbtp) = program.measure(channels);
            reading.limiter_gain_reduction_db = gain_reduction_db;
        }
        reading
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
mod tests {
    use super::*;

    fn sine(hz: f64, db: f64, phase: f64, frames: usize) -> Vec<f32> {
        let amplitude = 10f64.powf(db / 20.0);
        (0..frames)
            .map(|n| (amplitude * (2.0 * std::f64::consts::PI * hz * n as f64 / 48_000.0 + phase).sin()) as f32)
            .collect()
    }

    /// Feeds `channels` through a meter in blocks of 480 and returns the last reading.
    fn read(meter: &mut Meter, channels: &[&[f32]]) -> Meters {
        let frames = channels[0].len();
        let mut last = Meters::SILENT;
        for start in (0..frames).step_by(480) {
            let end = (start + 480).min(frames);
            let block: Vec<&[f32]> = channels.iter().map(|c| &c[start..end]).collect();
            last = meter.measure(&block, 0.0);
        }
        last
    }

    #[test]
    fn a_sine_at_minus_20_reads_its_peak_and_rms() {
        let s = sine(1_000.0, -20.0, 0.0, 96_000);
        let m = read(&mut Meter::plain(), &[&s]);
        assert!((m.peak_db + 20.0).abs() < 0.05, "peak {}", m.peak_db);
        assert!((m.rms_db + 23.01).abs() < 0.05, "rms {}", m.rms_db);
        assert_eq!(m.short_term_lufs, f32::NEG_INFINITY);
    }

    #[test]
    fn a_peak_holds_for_a_second_and_a_half_then_falls_at_20_db_a_second() {
        let mut m = Meter::plain();
        let loud = [0.5f32; 480];
        let quiet = [0.0f32; 480];
        let first = m.measure(&[&loud], 0.0).peak_db;
        // 1.5 s of silence: the peak holds.
        for _ in 0..150 {
            assert_eq!(m.measure(&[&quiet], 0.0).peak_db, first);
        }
        // Then 0.5 s of silence: it has fallen 10 dB.
        let mut reading = first;
        for _ in 0..50 {
            reading = m.measure(&[&quiet], 0.0).peak_db;
        }
        assert!((first - reading - 10.0).abs() < 1e-3, "{first} then {reading}");
        // A louder block takes over at once.
        assert_eq!(m.measure(&[&[0.9f32; 4]], 0.0).peak_db, to_db(f64::from(0.9f32)));
        // Long silence ends at silence, not at a very small number.
        for _ in 0..2_000 {
            reading = m.measure(&[&quiet], 0.0).peak_db;
        }
        assert_eq!(reading, f32::NEG_INFINITY);
    }

    /// EBU Tech 3341 case 1: a stereo 1 kHz sine at -23 dBFS reads -23.0 LUFS short-term; case 2 at -33.
    #[test]
    fn a_stereo_sine_at_minus_23_reads_minus_23_lufs() {
        for db in [-23.0, -33.0] {
            let s = sine(1_000.0, db, 0.0, 4 * 48_000);
            let m = read(&mut Meter::program(), &[&s, &s]);
            assert!((f64::from(m.short_term_lufs) - db).abs() < 0.1, "{db}: {}", m.short_term_lufs);
        }
    }

    #[test]
    fn the_k_weighting_lifts_highs_and_cuts_lows_by_its_curve() {
        // BS.1770-4's curve lifts 10 kHz by about 4 dB and cuts 40 Hz, against 1 kHz, where its gain is the 0.691 dB the
        // formula takes back: a mono -20 dBFS sine (mean square -23.01 dB) reads -23.01 LUFS.
        let lufs = |hz: f64| {
            let s = sine(hz, -20.0, 0.0, 4 * 48_000);
            f64::from(read(&mut Meter::program(), &[&s]).short_term_lufs)
        };
        let at_1k = lufs(1_000.0);
        assert!((at_1k + 23.01).abs() < 0.1, "{at_1k}");
        assert!(lufs(10_000.0) - at_1k > 3.0);
        assert!(lufs(40.0) - at_1k < -1.0);
    }

    /// True-peak cases in the manner of EBU Tech 3341 (sines whose peaks fall between samples): each reads its true
    /// peak within ±0.2 dB, where the sample peak can under-read by up to 3 dB.
    #[test]
    fn an_intersample_peak_reads_its_true_level() {
        let cases = [
            (12_000.0, std::f64::consts::FRAC_PI_4), // fs/4 at 45°: the samples sit 3 dB below the peak
            (997.0, 0.3),
            (5_000.0, 1.1),
            (8_000.0, std::f64::consts::FRAC_PI_6),
            (15_000.0, 0.7),
            (19_000.0, 0.2),
        ];
        for (hz, phase) in cases {
            // Three seconds: the abrupt start rings, and that peak is held for 1.5 s before the steady one shows.
            let s = sine(hz, -6.0, phase, 3 * 48_000);
            let m = read(&mut Meter::program(), &[&s, &s]);
            assert!((m.true_peak_dbtp + 6.0).abs() < 0.2, "{hz} Hz: {} dBTP", m.true_peak_dbtp);
        }
        let s = sine(12_000.0, -6.0, std::f64::consts::FRAC_PI_4, 48_000);
        let m = read(&mut Meter::program(), &[&s]);
        assert!(m.peak_db < -8.9, "the sample peak under-reads: {}", m.peak_db);
    }

    #[test]
    fn the_limiters_reduction_reaches_the_program_meter_and_only_it() {
        let s = sine(1_000.0, -6.0, 0.0, 480);
        assert_eq!(Meter::program().measure(&[&s], 2.5).limiter_gain_reduction_db, 2.5);
        assert_eq!(Meter::plain().measure(&[&s], 2.5).limiter_gain_reduction_db, 0.0);
    }

    #[test]
    fn silence_reads_silence() {
        let m = Meter::program().measure(&[&[0.0; 16], &[0.0; 16]], 0.0);
        assert_eq!(m, Meters::SILENT);
    }
}
