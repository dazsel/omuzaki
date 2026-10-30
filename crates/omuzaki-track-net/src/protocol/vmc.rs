//! The VMC protocol (Virtual Motion Capture, OSC over UDP): what VSeeFace, XR Animator, mocopi, VMagicMirror and
//! other mocap and tracking apps send.
//!
//! A frame is spread over messages, usually in one or more bundles:
//!
//! | Address | Arguments | Used for |
//! |---|---|---|
//! | `/VMC/Ext/Blend/Val` | name, value | a blendshape: perfect-sync `ARKit` names, and the VRM blink presets |
//! | `/VMC/Ext/Blend/Apply` | none | the frame is complete: the decoder emits it |
//! | `/VMC/Ext/Bone/Pos` | bone, position (3), rotation quaternion (4) | `Head`, `LeftEye` and `RightEye` for the face; every other humanoid bone for the body |
//! | `/VMC/Ext/T` | time (s) | the sender's clock |
//! | `/VMC/Ext/OK` | loaded, calibration state, calibration mode, tracking | a tracking value of 0 is a lost frame |
//!
//! Values persist between frames: a sender that sends only what changed still yields whole frames. Bone rotations are
//! local, in Unity's left-handed frame (+X the avatar's right); turning one into anim-api's right-handed frame (+X the
//! avatar's left) flips the sign of the quaternion's y and z ([`to_anim`]).
//!
//! **The body** (PF-04's full body: mocopi, SlimeVR, XR Animator, VSeeFace) is every other humanoid bone, kept as a
//! T-space rotation ([`Decoder::body`]). Senders drive a VRM through UniVRM, whose normalised bones rest at the identity
//! in the T-pose, so a bone's local rotation is already the T-space rotation the stage poses with. The head, neck, eyes
//! and jaw stay the face's; the hips' position is read past (the stage poses rotations).

use omuzaki_anim_api::{
    BoneMask, ChannelMask, EyeAngles, HeadPose, HumanoidBone, HumanoidPose, TrackFrame, TrackStatus,
};
use omuzaki_core::glam::{EulerRot, Quat};
use omuzaki_track_api::body::BodyFrame;
use omuzaki_track_api::camera::DeviceTime;

use super::osc::{self, Arg, Message};
use super::{DecodeError, Decoder, Stamp, blank};
use crate::names;
use crate::vmc_out::bone_from_unity;

/// The port VMC receivers listen on by default.
pub const PORT: u16 = 39539;

/// A Unity-frame quaternion `[x, y, z, w]` as yaw, pitch and roll in anim-api's convention (YXZ Euler, radians).
pub fn to_anim(unity: [f32; 4]) -> [f32; 3] {
    let [qx, qy, qz, qw] = unity;
    let rotation = Quat::from_xyzw(qx, -qy, -qz, qw);
    if !rotation.is_finite() || rotation.length_squared() < 1e-12 {
        return [0.0; 3];
    }
    let (yaw, pitch, roll) = rotation.normalize().to_euler(EulerRot::YXZ);
    [yaw, pitch, roll]
}

/// A Unity-frame quaternion `[x, y, z, w]` as an anim-api rotation, or `None` when it is not a rotation.
pub fn from_unity(unity: [f32; 4]) -> Option<Quat> {
    let [qx, qy, qz, qw] = unity;
    let rotation = Quat::from_xyzw(qx, -qy, -qz, qw);
    (rotation.is_finite() && rotation.length_squared() > 1e-12).then(|| rotation.normalize())
}

/// Whether `bone` is the face's: the head, neck, eyes and jaw never come from the body.
fn is_face(bone: HumanoidBone) -> bool {
    matches!(
        bone,
        HumanoidBone::Neck | HumanoidBone::Head | HumanoidBone::LeftEye | HumanoidBone::RightEye | HumanoidBone::Jaw
    )
}

