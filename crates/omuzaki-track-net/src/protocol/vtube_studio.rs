//! VTube Studio's iPhone face tracking: one JSON datagram per frame.
//!
//! This computer asks for data by sending [`request`] to the phone's port [`PORT`]: a JSON object naming the port to
//! send to and how many seconds to keep sending ([`LEASE_S`]). The receiver renews it every [`RENEW_S`] so the stream
//! never lapses, and stops renewing when the session stops. The phone answers with:
//!
//! ```json
//! {"Timestamp": 1729550000123, "Hotkey": -1, "FaceFound": true,
//!  "Rotation": {"x": -3.2, "y": 12.0, "z": 1.1}, "Position": {"x": 0.4, "y": -1.0, "z": 0.2},
//!  "EyeLeft": {"x": 2.0, "y": 4.0, "z": 0.0}, "EyeRight": {"x": 2.1, "y": 3.9, "z": 0.0},
//!  "BlendShapes": [{"k": "EyeBlinkLeft", "v": 0.12}, {"k": "JawOpen", "v": 0.3}]}
//! ```
//!
//! Blendshape keys are `ARKit` names with a capital first letter and values `0..1`; angles are degrees in Unity's
//! left-handed frame (x pitch, y yaw, z roll), so yaw and roll change sign on the way into anim-api's right-handed one
//! ([`HEAD_AXES`]). `Position` is in centimetres. `FaceFound: false` is a lost frame.
//!
//! Decoding allocates nothing for the fields above and for unknown scalar fields; an unknown field holding an array
//! or object costs one allocation (`serde_json` keeps a stack while it skips one). No known version sends such a field.

use std::borrow::Cow;
use std::fmt;

use omuzaki_anim_api::{ARKIT_COUNT, ChannelMask, EyeAngles, HeadPose, TrackFrame, TrackStatus};
use omuzaki_core::glam::Vec3;
use omuzaki_track_api::camera::DeviceTime;
use serde::de::{SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

use super::{Axes, DecodeError, Decoder, Protocol, Stamp, blank};
use crate::names;

/// The port the phone listens on for requests; this computer listens on the same number unless the setting says
/// otherwise.
pub const PORT: u16 = 21412;

/// Seconds a request asks the phone to keep sending.
pub const LEASE_S: f32 = 3.0;

/// Seconds between renewals of the request: well inside the lease, so one lost request costs nothing.
pub const RENEW_S: f64 = 1.0;

/// The sender's angles (x, y, z) onto yaw, pitch and roll.
pub const HEAD_AXES: Axes = Axes { index: [1, 0, 2], sign: [-1.0, 1.0, -1.0] };

/// The request that starts (or renews) the stream to `port` on this computer.
pub fn request(port: u16) -> Vec<u8> {
    let body = serde_json::json!({
        "messageType": "iOSTrackingDataRequest",
        "time": LEASE_S,
        "sentBy": "Omuzaki",
        "ports": [port],
    });
    body.to_string().into_bytes()
}

/// The VTube Studio [`Decoder`]. Stateless: every datagram is a whole frame.
#[derive(Copy, Clone, Debug, Default)]
pub struct VtubeStudioDecoder;

#[derive(Copy, Clone, Debug, Default, Deserialize)]
struct Xyz {
    #[serde(default)]
    x: f32,
    #[serde(default)]
    y: f32,
    #[serde(default)]
    z: f32,
}

impl Xyz {
    fn array(self) -> [f32; 3] {
        [self.x, self.y, self.z]
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Packet {
    #[serde(default)]
    timestamp: Option<f64>,
    #[serde(default = "found")]
    face_found: bool,
    #[serde(default)]
    rotation: Option<Xyz>,
    #[serde(default)]
    position: Option<Xyz>,
    #[serde(default)]
    eye_left: Option<Xyz>,
    #[serde(default)]
    eye_right: Option<Xyz>,
    #[serde(default)]
    blend_shapes: Shapes,
}

const fn found() -> bool {
    true
}

/// The blendshape list, read straight into a fixed array: no `Vec` per packet.
struct Shapes {
    blend: [f32; ARKIT_COUNT],
    present: ChannelMask,
}

impl Default for Shapes {
    fn default() -> Self {
        Self { blend: [0.0; ARKIT_COUNT], present: ChannelMask::EMPTY }
    }
}

#[derive(Deserialize)]
struct Shape<'a> {
    #[serde(borrow)]
    k: Cow<'a, str>,
    v: f32,
}

impl<'de> Deserialize<'de> for Shapes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct List;
        impl<'de> Visitor<'de> for List {
            type Value = Shapes;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a list of {k, v} blendshapes")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Shapes, A::Error> {
                let mut shapes = Shapes::default();
                while let Some(shape) = seq.next_element::<Shape<'de>>()? {
                    if let Some(channel) = names::arkit(&shape.k) {
                        shapes.blend[channel.index()] = shape.v.clamp(0.0, 1.0);
                        shapes.present.insert(channel);
                    }
                }
                Ok(shapes)
            }
        }
        deserializer.deserialize_seq(List)
    }
}

