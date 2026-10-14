//! A recorded tracker stream and a deterministic replayer for it (spec §39 rule 6: "fake tracker, replays a recorded
//! track"), plus a generator for synthetic tracks so tests need no recorded files.

use omuzaki_anim_api::{Channel, DeviceProfile, FrameSource, HeadPose, TrackFrame, TrackStatus};
use omuzaki_core::glam::Vec3;
use omuzaki_core::rng::Rng;
use omuzaki_core::time::Seconds;
use serde::{Deserialize, Serialize};

/// Frames captured at a steady rate, in time order. The at-rest form is JSON via serde.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecordedTrack {
    /// Capture rate, frames per second.
    pub fps: f32,
    /// The frames, with strictly increasing `time`.
    pub frames: Vec<TrackFrame>,
}

impl RecordedTrack {
    /// The at-rest form: JSON with channels by name (see `TrackFrame`'s serde form).
    ///
    /// # Errors
    /// Only if serialisation fails (it cannot for finite numbers).
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Reads a track written by [`RecordedTrack::to_json`]. Exact: times and values come back bit for bit, because the
    /// workspace's `serde_json` has `float_roundtrip` on (decision 0009, rule 3).
    ///
    /// # Errors
    /// Malformed JSON, an unknown channel name, or a non-ARKit channel in a frame.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Time of the last frame.
    pub fn duration(&self) -> Seconds {
        self.frames.last().map_or(Seconds(0.0), |f| f.time)
    }
}

/// Plays a [`RecordedTrack`] as a [`FrameSource`]: the same `poll` times always give the same frames, with the same
/// "newest frame wins" behaviour as a live tracker (frames skipped between polls are dropped, never queued).
#[derive(Clone, Debug)]
pub struct TrackReplay {
    track: RecordedTrack,
    looping: bool,
    /// The lap (0 for the first pass) and the next frame index within it that has not been considered yet.
    lap: u64,
    next: usize,
}

impl TrackReplay {
    /// Plays once; after the last frame `poll` returns `None`.
    pub fn new(track: RecordedTrack) -> Self {
        Self { track, looping: false, lap: 0, next: 0 }
    }

    /// Plays forever; frame times and sequence numbers keep increasing across laps.
    #[must_use]
    pub fn looping(mut self) -> Self {
        self.looping = true;
        self
    }

    /// Seconds per lap: the recorded span plus one frame, so the first frame of the next lap follows the last.
    fn period(&self) -> f64 {
        self.track.duration().0 + 1.0 / f64::from(self.track.fps)
    }
}

impl FrameSource for TrackReplay {
    #[allow(clippy::cast_precision_loss)] // lap counts stay far below 2^53
    fn poll(&mut self, now: Seconds) -> Option<TrackFrame> {
        if self.track.frames.is_empty() || now.0 < 0.0 {
            return None;
        }
        let period = self.period();
        let (lap, t) = if self.looping {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // now >= 0 and laps are small
            let lap = (now.0 / period).floor() as u64;
            (lap, now.0 - lap as f64 * period)
        } else {
            (0, now.0)
        };
        if lap != self.lap {
            (self.lap, self.next) = (lap, 0);
        }
        // The newest frame at or before `t`; anything between `next` and it was skipped, which is the point.
        let upto = self.track.frames.partition_point(|f| f.time.0 <= t);
        let index = upto.checked_sub(1).filter(|&i| i >= self.next)?;
        self.next = index + 1;
        let mut frame = self.track.frames[index];
        frame.time = Seconds(lap as f64 * period + frame.time.0);
        frame.seq += lap * self.track.frames.len() as u64;
        Some(frame)
    }
}

/// What a synthetic track contains.
#[derive(Clone, Debug, PartialEq)]
pub struct SyntheticSpec {
    /// Seeds the measurement noise and blink timing.
    pub seed: u64,
    /// Length, seconds.
    pub seconds: f32,
    /// Capture rate.
    pub fps: f32,
    /// How the raw numbers should be read.
    pub profile: DeviceProfile,
    /// A span during which the face is lost, if any.
    pub lost: Option<(Seconds, Seconds)>,
}

impl Default for SyntheticSpec {
    fn default() -> Self {
        Self { seed: 1, seconds: 10.0, fps: 30.0, profile: DeviceProfile::Webcam, lost: None }
    }
}

/// A triangle wave in `0..=1` with the given period: smooth enough to look like motion, built from plain arithmetic
/// so the numbers do not depend on the platform's libm (no `sin`).
fn triangle(t: f64, period: f64) -> f32 {
    let x = (t / period).fract();
    #[allow(clippy::cast_possible_truncation)] // 0..1
    {
        (1.0 - (2.0 * x - 1.0).abs()) as f32
    }
}

