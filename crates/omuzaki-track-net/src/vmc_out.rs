//! VMC out (spec v3 PF-04, "send our solved pose to other apps"): the performer's humanoid pose and face, sent over
//! OSC on UDP to any VMC receiver (VSeeFace, VMagicMirror, Warudo, VNyan, a game engine's VMC plugin).
//!
//! ```text
//!  perform thread                         sender thread (one per running VmcSender)
//!  PoseSlot::publish (wait-free) ┄┄▶ PoseSlot::read ─▶ VmcEncoder ─▶ send_to (two datagrams)
//! ```
//!
//! Each frame goes out as two bundles (about 5 KB and 2 KB, as receivers expect from one sender): the body (`/VMC/Ext/Root/Pos`, a
//! `/VMC/Ext/Bone/Pos` per humanoid bone, `/VMC/Ext/OK`, `/VMC/Ext/T`), then the face (a `/VMC/Ext/Blend/Val` per
//! `ARKit` channel the performer drives, in perfect-sync spelling, plus the VRM `Blink_L` and `Blink_R`, then
//! `/VMC/Ext/Blend/Apply`).
//!
//! **Frames.** Rotations are parent-local, as anim-api's [`HumanoidPose`] holds them (identity is the T-pose), turned
//! into Unity's left-handed frame (+X the avatar's right) by flipping the quaternion's y and z, the inverse of the
//! receiver's [`to_anim`](crate::protocol::vmc::to_anim); positions flip x. Bone names are Unity's `HumanBodyBones`:
//! the VRM name with a capital, except the thumbs, whose three joints Unity calls proximal, intermediate and distal.
//! The hips carry their rest height ([`VmcOutConfig::hips_height`]) plus the pose's offset.
//!
//! The encoder reuses one buffer, and the slot is a sequence lock over atomic words, so after the first frame neither
//! side allocates; the perform thread's part is one wait-free [`PoseSlot::publish`].

use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};
use std::thread::JoinHandle;
use std::time::Duration;

use omuzaki_anim_api::{ARKIT_COUNT, Channel, ChannelMask, HUMANOID_COUNT, HumanoidBone, HumanoidPose};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_track_api::pose_out::{PoseOutStatus, PoseOutput, PoseTap};

/// The port VMC receivers listen on by default (VSeeFace, VMagicMirror and most others).
pub const OUT_PORT: u16 = 39539;

/// The frames sent per second at most: a new pose is sent at most this often, and only when there is one.
pub const SEND_HZ: u32 = 60;

/// What [`VmcSender`] sends to, and how it places the hips.
#[derive(Copy, Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct VmcOutConfig {
    /// The receiver: an app on this computer by default.
    pub target: SocketAddr,
    /// The hips' rest height above the floor, metres, which their position carries (receivers place the avatar by
    /// it). 0.9 m by default, an adult avatar's.
    pub hips_height: f32,
}

impl Default for VmcOutConfig {
    fn default() -> Self {
        Self { target: SocketAddr::from((Ipv4Addr::LOCALHOST, OUT_PORT)), hips_height: 0.9 }
    }
}

impl VmcOutConfig {
    /// Sending to `target`.
    #[must_use]
    pub const fn with_target(mut self, target: SocketAddr) -> Self {
        self.target = target;
        self
    }

    /// With the hips' rest height, metres.
    #[must_use]
    pub const fn with_hips_height(mut self, metres: f32) -> Self {
        self.hips_height = metres;
        self
    }
}

/// One frame to send: the humanoid pose, the face channels the performer drives, and the time on the stage clock.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct VmcPose {
    /// The pose: parent-local rotations and the hips' offset from rest.
    pub pose: HumanoidPose,
    /// The 52 `ARKit` channels, `0..1`, by channel index.
    pub blend: [f32; ARKIT_COUNT],
    /// Which of them to send.
    pub present: ChannelMask,
    /// Stage time, seconds.
    pub time: f64,
}

impl Default for VmcPose {
    fn default() -> Self {
        Self { pose: HumanoidPose::REST, blend: [0.0; ARKIT_COUNT], present: ChannelMask::EMPTY, time: 0.0 }
    }
}

