//! Lip sync from the microphone: voice activity, loudness and the five visemes, published as the
//! [`VoiceSource`] the stage reads (spec v3 FE-08).
//!
//! [`VoiceAnalyser`] is the signal path, pure and testable: every 10 ms it takes the last 32 ms of the 16 kHz voice,
//! measures its level against a tracked noise floor (voice activity with a 150 ms hold), and estimates the first two
//! formants from a linear-prediction envelope; the vowel whose formants lie nearest (in octaves) gets the weight. That
//! is the classic formant route of the open lip-sync tools, with no model to download and well under a millisecond of
//! work per hop. [`LipSync`] runs it on a worker thread over the engine's `Mouth` voice tap, which is silent when the
//! person mutes, and stamps each frame on the engine clock at the moment the sound was made.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use omuzaki_audio_api::engine::{AudioEngine, AudioError};
use omuzaki_audio_api::tap::{TapFormat, TapRate, TapRequest, VoiceUse};
use omuzaki_audio_api::voice::{VISEME_COUNT, VoiceFrame, VoiceSource};
use omuzaki_core::time::Seconds;

use crate::executor::lock;

/// The voice tap's rate.
pub const VOICE_RATE: u32 = 16_000;
/// Samples per analysis window (32 ms).
const WINDOW: usize = 512;
/// Samples between frames (10 ms).
const HOP: usize = 160;
/// The linear-prediction order: two poles per formant up to 4 kHz, plus two for the glottal tilt.
const LPC_ORDER: usize = 14;
/// The envelope is searched for formants at this spacing, in Hz.
const ENVELOPE_STEP_HZ: f64 = 20.0;
/// How far above the noise floor speech must be, in dB.
const ACTIVITY_MARGIN_DB: f32 = 10.0;
/// Quieter than this is never speech, in dBFS.
const ACTIVITY_MIN_DB: f32 = -55.0;
/// How long speech holds after the level drops, in hops.
const HOLD_HOPS: u32 = 15;
/// How fast the noise floor may rise, in dB per hop (0.5 dB/s), so a held vowel never becomes "noise".
const FLOOR_RISE_DB: f32 = 0.005;
/// The level span from the activity threshold to a fully open mouth, in dB.
const OPEN_SPAN_DB: f32 = 30.0;
/// How much of the way to the new weights each hop moves: fast on the way up, slower down.
const ATTACK: f32 = 0.6;
const RELEASE: f32 = 0.3;
/// How wide a vowel's region is, in octaves of formant distance.
const VOWEL_SIGMA: f64 = 0.28;

/// The vowels in [`omuzaki_audio_api::voice::VISEMES`] order (aa, ih, ou, ee, oh), as typical first and second formants
/// in Hz, between adult male and female speakers.
const VOWEL_FORMANTS: [(f64, f64); VISEME_COUNT] =
    [(800.0, 1300.0), (320.0, 2400.0), (360.0, 1400.0), (520.0, 2000.0), (540.0, 900.0)];

/// One analysed hop.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Analysis {
    /// The 16 kHz stream index of the window's centre.
    pub centre: u64,
    /// Whether speech is on (with the hold).
    pub active: bool,
    /// The window's level, dBFS.
    pub level_db: f32,
    /// Mouth openness from level, 0 to 1.
    pub open: f32,
    /// The viseme weights, already scaled by `open`.
    pub visemes: [f32; VISEME_COUNT],
    /// The formants found (F1, F2) in Hz, when the window had two.
    pub formants: Option<(f32, f32)>,
}

/// The lip-sync signal path over a 16 kHz voice.
#[derive(Clone, Debug)]
pub struct VoiceAnalyser {
    window: Vec<f32>,
    filled: usize,
    since_hop: usize,
    /// The 16 kHz stream index of the next sample pushed.
    next_index: u64,
    floor_db: f32,
    hold: u32,
    visemes: [f32; VISEME_COUNT],
    open: f32,
    hamming: Vec<f32>,
}

impl Default for VoiceAnalyser {
    fn default() -> Self {
        Self::new()
    }
}

