//! The spec §10 budget rows the live record is compared with, and [`BudgetUse`], one derived row.
//!
//! The constants cover the rows that have no constant in a runtime crate. `GPU_FRAME` and `GPU_FACE_PAINT` repeat
//! `omuzaki-render-api`'s `budget::LIVE_GPU` and `FACE_PAINT_GPU`, which this crate cannot depend on; a test in
//! `omuzaki-test` checks every constant here, and that pair, against the spec table so the copies cannot drift.
//!
//! Every value in the record is live (a moving average, the last value or a one-second worst), not the §10 gate
//! statistic. Consumers print the word `live` beside each row.

use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::sections::{AudioMetrics, FrameMetrics, TrackerMetrics};

/// `gpu.frame`: the live GPU frame, 1.5 ms.
pub const GPU_FRAME: Seconds = Seconds(0.0015);
/// `gpu.face_paint`: the face paint on the GPU, 0.5 ms.
pub const GPU_FACE_PAINT: Seconds = Seconds(0.0005);
/// `track.face`: tracking one face, 5 ms.
pub const TRACK_FACE: Seconds = Seconds(0.005);
/// `voice.fx`: the voice effects chain, 10 ms.
pub const VOICE_FX: Seconds = Seconds(0.010);
/// `latency.laptop`: capture to photon on a laptop, 80 ms.
pub const LATENCY_LAPTOP: Seconds = Seconds(0.080);
/// `physics.step`: one physics step, 0.5 ms.
pub const PHYSICS_STEP: Seconds = Seconds(0.0005);
/// `cpu.total`: 3 % of all logical cores, as a fraction (the same base as a `cpu_share` reading).
pub const CPU_TOTAL_SHARE: f32 = 0.03;
/// `ram.runtime`: 400 MB.
pub const RAM_RUNTIME_MB: u32 = 400;

/// Rows [`derive`] can fill from the sections today; sizes its vector.
const DERIVED_ROWS: usize = 5;

/// A §10 row, by its spec key.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum BudgetKey {
    /// `gpu.frame`
    #[serde(rename = "gpu.frame")]
    GpuFrame,
    /// `gpu.face_paint`
    #[serde(rename = "gpu.face_paint")]
    GpuFacePaint,
    /// `track.face`
    #[serde(rename = "track.face")]
    TrackFace,
    /// `voice.fx`
    #[serde(rename = "voice.fx")]
    VoiceFx,
    /// `latency.laptop`
    #[serde(rename = "latency.laptop")]
    LatencyLaptop,
    /// `physics.step`
    #[serde(rename = "physics.step")]
    PhysicsStep,
    /// `cpu.total`
    #[serde(rename = "cpu.total")]
    CpuTotal,
    /// `ram.runtime`
    #[serde(rename = "ram.runtime")]
    RamRuntime,
}

/// One live value against its budget. `value` and `budget` share the key's unit: milliseconds for the time rows, a
/// fraction of all logical cores for `cpu.total`, megabytes for `ram.runtime`.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct BudgetUse {
    /// Which row.
    pub key: BudgetKey,
    /// The live reading.
    pub value: f64,
    /// The budget.
    pub budget: f64,
    /// `value / budget`; over `1.0` is over budget.
    pub ratio: f64,
    /// The reading covers only part of what the row measures (`latency.laptop` lacks exposure and the display).
    pub partial: bool,
}

impl BudgetUse {
    /// A row with `ratio` computed.
    pub fn new(key: BudgetKey, value: f64, budget: f64, partial: bool) -> Self {
        Self { key, value, budget, ratio: value / budget, partial }
    }

    fn time(key: BudgetKey, value_ms: f32, budget: Seconds, partial: bool) -> Self {
        Self::new(key, f64::from(value_ms), budget.0 * 1000.0, partial)
    }
}

/// The rows the sections can fill now, in a fixed order. A row whose reading is absent is left out.
pub(crate) fn derive(
    frame: Option<&FrameMetrics>,
    tracker: Option<&TrackerMetrics>,
    audio: Option<&AudioMetrics>,
) -> Vec<BudgetUse> {
    if frame.is_none() && tracker.is_none() && audio.is_none() {
        return Vec::new();
    }
    let mut rows = Vec::with_capacity(DERIVED_ROWS);
    if let Some(f) = frame {
        if let Some(v) = f.gpu_ms {
            rows.push(BudgetUse::time(BudgetKey::GpuFrame, v, GPU_FRAME, false));
        }
        if let Some(v) = f.face_paint_ms {
            rows.push(BudgetUse::time(BudgetKey::GpuFacePaint, v, GPU_FACE_PAINT, false));
        }
    }
    if let Some(t) = tracker {
        // The 95th percentile includes capture and convert, so it slightly over-estimates `track.face`.
        rows.push(BudgetUse::time(BudgetKey::TrackFace, t.track_ms_p95, TRACK_FACE, false));
    }
    if let Some(a) = audio {
        rows.push(BudgetUse::time(BudgetKey::VoiceFx, a.voice_fx_ms, VOICE_FX, false));
    }
    if let Some(v) = frame.and_then(|f| f.track_to_submit_ms) {
        rows.push(BudgetUse::time(BudgetKey::LatencyLaptop, v, LATENCY_LAPTOP, true));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_in_ms_with_a_ratio() {
        let row = BudgetUse::time(BudgetKey::TrackFace, 2.5, TRACK_FACE, false);
        assert!((row.budget - 5.0).abs() < 1e-9);
        assert!((row.ratio - 0.5).abs() < 1e-9);
    }

    #[test]
    fn only_rows_with_a_reading_appear() {
        assert_eq!(derive(None, None, None), Vec::new());
        let mut frame = FrameMetrics::new(Seconds(1.0));
        assert!(derive(Some(&frame), None, None).is_empty(), "no gpu_ms, no track_to_submit_ms");
        frame.gpu_ms = Some(3.0);
        frame.track_to_submit_ms = Some(40.0);
        let rows = derive(Some(&frame), None, None);
        let keys: Vec<_> = rows.iter().map(|r| r.key).collect();
        assert_eq!(keys, [BudgetKey::GpuFrame, BudgetKey::LatencyLaptop]);
        assert!((rows[0].ratio - 2.0).abs() < 1e-6, "3 ms against 1.5 ms");
        assert!(!rows[0].partial && rows[1].partial, "only the latency row is partial");
    }

    #[test]
    fn keys_serialise_as_the_spec_keys() {
        let json = serde_json::to_string(&BudgetKey::GpuFacePaint).expect("serialises");
        assert_eq!(json, "\"gpu.face_paint\"");
    }
}
