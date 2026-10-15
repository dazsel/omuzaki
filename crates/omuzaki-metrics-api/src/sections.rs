//! The four sections of the snapshot: what each subsystem's writer publishes.
//!
//! Every number is a live value (a moving average, the last value or the worst in a one-second window) for a person or
//! an agent looking now. It is not the spec §10 gate statistic (p95 over five minutes on a named tier). A section holds
//! numbers and enum states only: no names, device ids or paths. Times are milliseconds unless the field says otherwise.
//! `at` is the app-clock time the section was published.
//!
//! Sections are `#[non_exhaustive]`: build one with `new(at)` and assign the fields, so a field added later edits only
//! this crate (ADR 0054 §4).

use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::words::{Get, Put};

/// Bit `i` of a section's flags word says its `i`-th optional field is present.
fn flag(set: bool, bit: u32) -> u64 {
    u64::from(set) << bit
}

/// The value of an optional field when its flag bit is set in `flags`.
fn present<T>(flags: u64, bit: u32, value: T) -> Option<T> {
    ((flags >> bit) & 1 == 1).then_some(value)
}

/// How the face painter's smoothed GPU time compares with its budget (`gpu.face_paint`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FacePressure {
    /// No measurement yet.
    #[default]
    Unknown,
    /// Over the face budget.
    Over,
    /// Under the budget but not by a wide margin.
    Within,
    /// Well under the budget: there is room for a larger atlas.
    Under,
}

impl FacePressure {
    fn code(self) -> u64 {
        match self {
            Self::Unknown => 0,
            Self::Over => 1,
            Self::Within => 2,
            Self::Under => 3,
        }
    }

    fn from_code(code: u64) -> Option<Self> {
        Some(match code {
            0 => Self::Unknown,
            1 => Self::Over,
            2 => Self::Within,
            3 => Self::Under,
            _ => return None,
        })
    }
}

/// Whether the performer sees a face.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TrackingState {
    /// The source has not delivered a frame yet.
    #[default]
    NoFrames,
    /// The newest frame is fresh and sees a face.
    Tracking,
    /// The face is lost, or no frame has come for a while.
    Lost,
}

impl TrackingState {
    fn code(self) -> u64 {
        match self {
            Self::NoFrames => 0,
            Self::Tracking => 1,
            Self::Lost => 2,
        }
    }

    fn from_code(code: u64) -> Option<Self> {
        Some(match code {
            0 => Self::NoFrames,
            1 => Self::Tracking,
            2 => Self::Lost,
            _ => return None,
        })
    }
}

/// Where the neutral capture stands.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CalibrationPhase {
    /// No capture has been asked for.
    #[default]
    Idle,
    /// Collecting tracked frames.
    Capturing,
    /// The last capture finished and its calibration is in use.
    Done,
    /// The last capture gave up.
    Failed,
}

impl CalibrationPhase {
    fn code(self) -> u64 {
        match self {
            Self::Idle => 0,
            Self::Capturing => 1,
            Self::Done => 2,
            Self::Failed => 3,
        }
    }

    fn from_code(code: u64) -> Option<Self> {
        Some(match code {
            0 => Self::Idle,
            1 => Self::Capturing,
            2 => Self::Done,
            3 => Self::Failed,
            _ => return None,
        })
    }
}

/// Words in a [`FrameMetrics`].
pub(crate) const FRAME_WORDS: usize = 17;

/// The frame loop's section: speed, frame time, late frames and the quality ladder. Written by the stage frame loop.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct FrameMetrics {
    /// When it was published (app clock).
    pub at: Seconds,
    /// Frames per second: `render()` returns in the last second.
    pub fps: f32,
    /// Smoothed wall interval between frame submits, ms.
    pub frame_ms: f32,
    /// The longest interval between frame submits in the last second, ms.
    pub frame_ms_worst_1s: f32,
    /// Smoothed CPU time of the passes, ms.
    pub cpu_ms: f32,
    /// Smoothed GPU time of the passes without the face paint, ms; `None` until the first valid GPU timing, and
    /// always `None` when the backend has none. Never a CPU time.
    pub gpu_ms: Option<f32>,
    /// GPU time of the face paint on frames that repainted, ms.
    pub face_paint_ms: Option<f32>,
    /// Draw calls in the last frame.
    pub draw_calls: u32,
    /// Triangles in the last frame.
    pub triangles: u64,
    /// Render target size in pixels, `[width, height]`.
    pub render_size: [u32; 2],
    /// Frames that took over 1.5 times the target interval, since start.
    pub late_frames: u64,
    /// The quality ladder rung in use (`0` is the highest quality).
    pub quality_rung: u32,
    /// Quality changes since start.
    pub quality_changes: u64,
    /// The face painter against its budget.
    pub face_pressure: FacePressure,
    /// Frames the output (window, virtual camera) did not take; `None` until one of them reports.
    pub output_dropped: Option<u64>,
    /// From the capture time of the tracker frame in use to the instant the stage finished submitting the frame that
    /// shows it, ms; `None` when no tracker frame is in use. This is not camera to photon: exposure and the display
    /// are not software-measurable.
    pub track_to_submit_ms: Option<f32>,
}

