//! The query op `measure.liveliness`: how alive a life-layer run or a recorded track looks, against a baseline.

use std::io::Read;

use omuzaki_anim_api::{
    ARKIT_COUNT, Bus, Channel, ChannelBuf, Cues, LOOK_FULL_RAD, LifeConfig, LifeLayer, Personality, Tick, TrackFrame,
};
use omuzaki_ops_api::{FsMode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Baseline, liveliness};
use crate::life::Life;

/// Frame rate of a simulated life run: the rate the app renders at.
const LIFE_FPS: f32 = 60.0;
/// Longest simulated run, seconds. A minute of 60 fps data measures in under a millisecond, so this is generous; it
/// only stops a typo from asking for days.
const MAX_SECONDS: f32 = 3600.0;
/// Largest recording read, bytes.
const MAX_RECORDING_BYTES: u64 = 64 << 20;

/// Where the frames to measure come from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Run the life layer on a still face for a while.
    Life {
        /// The layer's seed.
        seed: u64,
        /// How long to run, seconds (at 60 fps).
        seconds: f32,
        /// The personality preset; natural when left out.
        #[serde(default)]
        personality: Option<Personality>,
    },
    /// A recorded track (the `RecordedTrack` JSON of the anim mock) at this project path.
    Recording(String),
}

/// Measure how alive a face looks: change per second, blinks, saccades and the longest frozen window, each judged low,
/// ok or high against a baseline (`rest`, `talking`, `reading`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MeasureLiveliness {
    /// The frames to measure.
    pub source: Source,
    /// The built-in baseline to compare with.
    pub baseline: String,
}

/// A still face under the life layer, `seconds` long.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 0 < seconds <= 3600: the frame count fits
fn run_life(seed: u64, seconds: f32, personality: Option<Personality>) -> Vec<ChannelBuf> {
    let config = personality.map_or_else(|| LifeConfig::new(seed), |p| LifeConfig::new(seed).with_personality(p));
    let mut life = Life::new(config);
    let frames = (seconds * LIFE_FPS).round() as u64;
    (0..frames)
        .map(|i| {
            let mut bus = Bus::default();
            life.step(Tick::at_rate(i, LIFE_FPS), &Cues::default(), &mut bus);
            bus.values
        })
        .collect()
}

/// The recorded-track file: the frame rate and the frames (the anim mock's `RecordedTrack`, read here without the mock).
#[derive(Deserialize)]
struct Recording {
    fps: f32,
    frames: Vec<TrackFrame>,
}

/// One tracker frame as channel values, raw (uncalibrated): the blendshapes it supplies, the head pose and position, and gaze
/// angles when it has no `eyeLook*` blendshapes.
fn channels_of(frame: &TrackFrame) -> ChannelBuf {
    let mut buf = ChannelBuf::NEUTRAL;
    for c in frame.present {
        if c.index() < ARKIT_COUNT {
            buf.set(c, frame.blend[c.index()]);
        }
    }
    if let Some(head) = frame.head {
        buf.set(Channel::HEAD_YAW, head.yaw);
        buf.set(Channel::HEAD_PITCH, head.pitch);
        buf.set(Channel::HEAD_ROLL, head.roll);
        // Camera-frame metres: `x` toward image right, `y` up, as `headShiftX/Y` read them before any mirroring.
        // `z` (lean) is a body channel and is not measured.
        if let Some(p) = head.position {
            buf.set(Channel::HEAD_SHIFT_X, p.x);
            buf.set(Channel::HEAD_SHIFT_Y, p.y);
        }
    }
    let has_look = frame.present.intersects(omuzaki_anim_api::ChannelMask::GAZE);
    if let (Some(eyes), false) = (frame.eyes, has_look) {
        // Right and down are positive in the angles; look units are positive toward the character's left and up.
        let x = f32::midpoint(eyes.left_yaw, eyes.right_yaw) / LOOK_FULL_RAD;
        let y = -f32::midpoint(eyes.left_pitch, eyes.right_pitch) / LOOK_FULL_RAD;
        buf.set_gaze(x, y);
    }
    buf
}

fn read_recording(ctx: &OpCtx<'_>, path: &str) -> Result<(Vec<ChannelBuf>, f32), OpError> {
    let mut text = String::new();
    ctx.fs_access(path, FsMode::Read)?
        .open_read()?
        .take(MAX_RECORDING_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|e| OpError::invalid("source.recording", format!("`{path}` cannot be read as text: {e}")))?;
    if text.len() as u64 > MAX_RECORDING_BYTES {
        return Err(OpError::invalid("source.recording", format!("`{path}` is larger than 64 MiB")));
    }
    let rec: Recording = serde_json::from_str(&text)
        .map_err(|e| OpError::invalid("source.recording", format!("`{path}` is not a recorded track: {e}")))?;
    if !rec.fps.is_finite() || rec.fps <= 0.0 {
        return Err(OpError::invalid("source.recording", "the track's fps must be positive"));
    }
    Ok((rec.frames.iter().map(channels_of).collect(), 1.0 / rec.fps))
}

impl Op for MeasureLiveliness {
    fn meta() -> OpMeta {
        OpMeta {
            name: "measure.liveliness",
            summary: "Measure change per second, blink rate, saccade rate and the longest frozen window of a life-layer run or a recording, against a baseline.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let baseline = Baseline::builtin(&self.baseline).map_err(|e| OpError::invalid("baseline", e.to_string()))?;
        let (frames, dt) = match &self.source {
            Source::Life { seed, seconds, personality } => {
                if !(seconds.is_finite() && *seconds > 0.0 && *seconds <= MAX_SECONDS) {
                    return Err(OpError::invalid("source.life.seconds", "must be above 0 and at most 3600"));
                }
                (run_life(*seed, *seconds, *personality), 1.0 / LIFE_FPS)
            }
            Source::Recording(path) => read_recording(ctx, path)?,
        };
        let measured = liveliness(&frames, dt);
        let verdict = baseline.compare(&measured);
        Ok(Plan::answer(json!({ "baseline": baseline.name, "measured": measured, "verdict": verdict })))
    }
}

omuzaki_ops_api::register_op!(MeasureLiveliness);
