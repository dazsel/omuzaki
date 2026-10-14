//! Frame data: the clock tick every stage receives, and the normalised frame a tracker hands to the pipeline.
//!
//! [`TrackFrame`] is the Rust form of the prototype bridge's "normalised frame (contract v1)": whatever the source
//! (webcam model, iPhone app, VMC, a recording), the pipeline sees the same plain, fixed-size, `Copy` value. Trackers
//! convert their sender's conventions *before* building a frame, so nothing downstream knows which protocol it came
//! from. The cross-thread hand-off ("newest frame wins, never queue", spec §8) is the tracker crate's job; this type is
//! what makes it cheap: 300 bytes of plain data, no heap, nothing to free.

use std::collections::BTreeMap;

use omuzaki_core::glam::Vec3;
use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::channel::{ARKIT_COUNT, Channel, ChannelMask};

/// When the current frame happens and how long the previous one took.
///
/// Live, `dt` is the measured frame time; in tests and offline renders it is `1 / fps` exactly, which is what makes a
/// replay bit-identical (spec §4 "deterministic by default").
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Tick {
    /// Frame number, from 0.
    pub index: u64,
    /// Time of this frame on the pipeline clock. Systems that need a smooth noise clock read this; their own timers
    /// advance by `dt`, so scrubbing `time` never derails them.
    pub time: Seconds,
    /// Seconds since the previous frame. Stages clamp it themselves (the life layer caps it at 0.25 s).
    pub dt: f32,
}

impl Tick {
    /// Frame `index` of a fixed-rate run at `fps`.
    #[allow(clippy::cast_precision_loss)] // frame counts stay far below 2^53
    pub fn at_rate(index: u64, fps: f32) -> Self {
        Self { index, time: Seconds(index as f64 / f64::from(fps)), dt: 1.0 / fps }
    }
}

/// How a source's raw numbers should be read when the user hasn't calibrated yet.
///
/// Webcam models report subtle values on a human face (a wide-open eye rarely exceeds 0.2); phone depth sensors
/// report the full `ARKit` range. The retarget picks its default [`Calibration`](crate::Calibration) from this.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DeviceProfile {
    /// Already `ARKit`-scaled: an iPhone depth camera, or an app that relays one (`iFacialMocap`, Live Link Face, `VTube Studio`).
    Arkit,
    /// A webcam landmark model (`MediaPipe` class): the same channels, but reads low.
    Webcam,
}

/// Whether the tracker currently sees a face.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrackStatus {
    /// A face is tracked; the values are valid.
    Tracking,
    /// The face is lost (covered, left frame). Values are stale; the retarget eases to rest and hands over to the
    /// life layer after its grace period (spec PF-23).
    Lost,
}

/// Head pose in the **performer's camera frame**, radians and metres.
///
/// Conventions (identical to the prototype bridge): `yaw` is positive when the nose turns toward the image's right,
/// `pitch` positive when the nose turns down, `roll` positive counter-clockwise in the image. For a performer facing
/// the camera that is the character's own yaw/pitch/roll in the bus convention (see the crate docs), so an unmirrored
/// performer maps straight through; mirrored previews flip yaw and roll (the retarget's `mirror` setting).
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HeadPose {
    /// Turn left/right, radians.
    pub yaw: f32,
    /// Nod up/down, radians.
    pub pitch: f32,
    /// Tilt toward a shoulder, radians.
    pub roll: f32,
    /// Head position in the camera frame, metres (`x` toward the image's right, `y` up, `z` toward the camera); `None`
    /// if the source has no depth. Lean-in is derived from `z`.
    #[serde(default)]
    #[schemars(with = "Option<[f32; 3]>")]
    pub position: Option<Vec3>,
}

/// Eye rotations in radians, same sign convention as [`HeadPose`]. Sources that send gaze as angles rather than the
/// eight `eyeLook*` blendshapes fill this; the retarget converts it to those channels.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EyeAngles {
    /// Left eye, toward image right is positive.
    pub left_yaw: f32,
    /// Left eye, down is positive.
    pub left_pitch: f32,
    /// Right eye, toward image right is positive.
    pub right_yaw: f32,
    /// Right eye, down is positive.
    pub right_pitch: f32,
}

/// One normalised tracker sample. `Copy`, fixed-size, allocation-free (spec §8 real-time plane).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct TrackFrame {
    /// Sender sequence number; gaps mean dropped frames (feeds the live health meter, PF-26).
    pub seq: u64,
    /// Capture time on the pipeline clock (the receiver maps the sender's clock onto it).
    pub time: Seconds,
    /// How to read the raw numbers if there is no calibration.
    pub profile: DeviceProfile,
    /// Face found or lost.
    pub status: TrackStatus,
    /// The 52 `ARKit` blendshapes, raw (uncalibrated), in channel order; entries not in `present` are `0.0`.
    pub blend: [f32; ARKIT_COUNT],
    /// Which blendshapes this source actually supplies. Webcam models send 51 (no `tongueOut`); the retarget must not
    /// treat an absent channel as "measured zero" when it blends sources (spec PF-23).
    pub present: ChannelMask,
    /// Head pose, if the source has one.
    pub head: Option<HeadPose>,
    /// Gaze angles, if the source sends them.
    pub eyes: Option<EyeAngles>,
}

impl TrackFrame {
    /// A tracked frame with every blendshape at zero and nothing present: a base for builders and tests.
    pub fn empty(seq: u64, time: Seconds, profile: DeviceProfile) -> Self {
        Self {
            seq,
            time,
            profile,
            status: TrackStatus::Tracking,
            blend: [0.0; ARKIT_COUNT],
            present: ChannelMask::EMPTY,
            head: None,
            eyes: None,
        }
    }