/// Unity's `HumanBodyBones` name for `bone`.
pub fn unity_bone_name(bone: HumanoidBone) -> &'static str {
    UNITY_NAMES.get(bone.index()).copied().unwrap_or("")
}

/// The humanoid bone Unity's `HumanBodyBones` name `name` stands for, if any: the inverse of [`unity_bone_name`].
pub fn bone_from_unity(name: &str) -> Option<HumanoidBone> {
    UNITY_NAMES.iter().position(|n| *n == name).and_then(|i| HumanoidBone::ALL.get(i).copied())
}

/// Unity's names in [`HumanoidBone::ALL`] order.
const UNITY_NAMES: [&str; HUMANOID_COUNT] = [
    "Hips",
    "Spine",
    "Chest",
    "UpperChest",
    "Neck",
    "Head",
    "LeftEye",
    "RightEye",
    "Jaw",
    "LeftUpperLeg",
    "LeftLowerLeg",
    "LeftFoot",
    "LeftToes",
    "RightUpperLeg",
    "RightLowerLeg",
    "RightFoot",
    "RightToes",
    "LeftShoulder",
    "LeftUpperArm",
    "LeftLowerArm",
    "LeftHand",
    "RightShoulder",
    "RightUpperArm",
    "RightLowerArm",
    "RightHand",
    "LeftThumbProximal",
    "LeftThumbIntermediate",
    "LeftThumbDistal",
    "LeftIndexProximal",
    "LeftIndexIntermediate",
    "LeftIndexDistal",
    "LeftMiddleProximal",
    "LeftMiddleIntermediate",
    "LeftMiddleDistal",
    "LeftRingProximal",
    "LeftRingIntermediate",
    "LeftRingDistal",
    "LeftLittleProximal",
    "LeftLittleIntermediate",
    "LeftLittleDistal",
    "RightThumbProximal",
    "RightThumbIntermediate",
    "RightThumbDistal",
    "RightIndexProximal",
    "RightIndexIntermediate",
    "RightIndexDistal",
    "RightMiddleProximal",
    "RightMiddleIntermediate",
    "RightMiddleDistal",
    "RightRingProximal",
    "RightRingIntermediate",
    "RightRingDistal",
    "RightLittleProximal",
    "RightLittleIntermediate",
    "RightLittleDistal",
];

/// An anim-api rotation in Unity's frame, `[x, y, z, w]`.
pub fn to_unity(rotation: Quat) -> [f32; 4] {
    let q = if rotation.is_finite() { rotation } else { Quat::IDENTITY };
    [q.x, -q.y, -q.z, q.w]
}

/// An anim-api position in Unity's frame.
fn position_to_unity(v: Vec3) -> [f32; 3] {
    [-v.x, v.y, v.z]
}

/// The `/VMC/Ext/Blend/Val` name of an `ARKit` channel: perfect-sync spelling, a capital first letter
/// (`EyeBlinkLeft`), as VSeeFace and the VRM perfect-sync convention expect.
fn perfect_sync(channel: Channel, out: &mut [u8; 32]) -> &str {
    let name = channel.name().as_bytes();
    let n = name.len().min(out.len());
    out[..n].copy_from_slice(&name[..n]);
    if let Some(first) = out.first_mut() {
        *first = first.to_ascii_uppercase();
    }
    std::str::from_utf8(&out[..n]).unwrap_or("")
}

/// Builds a frame's two bundles in reused buffers: nothing is allocated once they have grown.
#[derive(Debug, Default)]
pub struct VmcEncoder {
    body: Vec<u8>,
    face: Vec<u8>,
}

