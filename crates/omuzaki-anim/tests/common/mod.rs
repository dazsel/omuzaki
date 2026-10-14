//! Shared by the allocation test and the benchmark: a deterministic track that drives every `ARKit` channel, the head
//! and the gaze, with a lost-face span, so the hot path is measured at its busiest.
//!
//! Plain arithmetic only (no libm), so the numbers are the same everywhere.

use omuzaki_anim_api::{Channel, DeviceProfile, EyeAngles, HeadPose, TrackFrame};
use omuzaki_core::glam::Vec3;
use omuzaki_core::time::Seconds;

/// A triangle wave in `0..=1`, `phase` frames into a cycle of `period` frames.
fn triangle(phase: usize, period: usize) -> f32 {
    let x = (phase % period) as f32 / period as f32;
    1.0 - (2.0 * x - 1.0).abs()
}

/// `count` frames at 30 fps. Every third frame carries gaze as angles instead of `eyeLook*` blendshapes, and the
/// frames from `count / 2` for `count / 8` frames have the face lost.
pub fn frames(count: usize) -> Vec<TrackFrame> {
    (0..count)
        .map(|k| {
            let time = Seconds(k as f64 / 30.0);
            if (count / 2..count / 2 + count / 8).contains(&k) {
                return TrackFrame::lost(k as u64, time, DeviceProfile::Webcam);
            }
            let mut frame = TrackFrame::empty(k as u64, time, DeviceProfile::Webcam);
            for info in Channel::arkit() {
                let i = info.channel.index();
                frame.set(info.channel, 0.5 * triangle(k * (i + 3), 29 + i));
            }
            frame.head = Some(HeadPose {
                yaw: 0.3 * (triangle(k, 90) - 0.5),
                pitch: 0.15 * (triangle(k, 60) - 0.5),
                roll: 0.1 * (triangle(k, 120) - 0.5),
                position: Some(Vec3::new(0.02 * (triangle(k, 70) - 0.5), 0.0, 0.55 + 0.05 * triangle(k, 100))),
            });
            if k % 3 == 0 {
                for c in omuzaki_anim_api::ChannelMask::GAZE {
                    frame.present.remove(c);
                }
                let turn = 0.5 * (triangle(k, 45) - 0.5);
                frame.eyes = Some(EyeAngles { left_yaw: turn, left_pitch: 0.0, right_yaw: turn, right_pitch: 0.1 });
            }
            frame
        })
        .collect()
}
