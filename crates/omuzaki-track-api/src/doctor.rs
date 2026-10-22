//! The tracking doctor's contract (spec v3 PF-28): what a tracker can find wrong with its input, how it measures it,
//! and how a finding crosses from the tracker's thread to whoever shows it.
//!
//! | Finding | Measured | On when |
//! |---|---|---|
//! | [`LowLight`](Finding::LowLight) | the face's mean luma (the whole frame's with no face) | under 0.25 |
//! | [`Backlight`](Finding::Backlight) | how much brighter the background is than the face | over 0.25 |
//! | [`GlassesGlare`](Finding::GlassesGlare) | the share of near-white pixels around the eyes | over 3% |
//! | [`FaceNearEdge`](Finding::FaceNearEdge) | the face's distance from the frame's nearest edge | under 2% of the frame |
//! | [`DroppedFrames`](Finding::DroppedFrames) | the share of frames that never arrived or were skipped | over 5% |
//! | [`Overloaded`](Finding::Overloaded) | the tracker's latency, p95 | over the frame interval |
//! | [`Jitter`](Finding::Jitter) | the spread of the gaps between frames (a phone over Wi-Fi) | over 15 ms |
//!
//! [`Doctor`] holds the readings: each is averaged over about a second ([`AVERAGE_SECS`]) and each finding has
//! hysteresis (it clears only once the reading is back past [`RELEASE`] of its threshold), so a flicker of light or one
//! late packet does not make a diagnosis blink. The readings that need no pixels (drops, latency, arrival jitter) are
//! measured here, so every tracker can run them; the image readings live with the webcam tracker, which feeds
//! [`Doctor::record`]. The tracker's thread owns its doctor and publishes to a [`DoctorBoard`], which the
//! [`Tracker`](crate::tracker::Tracker) reads for [`diagnoses`](crate::tracker::Tracker::diagnoses) and asks for a
//! [`recheck`](crate::tracker::Tracker::recheck) through.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::tracker::LatencyStats;

/// How long readings are averaged over, in seconds.
pub const AVERAGE_SECS: f32 = 1.0;

/// A finding clears once its reading is back past this share of its threshold (hysteresis).
pub const RELEASE: f32 = 0.8;

/// What the doctor can find.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Finding {
    /// The face is too dark for the landmarks to hold.
    LowLight,
    /// A bright window or lamp behind the person leaves the face in shadow.
    Backlight,
    /// Light reflected on glasses hides the eyes.
    GlassesGlare,
    /// The face is at the edge of the picture or partly out of it.
    FaceNearEdge,
    /// Frames are lost before they reach the tracker.
    DroppedFrames,
    /// The machine cannot keep up: tracking takes longer than a frame.
    Overloaded,
    /// Frames arrive unevenly: a phone over a busy Wi-Fi network.
    Jitter,
}

/// How many findings there are.
pub const FINDINGS: usize = Finding::ALL.len();

impl Finding {
    /// Every finding, in report order.
    pub const ALL: [Self; 7] = [
        Self::LowLight,
        Self::Backlight,
        Self::GlassesGlare,
        Self::FaceNearEdge,
        Self::DroppedFrames,
        Self::Overloaded,
        Self::Jitter,
    ];

    /// Its stable id, for ops and settings (`low_light` …), the same as its serialised form.
    pub const fn id(self) -> &'static str {
        match self {
            Self::LowLight => "low_light",
            Self::Backlight => "backlight",
            Self::GlassesGlare => "glasses_glare",
            Self::FaceNearEdge => "face_near_edge",
            Self::DroppedFrames => "dropped_frames",
            Self::Overloaded => "overloaded",
            Self::Jitter => "jitter",
        }
    }

    /// The finding with this [`id`](Self::id).
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.id() == id)
    }

    /// Its place in [`ALL`](Self::ALL).
    pub const fn index(self) -> usize {
        self as usize
    }

    /// What to do about it, in plain words.
    pub const fn fix(self) -> &'static str {
        match self {
            Self::LowLight => "Your face is too dark. Turn on a lamp in front of you, or raise the camera's exposure.",
            Self::Backlight => {
                "A bright light is behind you. Close the curtain or move the lamp in front of you, facing you."
            }
            Self::GlassesGlare => {
                "Light is reflecting off your glasses. Move the lamp or screen higher or to the side, or tilt your \
                 glasses down a little."
            }
            Self::FaceNearEdge => "Your face is at the edge of the picture. Move to the middle, or turn the camera.",
            Self::DroppedFrames => {
                "Frames are being lost. Close other apps using the camera, use a USB port on the computer itself, or \
                 pick a lower resolution."
            }
            Self::Overloaded => {
                "Your computer can't keep up with tracking. Switch to a lighter tracking quality, or close other \
                 heavy apps."
            }
            Self::Jitter => {
                "The phone's frames arrive unevenly. Move closer to the Wi-Fi router, use 5 GHz Wi-Fi, or connect \
                 the computer by cable."
            }
        }
    }

    /// Whether a high reading is the bad side (otherwise a low one is).
    pub const fn high_is_bad(self) -> bool {
        !matches!(self, Self::LowLight | Self::FaceNearEdge)
    }
}