impl VoiceAnalyser {
    /// An analyser with an empty window.
    #[must_use]
    pub fn new() -> Self {
        #[allow(clippy::cast_precision_loss)] // Window indices: tiny.
        let hamming = (0..WINDOW)
            .map(|n| {
                let x = 2.0 * std::f64::consts::PI * n as f64 / (WINDOW - 1) as f64;
                #[allow(clippy::cast_possible_truncation)]
                let w = (0.54 - 0.46 * x.cos()) as f32;
                w
            })
            .collect();
        Self {
            window: vec![0.0; WINDOW],
            filled: 0,
            since_hop: 0,
            next_index: 0,
            floor_db: -70.0,
            hold: 0,
            visemes: [0.0; VISEME_COUNT],
            open: 0.0,
            hamming,
        }
    }

    /// Starts the stream at `index` (after a gap the analyser forgets the old window).
    pub fn restart_at(&mut self, index: u64) {
        self.filled = 0;
        self.since_hop = 0;
        self.next_index = index;
    }

    /// The 16 kHz stream index of the next sample expected.
    #[must_use]
    pub fn next_index(&self) -> u64 {
        self.next_index
    }

    /// Takes 16 kHz samples, calling `out` once per completed hop.
    pub fn push(&mut self, samples: &[f32], mut out: impl FnMut(Analysis)) {
        for &s in samples {
            self.window.copy_within(1.., 0);
            self.window[WINDOW - 1] = s;
            self.filled = (self.filled + 1).min(WINDOW);
            self.next_index += 1;
            self.since_hop += 1;
            if self.since_hop == HOP {
                self.since_hop = 0;
                if self.filled == WINDOW {
                    out(self.analyse());
                }
            }
        }
    }

    fn analyse(&mut self) -> Analysis {
        #[allow(clippy::cast_precision_loss)] // The window length: tiny.
        let mean_square = self.window.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>() / WINDOW as f64;
        #[allow(clippy::cast_possible_truncation)]
        let level_db = if mean_square > 0.0 { (10.0 * mean_square.log10()) as f32 } else { f32::NEG_INFINITY };
        // The floor follows quiet stretches at once and rises slowly through speech.
        if level_db < self.floor_db {
            self.floor_db = level_db.max(-100.0);
        } else {
            self.floor_db = (self.floor_db + FLOOR_RISE_DB).min(-30.0);
        }
        let threshold = (self.floor_db + ACTIVITY_MARGIN_DB).max(ACTIVITY_MIN_DB);
        if level_db > threshold {
            self.hold = HOLD_HOPS;
        } else {
            self.hold = self.hold.saturating_sub(1);
        }
        let active = self.hold > 0;
        let target_open = if active { ((level_db - threshold) / OPEN_SPAN_DB).clamp(0.0, 1.0) } else { 0.0 };
        let formants = if level_db > threshold { self.formants() } else { None };
        let mut shape = [0.0f32; VISEME_COUNT];
        if let Some((f1, f2)) = formants {
            shape = vowel_weights(f64::from(f1), f64::from(f2));
        } else if active {
            // Speech without a clear vowel (a consonant, a breath): keep the last shape.
            let sum: f32 = self.visemes.iter().sum();
            if sum > 1e-6 {
                shape = self.visemes.map(|w| w / sum);
            }
        }
        let step =
            |current: f32, target: f32| current + (target - current) * if target > current { ATTACK } else { RELEASE };
        self.open = step(self.open, target_open);
        for (w, s) in self.visemes.iter_mut().zip(shape) {
            *w = step(*w, s * target_open);
        }
        let centre = self.next_index.saturating_sub(WINDOW as u64 / 2);
        Analysis { centre, active, level_db, open: self.open, visemes: self.visemes, formants }
    }

