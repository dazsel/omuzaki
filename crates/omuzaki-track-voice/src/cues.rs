//! [`CueExtractor`]: the life layer's [`Cues`] from the `cues` voice tap (PF-08), stamped with performance time.
//!
//! Per 10 ms hop of the tap (48 kHz `Overwrite`, any rate works):
//!
//! - **Loudness:** the speech band (a 100 Hz high-pass and a 4 kHz low-pass, both second-order Butterworth), RMS over
//!   [`LOUDNESS_WINDOW_S`], mapped from [`LOUDNESS_DBFS`] to `0..1`, smoothed with [`ATTACK_S`] attack and
//!   [`RELEASE_S`] release.
//! - **Onset:** the three-band onset strength peak-picked by [`OnsetDetector`] (an adaptive threshold, 120 ms
//!   refractory).
//! - **Laugh:** 0 here; laughter detection is a later task.
//!
//! **Stamping** (`audio-api.md` §5.1): a hop's time is `clock.time_of(sample) - capture_offset`, where `sample` is the
//! engine-clock index of its last sample, so the cue lands on the moment the person made the sound, not the moment
//! the changer or the chain let it through. An onset carries its own time the same way.
//!
//! **Mute:** a muted run is analysed as silence and its hops carry zero cues, so a muted person neither nods nor
//! sways, and the first word after the unmute is an onset.
//!
//! [`cue_mailbox`] hands frames to the perform thread newest-wins with onsets as a running count, as the voice
//! mailbox does; [`CueListener`] runs the extractor on its own thread over the tap ring. Nothing allocates after
//! construction (`tests/no_alloc.rs`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering, fence};
use std::thread::JoinHandle;
use std::time::Duration;

use omuzaki_anim_api::Cues;
use omuzaki_audio_api::clock::EngineClock;
use omuzaki_audio_api::tap::{TapInfo, TapReader};
use omuzaki_core::time::Seconds;

use crate::analyzer::{RATE_RANGE, VoiceError};
use crate::onset::{Biquad, OnsetDetector};

/// Time between cue frames, seconds.
pub const CUE_HOP_S: f32 = 0.010;
/// The loudness RMS window, seconds.
pub const LOUDNESS_WINDOW_S: f32 = 0.050;
/// Loudness is 0 at the first level and 1 at the second, dBFS.
pub const LOUDNESS_DBFS: (f32, f32) = (-50.0, -10.0);
/// The speech band, Hz.
pub const SPEECH_BAND_HZ: (f32, f32) = (100.0, 4_000.0);
/// Loudness smoothing: how fast it rises and falls, time constants in seconds.
pub const ATTACK_S: f32 = 0.030;
/// See [`ATTACK_S`].
pub const RELEASE_S: f32 = 0.150;

/// One hop of cues. `Copy`, fixed-size.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct CueFrame {
    /// The engine-clock index of the hop's last sample, at the tap's rate.
    pub sample: u64,
    /// The performance time of that sample: `clock.time_of(sample) - capture_offset`.
    pub at: Seconds,
    /// Smoothed speech-band loudness, `0..1`; 0 while muted.
    pub loudness: f32,
    /// Onsets since the extractor started. A count, not a flag, so a reader that skips frames still sees every one.
    pub onsets: u32,
    /// The performance time of the newest onset, `None` before the first.
    pub onset_at: Option<Seconds>,
    /// The hop was muted.
    pub muted: bool,
}

impl CueFrame {
    /// The life layer's cues for this frame, given the onset count a reader last saw: `onset` is set when there is a
    /// newer one.
    pub fn cues(&self, seen_onsets: u32) -> Cues {
        Cues { loudness: self.loudness, onset: self.onsets != seen_onsets, laugh: 0.0, ..Cues::default() }
    }
}

/// Speech-band loudness, smoothed, at a 1 ms hop.
#[derive(Clone, Debug)]
struct Loudness {
    high: Biquad,
    low: Biquad,
    hop: usize,
    in_hop: usize,
    acc: f32,
    /// Energy per hop over the window, circular.
    window: Box<[f32]>,
    head: usize,
    attack: f32,
    release: f32,
    value: f32,
}