impl VmcEncoder {
    /// The two bundles of `frame`: the body, then the face.
    pub fn encode(&mut self, frame: &VmcPose, config: &VmcOutConfig) -> (&[u8], &[u8]) {
        let body = &mut self.body;
        body.clear();
        bundle_header(body);
        message(body, "/VMC/Ext/Root/Pos", |m| {
            m.str("root");
            m.floats(&[0.0; 3]);
            m.floats(&[0.0, 0.0, 0.0, 1.0]);
        });
        let hips = frame.pose.hips_offset + Vec3::new(0.0, config.hips_height, 0.0);
        for bone in HumanoidBone::ALL {
            let position = if bone == HumanoidBone::Hips { position_to_unity(hips) } else { [0.0; 3] };
            message(body, "/VMC/Ext/Bone/Pos", |m| {
                m.str(unity_bone_name(bone));
                m.floats(&position);
                m.floats(&to_unity(frame.pose.get(bone)));
            });
        }
        // Loaded, and (the fourth argument) tracking.
        message(body, "/VMC/Ext/OK", |m| m.ints(&[1, 0, 0, 1]));
        #[allow(clippy::cast_possible_truncation)] // the protocol's time is a float
        message(body, "/VMC/Ext/T", |m| m.floats(&[frame.time as f32]));

        let face = &mut self.face;
        face.clear();
        bundle_header(face);
        let mut name = [0u8; 32];
        for info in Channel::arkit() {
            let channel = info.channel;
            if !frame.present.contains(channel) {
                continue;
            }
            let value = frame.blend.get(channel.index()).copied().unwrap_or(0.0);
            let value = if value.is_finite() { value.clamp(0.0, 1.0) } else { 0.0 };
            message(face, "/VMC/Ext/Blend/Val", |m| {
                m.str(perfect_sync(channel, &mut name));
                m.floats(&[value]);
            });
        }
        // The VRM blink presets, for avatars without perfect sync.
        for (preset, channel) in [("Blink_L", Channel::EYE_BLINK_LEFT), ("Blink_R", Channel::EYE_BLINK_RIGHT)] {
            if frame.present.contains(channel) {
                let value = frame.blend.get(channel.index()).copied().unwrap_or(0.0).clamp(0.0, 1.0);
                message(face, "/VMC/Ext/Blend/Val", |m| {
                    m.str(preset);
                    m.floats(&[value]);
                });
            }
        }
        message(face, "/VMC/Ext/Blend/Apply", |_| {});
        (&self.body, &self.face)
    }
}

/// `#bundle`, then the time tag "immediately".
fn bundle_header(buf: &mut Vec<u8>) {
    buf.extend_from_slice(b"#bundle\0");
    buf.extend_from_slice(&1u64.to_be_bytes());
}

/// One message's arguments as they are written: the type tags and the data go to two places, so the tags are written
/// into a fixed buffer first and copied in once the arguments are known.
struct Args<'a> {
    tags: [u8; 16],
    count: usize,
    data: &'a mut Vec<u8>,
}

impl Args<'_> {
    fn tag(&mut self, tag: u8) {
        if let Some(slot) = self.tags.get_mut(self.count) {
            *slot = tag;
            self.count += 1;
        }
    }

    fn str(&mut self, s: &str) {
        self.tag(b's');
        put_str(self.data, s);
    }

    fn floats(&mut self, values: &[f32]) {
        for v in values {
            self.tag(b'f');
            self.data.extend_from_slice(&v.to_be_bytes());
        }
    }

    fn ints(&mut self, values: &[i32]) {
        for v in values {
            self.tag(b'i');
            self.data.extend_from_slice(&v.to_be_bytes());
        }
    }
}

/// Appends a bundle element: its length, then the message at `address` whose arguments `write` adds.
fn message(buf: &mut Vec<u8>, address: &str, write: impl FnOnce(&mut Args<'_>)) {
    let at = buf.len();
    buf.extend_from_slice(&[0; 4]);
    put_str(buf, address);
    // The arguments go after a gap for the tags, which are only known once they are written.
    let mut data = std::mem::take(buf);
    let tags_at = data.len();
    let mut args = Args { tags: [0; 16], count: 0, data: &mut data };
    // Reserve the tag string's room: a comma, up to 16 tags, a terminator, padded to 4.
    let reserve = pad4(1 + 16 + 1);
    args.data.resize(tags_at + reserve, 0);
    write(&mut args);
    let (tags, count) = (args.tags, args.count);
    let tag_len = pad4(1 + count + 1);
    data[tags_at] = b',';
    data[tags_at + 1..tags_at + 1 + count].copy_from_slice(&tags[..count]);
    data[tags_at + 1 + count..tags_at + reserve].fill(0);
    // Close the gap left by the reservation beyond what the tags need.
    data.drain(tags_at + tag_len..tags_at + reserve);
    *buf = data;
    let len = u32::try_from(buf.len() - at - 4).unwrap_or(u32::MAX);
    buf[at..at + 4].copy_from_slice(&len.to_be_bytes());
}

fn put_str(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(s.as_bytes());
    buf.push(0);
    buf.resize(pad4(buf.len()), 0);
}

const fn pad4(n: usize) -> usize {
    (n + 3) & !3
}

/// The words of a [`VmcPose`] in a [`PoseSlot`]: the hips offset (3), a quaternion per bone (4 each), the 52 channels,
/// the present mask's low 64 bits, where the ARKit channels are (2), the time (2).
const WORDS: usize = 3 + 4 * HUMANOID_COUNT + ARKIT_COUNT + 2 + 2;

/// The newest pose, from the perform thread to the sender: a sequence lock over atomic words, so publishing is
/// wait-free and allocates nothing, and the reader retries on the rare frame it catches mid-write.
pub struct PoseSlot {
    seq: AtomicU64,
    words: [AtomicU32; WORDS],
}

impl Default for PoseSlot {
    fn default() -> Self {
        Self { seq: AtomicU64::new(0), words: std::array::from_fn(|_| AtomicU32::new(0)) }
    }
}

impl std::fmt::Debug for PoseSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PoseSlot").field("seq", &self.seq.load(Ordering::Relaxed)).finish_non_exhaustive()
    }
}