/// The VMC [`Decoder`]: it gathers messages until `/VMC/Ext/Blend/Apply`, then emits the frame.
#[derive(Clone, Debug)]
pub struct VmcDecoder {
    /// Everything received so far; values persist across frames.
    frame: TrackFrame,
    eyes: Option<EyeAngles>,
    sent: Option<DeviceTime>,
    /// The sender's tracking flag from `/VMC/Ext/OK`, when it sends one.
    tracking: bool,
    /// The body bones received so far, in T space, and which they are.
    bones: HumanoidPose,
    seen: BoneMask,
}

impl Default for VmcDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl VmcDecoder {
    /// A decoder with nothing received.
    pub fn new() -> Self {
        Self {
            frame: blank(),
            eyes: None,
            sent: None,
            tracking: true,
            bones: HumanoidPose::REST,
            seen: BoneMask::EMPTY,
        }
    }

    /// Reads one message; returns whether it completes a frame.
    fn read(&mut self, message: &Message<'_>) -> Result<bool, osc::OscError> {
        match message.address {
            "/VMC/Ext/Blend/Val" => {
                if let [Arg::Str(name), value] = message.first::<2>()?
                    && let Some(value) = value.as_f32().filter(|v| v.is_finite())
                {
                    for channel in names::vmc(name).into_iter().flatten() {
                        self.frame.set(channel, value.clamp(0.0, 1.0));
                    }
                }
            }
            "/VMC/Ext/Blend/Apply" => return Ok(true),
            "/VMC/Ext/Bone/Pos" => {
                let [Arg::Str(bone)] = message.first::<1>()? else { return Ok(false) };
                let face = matches!(bone, "Head" | "LeftEye" | "RightEye");
                let body = if face { None } else { bone_from_unity(bone).filter(|b| !is_face(*b)) };
                if !face && body.is_none() {
                    return Ok(false);
                }
                let args = message.first::<8>()?;
                let mut q = [0.0f32; 4];
                for (slot, arg) in q.iter_mut().zip(&args[4..]) {
                    *slot = arg.as_f32().unwrap_or(0.0);
                }
                if let Some(body) = body {
                    if let Some(rotation) = from_unity(q) {
                        self.bones.set(body, rotation);
                        self.seen = self.seen.with(body);
                    }
                    return Ok(false);
                }
                let [yaw, pitch, roll] = to_anim(q);
                match bone {
                    "Head" => self.frame.head = Some(HeadPose { yaw, pitch, roll, position: None }),
                    "LeftEye" => {
                        let eyes = self.eyes.get_or_insert_with(EyeAngles::default);
                        (eyes.left_yaw, eyes.left_pitch) = (yaw, pitch);
                    }
                    _ => {
                        let eyes = self.eyes.get_or_insert_with(EyeAngles::default);
                        (eyes.right_yaw, eyes.right_pitch) = (yaw, pitch);
                    }
                }
            }
            "/VMC/Ext/T" => {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // checked finite and non-negative
                if let [time] = message.first::<1>()?
                    && let Some(t) = time.as_f32().filter(|t| t.is_finite() && *t >= 0.0)
                {
                    self.sent = Some(DeviceTime((f64::from(t) * 1e9) as u64));
                }
            }
            "/VMC/Ext/OK" => {
                // Older senders send only `loaded`; the tracking flag is the fourth argument when present.
                if let Some(Ok(Arg::Int(tracking))) = message.args().nth(3) {
                    self.tracking = tracking != 0;
                }
            }
            _ => {}
        }
        Ok(false)
    }
}

impl Decoder for VmcDecoder {
    fn decode(&mut self, packet: &[u8], out: &mut TrackFrame) -> Result<Option<Stamp>, DecodeError> {
        let mut applied = false;
        let mut failed = None;
        osc::for_each_message(packet, &mut |message| match self.read(&message) {
            Ok(done) => applied |= done,
            Err(e) => failed = Some(e),
        })?;
        if let Some(e) = failed {
            return Err(e.into());
        }
        if !applied {
            return Ok(None);
        }
        let mut frame = self.frame;
        frame.eyes = self.eyes;
        if !self.tracking {
            frame = TrackFrame { status: TrackStatus::Lost, present: ChannelMask::EMPTY, ..blank() };
        }
        *out = frame;
        Ok(Some(Stamp { sent: self.sent, seq: None }))
    }