/// One thing the doctor found: what, how bad it measured and where the line is.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Diagnosis {
    /// What is wrong.
    pub finding: Finding,
    /// The averaged reading, in the finding's unit (a 0 to 1 share or luma, or milliseconds).
    pub measured: f32,
    /// The threshold it crossed, in the same unit.
    pub threshold: f32,
}

impl Diagnosis {
    /// `finding`, measured at `measured` against `threshold`.
    pub const fn new(finding: Finding, measured: f32, threshold: f32) -> Self {
        Self { finding, measured, threshold }
    }

    /// What to do about it.
    pub const fn fix(&self) -> &'static str {
        self.finding.fix()
    }
}

/// The thresholds, in each finding's unit (see the module table).
#[derive(Copy, Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct DoctorConfig {
    /// Thresholds in [`Finding::ALL`] order.
    pub thresholds: [f32; FINDINGS],
}

impl Default for DoctorConfig {
    fn default() -> Self {
        Self { thresholds: [0.25, 0.25, 0.03, 0.02, 0.05, 33.3, 15.0] }
    }
}

impl DoctorConfig {
    /// The threshold of `finding`.
    pub const fn threshold(&self, finding: Finding) -> f32 {
        self.thresholds[finding.index()]
    }

    /// With `finding`'s threshold set to `value`.
    #[must_use]
    pub const fn with_threshold(mut self, finding: Finding, value: f32) -> Self {
        self.thresholds[finding.index()] = value;
        self
    }
}

/// A reading averaged over [`AVERAGE_SECS`]: the first sample sets it, later ones move it by their share of time.
#[derive(Copy, Clone, Debug, Default)]
struct Average(Option<f32>);

impl Average {
    fn add(&mut self, value: f32, dt: f32) {
        if !value.is_finite() {
            return;
        }
        let dt = if dt.is_finite() { dt.clamp(0.0, AVERAGE_SECS) } else { 0.0 };
        self.0 = Some(match self.0 {
            None => value,
            Some(old) => old + (value - old) * (1.0 - (-dt / AVERAGE_SECS).exp()),
        });
    }
}

/// The doctor: feed it readings, the tracker's latency and frame arrivals; read its [`diagnoses`](Self::diagnoses).
/// Nothing in it allocates, so it runs on a tracker's thread.
#[derive(Clone, Debug)]
pub struct Doctor {
    config: DoctorConfig,
    readings: [Average; FINDINGS],
    active: [bool; FINDINGS],
    /// The last latency counters seen, to turn the totals into a rate.
    counters: Option<(u64, u64)>,
    /// The last frame arrival, and the averaged gap and squared deviation between arrivals, in seconds.
    arrival: Option<f64>,
    gap: Average,
    gap_variance: Average,
}

impl Default for Doctor {
    fn default() -> Self {
        Self::new(DoctorConfig::default())
    }
}

impl Doctor {
    /// A doctor with these thresholds, having seen nothing.
    pub fn new(config: DoctorConfig) -> Self {
        Self {
            config,
            readings: [Average::default(); FINDINGS],
            active: [false; FINDINGS],
            counters: None,
            arrival: None,
            gap: Average::default(),
            gap_variance: Average::default(),
        }
    }

    /// Adds one reading of `finding`, `dt` seconds after its last one, in the finding's unit, and turns the finding on
    /// or off. The webcam tracker's image readings come in through here.
    pub fn record(&mut self, finding: Finding, value: f32, dt: f32) {
        let i = finding.index();
        self.readings[i].add(value, dt);
        let Some(reading) = self.readings[i].0 else { return };
        let threshold = self.config.thresholds[i];
        let (on, off) = if finding.high_is_bad() {
            (reading > threshold, reading < threshold * RELEASE)
        } else {
            (reading < threshold, reading > threshold / RELEASE)
        };
        if on {
            self.active[i] = true;
        } else if off {
            self.active[i] = false;
        }
    }