impl PoseSlot {
    /// Leaves `frame` as the newest. One writer at a time (the perform thread). Hot path: wait-free, no allocation.
    pub fn publish(&self, frame: &VmcPose) {
        self.publish_parts(&frame.pose, &frame.blend, frame.present, frame.time);
    }

    /// [`publish`](Self::publish) from its parts: `blend` holds at least the 52 `ARKit` channels (more are ignored,
    /// fewer read as 0). Hot path, as `publish`.
    pub fn publish_parts(&self, pose: &HumanoidPose, blend: &[f32], present: ChannelMask, time: f64) {
        self.seq.fetch_add(1, Ordering::Relaxed);
        fence(Ordering::Release);
        let mut i = 0;
        let mut put = |bits: u32| {
            if let Some(word) = self.words.get(i) {
                word.store(bits, Ordering::Relaxed);
            }
            i += 1;
        };
        for v in pose.hips_offset.to_array() {
            put(v.to_bits());
        }
        for q in pose.rotations {
            for v in q.to_array() {
                put(v.to_bits());
            }
        }
        for c in 0..ARKIT_COUNT {
            put(blend.get(c).copied().unwrap_or(0.0).to_bits());
        }
        // The ARKit channels are the low 52 bits; nothing above them is sent.
        let mask = present.bits();
        #[allow(clippy::cast_possible_truncation)] // split into 32-bit halves
        {
            put(mask as u32);
            put((mask >> 32) as u32);
            let time = time.to_bits();
            put(time as u32);
            put((time >> 32) as u32);
        }
        self.seq.fetch_add(1, Ordering::Release);
    }

    /// The newest frame and its sequence number (even, and growing by 2 per publish), or `None` before the first.
    pub fn read(&self) -> Option<(u64, VmcPose)> {
        loop {
            let before = self.seq.load(Ordering::Acquire);
            if before == 0 {
                return None;
            }
            if before % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let mut words = [0u32; WORDS];
            for (out, word) in words.iter_mut().zip(&self.words) {
                *out = word.load(Ordering::Relaxed);
            }
            fence(Ordering::Acquire);
            if self.seq.load(Ordering::Relaxed) == before {
                return Some((before, decode(&words)));
            }
        }
    }
}

fn decode(words: &[u32; WORDS]) -> VmcPose {
    let f = |i: usize| f32::from_bits(words.get(i).copied().unwrap_or(0));
    let mut frame = VmcPose::default();
    frame.pose.hips_offset = Vec3::new(f(0), f(1), f(2));
    for (b, rotation) in frame.pose.rotations.iter_mut().enumerate() {
        let at = 3 + 4 * b;
        *rotation = Quat::from_xyzw(f(at), f(at + 1), f(at + 2), f(at + 3));
    }
    let blend_at = 3 + 4 * HUMANOID_COUNT;
    for (c, value) in frame.blend.iter_mut().enumerate() {
        *value = f(blend_at + c);
    }
    let w = |i: usize| u64::from(words.get(i).copied().unwrap_or(0));
    let tail = blend_at + ARKIT_COUNT;
    frame.present = ChannelMask::from_bits_truncate(u128::from(w(tail) | (w(tail + 1) << 32)));
    frame.time = f64::from_bits(w(tail + 2) | (w(tail + 3) << 32));
    frame
}