impl FrameMetrics {
    /// An all-zero section published at `at`.
    pub fn new(at: Seconds) -> Self {
        Self { at, ..Self::default() }
    }

    pub(crate) fn to_words(self) -> [u64; FRAME_WORDS] {
        let flags = flag(self.gpu_ms.is_some(), 0)
            | flag(self.face_paint_ms.is_some(), 1)
            | flag(self.output_dropped.is_some(), 2)
            | flag(self.track_to_submit_ms.is_some(), 3);
        let [width, height] = self.render_size;
        let mut put = Put::<FRAME_WORDS>::new();
        put.f64(self.at.0)
            .u64(flags)
            .f32(self.fps)
            .f32(self.frame_ms)
            .f32(self.frame_ms_worst_1s)
            .f32(self.cpu_ms)
            .f32(self.gpu_ms.unwrap_or_default())
            .f32(self.face_paint_ms.unwrap_or_default())
            .u64(u64::from(self.draw_calls))
            .u64(self.triangles)
            .u64(u64::from(width) << 32 | u64::from(height))
            .u64(self.late_frames)
            .u64(u64::from(self.quality_rung))
            .u64(self.quality_changes)
            .u64(self.face_pressure.code())
            .u64(self.output_dropped.unwrap_or_default())
            .f32(self.track_to_submit_ms.unwrap_or_default());
        put.finish()
    }

    /// `None` if a word is not one this version wrote.
    pub(crate) fn from_words(words: &[u64; FRAME_WORDS]) -> Option<Self> {
        let mut get = Get::new(words);
        let at = Seconds(get.f64());
        let flags = get.u64();
        let (fps, frame_ms, frame_ms_worst_1s, cpu_ms) = (get.f32()?, get.f32()?, get.f32()?, get.f32()?);
        let (gpu_ms, face_paint_ms) = (get.f32()?, get.f32()?);
        let (draw_calls, triangles) = (get.u32()?, get.u64());
        let size = get.u64();
        let (late_frames, quality_rung, quality_changes) = (get.u64(), get.u32()?, get.u64());
        let face_pressure = FacePressure::from_code(get.u64())?;
        let (output_dropped, track_to_submit_ms) = (get.u64(), get.f32()?);
        Some(Self {
            at,
            fps,
            frame_ms,
            frame_ms_worst_1s,
            cpu_ms,
            gpu_ms: present(flags, 0, gpu_ms),
            face_paint_ms: present(flags, 1, face_paint_ms),
            draw_calls,
            triangles,
            render_size: [u32::try_from(size >> 32).ok()?, u32::try_from(size & 0xFFFF_FFFF).ok()?],
            late_frames,
            quality_rung,
            quality_changes,
            face_pressure,
            output_dropped: present(flags, 2, output_dropped),
            track_to_submit_ms: present(flags, 3, track_to_submit_ms),
        })
    }
}

/// Words in a [`PerformMetrics`].
pub(crate) const PERFORM_WORDS: usize = 5;

/// The perform loop's section. Written by the perform thread.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct PerformMetrics {
    /// When it was published (app clock).
    pub at: Seconds,
    /// Tracker frames per second as the performer sees them (a moving average).
    pub source_fps: f32,
    /// Whether a face is tracked.
    pub tracking: TrackingState,
    /// Where the neutral capture stands.
    pub calibration: CalibrationPhase,
    /// Perform steps dropped when the loop fell behind, since start.
    pub skipped_steps: u64,
}

impl PerformMetrics {
    /// An all-zero section published at `at`.
    pub fn new(at: Seconds) -> Self {
        Self { at, ..Self::default() }
    }

    pub(crate) fn to_words(self) -> [u64; PERFORM_WORDS] {
        let mut put = Put::<PERFORM_WORDS>::new();
        put.f64(self.at.0)
            .f32(self.source_fps)
            .u64(self.tracking.code())
            .u64(self.calibration.code())
            .u64(self.skipped_steps);
        put.finish()
    }