impl Decoder for VtubeStudioDecoder {
    fn decode(&mut self, packet: &[u8], out: &mut TrackFrame) -> Result<Option<Stamp>, DecodeError> {
        let packet: Packet = serde_json::from_slice(packet)
            .map_err(|_| DecodeError::format(Protocol::VtubeStudio, "not a tracking-data JSON object"))?;
        let mut frame = blank();
        if packet.face_found {
            frame.blend = packet.blend_shapes.blend;
            frame.present = packet.blend_shapes.present;
            frame.head = packet.rotation.map(|r| {
                let [yaw, pitch, roll] = HEAD_AXES.apply(r.array().map(f32::to_radians));
                // Centimetres, Unity's left-handed frame: x flips with the handedness.
                let position = packet.position.map(|p| Vec3::new(-p.x, p.y, p.z) / 100.0);
                HeadPose { yaw, pitch, roll, position }
            });
            frame.eyes = match (packet.eye_left, packet.eye_right) {
                (None, None) => None,
                (left, right) => {
                    let [left_yaw, left_pitch, _] =
                        HEAD_AXES.apply(left.unwrap_or_default().array().map(f32::to_radians));
                    let [right_yaw, right_pitch, _] =
                        HEAD_AXES.apply(right.unwrap_or_default().array().map(f32::to_radians));
                    Some(EyeAngles { left_yaw, left_pitch, right_yaw, right_pitch })
                }
            };
        } else {
            frame.status = TrackStatus::Lost;
        }
        *out = frame;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        // a positive millisecond count far below 2^53
        let sent = packet.timestamp.filter(|t| t.is_finite() && *t >= 0.0).map(|ms| DeviceTime((ms * 1e6) as u64));
        Ok(Some(Stamp { sent, seq: None }))
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_anim_api::Channel;

    use super::*;

    #[test]
    fn a_frame_decodes() {
        let packet = br#"{"Timestamp": 1000.5, "Hotkey": -1, "FaceFound": true,
            "Rotation": {"x": -3.0, "y": 12.0, "z": 2.0}, "Position": {"x": 1.0, "y": -2.0, "z": 3.0},
            "EyeLeft": {"x": 2.0, "y": 4.0, "z": 0.0},
            "BlendShapes": [{"k": "EyeBlinkLeft", "v": 0.25}, {"k": "JawOpen", "v": 0.5}, {"k": "Unknown", "v": 1}],
            "VersionNumber": 2}"#;
        let mut out = blank();
        let stamp = VtubeStudioDecoder.decode(packet, &mut out).unwrap().unwrap();
        assert_eq!(stamp.sent, Some(DeviceTime(1_000_500_000)));
        assert_eq!(out.status, TrackStatus::Tracking);
        assert_eq!(out.get(Channel::EYE_BLINK_LEFT), 0.25);
        assert_eq!(out.get(Channel::JAW_OPEN), 0.5);
        assert_eq!(out.present, ChannelMask::of(Channel::EYE_BLINK_LEFT) | ChannelMask::of(Channel::JAW_OPEN));
        let head = out.head.unwrap();
        assert!((head.yaw + 12f32.to_radians()).abs() < 1e-6);
        assert!((head.pitch + 3f32.to_radians()).abs() < 1e-6);
        assert!((head.roll + 2f32.to_radians()).abs() < 1e-6);
        assert_eq!(head.position, Some(Vec3::new(-0.01, -0.02, 0.03)));
        let eyes = out.eyes.unwrap();
        assert!((eyes.left_yaw + 4f32.to_radians()).abs() < 1e-6);
        assert_eq!(eyes.right_yaw, 0.0);
    }

    #[test]
    fn face_not_found_is_lost() {
        let mut out = blank();
        VtubeStudioDecoder.decode(br#"{"FaceFound": false, "BlendShapes": []}"#, &mut out).unwrap();
        assert_eq!(out.status, TrackStatus::Lost);
        assert!(VtubeStudioDecoder.decode(b"[1,2]", &mut out).is_err());
        assert!(VtubeStudioDecoder.decode(b"{\"BlendShapes\": 3}", &mut out).is_err());
    }

    #[test]
    fn the_request_names_the_port() {
        let request: serde_json::Value = serde_json::from_slice(&request(5000)).unwrap();
        assert_eq!(request["messageType"], "iOSTrackingDataRequest");
        assert_eq!(request["ports"], serde_json::json!([5000]));
        assert_eq!(request["time"], f64::from(LEASE_S));
    }
}