impl Loudness {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // small counts
    fn new(rate: f32) -> Self {
        let hop = (rate / 1000.0).round().max(1.0) as usize;
        let dt = hop as f32 / rate;
        Self {
            high: Biquad::high_pass(SPEECH_BAND_HZ.0, rate),
            low: Biquad::low_pass(SPEECH_BAND_HZ.1, rate),
            hop,
            in_hop: 0,
            acc: 0.0,
            window: vec![0.0; (LOUDNESS_WINDOW_S / dt).round().max(1.0) as usize].into(),
            head: 0,
            attack: 1.0 - (-dt / ATTACK_S).exp(),
            release: 1.0 - (-dt / RELEASE_S).exp(),
            value: 0.0,
        }
    }

    #[allow(clippy::cast_precision_loss)] // window lengths are small
    fn step(&mut self, x: f32) {
        let y = self.low.step(self.high.step(x));
        self.acc += y * y;
        self.in_hop += 1;
        if self.in_hop < self.hop {
            return;
        }
        self.in_hop = 0;
        self.window[self.head] = self.acc;
        self.head = (self.head + 1) % self.window.len();
        self.acc = 0.0;
        let mean = self.window.iter().sum::<f32>() / (self.window.len() * self.hop) as f32;
        let db = 10.0 * (mean + 1e-12).log10();
        let (lo, hi) = LOUDNESS_DBFS;
        let target = ((db - lo) / (hi - lo)).clamp(0.0, 1.0);
        let rate = if target > self.value { self.attack } else { self.release };
        self.value += (target - self.value) * rate;
    }
}

/// The extractor. One per cues tap; feed it with [`push`](Self::push) from one thread.
pub struct CueExtractor {
    clock: Arc<EngineClock>,
    capture_offset: f64,
    loudness: Loudness,
    onset: OnsetDetector,
    /// Samples per cue hop, and how many of this hop have been seen.
    hop: usize,
    in_hop: usize,
    /// The engine-clock index of the next sample expected.
    next: u64,
    /// The current hop has a muted sample.
    muted: bool,
    frame: CueFrame,
}

impl CueExtractor {
    /// An extractor for the tap `info` describes, stamping on `clock`. Cold: allocates.
    ///
    /// # Errors
    /// [`VoiceError::Rate`] for a rate outside [`RATE_RANGE`].
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // small counts
    pub fn new(info: &TapInfo, clock: Arc<EngineClock>) -> Result<Self, VoiceError> {
        if !(RATE_RANGE.0..=RATE_RANGE.1).contains(&info.rate) {
            return Err(VoiceError::Rate(info.rate));
        }
        let rate = info.rate as f32;
        Ok(Self {
            clock,
            capture_offset: info.capture_offset.0,
            loudness: Loudness::new(rate),
            onset: OnsetDetector::new(info.rate),
            hop: (CUE_HOP_S * rate).round() as usize,
            in_hop: 0,
            next: info.first_sample,
            muted: false,
            frame: CueFrame::default(),
        })
    }

    /// The newest frame (all zero before the first hop).
    pub fn frame(&self) -> &CueFrame {
        &self.frame
    }

    fn time_of(&self, sample: u64) -> Seconds {
        Seconds(self.clock.time_of(sample).0 - self.capture_offset)
    }

    /// Feeds a run of `samples` (mono, `-1..1`) whose first sample is engine-clock index `start`, muted or not, and
    /// calls `on_frame` for every hop they complete. A run that starts later than the last one ended skips the gap:
    /// nothing is analysed for it and the hops keep their place on the clock. Allocates nothing.
    pub fn push(&mut self, start: u64, muted: bool, samples: &[f32], mut on_frame: impl FnMut(&CueFrame)) {
        if start > self.next {
            self.next = start;
        }
        for &s in samples {
            let x = if muted { 0.0 } else { s };
            self.muted |= muted;
            self.loudness.step(x);
            if let Some(ago) = self.onset.step(x)
                && !self.muted
            {
                self.frame.onsets = self.frame.onsets.wrapping_add(1);
                self.frame.onset_at = Some(self.time_of(self.next.saturating_sub(ago as u64 - 1)));
            }
            self.in_hop += 1;
            if self.in_hop >= self.hop {
                self.in_hop = 0;
                self.frame.sample = self.next;
                self.frame.at = self.time_of(self.next);
                self.frame.muted = self.muted;
                if self.muted {
                    // A muted voice is silent to the life layer at once, not after a release.
                    self.loudness.value = 0.0;
                }
                self.frame.loudness = self.loudness.value;
                self.muted = false;
                on_frame(&self.frame);
            }
            self.next += 1;
        }
    }
}