    /// F1 and F2 from the window's linear-prediction envelope.
    fn formants(&self) -> Option<(f32, f32)> {
        // Pre-emphasis lifts the spectrum's tilt so the formants stand out; the window tapers the edges.
        let mut x = [0.0f64; WINDOW];
        let mut previous = 0.0f32;
        for (i, (s, w)) in self.window.iter().zip(&self.hamming).enumerate() {
            x[i] = f64::from((s - 0.97 * previous) * w);
            previous = *s;
        }
        let mut r = [0.0f64; LPC_ORDER + 1];
        for (lag, value) in r.iter_mut().enumerate() {
            *value = x[lag..].iter().zip(&x).map(|(a, b)| a * b).sum();
        }
        if r[0] <= 0.0 {
            return None;
        }
        r[0] *= 1.0 + 1e-9;
        let a = levinson(&r)?;
        let mut peaks = [0.0f64; 4];
        let mut found = 0;
        let mut previous = (0.0, 0.0);
        let mut hz = 100.0;
        while hz < 4_000.0 && found < peaks.len() {
            let g = envelope(&a, hz);
            if previous.1 > g && previous.1 > envelope(&a, previous.0 - ENVELOPE_STEP_HZ) {
                peaks[found] = previous.0;
                found += 1;
            }
            previous = (hz, g);
            hz += ENVELOPE_STEP_HZ;
        }
        let peaks = &peaks[..found];
        let f1 = *peaks.iter().find(|f| (200.0..1_100.0).contains(*f))?;
        let f2 = *peaks.iter().find(|f| **f >= (f1 + 250.0).max(650.0) && **f < 3_200.0)?;
        #[allow(clippy::cast_possible_truncation)]
        Some((f1 as f32, f2 as f32))
    }
}

/// The Levinson-Durbin recursion: prediction coefficients `a[0..=p]` (with `a[0] = 1`) from autocorrelation `r`.
fn levinson(r: &[f64; LPC_ORDER + 1]) -> Option<[f64; LPC_ORDER + 1]> {
    let mut a = [0.0f64; LPC_ORDER + 1];
    a[0] = 1.0;
    let mut error = r[0];
    for i in 1..=LPC_ORDER {
        let mut acc = r[i];
        for j in 1..i {
            acc += a[j] * r[i - j];
        }
        let k = -acc / error;
        let previous = a;
        for j in 1..i {
            a[j] = previous[j] + k * previous[i - j];
        }
        a[i] = k;
        error *= 1.0 - k * k;
        if error <= 0.0 {
            return None;
        }
    }
    Some(a)
}

/// The envelope's power gain `1 / |A(e^jw)|^2` at `hz`.
fn envelope(a: &[f64; LPC_ORDER + 1], hz: f64) -> f64 {
    let w = 2.0 * std::f64::consts::PI * hz / f64::from(VOICE_RATE);
    let (mut re, mut im) = (0.0, 0.0);
    for (k, c) in a.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let phase = w * k as f64;
        re += c * phase.cos();
        im -= c * phase.sin();
    }
    1.0 / (re * re + im * im).max(1e-30)
}

/// The weights of the five vowels for formants `f1`, `f2`: a Gaussian of the distance in octaves, normalised to 1.
fn vowel_weights(f1: f64, f2: f64) -> [f32; VISEME_COUNT] {
    let mut w = [0.0f64; VISEME_COUNT];
    for (weight, (v1, v2)) in w.iter_mut().zip(VOWEL_FORMANTS) {
        let d1 = (f1 / v1).log2();
        let d2 = (f2 / v2).log2();
        *weight = (-(d1 * d1 + d2 * d2) / (2.0 * VOWEL_SIGMA * VOWEL_SIGMA)).exp();
    }
    let sum: f64 = w.iter().sum();
    if sum <= 1e-12 {
        return [0.0; VISEME_COUNT];
    }
    #[allow(clippy::cast_possible_truncation)]
    w.map(|v| (v / sum) as f32)
}

/// How often the worker reads the tap.
const POLL: Duration = Duration::from_millis(5);
/// The largest gap the analyser bridges with silence instead of starting over, in 16 kHz samples.
const BRIDGE_GAP: u64 = WINDOW as u64;

/// Lip sync on a running engine: a worker reading the `Mouth` voice tap, publishing the latest [`VoiceFrame`].
pub struct LipSync {
    latest: Arc<Mutex<Option<VoiceFrame>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for LipSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LipSync").finish_non_exhaustive()
    }
}

