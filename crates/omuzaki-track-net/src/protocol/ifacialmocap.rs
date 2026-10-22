//! iFacialMocap: one UTF-8 text datagram per frame, entries separated by `|`.
//!
//! ```text
//! mouthSmile_R-12|eyeBlink_L-87|…|tongueOut-0|=head#-2.1,8.4,0.6,0.01,-0.02,-0.45|rightEye#3.0,1.2,0.0|leftEye#3.1,0.9,0.0|
//! ```
//!
//! - A blendshape is `name-value` (newer builds also write `name&value`): the `ARKit` name with `_L`/`_R` for the
//!   sides, the value a whole number `0..=100`.
//! - `head#` is the head's rotation in degrees (x, y, z) then its position in metres; `rightEye#` and `leftEye#` are
//!   eye rotations in degrees. The head entry may carry a leading `=`.
//! - The phone starts sending once this computer sends [`HANDSHAKE`] to its port [`PORT`]; it then sends to the
//!   address the handshake came from, at the same port.
//!
//! The angles are `ARKit`'s face-anchor angles (right-handed, the face looking along +Z), which is anim-api's own frame:
//! x is pitch, y is yaw and z is roll ([`HEAD_AXES`]).

use omuzaki_anim_api::{EyeAngles, HeadPose, TrackFrame};
use omuzaki_core::glam::Vec3;

use super::{Axes, DecodeError, Decoder, Protocol, Stamp, blank};
use crate::names;

/// The port the phone listens on and sends to.
pub const PORT: u16 = 49983;

/// What this computer sends the phone to start (and keep) the stream, as the app's own receivers do.
pub const HANDSHAKE: &[u8] = b"iFacialMocap_sahuasouryya9218sauhuiayeta91555dy3719";

/// The sender's head angles (x, y, z) onto yaw, pitch and roll.
pub const HEAD_AXES: Axes = Axes { index: [1, 0, 2], sign: [1.0, 1.0, 1.0] };

/// The sender's eye angles (x, y, z) onto yaw and pitch (the third is unused).
pub const EYE_AXES: Axes = Axes { index: [1, 0, 2], sign: [1.0, 1.0, 1.0] };

/// The iFacialMocap [`Decoder`]. Stateless: every datagram is a whole frame.
#[derive(Copy, Clone, Debug, Default)]
pub struct IfacialmocapDecoder;

fn bad(why: &'static str) -> DecodeError {
    DecodeError::format(Protocol::Ifacialmocap, why)
}

/// Up to `N` comma-separated floats; missing ones are zero.
fn floats<const N: usize>(text: &str) -> Result<[f32; N], DecodeError> {
    let mut out = [0.0; N];
    for (slot, part) in out.iter_mut().zip(text.split(',')) {
        *slot = part.trim().parse().map_err(|_| bad("a number in an angle entry does not parse"))?;
    }
    Ok(out)
}

impl Decoder for IfacialmocapDecoder {
    fn decode(&mut self, packet: &[u8], out: &mut TrackFrame) -> Result<Option<Stamp>, DecodeError> {
        let text = std::str::from_utf8(packet).map_err(|_| bad("not UTF-8"))?;
        let mut frame = blank();
        let mut eyes = EyeAngles::default();
        let mut any = false;
        for entry in text.split('|').map(|e| e.trim_start_matches('=').trim()).filter(|e| !e.is_empty()) {
            if let Some((key, values)) = entry.split_once('#') {
                match key {
                    "head" => {
                        let [rx, ry, rz, px, py, pz] = floats::<6>(values)?;
                        let [yaw, pitch, roll] = HEAD_AXES.apply([rx, ry, rz].map(f32::to_radians));
                        frame.head = Some(HeadPose { yaw, pitch, roll, position: Some(Vec3::new(px, py, pz)) });
                    }
                    "rightEye" | "leftEye" => {
                        let [yaw, pitch, _] = EYE_AXES.apply(floats::<3>(values)?.map(f32::to_radians));
                        if key == "leftEye" {
                            (eyes.left_yaw, eyes.left_pitch) = (yaw, pitch);
                        } else {
                            (eyes.right_yaw, eyes.right_pitch) = (yaw, pitch);
                        }
                        frame.eyes = Some(eyes);
                    }
                    _ => {} // later app versions add entries; they are not ours to refuse
                }
                any = true;
                continue;
            }
            let Some(split) = entry.find(['-', '&']) else {
                return Err(bad("an entry is neither name-value nor key#values"));
            };
            let (name, value) = (&entry[..split], &entry[split + 1..]);
            let value: f32 = value.parse().map_err(|_| bad("a blendshape value does not parse"))?;
            if let Some(channel) = names::ifacialmocap(name) {
                frame.set(channel, (value / 100.0).clamp(0.0, 1.0));
                any = true;
            }
        }
        if !any {
            return Err(bad("no blendshape or head entry"));
        }
        *out = frame;
        Ok(Some(Stamp::default()))
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_anim_api::{Channel, TrackStatus};

    use super::*;

    #[test]
    fn a_frame_decodes() {
        let packet = b"mouthSmile_R-12|eyeBlink_L-87|jawOpen&40|tongueOut-0|=head#-2.0,10.0,1.5,0.01,-0.02,-0.45|rightEye#3.0,1.0,0.0|leftEye#4.0,2.0,0.0|";
        let mut out = blank();
        let stamp = IfacialmocapDecoder.decode(packet, &mut out).unwrap();
        assert_eq!(stamp, Some(Stamp::default()));
        assert_eq!(out.status, TrackStatus::Tracking);
        assert!((out.get(Channel::MOUTH_SMILE_RIGHT) - 0.12).abs() < 1e-6);
        assert!((out.get(Channel::EYE_BLINK_LEFT) - 0.87).abs() < 1e-6);
        assert!((out.get(Channel::JAW_OPEN) - 0.40).abs() < 1e-6);
        assert!(out.present.contains(Channel::TONGUE_OUT), "a measured zero is present");
        assert!(!out.present.contains(Channel::EYE_BLINK_RIGHT));
        let head = out.head.unwrap();
        assert!((head.yaw - 10f32.to_radians()).abs() < 1e-6);
        assert!((head.pitch + 2f32.to_radians()).abs() < 1e-6);
        assert!((head.roll - 1.5f32.to_radians()).abs() < 1e-6);
        assert_eq!(head.position, Some(Vec3::new(0.01, -0.02, -0.45)));
        let eyes = out.eyes.unwrap();
        assert!((eyes.left_yaw - 2f32.to_radians()).abs() < 1e-6);
        assert!((eyes.right_pitch - 3f32.to_radians()).abs() < 1e-6);
    }

    #[test]
    fn garbage_is_refused() {
        let mut out = blank();
        for packet in [&b""[..], b"|||", b"\xff\xfe", b"eyeBlink_L-abc|", b"head#1,x,3|", b"justaword|"] {
            assert!(IfacialmocapDecoder.decode(packet, &mut out).is_err(), "{packet:?}");
        }
        assert_eq!(out, blank(), "a refused packet leaves the frame alone");
    }
}