/// A running VMC sender: a thread that sends the slot's newest pose to the receiver, at most [`SEND_HZ`] times a
/// second and only when there is a new one. Dropping it stops the thread.
pub struct VmcSender {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    sent: Arc<AtomicU64>,
    local: SocketAddr,
}

impl std::fmt::Debug for VmcSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VmcSender").field("sent", &self.sent()).finish_non_exhaustive()
    }
}

impl VmcSender {
    /// Starts sending what `slot` holds as `config` says.
    ///
    /// # Errors
    /// The socket could not be opened, or the thread started.
    pub fn start(slot: Arc<PoseSlot>, config: VmcOutConfig) -> io::Result<Self> {
        let bind: SocketAddr = if config.target.is_ipv4() {
            (Ipv4Addr::UNSPECIFIED, 0).into()
        } else {
            (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
        };
        let socket = UdpSocket::bind(bind)?;
        let local = socket.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let sent = Arc::new(AtomicU64::new(0));
        let (stop_flag, sent_count) = (Arc::clone(&stop), Arc::clone(&sent));
        let thread = std::thread::Builder::new().name("omuzaki-vmc-out".to_owned()).spawn(move || {
            let mut encoder = VmcEncoder::default();
            let mut last = 0;
            let period = Duration::from_secs(1) / SEND_HZ;
            while !stop_flag.load(Ordering::Relaxed) {
                if let Some((seq, frame)) = slot.read().filter(|(seq, _)| *seq != last) {
                    last = seq;
                    let (body, face) = encoder.encode(&frame, &config);
                    // A receiver that is not running refuses the datagram; the next frame tries again.
                    let ok = socket.send_to(body, config.target).is_ok() & socket.send_to(face, config.target).is_ok();
                    if ok {
                        sent_count.fetch_add(1, Ordering::Relaxed);
                    }
                }
                std::thread::sleep(period);
            }
        })?;
        Ok(Self { stop, thread: Some(thread), sent, local })
    }

    /// Frames sent so far.
    pub fn sent(&self) -> u64 {
        self.sent.load(Ordering::Relaxed)
    }

    /// The local address it sends from.
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }
}

impl Drop for VmcSender {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

/// The performer's tap: every `ARKit` channel is sent, since the performer drives them all (the life layer moves an
/// untracked face too).
impl PoseTap for PoseSlot {
    fn publish(&self, pose: &HumanoidPose, channels: &[f32], time: Seconds) {
        self.publish_parts(pose, channels, ChannelMask::ARKIT, time.0);
    }
}

/// The App's VMC output: what the `input.vmc_out` op starts and stops. One sender at a time, over one slot that
/// outlives it, so the perform thread's tap stays valid across restarts.
#[derive(Debug, Default)]
pub struct VmcOut {
    slot: Arc<PoseSlot>,
    sender: Option<(VmcSender, SocketAddr)>,
    config: VmcOutConfig,
}

impl VmcOut {
    /// An output that sends as `config` says (its target is replaced by each start's).
    pub fn new(config: VmcOutConfig) -> Self {
        Self { config, ..Self::default() }
    }
}

impl PoseOutput for VmcOut {
    fn start(&mut self, target: SocketAddr) -> io::Result<Arc<dyn PoseTap>> {
        self.stop();
        let sender = VmcSender::start(Arc::clone(&self.slot), self.config.with_target(target))?;
        self.sender = Some((sender, target));
        Ok(Arc::clone(&self.slot) as Arc<dyn PoseTap>)
    }

    fn stop(&mut self) {
        self.sender = None;
    }