impl LipSync {
    /// Opens the `Mouth` voice tap on `engine` and starts the worker.
    ///
    /// # Errors
    /// The engine's error when the tap cannot open; [`AudioError::Internal`] when the thread cannot start.
    pub fn start(engine: &dyn AudioEngine) -> Result<Self, AudioError> {
        // The mouth's voice tap, at the analyser's 16 kHz (the engine decimates with its speech filter).
        let mut request = TapRequest::voice(VoiceUse::Mouth, "lip sync");
        request.format = TapFormat { rate: TapRate::Speech16k, mono: true };
        let (mut reader, info) = engine.tap_ring(&request)?;
        let clock = engine.clock();
        let latest = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (out, flag) = (Arc::clone(&latest), Arc::clone(&stop));
        let ratio = u64::from(omuzaki_audio_api::node::SAMPLE_RATE / VOICE_RATE.max(1));
        let worker = std::thread::Builder::new()
            .name("omuzaki-lip-sync".into())
            .spawn(move || {
                let mut analyser = VoiceAnalyser::new();
                analyser.restart_at(info.first_sample);
                let mut buf = vec![0.0f32; 2_048];
                let silence = [0.0f32; WINDOW];
                while !flag.load(Ordering::Acquire) {
                    loop {
                        let run = reader.read(&mut buf);
                        if run.len == 0 {
                            break;
                        }
                        let expected = analyser.next_index();
                        if run.start_sample > expected {
                            let gap = run.start_sample - expected;
                            if gap <= BRIDGE_GAP {
                                let n = usize::try_from(gap).unwrap_or(WINDOW).min(WINDOW);
                                analyser.push(&silence[..n], |_| {});
                            } else {
                                analyser.restart_at(run.start_sample);
                            }
                        }
                        if run.muted || run.private {
                            buf[..run.len].fill(0.0);
                        }
                        let mut last = None;
                        analyser.push(&buf[..run.len], |a| last = Some(a));
                        if let Some(a) = last {
                            let snapshot = clock.read();
                            let time = Seconds(snapshot.time_of(a.centre * ratio).0 - info.capture_offset.0);
                            let frame = VoiceFrame {
                                sample: snapshot.sample_at(time),
                                time,
                                active: a.active,
                                level_db: a.level_db,
                                open: a.open,
                                visemes: a.visemes,
                            };
                            *lock(&out) = Some(frame);
                        }
                    }
                    std::thread::sleep(POLL);
                }
            })
            .map_err(|e| AudioError::Internal(format!("the lip-sync thread could not start: {e}")))?;
        Ok(Self { latest, stop, worker: Some(worker) })
    }
}

impl VoiceSource for LipSync {
    fn latest(&self) -> Option<VoiceFrame> {
        *lock(&self.latest)
    }
}

impl Drop for LipSync {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss, clippy::cast_sign_loss)]
mod tests {
    use super::*;
    use omuzaki_audio_api::voice::VISEMES;

    /// A vowel from a formant synthesiser: a glottal pulse train at `f0`, tilted like a voice, through three resonators,
    /// peaking at 0.3.
    fn voiced(f0: f64, f1: f64, f2: f64, f3: f64, seconds: f64) -> Vec<f32> {
        let rate = f64::from(VOICE_RATE);
        let n = (seconds * rate) as usize;
        let period = (rate / f0) as usize;
        let mut x: Vec<f64> = (0..n).map(|i| if i % period == 0 { 1.0 } else { 0.0 }).collect();
        // The glottal source falls about 12 dB per octave and the lips' radiation lifts 6: net -6, one pole.
        let mut tilt = 0.0;
        for s in &mut x {
            tilt = *s + 0.95 * tilt;
            *s = tilt;
        }
        for (f, bw) in [(f1, 80.0), (f2, 100.0), (f3, 140.0)] {
            let r = (-std::f64::consts::PI * bw / rate).exp();
            let (c1, c2) = (2.0 * r * (2.0 * std::f64::consts::PI * f / rate).cos(), -r * r);
            let (mut y1, mut y2) = (0.0, 0.0);
            for s in &mut x {
                let y = *s + c1 * y1 + c2 * y2;
                y2 = y1;
                y1 = y;
                *s = y;
            }
        }
        let peak = x.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        x.iter().map(|v| (v / peak * 0.3) as f32).collect()
    }