    /// `None` if a word is not one this version wrote.
    pub(crate) fn from_words(words: &[u64; PERFORM_WORDS]) -> Option<Self> {
        let mut get = Get::new(words);
        Some(Self {
            at: Seconds(get.f64()),
            source_fps: get.f32()?,
            tracking: TrackingState::from_code(get.u64())?,
            calibration: CalibrationPhase::from_code(get.u64())?,
            skipped_steps: get.u64(),
        })
    }
}

/// Words in a [`TrackerMetrics`].
pub(crate) const TRACKER_WORDS: usize = 6;

/// The tracker's section: capture to publish, sampled at 10 Hz on the control thread that owns the tracker.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct TrackerMetrics {
    /// When it was published (app clock).
    pub at: Seconds,
    /// Median capture-to-publish latency, ms.
    pub track_ms_p50: f32,
    /// 95th percentile capture-to-publish latency, ms.
    pub track_ms_p95: f32,
    /// Worst capture-to-publish latency seen, ms.
    pub track_ms_max: f32,
    /// Tracker frames published since start.
    pub tracker_frames: u64,
    /// Camera frames skipped because the tracker was still busy, since start.
    pub tracker_dropped: u64,
}

impl TrackerMetrics {
    /// An all-zero section published at `at`.
    pub fn new(at: Seconds) -> Self {
        Self { at, ..Self::default() }
    }

    pub(crate) fn to_words(self) -> [u64; TRACKER_WORDS] {
        let mut put = Put::<TRACKER_WORDS>::new();
        put.f64(self.at.0)
            .f32(self.track_ms_p50)
            .f32(self.track_ms_p95)
            .f32(self.track_ms_max)
            .u64(self.tracker_frames)
            .u64(self.tracker_dropped);
        put.finish()
    }

    /// `None` if a word is not one this version wrote.
    pub(crate) fn from_words(words: &[u64; TRACKER_WORDS]) -> Option<Self> {
        let mut get = Get::new(words);
        Some(Self {
            at: Seconds(get.f64()),
            track_ms_p50: get.f32()?,
            track_ms_p95: get.f32()?,
            track_ms_max: get.f32()?,
            tracker_frames: get.u64(),
            tracker_dropped: get.u64(),
        })
    }
}

/// Words in an [`AudioMetrics`].
pub(crate) const AUDIO_WORDS: usize = 9;

/// The audio engine's section, condensed from its status at 4 Hz by the audio control thread. Sums only: no device or
/// consumer names.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct AudioMetrics {
    /// When it was published (app clock).
    pub at: Seconds,
    /// The declared latency of the heard route, ms.
    pub heard_latency_declared_ms: f32,
    /// The loopback-measured latency of the heard route, ms; `None` until a loopback has run.
    pub heard_latency_measured_ms: Option<f32>,
    /// The `mic` bus latency plus one block of the actual block size, ms.
    pub voice_fx_ms: f32,
    /// Bridge underruns, summed, since start.
    pub underruns: u64,
    /// Bridge overruns, summed, since start.
    pub overruns: u64,
    /// Tap blocks dropped, summed, since start.
    pub tap_drops: u64,
    /// Blocks the audio models did not finish in time, since start.
    pub model_blocks_missed: u64,
}

impl AudioMetrics {
    /// An all-zero section published at `at`.
    pub fn new(at: Seconds) -> Self {
        Self { at, ..Self::default() }
    }

    pub(crate) fn to_words(self) -> [u64; AUDIO_WORDS] {
        let mut put = Put::<AUDIO_WORDS>::new();
        put.f64(self.at.0)
            .u64(flag(self.heard_latency_measured_ms.is_some(), 0))
            .f32(self.heard_latency_declared_ms)
            .f32(self.heard_latency_measured_ms.unwrap_or_default())
            .f32(self.voice_fx_ms)
            .u64(self.underruns)
            .u64(self.overruns)
            .u64(self.tap_drops)
            .u64(self.model_blocks_missed);
        put.finish()
    }