/// The fields of a [`CueFrame`], as bits: sample, at, loudness and muted, onsets, onset_at (NaN for none).
const WORDS: usize = 5;

struct Shared {
    /// Twice the number of frames written, plus one while a write is under way.
    version: AtomicU64,
    words: [AtomicU64; WORDS],
}

/// The writing end of a [`cue_mailbox`]. Owned by the extracting thread.
pub struct CueWriter {
    shared: Arc<Shared>,
}

/// The reading end of a [`cue_mailbox`]. Owned by the perform thread.
pub struct CueReader {
    shared: Arc<Shared>,
    seen: u64,
    onsets: u32,
}

/// A connected writer and reader: newest wins, wait-free, nothing allocates.
pub fn cue_mailbox() -> (CueWriter, CueReader) {
    let shared = Arc::new(Shared { version: AtomicU64::new(0), words: std::array::from_fn(|_| AtomicU64::new(0)) });
    (CueWriter { shared: Arc::clone(&shared) }, CueReader { shared, seen: 0, onsets: 0 })
}

fn pack(frame: &CueFrame) -> [u64; WORDS] {
    [
        frame.sample,
        frame.at.0.to_bits(),
        u64::from(frame.loudness.to_bits()) | u64::from(frame.muted) << 32,
        u64::from(frame.onsets),
        frame.onset_at.map_or(f64::NAN, |t| t.0).to_bits(),
    ]
}

#[allow(clippy::cast_possible_truncation)] // the low 32 bits are the loudness and the count
fn unpack(w: [u64; WORDS]) -> CueFrame {
    let onset_at = f64::from_bits(w[4]);
    CueFrame {
        sample: w[0],
        at: Seconds(f64::from_bits(w[1])),
        loudness: f32::from_bits(w[2] as u32),
        muted: w[2] >> 32 != 0,
        onsets: w[3] as u32,
        onset_at: (!onset_at.is_nan()).then_some(Seconds(onset_at)),
    }
}

impl CueWriter {
    /// Publishes `frame`, replacing one not read yet.
    pub fn publish(&mut self, frame: &CueFrame) {
        let s = &*self.shared;
        let v = s.version.load(Ordering::Relaxed);
        s.version.store(v + 1, Ordering::Relaxed);
        fence(Ordering::Release);
        for (slot, word) in s.words.iter().zip(pack(frame)) {
            slot.store(word, Ordering::Relaxed);
        }
        s.version.store(v + 2, Ordering::Release);
    }
}

impl CueReader {
    /// The newest frame published since the last call, or `None`. Never blocks, locks or allocates.
    pub fn poll(&mut self) -> Option<CueFrame> {
        let s = &*self.shared;
        for _ in 0..crate::mailbox::MAX_RETRIES {
            let before = s.version.load(Ordering::Acquire);
            if before == self.seen {
                return None;
            }
            if before % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let words: [u64; WORDS] = std::array::from_fn(|i| s.words[i].load(Ordering::Relaxed));
            fence(Ordering::Acquire);
            if s.version.load(Ordering::Relaxed) == before {
                self.seen = before;
                return Some(unpack(words));
            }
        }
        None
    }

    /// The newest frame as the life layer's cues, with `onset` set once per new onset, or `None` when nothing new
    /// arrived.
    pub fn cues(&mut self) -> Option<(CueFrame, Cues)> {
        let frame = self.poll()?;
        let cues = frame.cues(self.onsets);
        self.onsets = frame.onsets;
        Some((frame, cues))
    }

    /// Whether the writer is gone (its thread stopped).
    pub fn writer_gone(&self) -> bool {
        Arc::strong_count(&self.shared) == 1
    }
}

