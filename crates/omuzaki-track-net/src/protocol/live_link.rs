//! Epic's Live Link Face: one binary datagram per frame, big-endian.
//!
//! ```text
//! u8   packet version
//! …    device id and subject name (length-prefixed strings; their framing has changed between versions)
//! i32  frame number          ┐
//! f32  sub-frame             │ the frame time: (frame + sub-frame) / (numerator / denominator) seconds
//! i32  rate numerator        │
//! i32  rate denominator      ┘
//! u8   value count (61)
//! f32 × 61                    the 52 ARKit blendshapes in Apple's declaration order (`names::LIVE_LINK_ORDER`),
//!                             then head yaw, pitch, roll and left and right eye yaw, pitch, roll (radians)
//! ```
//!
//! The header's strings are skipped: the frame is read from the **end** of the datagram, whose layout is fixed, so a
//! version that frames its strings differently still decodes. A datagram without values (the app sends them while it
//! has no face, or between takes) is a lost frame. The phone is set to send to this computer's address and [`PORT`];
//! nothing is sent back.
//!
//! Live Link Face's angles follow Unreal's convention (left-handed, Z up): yaw turns toward the performer's right and
//! roll toward the left shoulder, so both change sign ([`HEAD_AXES`]); pitch is positive nodding down.

use omuzaki_anim_api::{EyeAngles, HeadPose, TrackFrame, TrackStatus};
use omuzaki_track_api::camera::DeviceTime;

use super::{Axes, DecodeError, Decoder, Protocol, Stamp, blank};
use crate::names::{self, LIVE_LINK_ORDER};

/// The port the app sends to by default.
pub const PORT: u16 = 11111;

/// Values per frame: 52 blendshapes and nine angles.
pub const VALUES: usize = 61;

/// The fixed tail: frame time (16 bytes), the count and the values.
const TAIL: usize = 16 + 1 + VALUES * 4;

/// The sender's head angles (yaw, pitch, roll) onto ours.
pub const HEAD_AXES: Axes = Axes { index: [0, 1, 2], sign: [-1.0, 1.0, -1.0] };

/// The Live Link Face [`Decoder`]. Stateless: every datagram is a whole frame.
#[derive(Copy, Clone, Debug, Default)]
pub struct LiveLinkDecoder;

fn bad(why: &'static str) -> DecodeError {
    DecodeError::format(Protocol::LiveLinkFace, why)
}

fn be_f32(bytes: &[u8], at: usize) -> f32 {
    let mut b = [0; 4];
    b.copy_from_slice(&bytes[at..at + 4]);
    f32::from_be_bytes(b)
}

fn be_i32(bytes: &[u8], at: usize) -> i32 {
    let mut b = [0; 4];
    b.copy_from_slice(&bytes[at..at + 4]);
    i32::from_be_bytes(b)
}

/// The frame time in nanoseconds and the frame number, when the rate makes sense.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // checked non-negative and finite first
fn frame_time(time: &[u8]) -> (Option<DeviceTime>, Option<u64>) {
    let (frame, sub, num, den) = (be_i32(time, 0), be_f32(time, 4), be_i32(time, 8), be_i32(time, 12));
    let seq = u64::try_from(frame).ok();
    if num <= 0 || den <= 0 || frame < 0 || !sub.is_finite() {
        return (None, seq);
    }
    let seconds = (f64::from(frame) + f64::from(sub)) * f64::from(den) / f64::from(num);
    let sent = (seconds.is_finite() && seconds >= 0.0).then_some(DeviceTime((seconds * 1e9) as u64));
    (sent, seq)
}