    /// `None` if a word is not one this version wrote.
    pub(crate) fn from_words(words: &[u64; AUDIO_WORDS]) -> Option<Self> {
        let mut get = Get::new(words);
        let at = Seconds(get.f64());
        let flags = get.u64();
        let (declared, measured, voice_fx_ms) = (get.f32()?, get.f32()?, get.f32()?);
        Some(Self {
            at,
            heard_latency_declared_ms: declared,
            heard_latency_measured_ms: present(flags, 0, measured),
            voice_fx_ms,
            underruns: get.u64(),
            overruns: get.u64(),
            tap_drops: get.u64(),
            model_blocks_missed: get.u64(),
        })
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn ms() -> impl Strategy<Value = f32> {
        -1.0e6f32..1.0e6
    }

    fn opt_ms() -> impl Strategy<Value = Option<f32>> {
        proptest::option::of(ms())
    }

    fn seconds() -> impl Strategy<Value = Seconds> {
        (0.0f64..1.0e9).prop_map(Seconds)
    }

    fn pressure() -> impl Strategy<Value = FacePressure> {
        prop_oneof![
            Just(FacePressure::Unknown),
            Just(FacePressure::Over),
            Just(FacePressure::Within),
            Just(FacePressure::Under)
        ]
    }

    fn tracking() -> impl Strategy<Value = TrackingState> {
        prop_oneof![Just(TrackingState::NoFrames), Just(TrackingState::Tracking), Just(TrackingState::Lost)]
    }

    fn calibration() -> impl Strategy<Value = CalibrationPhase> {
        prop_oneof![
            Just(CalibrationPhase::Idle),
            Just(CalibrationPhase::Capturing),
            Just(CalibrationPhase::Done),
            Just(CalibrationPhase::Failed)
        ]
    }

    fn frame() -> impl Strategy<Value = FrameMetrics> {
        let timings = (seconds(), ms(), ms(), ms(), ms(), opt_ms(), opt_ms(), opt_ms());
        let counts = (any::<u32>(), any::<u64>(), any::<[u32; 2]>(), any::<u64>(), any::<u32>(), any::<u64>());
        (timings, counts, pressure(), proptest::option::of(any::<u64>())).prop_map(
            |((at, fps, frame_ms, worst, cpu, gpu, face, track), counts, face_pressure, output_dropped)| {
                let (draw_calls, triangles, render_size, late_frames, quality_rung, quality_changes) = counts;
                FrameMetrics {
                    at,
                    fps,
                    frame_ms,
                    frame_ms_worst_1s: worst,
                    cpu_ms: cpu,
                    gpu_ms: gpu,
                    face_paint_ms: face,
                    draw_calls,
                    triangles,
                    render_size,
                    late_frames,
                    quality_rung,
                    quality_changes,
                    face_pressure,
                    output_dropped,
                    track_to_submit_ms: track,
                }
            },
        )
    }

    proptest! {
        #[test]
        fn frame_round_trips(m in frame()) {
            prop_assert_eq!(FrameMetrics::from_words(&m.to_words()), Some(m));
        }

        #[test]
        fn perform_round_trips(
            at in seconds(), source_fps in ms(), tracking in tracking(), calibration in calibration(),
            skipped_steps in any::<u64>(),
        ) {
            let m = PerformMetrics { at, source_fps, tracking, calibration, skipped_steps };
            prop_assert_eq!(PerformMetrics::from_words(&m.to_words()), Some(m));
        }

        #[test]
        fn tracker_round_trips(
            at in seconds(), p50 in ms(), p95 in ms(), max in ms(), frames in any::<u64>(), dropped in any::<u64>(),
        ) {
            let m = TrackerMetrics {
                at, track_ms_p50: p50, track_ms_p95: p95, track_ms_max: max, tracker_frames: frames,
                tracker_dropped: dropped,
            };
            prop_assert_eq!(TrackerMetrics::from_words(&m.to_words()), Some(m));
        }

        #[test]
        fn audio_round_trips(
            at in seconds(), declared in ms(), measured in opt_ms(), fx in ms(),
            counts in any::<[u64; 4]>(),
        ) {
            let m = AudioMetrics {
                at, heard_latency_declared_ms: declared, heard_latency_measured_ms: measured, voice_fx_ms: fx,
                underruns: counts[0], overruns: counts[1], tap_drops: counts[2], model_blocks_missed: counts[3],
            };
            prop_assert_eq!(AudioMetrics::from_words(&m.to_words()), Some(m));
        }

        /// Floats come back bit for bit, including values `==` cannot compare.
        #[test]
        fn floats_keep_their_bits(bits in any::<u32>()) {
            let mut m = FrameMetrics::new(Seconds(1.0));
            m.fps = f32::from_bits(bits);
            let back = FrameMetrics::from_words(&m.to_words()).map(|b| b.fps.to_bits());
            prop_assert_eq!(back, Some(bits));
        }
    }

    #[test]
    fn a_word_this_version_did_not_write_reads_as_none() {
        let mut words = FrameMetrics::new(Seconds(1.0)).to_words();
        words[14] = 99; // face_pressure
        assert_eq!(FrameMetrics::from_words(&words), None);
        let mut words = PerformMetrics::new(Seconds(1.0)).to_words();
        words[1] = 1 << 40; // source_fps with a high half
        assert_eq!(PerformMetrics::from_words(&words), None);
    }
}