    fn body(&self, out: &mut BodyFrame) -> bool {
        use HumanoidBone as B;
        if self.seen == BoneMask::EMPTY {
            return false;
        }
        let sure = |bones: &[HumanoidBone]| {
            if self.tracking && bones.iter().any(|b| self.seen.contains(*b)) { 1.0 } else { 0.0 }
        };
        // A hand is sure when its wrist or any of its fingers came.
        let hand = |wrist: HumanoidBone| {
            let mine =
                |b: &HumanoidBone| *b == wrist || std::iter::successors(b.parent(), |p| p.parent()).any(|p| p == wrist);
            if self.tracking && HumanoidBone::ALL.iter().filter(|b| mine(b)).any(|b| self.seen.contains(*b)) {
                1.0
            } else {
                0.0
            }
        };
        out.pose = self.bones;
        out.mask = self.seen;
        out.fingers = [None, None];
        out.arms = [sure(&[B::LeftUpperArm, B::LeftLowerArm]), sure(&[B::RightUpperArm, B::RightLowerArm])];
        out.hands = [hand(B::LeftHand), hand(B::RightHand)];
        out.body = sure(&[B::Hips, B::Spine, B::Chest, B::LeftUpperLeg, B::RightUpperLeg]);
        out.head_relative = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_anim_api::Channel;

    use super::osc::Writer;
    use super::*;

    fn blend(name: &str, v: f32) -> Vec<u8> {
        Writer::message("/VMC/Ext/Blend/Val", &[Arg::Str(name), Arg::Float(v)])
    }

    fn bone(name: &str, q: Quat) -> Vec<u8> {
        let args = [Arg::Str(name), Arg::Float(0.0), Arg::Float(1.5), Arg::Float(0.0)];
        let q = [Arg::Float(q.x), Arg::Float(q.y), Arg::Float(q.z), Arg::Float(q.w)];
        Writer::message("/VMC/Ext/Bone/Pos", &[&args[..], &q[..]].concat())
    }

    fn apply() -> Vec<u8> {
        Writer::message("/VMC/Ext/Blend/Apply", &[])
    }

    #[test]
    fn a_frame_is_emitted_on_apply_and_values_persist() {
        let mut decoder = VmcDecoder::new();
        let mut out = blank();
        let first = Writer::bundle(&[
            Writer::message("/VMC/Ext/T", &[Arg::Float(2.5)]),
            blend("JawOpen", 0.5),
            blend("Blink", 0.8),
            bone("Head", Quat::from_rotation_y(-0.3)),
        ]);
        assert_eq!(decoder.decode(&first, &mut out).unwrap(), None, "no Apply yet");
        let stamp = decoder.decode(&Writer::bundle(&[apply()]), &mut out).unwrap().unwrap();
        assert_eq!(stamp.sent, Some(DeviceTime(2_500_000_000)));
        assert_eq!(out.get(Channel::JAW_OPEN), 0.5);
        assert_eq!(out.get(Channel::EYE_BLINK_LEFT), 0.8);
        assert_eq!(out.get(Channel::EYE_BLINK_RIGHT), 0.8);
        // Unity: a negative turn about +Y faces the avatar's left, which is a positive yaw here.
        let head = out.head.unwrap();
        assert!((head.yaw - 0.3).abs() < 1e-5, "{head:?}");
        assert!(head.pitch.abs() < 1e-5 && head.roll.abs() < 1e-5);

        let second = Writer::bundle(&[blend("JawOpen", 0.1), apply()]);
        decoder.decode(&second, &mut out).unwrap().unwrap();
        assert_eq!(out.get(Channel::JAW_OPEN), 0.1);
        assert_eq!(out.get(Channel::EYE_BLINK_LEFT), 0.8, "a value not resent persists");
    }

    #[test]
    fn pitch_and_roll_keep_and_flip_sign() {
        // Unity: +X rotation looks down (pitch kept); +Z rolls toward the avatar's left shoulder (roll flipped).
        let [_, pitch, _] = to_anim(Quat::from_rotation_x(0.2).to_array());
        assert!((pitch - 0.2).abs() < 1e-5);
        let q = Quat::from_rotation_z(0.2);
        let [_, _, roll] = to_anim(q.to_array());
        assert!((roll + 0.2).abs() < 1e-5);
        assert_eq!(to_anim([0.0; 4]), [0.0; 3]);
        assert_eq!(to_anim([f32::NAN, 0.0, 0.0, 1.0]), [0.0; 3]);
    }

    #[test]
    fn tracking_off_is_lost() {
        let mut decoder = VmcDecoder::new();
        let mut out = blank();
        let ok = Writer::message("/VMC/Ext/OK", &[Arg::Int(1), Arg::Int(3), Arg::Int(0), Arg::Int(0)]);
        decoder.decode(&Writer::bundle(&[ok, blend("JawOpen", 0.5), apply()]), &mut out).unwrap();
        assert_eq!(out.status, TrackStatus::Lost);
        let malformed = Writer::message("/VMC/Ext/Bone/Pos", &[Arg::Str("Head"), Arg::Float(1.0)]);
        assert!(decoder.decode(&malformed, &mut out).is_err());
    }

    #[test]
    fn body_bones_become_a_body_frame_and_the_face_stays_the_faces() {
        let mut decoder = VmcDecoder::new();
        let mut out = blank();
        let mut body = BodyFrame::empty(0, omuzaki_core::time::Seconds(0.0));
        assert!(!decoder.body(&mut body), "nothing sent yet");
        // Unity: the left upper arm turned down about +Z is a turn about -Z here (the x axis mirrors).
        let packet = Writer::bundle(&[
            bone("Hips", Quat::from_rotation_y(0.3)),
            bone("LeftUpperArm", Quat::from_rotation_z(1.2)),
            bone("LeftThumbProximal", Quat::from_rotation_x(0.4)),
            bone("Neck", Quat::from_rotation_x(0.5)),
            bone("Head", Quat::from_rotation_y(-0.3)),
            bone("NotABone", Quat::from_rotation_x(0.5)),
            apply(),
        ]);
        decoder.decode(&packet, &mut out).unwrap().unwrap();
        assert!(out.head.is_some(), "the head still drives the face");
        assert!(decoder.body(&mut body));
        let expect = |q: Quat| from_unity(q.to_array()).unwrap();
        assert!(body.pose.get(HumanoidBone::Hips).angle_between(expect(Quat::from_rotation_y(0.3))) < 1e-5);
        let arm = body.pose.get(HumanoidBone::LeftUpperArm);
        assert!(arm.angle_between(Quat::from_rotation_z(-1.2)) < 1e-5, "{arm}");
        // Unity's thumb proximal is the VRM thumb metacarpal.
        assert!(body.mask.contains(HumanoidBone::LeftThumbMetacarpal));
        assert!(!body.mask.contains(HumanoidBone::Neck) && !body.mask.contains(HumanoidBone::Head));
        assert_eq!((body.arms, body.hands, body.body), ([1.0, 0.0], [1.0, 0.0], 1.0));
        assert!(body.head_relative);

        // The sender loses tracking: the bones are kept, but nothing is sure.
        let ok = Writer::message("/VMC/Ext/OK", &[Arg::Int(1), Arg::Int(3), Arg::Int(0), Arg::Int(0)]);
        decoder.decode(&Writer::bundle(&[ok, apply()]), &mut out).unwrap();
        assert!(decoder.body(&mut body));
        assert!(!body.sees_anything());
    }
}