    /// Reads the tracker's counters, `dt` seconds after the last time: the share of frames dropped since then
    /// ([`observe_drops`](Self::observe_drops)), and its p95 against `frame_interval` (the camera's, or the frame rate
    /// the stage asks for).
    pub fn observe_latency(&mut self, stats: &LatencyStats, frame_interval: Duration, dt: f32) {
        self.observe_drops(stats, dt);
        // The threshold follows the frame interval, so a 60 fps camera is held to 16.7 ms.
        let interval_ms = frame_interval.as_secs_f32() * 1e3;
        if interval_ms > 0.0 {
            self.config.thresholds[Finding::Overloaded.index()] = interval_ms;
        }
        self.record(Finding::Overloaded, stats.p95.as_secs_f32() * 1e3, dt);
    }

    /// Reads only the share of frames dropped since the last read, `dt` seconds ago: for a tracker whose latency is
    /// the network's, not the machine's (a phone), where [`Overloaded`](Finding::Overloaded)'s fix would mislead.
    #[allow(clippy::cast_precision_loss)] // frame counts between two reads are small
    pub fn observe_drops(&mut self, stats: &LatencyStats, dt: f32) {
        if let Some((frames, dropped)) = self.counters {
            let (new_frames, new_dropped) =
                (stats.frames.saturating_sub(frames), stats.dropped.saturating_sub(dropped));
            let total = new_frames + new_dropped;
            if total > 0 {
                self.record(Finding::DroppedFrames, new_dropped as f32 / total as f32, dt);
            }
        }
        self.counters = Some((stats.frames, stats.dropped));
    }

    /// Notes a frame arriving at `time` seconds (any clock, increasing): the spread of the gaps is the jitter.
    #[allow(clippy::cast_possible_truncation)] // gaps between frames are far inside f32
    pub fn observe_arrival(&mut self, time: f64) {
        if let Some(last) = self.arrival.replace(time) {
            let gap = (time - last) as f32;
            if gap <= 0.0 || !gap.is_finite() {
                return;
            }
            let mean = self.gap.0.unwrap_or(gap);
            self.gap.add(gap, gap);
            self.gap_variance.add((gap - mean) * (gap - mean), gap);
            let jitter_ms = self.gap_variance.0.unwrap_or(0.0).sqrt() * 1e3;
            self.record(Finding::Jitter, jitter_ms, gap);
        }
    }

    /// What it finds now, in [`Finding::ALL`] order.
    pub fn diagnoses(&self) -> impl Iterator<Item = Diagnosis> + '_ {
        Finding::ALL.into_iter().filter(|f| self.active[f.index()]).filter_map(|finding| {
            let measured = self.readings[finding.index()].0?;
            Some(Diagnosis { finding, measured, threshold: self.config.threshold(finding) })
        })
    }

    /// The averaged reading of `finding`, found or not; `None` before anything was measured for it.
    pub fn reading(&self, finding: Finding) -> Option<f32> {
        self.readings[finding.index()].0
    }

    /// Forgets every reading and finding, to measure again from now (the "recheck" after a fix).
    pub fn recheck(&mut self) {
        *self = Self::new(self.config);
    }
}

/// Where a tracker's thread leaves its doctor's findings, and where a recheck is asked for: the thread
/// [`publish`](Self::publish)es with a `try_lock` (it never waits, and a reader holding the lock only delays the update
/// by a frame) and takes the recheck flag; the [`Tracker`](crate::tracker::Tracker) reads and asks from any thread.
/// Nothing is allocated on the thread's side.
#[derive(Debug, Default)]
pub struct DoctorBoard {
    findings: Mutex<[Option<Diagnosis>; FINDINGS]>,
    recheck: AtomicBool,
}

impl DoctorBoard {
    /// Leaves `doctor`'s findings here, unless a reader holds the board this instant. Hot path: never waits or
    /// allocates.
    pub fn publish(&self, doctor: &Doctor) {
        let Ok(mut findings) = self.findings.try_lock() else { return };
        *findings = [None; FINDINGS];
        for diagnosis in doctor.diagnoses() {
            findings[diagnosis.finding.index()] = Some(diagnosis);
        }
    }

    /// The findings last published, in [`Finding::ALL`] order.
    pub fn diagnoses(&self) -> Vec<Diagnosis> {
        self.findings.lock().unwrap_or_else(PoisonError::into_inner).iter().flatten().copied().collect()
    }