    fn vowel(f1: f64, f2: f64, f3: f64, seconds: f64) -> Vec<f32> {
        voiced(140.0, f1, f2, f3, seconds)
    }

    fn dominant(samples: &[f32]) -> (usize, Analysis) {
        let mut a = VoiceAnalyser::new();
        let mut last = None;
        a.push(samples, |x| last = Some(x));
        let last = last.unwrap_or_else(|| panic!("no frames"));
        let best = (0..VISEME_COUNT).max_by(|i, j| last.visemes[*i].total_cmp(&last.visemes[*j])).unwrap_or(0);
        (best, last)
    }

    #[test]
    fn each_vowel_reads_as_its_viseme() {
        // Typical adult formants for a, i, u, e, o; a higher voice scales them by about 1.15.
        let vowels = [
            (750.0, 1250.0, 2600.0),
            (300.0, 2300.0, 3000.0),
            (340.0, 1350.0, 2400.0),
            (480.0, 1900.0, 2600.0),
            (500.0, 850.0, 2500.0),
        ];
        // Pitches up to 200 Hz. Above that, linear prediction starts to lock onto harmonics and `ou` blurs into `ee`; the
        // per-person vowel calibration (a leaf of FE-08) is the fix.
        for (f0, scale) in [(120.0, 1.0), (140.0, 1.0), (140.0, 1.15), (200.0, 1.15)] {
            for (k, (f1, f2, f3)) in vowels.iter().enumerate() {
                let (best, a) = dominant(&voiced(f0, f1 * scale, f2 * scale, f3 * scale, 0.4));
                assert!(a.active, "{} is speech", VISEMES[k]);
                assert_eq!(
                    VISEMES[best], VISEMES[k],
                    "f0 {f0}, scale {scale}: formants {:?}, weights {:?}",
                    a.formants, a.visemes
                );
            }
        }
    }

    #[test]
    fn silence_and_a_quiet_room_are_not_speech() {
        let mut a = VoiceAnalyser::new();
        let mut frames = Vec::new();
        a.push(&vec![0.0; 16_000], |x| frames.push(x));
        assert!(frames.iter().all(|f| !f.active && f.open == 0.0));
        // Low noise (about -60 dBFS): still not speech.
        let mut seed = 1u32;
        let noise: Vec<f32> = (0..16_000)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (f64::from(seed >> 8) / f64::from(1u32 << 24) - 0.5) as f32 * 0.003
            })
            .collect();
        frames.clear();
        a.push(&noise, |x| frames.push(x));
        assert!(frames.iter().all(|f| !f.active), "a quiet room is not speech");
    }

    #[test]
    fn the_mouth_closes_after_the_hold() {
        let mut a = VoiceAnalyser::new();
        let mut frames = Vec::new();
        a.push(&vowel(750.0, 1250.0, 2600.0, 0.3), |x| frames.push(x));
        assert!(frames.last().is_some_and(|f| f.active && f.open > 0.5), "{:?}", frames.last());
        frames.clear();
        a.push(&vec![0.0; 8_000], |x| frames.push(x));
        let last = frames.last().copied().unwrap_or_else(|| panic!("frames"));
        assert!(!last.active && last.open < 0.01 && last.visemes.iter().all(|w| *w < 0.01), "{last:?}");
    }

    #[test]
    fn a_hop_is_well_inside_its_budget() {
        let mut a = VoiceAnalyser::new();
        let speech = vowel(500.0, 850.0, 2500.0, 2.0);
        let started = std::time::Instant::now();
        let mut hops = 0u32;
        a.push(&speech, |_| hops += 1);
        let per_hop = started.elapsed().as_secs_f64() / f64::from(hops.max(1));
        // A ceiling that holds in a debug build on a slow runner; the release cost is far lower.
        assert!(per_hop < 0.005, "{:.3} ms per hop", per_hop * 1e3);
    }
}