    /// Sets an `ARKit` blendshape and marks it present. Non-`ARKit` channels are ignored (a tracker cannot supply them).
    pub fn set(&mut self, channel: Channel, value: f32) {
        if channel.is_arkit() {
            self.blend[channel.index()] = value;
            self.present.insert(channel);
        }
    }

    /// A blendshape's raw value (`0.0` if absent or not an `ARKit` channel).
    pub fn get(&self, channel: Channel) -> f32 {
        if channel.is_arkit() { self.blend[channel.index()] } else { 0.0 }
    }

    /// A lost-face frame at `time`.
    pub fn lost(seq: u64, time: Seconds, profile: DeviceProfile) -> Self {
        Self { status: TrackStatus::Lost, ..Self::empty(seq, time, profile) }
    }
}

/// The input port of the pipeline: the freshest tracker frame, without ever blocking or queueing.
///
/// Real trackers (`omuzaki-track-*`) implement it over a lock-free mailbox; the mock implements it over a recording.
pub trait FrameSource: Send {
    /// The newest frame that arrived since the last call, or `None` if there is nothing new. Must not block, and when
    /// several frames arrived it returns only the newest (the older ones are dropped, never queued).
    fn poll(&mut self, now: Seconds) -> Option<TrackFrame>;
}

/// The at-rest form of a [`TrackFrame`]: `ARKit` values by name (only the present ones), so recorded tracks are
/// readable, diffable, and survive a reordering of the table.
#[derive(Serialize, Deserialize, JsonSchema)]
struct TrackFrameFile {
    seq: u64,
    time: Seconds,
    profile: DeviceProfile,
    status: TrackStatus,
    #[serde(default)]
    blend: BTreeMap<Channel, f32>,
    #[serde(default)]
    head: Option<HeadPose>,
    #[serde(default)]
    eyes: Option<EyeAngles>,
}

/// A recorded blendshape name that isn't one of the 52 `ARKit` channels.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0} is not an ARKit blendshape, so a tracker cannot supply it")]
pub struct NotArkit(pub Channel);

impl From<TrackFrame> for TrackFrameFile {
    fn from(f: TrackFrame) -> Self {
        Self {
            seq: f.seq,
            time: f.time,
            profile: f.profile,
            status: f.status,
            blend: f.present.iter().map(|c| (c, f.blend[c.index()])).collect(),
            head: f.head,
            eyes: f.eyes,
        }
    }
}

impl TryFrom<TrackFrameFile> for TrackFrame {
    type Error = NotArkit;
    fn try_from(file: TrackFrameFile) -> Result<Self, NotArkit> {
        let mut frame = Self::empty(file.seq, file.time, file.profile);
        frame.status = file.status;
        for (channel, value) in file.blend {
            if !channel.is_arkit() {
                return Err(NotArkit(channel));
            }
            frame.set(channel, value);
        }
        frame.head = file.head;
        frame.eyes = file.eyes;
        Ok(frame)
    }
}

impl Serialize for TrackFrame {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        TrackFrameFile::from(*self).serialize(s)
    }
}

impl<'de> Deserialize<'de> for TrackFrame {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        TrackFrameFile::deserialize(d)?.try_into().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for TrackFrame {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "TrackFrame".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        TrackFrameFile::json_schema(generator)
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_precision_loss, clippy::cast_possible_truncation)] // tests compare values that are copied or exactly computed, and cast small loop counters
mod tests {
    use super::*;

    #[test]
    fn fixed_rate_ticks_are_exact() {
        let t = Tick::at_rate(90, 30.0);
        assert_eq!(t.time, Seconds(3.0));
        assert!((t.dt - 1.0 / 30.0).abs() < f32::EPSILON);
    }

    #[test]
    fn frames_are_copy_and_small() {
        fn assert_copy<T: Copy + Send>() {}
        assert_copy::<TrackFrame>();
        assert!(std::mem::size_of::<TrackFrame>() <= 384, "a frame stays a few cache lines");
    }

    #[test]
    fn set_marks_present_and_ignores_non_arkit() {
        let mut f = TrackFrame::empty(1, Seconds(0.0), DeviceProfile::Webcam);
        f.set(Channel::JAW_OPEN, 0.4);
        f.set(Channel::HEAD_YAW, 1.0);
        assert_eq!(f.get(Channel::JAW_OPEN), 0.4);
        assert_eq!(f.get(Channel::HEAD_YAW), 0.0);
        assert!(f.present.contains(Channel::JAW_OPEN) && f.present.count() == 1);
    }

    #[test]
    fn serde_round_trip_keeps_only_present_channels() {
        let mut f = TrackFrame::empty(7, Seconds(0.25), DeviceProfile::Arkit);
        f.set(Channel::EYE_BLINK_LEFT, 0.9);
        f.head = Some(HeadPose { yaw: 0.1, pitch: -0.2, roll: 0.0, position: Some(Vec3::new(0.0, 0.0, 0.5)) });
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("\"eyeBlinkLeft\":0.9") && !json.contains("jawOpen"), "{json}");
        assert_eq!(serde_json::from_str::<TrackFrame>(&json).unwrap(), f);
    }

    #[test]
    fn a_recorded_non_arkit_channel_is_rejected() {
        let json = r#"{"seq":1,"time":0.0,"profile":"arkit","status":"tracking","blend":{"headYaw":0.3}}"#;
        let err = serde_json::from_str::<TrackFrame>(json).unwrap_err().to_string();
        assert!(err.contains("headYaw"), "{err}");
    }
}