/// How often the listener reads the ring.
pub const POLL: Duration = Duration::from_millis(5);

/// Samples read per call: 20 ms at 48 kHz.
const CHUNK: usize = 1024;

/// The extractor on its own thread over the `cues` tap ring. Dropping it stops the thread.
pub struct CueListener {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl CueListener {
    /// Starts extracting from `reader` with `extractor` (made from the tap's info), publishing into `writer`. Cold.
    ///
    /// # Errors
    /// The OS refused a thread.
    pub fn start(mut reader: TapReader, mut extractor: CueExtractor, mut writer: CueWriter) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new().name("omuzaki-cues".into()).spawn(move || {
            let mut buf = [0.0f32; CHUNK];
            while !flag.load(Ordering::Relaxed) && !reader.writer_gone() {
                loop {
                    let run = reader.read(&mut buf);
                    if run.len == 0 {
                        break;
                    }
                    extractor.push(run.start_sample, run.muted, &buf[..run.len], |frame| writer.publish(frame));
                }
                std::thread::sleep(POLL);
            }
        })?;
        Ok(Self { stop, thread: Some(thread) })
    }

    /// Stops the thread and waits for it.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            // A panic on the thread has ended it already; there is nothing to recover here.
            let _ = thread.join();
        }
    }
}

impl Drop for CueListener {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_audio_api::clock::ClockSnapshot;

    use super::*;

    fn info(rate: u32) -> TapInfo {
        TapInfo { first_sample: 0, rate, capture_offset: Seconds(0.0), honours_mute: true }
    }

    fn clock() -> Arc<EngineClock> {
        Arc::new(EngineClock::new(ClockSnapshot::nominal(48_000)))
    }

    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn tone(seconds: f32, level: f32) -> Vec<f32> {
        (0..(seconds * 48_000.0) as usize)
            .map(|i| level * (2.0 * std::f32::consts::PI * 300.0 * i as f32 / 48_000.0).sin())
            .collect()
    }

    fn run(samples: &[f32]) -> Vec<CueFrame> {
        let mut cues = CueExtractor::new(&info(48_000), clock()).unwrap();
        let mut out = Vec::new();
        cues.push(0, false, samples, |f| out.push(*f));
        out
    }

    #[test]
    fn loudness_maps_minus_50_to_minus_10_dbfs() {
        // A sine's RMS is its peak less 3 dB.
        for (peak_db, want) in [(-60.0f32, 0.0), (-27.0, 0.5), (-7.0, 1.0)] {
            let level = 10f32.powf(peak_db / 20.0);
            let out = run(&tone(1.0, level));
            let last = out.last().unwrap().loudness;
            assert!((last - want).abs() < 0.03, "{peak_db} dB: {last}");
        }
    }

    #[test]
    fn loudness_rises_fast_and_falls_slow() {
        let mut samples = tone(0.5, 0.1);
        samples.extend(vec![0.0; 24_000]);
        let out = run(&samples);
        let crossing = |from: usize, above: bool| {
            out[from..].iter().position(|f| (f.loudness > 0.3) == above).map(|i| i + from).unwrap()
        };
        // Up within 50 ms of window and 30 ms of attack; down only after the window empties and 150 ms of release.
        let up = crossing(0, true);
        let down = crossing(50, false);
        assert!(up <= 8, "{up}");
        assert!((60..80).contains(&down), "{down}");
    }

    #[test]
    fn the_mailbox_turns_counts_into_one_step_onsets() {
        let (mut writer, mut reader) = cue_mailbox();
        assert!(reader.cues().is_none());
        let frame = CueFrame { onsets: 1, onset_at: Some(Seconds(1.5)), loudness: 0.4, ..CueFrame::default() };
        writer.publish(&frame);
        let (got, cues) = reader.cues().unwrap();
        assert_eq!(got, frame);
        assert!(cues.onset && (cues.loudness - 0.4).abs() < 1e-6);
        writer.publish(&CueFrame { sample: 480, ..frame });
        assert!(!reader.cues().unwrap().1.onset, "the same count is not a new onset");
        writer.publish(&CueFrame::default());
        assert_eq!(reader.poll().unwrap().onset_at, None);
    }
}