impl Decoder for LiveLinkDecoder {
    fn decode(&mut self, packet: &[u8], out: &mut TrackFrame) -> Result<Option<Stamp>, DecodeError> {
        if packet.is_empty() {
            return Err(bad("empty"));
        }
        if packet.len() < 1 + TAIL {
            // A header without values: the app has no face to send.
            let mut frame = blank();
            frame.status = TrackStatus::Lost;
            *out = frame;
            return Ok(Some(Stamp::default()));
        }
        let tail = &packet[packet.len() - TAIL..];
        if usize::from(tail[16]) != VALUES {
            return Err(bad("the value count is not 61"));
        }
        let (sent, seq) = frame_time(&tail[..16]);
        let values = &tail[17..];
        let value = |i: usize| be_f32(values, i * 4);
        if (0..VALUES).any(|i| !value(i).is_finite()) {
            return Err(bad("a value is not finite"));
        }
        let mut frame = blank();
        for (i, name) in LIVE_LINK_ORDER.iter().enumerate() {
            if let Some(channel) = names::arkit(name) {
                frame.set(channel, value(i).clamp(0.0, 1.0));
            }
        }
        let [yaw, pitch, roll] = HEAD_AXES.apply([value(52), value(53), value(54)]);
        frame.head = Some(HeadPose { yaw, pitch, roll, position: None });
        let [left_yaw, left_pitch, _] = HEAD_AXES.apply([value(55), value(56), value(57)]);
        let [right_yaw, right_pitch, _] = HEAD_AXES.apply([value(58), value(59), value(60)]);
        frame.eyes = Some(EyeAngles { left_yaw, left_pitch, right_yaw, right_pitch });
        *out = frame;
        Ok(Some(Stamp { sent, seq }))
    }
}

/// Builds a Live Link Face datagram, for tests and tools: version 6, the device id and subject as Unreal frames
/// them (a length that counts a terminating zero, then the bytes), then the fixed tail.
pub fn encode(device: &str, subject: &str, frame: i32, fps: i32, values: &[f32; VALUES]) -> Vec<u8> {
    let mut out = vec![6u8];
    for text in [device, subject] {
        let len = i32::try_from(text.len() + 1).unwrap_or(i32::MAX);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(text.as_bytes());
        out.push(0);
    }
    out.extend_from_slice(&frame.to_be_bytes());
    out.extend_from_slice(&0f32.to_be_bytes());
    out.extend_from_slice(&fps.to_be_bytes());
    out.extend_from_slice(&1i32.to_be_bytes());
    out.push(u8::try_from(VALUES).unwrap_or(u8::MAX));
    for v in values {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use omuzaki_anim_api::Channel;

    use super::*;

    #[test]
    fn a_frame_decodes_from_its_tail() {
        let mut values = [0.0; VALUES];
        values[0] = 0.75; // eyeBlinkLeft
        values[17] = 0.5; // jawOpen
        values[51] = 1.0; // tongueOut
        values[52] = 0.2; // head yaw
        values[53] = -0.1; // head pitch
        values[55] = 0.05; // left eye yaw
        for header in ["", "a much longer device id that shifts everything"] {
            let packet = encode(header, "subject", 120, 60, &values);
            let mut out = blank();
            let stamp = LiveLinkDecoder.decode(&packet, &mut out).unwrap().unwrap();
            assert_eq!(stamp.seq, Some(120));
            assert_eq!(stamp.sent, Some(DeviceTime(2_000_000_000)));
            assert_eq!(out.get(Channel::EYE_BLINK_LEFT), 0.75);
            assert_eq!(out.get(Channel::JAW_OPEN), 0.5);
            assert_eq!(out.get(Channel::TONGUE_OUT), 1.0);
            assert_eq!(out.present.count(), 52, "every ARKit channel is sent");
            let head = out.head.unwrap();
            assert_eq!((head.yaw, head.pitch, head.roll), (-0.2, -0.1, -0.0));
            assert_eq!(out.eyes.unwrap().left_yaw, -0.05);
        }
    }

    #[test]
    fn a_header_alone_is_lost_and_a_bad_count_is_refused() {
        let mut out = blank();
        LiveLinkDecoder.decode(&[6, 0, 0, 0, 1, 0], &mut out).unwrap();
        assert_eq!(out.status, TrackStatus::Lost);
        let mut packet = encode("d", "s", 1, 60, &[0.0; VALUES]);
        let count = packet.len() - VALUES * 4 - 1;
        packet[count] = 52;
        assert!(LiveLinkDecoder.decode(&packet, &mut out).is_err());
        assert!(LiveLinkDecoder.decode(&[], &mut out).is_err());
    }
}