/// Builds a plausible, fully deterministic performance: talking (jaw), a slow smile, blinks every few seconds, head
/// sway and a little sensor noise. Webcam-profile tracks read low (as a real webcam does), so calibration matters.
#[allow(clippy::cast_possible_truncation)] // test-sized times and counts
pub fn synthetic_track(spec: &SyntheticSpec) -> RecordedTrack {
    let mut rng = Rng::new(spec.seed);
    let scale = if spec.profile == DeviceProfile::Webcam { 0.5 } else { 1.0 };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a test-sized frame count
    let count = (spec.seconds * spec.fps).floor() as u64 + 1;
    let mut next_blink = 1.0 + 3.0 * f64::from(rng.next_f32());
    let mut blink_start = f64::NEG_INFINITY;
    let mut frames = Vec::new();
    for seq in 0..count {
        #[allow(clippy::cast_precision_loss)] // test-sized
        let t = seq as f64 / f64::from(spec.fps);
        let time = Seconds(t);
        let lost = spec.lost.is_some_and(|(from, to)| t >= from.0 && t < to.0);
        if lost {
            frames.push(TrackFrame::lost(seq, time, spec.profile));
            continue;
        }
        if t >= next_blink {
            (blink_start, next_blink) = (t, t + 1.5 + 3.0 * f64::from(rng.next_f32()));
        }
        let blink = if t - blink_start < 0.15 { 1.0 - ((t - blink_start - 0.075).abs() / 0.075) as f32 } else { 0.0 };
        let mut noise = || scale * 0.01 * rng.next_f32();
        let mut frame = TrackFrame::empty(seq, time, spec.profile);
        frame.set(Channel::JAW_OPEN, scale * 0.6 * triangle(t, 0.7) + noise());
        let smile = scale * 0.5 * triangle(t, 8.0);
        frame.set(Channel::MOUTH_SMILE_LEFT, smile + noise());
        frame.set(Channel::MOUTH_SMILE_RIGHT, 0.9 * smile + noise());
        frame.set(Channel::EYE_BLINK_LEFT, blink.clamp(0.0, 1.0) + noise());
        frame.set(Channel::EYE_BLINK_RIGHT, blink.clamp(0.0, 1.0) + noise());
        frame.set(Channel::BROW_INNER_UP, scale * 0.3 * triangle(t, 5.0) + noise());
        frame.head = Some(HeadPose {
            yaw: 0.3 * (triangle(t, 6.0) - 0.5),
            pitch: 0.15 * (triangle(t, 4.0) - 0.5),
            roll: 0.1 * (triangle(t, 9.0) - 0.5),
            position: Some(Vec3::new(0.0, 0.0, 0.6 + 0.05 * triangle(t, 7.0))),
        });
        frames.push(frame);
    }
    RecordedTrack { fps: spec.fps, frames }
}

/// True if the frame is a lost-face frame (tests read better with a name).
pub fn is_lost(frame: &TrackFrame) -> bool {
    frame.status == TrackStatus::Lost
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // exact values copied through; small loop counters
mod tests {
    use super::*;

    fn track() -> RecordedTrack {
        synthetic_track(&SyntheticSpec { seconds: 1.0, fps: 10.0, ..SyntheticSpec::default() })
    }

    #[test]
    fn synthetic_tracks_are_reproducible_and_seeds_differ() {
        assert_eq!(track(), track());
        let other = synthetic_track(&SyntheticSpec { seed: 2, seconds: 1.0, fps: 10.0, ..SyntheticSpec::default() });
        assert_ne!(track(), other);
        assert_eq!(track().frames.len(), 11);
        assert_eq!(track().duration(), Seconds(1.0));
    }

    #[test]
    fn webcam_tracks_read_low_and_lost_spans_are_lost() {
        let webcam = synthetic_track(&SyntheticSpec::default());
        let arkit = synthetic_track(&SyntheticSpec { profile: DeviceProfile::Arkit, ..SyntheticSpec::default() });
        let peak = |t: &RecordedTrack| t.frames.iter().map(|f| f.get(Channel::JAW_OPEN)).fold(0.0, f32::max);
        assert!(peak(&webcam) < 0.4 && peak(&arkit) > 0.5);
        let gappy =
            synthetic_track(&SyntheticSpec { lost: Some((Seconds(2.0), Seconds(3.0))), ..SyntheticSpec::default() });
        let lost = gappy.frames.iter().filter(|f| is_lost(f)).count();
        assert_eq!(lost, 30, "one second at 30 fps");
        assert!(gappy.frames[100].head.is_some() && is_lost(&gappy.frames[60]));
    }

    #[test]
    fn replay_gives_the_newest_frame_once_and_drops_the_skipped() {
        let mut replay = TrackReplay::new(track());
        assert_eq!(replay.poll(Seconds(0.0)).map(|f| f.seq), Some(0));
        assert_eq!(replay.poll(Seconds(0.0)), None, "nothing new yet");
        assert_eq!(replay.poll(Seconds(0.05)), None, "between frames");
        assert_eq!(replay.poll(Seconds(0.45)).map(|f| f.seq), Some(4), "frames 1 to 3 were skipped, not queued");
        assert_eq!(replay.poll(Seconds(0.45)), None);
        assert_eq!(replay.poll(Seconds(50.0)).map(|f| f.seq), Some(10), "the last frame");
        assert_eq!(replay.poll(Seconds(60.0)), None, "then the track is over");
    }

    #[test]
    fn identical_polls_give_identical_frames_across_replayers() {
        let times = [0.0, 0.12, 0.31, 0.32, 0.9, 1.5];
        let play = || {
            let mut r = TrackReplay::new(track());
            times.iter().map(|&t| r.poll(Seconds(t))).collect::<Vec<_>>()
        };
        assert_eq!(play(), play());
    }

    #[test]
    fn looping_keeps_time_and_sequence_increasing() {
        let mut replay = TrackReplay::new(track()).looping();
        let mut last: Option<TrackFrame> = None;
        for step in 0..40 {
            let now = Seconds(f64::from(step) * 0.1);
            if let Some(f) = replay.poll(now) {
                if let Some(prev) = last {
                    assert!(f.time.0 > prev.time.0 && f.seq > prev.seq, "{prev:?} then {f:?}");
                }
                last = Some(f);
            }
        }
        assert!(last.is_some_and(|f| f.time.0 > 3.0), "ran past the end of the first lap");
    }

    #[test]
    fn tracks_round_trip_through_json() {
        let t = track();
        let back = RecordedTrack::from_json(&t.to_json().unwrap()).unwrap();
        assert_eq!(back, t);
        assert!(
            t.frames.iter().zip(&back.frames).all(|(a, b)| a.time.0.to_bits() == b.time.0.to_bits()),
            "times survive bit for bit"
        );
    }
}