    /// Asks the tracker's thread to [`Doctor::recheck`], and clears the findings shown until it measures again.
    pub fn request_recheck(&self) {
        *self.findings.lock().unwrap_or_else(PoisonError::into_inner) = [None; FINDINGS];
        self.recheck.store(true, Ordering::Relaxed);
    }

    /// Whether a recheck was asked for since the last call; the tracker's thread calls it once a frame.
    pub fn take_recheck(&self) -> bool {
        self.recheck.swap(false, Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(doctor: &Doctor) -> Vec<Finding> {
        doctor.diagnoses().map(|d| d.finding).collect()
    }

    #[test]
    fn dropped_frames_and_a_slow_tracker_come_from_the_counters() {
        let mut doctor = Doctor::default();
        let interval = Duration::from_secs_f32(1.0 / 30.0);
        let ms = Duration::from_millis;
        doctor.observe_latency(&LatencyStats::new(0, 0, ms(8), ms(12), ms(20)), interval, 1.0);
        doctor.observe_latency(&LatencyStats::new(30, 0, ms(8), ms(12), ms(20)), interval, 1.0);
        assert_eq!(found(&doctor), []);
        // A second of drops and slow frames, twice: the average needs more than one bad reading.
        doctor.observe_latency(&LatencyStats::new(50, 10, ms(30), ms(45), ms(60)), interval, 1.0);
        doctor.observe_latency(&LatencyStats::new(70, 20, ms(30), ms(45), ms(60)), interval, 1.0);
        assert_eq!(found(&doctor), [Finding::DroppedFrames, Finding::Overloaded]);
        // A 60 fps camera is held to its own frame interval.
        let mut doctor = Doctor::default();
        doctor.observe_latency(
            &LatencyStats::new(0, 0, ms(15), ms(20), ms(25)),
            Duration::from_secs_f32(1.0 / 60.0),
            1.0,
        );
        assert_eq!(found(&doctor), [Finding::Overloaded]);
    }

    #[test]
    fn uneven_arrivals_are_jitter_and_even_ones_are_not() {
        let mut steady = Doctor::default();
        let mut uneven = Doctor::default();
        let mut t = 0.0;
        for i in 0..120 {
            steady.observe_arrival(f64::from(i) / 60.0);
            t += if i % 3 == 0 { 0.06 } else { 0.005 };
            uneven.observe_arrival(t);
        }
        assert_eq!(found(&steady), []);
        assert_eq!(found(&uneven), [Finding::Jitter]);
        assert!(uneven.reading(Finding::Jitter).unwrap() > 15.0);
    }

    #[test]
    fn a_low_reading_is_bad_for_light_and_clears_with_hysteresis() {
        let mut doctor = Doctor::default();
        doctor.record(Finding::LowLight, 0.1, 1.0);
        assert_eq!(found(&doctor), [Finding::LowLight]);
        // Back over the threshold but not past the release (0.25 / 0.8): still found.
        doctor.record(Finding::LowLight, 0.3, 1.0);
        doctor.record(Finding::LowLight, 0.3, 1.0);
        assert_eq!(found(&doctor), [Finding::LowLight]);
        for _ in 0..5 {
            doctor.record(Finding::LowLight, 0.5, 1.0);
        }
        assert_eq!(found(&doctor), []);
        doctor.record(Finding::LowLight, 0.1, 1.0);
        doctor.recheck();
        assert_eq!(found(&doctor), []);
        assert_eq!(doctor.reading(Finding::LowLight), None);
    }

    #[test]
    fn ids_round_trip_serialise_as_themselves_and_every_finding_has_a_fix() {
        for (i, f) in Finding::ALL.into_iter().enumerate() {
            assert_eq!(f.index(), i);
            assert_eq!(Finding::from_id(f.id()), Some(f));
            assert_eq!(serde_json::to_value(f).unwrap(), f.id());
            assert!(f.fix().ends_with('.'));
        }
    }

    #[test]
    fn the_board_shows_what_was_published_and_a_recheck_clears_it_once() {
        let board = DoctorBoard::default();
        let mut doctor = Doctor::default();
        doctor.record(Finding::Backlight, 0.6, 1.0);
        doctor.record(Finding::Jitter, 40.0, 1.0);
        board.publish(&doctor);
        let shown: Vec<_> = board.diagnoses().into_iter().map(|d| d.finding).collect();
        assert_eq!(shown, [Finding::Backlight, Finding::Jitter]);
        assert!(!board.take_recheck());
        board.request_recheck();
        assert_eq!(board.diagnoses(), []);
        assert!(board.take_recheck());
        assert!(!board.take_recheck());
    }
}