    fn status(&self) -> PoseOutStatus {
        self.sender.as_ref().map(|(sender, target)| PoseOutStatus::sending(*target, sender.sent())).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::osc::{self, Arg};

    fn sample() -> VmcPose {
        let mut frame = VmcPose::default();
        frame.pose.set(HumanoidBone::Head, Quat::from_rotation_y(0.3));
        frame.pose.hips_offset = Vec3::new(0.25, -0.125, 0.0);
        frame.blend[Channel::JAW_OPEN.index()] = 0.4;
        frame.blend[Channel::EYE_BLINK_LEFT.index()] = 0.9;
        frame.present = ChannelMask::from_channels(&[Channel::JAW_OPEN, Channel::EYE_BLINK_LEFT]);
        frame.time = 12.5;
        frame
    }

    /// Every message in `packet` as (address, arguments).
    fn messages(packet: &[u8]) -> Vec<(String, Vec<String>)> {
        let mut out = Vec::new();
        osc::for_each_message(packet, &mut |m| {
            let args = m.args().map(|a| a.map(|a| format!("{a:?}")).unwrap_or_default()).collect();
            out.push((m.address.to_owned(), args));
        })
        .unwrap();
        out
    }

    #[test]
    fn the_body_bundle_names_every_bone_the_unity_way_and_places_the_hips() {
        let mut encoder = VmcEncoder::default();
        let (body, _) = encoder.encode(&sample(), &VmcOutConfig::default());
        let body = messages(body);
        let bones: Vec<&str> =
            body.iter().filter(|(a, _)| a == "/VMC/Ext/Bone/Pos").map(|(_, args)| args[0].as_str()).collect();
        assert_eq!(bones.len(), HUMANOID_COUNT);
        assert!(bones.contains(&"Str(\"LeftThumbIntermediate\")"), "{bones:?}");
        let hips = body.iter().find(|(a, args)| a == "/VMC/Ext/Bone/Pos" && args[0] == "Str(\"Hips\")").unwrap();
        // x flips into Unity's frame; the rest height is added.
        assert_eq!(hips.1[1..4], ["Float(-0.25)", "Float(0.775)", "Float(0.0)"]);
        assert!(body.iter().any(|(a, args)| a == "/VMC/Ext/T" && args == &["Float(12.5)"]));
        assert_eq!(body.first().map(|(a, _)| a.as_str()), Some("/VMC/Ext/Root/Pos"));
    }

    #[test]
    fn the_face_bundle_sends_what_is_driven_in_perfect_sync_spelling_then_applies() {
        let mut encoder = VmcEncoder::default();
        let (_, face) = encoder.encode(&sample(), &VmcOutConfig::default());
        let face = messages(face);
        let names: Vec<String> = face.iter().map(|(a, args)| format!("{a} {}", args.join(" "))).collect();
        assert_eq!(
            names,
            [
                "/VMC/Ext/Blend/Val Str(\"EyeBlinkLeft\") Float(0.9)",
                "/VMC/Ext/Blend/Val Str(\"JawOpen\") Float(0.4)",
                "/VMC/Ext/Blend/Val Str(\"Blink_L\") Float(0.9)",
                "/VMC/Ext/Blend/Apply ",
            ]
        );
    }

    #[test]
    fn the_receiver_reads_back_what_the_encoder_wrote() {
        use crate::protocol::Decoder;
        use crate::protocol::vmc::VmcDecoder;
        let mut encoder = VmcEncoder::default();
        let (body, face) = encoder.encode(&sample(), &VmcOutConfig::default());
        let (body, face) = (body.to_vec(), face.to_vec());
        let mut decoder = VmcDecoder::new();
        let mut out = crate::protocol::blank();
        assert_eq!(decoder.decode(&body, &mut out).unwrap(), None);
        decoder.decode(&face, &mut out).unwrap().unwrap();
        assert!((out.get(Channel::JAW_OPEN) - 0.4).abs() < 1e-6);
        assert!((out.get(Channel::EYE_BLINK_LEFT) - 0.9).abs() < 1e-6);
        let head = out.head.unwrap();
        assert!((head.yaw - 0.3).abs() < 1e-5, "the head's turn survives both frame changes: {head:?}");
    }

    #[test]
    fn the_slot_hands_over_the_newest_frame_whole() {
        let slot = PoseSlot::default();
        assert!(slot.read().is_none());
        slot.publish(&sample());
        let (seq, frame) = slot.read().unwrap();
        assert_eq!((seq, frame), (2, sample()));
        let mut later = sample();
        later.time = 13.0;
        slot.publish(&later);
        assert_eq!(slot.read().unwrap(), (4, later));
    }

    #[test]
    fn the_output_sends_what_the_perform_thread_publishes_to_a_receiver() {
        use crate::protocol::Decoder;
        use crate::protocol::vmc::VmcDecoder;
        let receiver = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        receiver.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut out = VmcOut::default();
        assert_eq!(out.status(), PoseOutStatus::default());
        let tap = out.start(receiver.local_addr().unwrap()).unwrap();
        let frame = sample();
        let mut channels = [0.0; 64];
        channels[..ARKIT_COUNT].copy_from_slice(&frame.blend);
        tap.publish(&frame.pose, &channels, Seconds(frame.time));
        let mut decoder = VmcDecoder::new();
        let mut got = crate::protocol::blank();
        let mut buf = vec![0u8; 65_536];
        // The body, then the face, whose Apply completes the frame.
        let face = loop {
            let n = receiver.recv(&mut buf).unwrap();
            if decoder.decode(&buf[..n], &mut got).unwrap().is_some() {
                break got;
            }
        };
        assert!((face.get(Channel::JAW_OPEN) - 0.4).abs() < 1e-6);
        assert!((face.head.unwrap().yaw - 0.3).abs() < 1e-5);
        assert_eq!(out.status().target, receiver.local_addr().ok());
        out.stop();
        assert_eq!(out.status(), PoseOutStatus::default());
    }

    #[test]
    fn encoding_again_allocates_into_the_same_buffers() {
        let mut encoder = VmcEncoder::default();
        encoder.encode(&sample(), &VmcOutConfig::default());
        let capacity = (encoder.body.capacity(), encoder.face.capacity());
        let (body_ptr, face_ptr) = (encoder.body.as_ptr(), encoder.face.as_ptr());
        encoder.encode(&sample(), &VmcOutConfig::default());
        assert_eq!((encoder.body.capacity(), encoder.face.capacity()), capacity);
        assert_eq!((encoder.body.as_ptr(), encoder.face.as_ptr()), (body_ptr, face_ptr));
        let _ = Arg::Nil;
    }

    #[test]
    fn a_pose_sent_out_comes_back_in_as_the_same_body() {
        use crate::protocol::Decoder;
        use crate::protocol::vmc::VmcDecoder;
        let mut frame = sample();
        for (bone, q) in [
            (HumanoidBone::Hips, Quat::from_rotation_y(0.4)),
            (HumanoidBone::Spine, Quat::from_rotation_x(0.2)),
            (HumanoidBone::LeftUpperArm, Quat::from_euler(omuzaki_core::glam::EulerRot::XYZ, 0.3, -0.2, -1.1)),
            (HumanoidBone::RightLowerArm, Quat::from_rotation_y(0.9)),
            (HumanoidBone::LeftThumbMetacarpal, Quat::from_rotation_z(0.5)),
            (HumanoidBone::RightUpperLeg, Quat::from_rotation_x(-0.7)),
        ] {
            frame.pose.set(bone, q);
        }
        let mut encoder = VmcEncoder::default();
        let (body, face) = encoder.encode(&frame, &VmcOutConfig::default());
        let (body, face) = (body.to_vec(), face.to_vec());
        let mut decoder = VmcDecoder::new();
        let mut track = crate::protocol::blank();
        decoder.decode(&body, &mut track).unwrap();
        decoder.decode(&face, &mut track).unwrap().unwrap();
        let mut back = omuzaki_track_api::body::BodyFrame::empty(0, omuzaki_core::time::Seconds(0.0));
        assert!(decoder.body(&mut back));
        for bone in HumanoidBone::ALL {
            if matches!(
                bone,
                HumanoidBone::Neck
                    | HumanoidBone::Head
                    | HumanoidBone::LeftEye
                    | HumanoidBone::RightEye
                    | HumanoidBone::Jaw
            ) {
                assert!(!back.mask.contains(bone), "{bone:?} is the face's");
                continue;
            }
            assert!(back.mask.contains(bone), "{bone:?}");
            let (sent, got) = (frame.pose.get(bone), back.pose.get(bone));
            assert!(sent.angle_between(got) < 1e-4, "{bone:?}: sent {sent}, got {got}");
        }
    }
}
