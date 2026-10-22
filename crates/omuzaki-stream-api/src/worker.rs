//! The worker protocol (`docs/design/supervisor.md` §4.2, §15.2): the messages between the output host and a
//! supervised worker, the shared objects one worker generation is given, and their framing.
//!
//! Each worker's control channel is one duplex byte stream, the worker's stdin (a socketpair on Unix, a duplex named
//! pipe on Windows). A frame is a fixed magic, a little-endian `u32` length of at most [`MAX_CONTROL_MESSAGE`] and a
//! `postcard` body. On Unix a message may carry descriptors (shared-memory objects): [`write_msg_fds`] attaches them
//! to the frame's first byte with `SCM_RIGHTS`, and [`read_msg_fds`] reads a frame exactly, so a descriptor can only
//! arrive with the first byte of the frame it belongs to.
//!
//! Real-time loops never touch the channel: a worker's writer thread sends coalesced beats and drains a small
//! announcement queue; the host reads on a thread of its own.

use std::io::{Read, Write};

use omuzaki_core::glam::UVec2;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::encoder::{EncodeConfig, StreamSpec};
use crate::error::StreamError;
use crate::supervisor::{WorkerId, WorkerKind};

/// The protocol's version, checked in `Hello`.
pub const PROTOCOL_VERSION: u32 = 1;

/// The longest frame body.
pub const MAX_CONTROL_MESSAGE: usize = 64 * 1024;

/// The bytes every frame starts with.
pub const MAGIC: [u8; 4] = *b"OMZW";

/// A frame's header: the magic and the body's length.
const HEADER: usize = 8;

/// The most descriptors one message carries: a stage's `Start`, one per [`ChannelRole`].
pub const MAX_FDS: usize = 8;

/// What a worker says to the host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum WorkerMsg {
    /// The first message: who it is.
    #[non_exhaustive]
    Hello {
        /// Its kind.
        kind: WorkerKind,
        /// Its protocol version.
        version: u32,
        /// Its process id.
        pid: u32,
        /// The generation it was started as (from its arguments).
        generation: u32,
    },
    /// Progress, coalesced every 50 ms by the worker's writer thread.
    #[non_exhaustive]
    Beat {
        /// The main loop's iteration counter.
        progress: u64,
        /// The kind's work counter: frames, packets or blocks.
        work: u64,
    },
    /// An encoder's answer to `Start`: its stream, extradata included.
    #[non_exhaustive]
    Opened {
        /// The stream it produces.
        spec: StreamSpec,
    },
    /// Started and ready to work, with its start-up split in milliseconds.
    #[non_exhaustive]
    Ready {
        /// Opening the device.
        device_ms: f32,
        /// Building pipelines.
        pipelines_ms: f32,
        /// Uploading resources.
        upload_ms: f32,
    },
    /// A stage frame complete in `slot`.
    #[non_exhaustive]
    Frame {
        /// The slot.
        slot: u32,
        /// The frame's index.
        index: u64,
    },
    /// An encoder is done reading `slot`.
    #[non_exhaustive]
    SlotFree {
        /// The slot.
        slot: u32,
    },
    /// A clean, explained exit (a configuration error); the host still counts it.
    #[non_exhaustive]
    Gone {
        /// Why.
        reason: String,
    },
}

impl WorkerMsg {
    /// `Hello` for this process.
    pub fn hello(kind: WorkerKind, generation: u32) -> Self {
        Self::Hello { kind, version: PROTOCOL_VERSION, pid: std::process::id(), generation }
    }

    /// `Beat { progress, work }`.
    pub fn beat(progress: u64, work: u64) -> Self {
        Self::Beat { progress, work }
    }

    /// `Opened { spec }`.
    pub fn opened(spec: StreamSpec) -> Self {
        Self::Opened { spec }
    }

    /// `Ready` with the start-up split.
    pub fn ready(device_ms: f32, pipelines_ms: f32, upload_ms: f32) -> Self {
        Self::Ready { device_ms, pipelines_ms, upload_ms }
    }

    /// `Frame { slot, index }`.
    pub fn frame(slot: u32, index: u64) -> Self {
        Self::Frame { slot, index }
    }

    /// `SlotFree { slot }`.
    pub fn slot_free(slot: u32) -> Self {
        Self::SlotFree { slot }
    }

    /// `Gone { reason }`.
    pub fn gone(reason: impl Into<String>) -> Self {
        Self::Gone { reason: reason.into() }
    }
}

/// What the host says to a worker.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum HostMsg {
    /// Start: the generation's shared objects and the kind's parameters.
    #[non_exhaustive]
    Start {
        /// The shared objects.
        channels: ChannelNames,
        /// What the kind needs to start.
        params: StartParams,
    },
    /// An encoder's frame to encode is in `slot`.
    #[non_exhaustive]
    Frame {
        /// The slot.
        slot: u32,
        /// The frame's index.
        index: u64,
        /// Its presentation time, from the tick index.
        pts: i64,
    },
    /// The host is done with a stage's `slot`.
    #[non_exhaustive]
    SlotFree {
        /// The slot.
        slot: u32,
    },
    /// An encoder's next frame is a keyframe.
    Keyframe,
    /// The stage reloads the scene snapshot of revision `rev`.
    #[non_exhaustive]
    Scene {
        /// The revision.
        rev: u64,
    },
    /// A peer's new generation: switch to its object.
    #[non_exhaustive]
    Attach {
        /// Which channel.
        role: ChannelRole,
        /// The object.
        channel: ChannelRef,
        /// The peer's generation.
        generation: u32,
    },
    /// A source's current state, one latch per source.
    #[non_exhaustive]
    Source {
        /// The source worker.
        source: WorkerId,
        /// Whether it serves.
        up: bool,
    },
    /// An encoder flushes, sends its last packets and exits 0; others exit 0.
    Stop,
}

impl HostMsg {
    /// `Start { channels, params }`.
    pub fn start(channels: ChannelNames, params: StartParams) -> Self {
        Self::Start { channels, params }
    }

    /// `Frame { slot, index, pts }`.
    pub fn frame(slot: u32, index: u64, pts: i64) -> Self {
        Self::Frame { slot, index, pts }
    }

    /// `SlotFree { slot }`.
    pub fn slot_free(slot: u32) -> Self {
        Self::SlotFree { slot }
    }

    /// `Scene { rev }`.
    pub fn scene(rev: u64) -> Self {
        Self::Scene { rev }
    }

    /// `Attach { role, channel, generation }`.
    pub fn attach(role: ChannelRole, channel: ChannelRef, generation: u32) -> Self {
        Self::Attach { role, channel, generation }
    }

    /// `Source { source, up }`.
    pub fn source(source: WorkerId, up: bool) -> Self {
        Self::Source { source, up }
    }

    /// Whether it carries state that is latched and replayed after every `Start` (`Scene`, `Source`, `Attach`), as
    /// opposed to a command about one generation's objects.
    pub fn is_state(&self) -> bool {
        matches!(self, Self::Scene { .. } | Self::Source { .. } | Self::Attach { .. })
    }

    /// The descriptor positions it names, in order: `Start`'s entries and `Attach`'s object.
    pub fn fd_positions(&self) -> Vec<u16> {
        let refs: Vec<&ChannelRef> = match self {
            Self::Start { channels, .. } => channels.entries.iter().map(|(_, r)| r).collect(),
            Self::Attach { channel, .. } => vec![channel],
            _ => Vec::new(),
        };
        refs.into_iter()
            .filter_map(|r| match r {
                ChannelRef::Fd { position } => Some(*position),
                ChannelRef::Name { .. } => None,
            })
            .collect()
    }
}

/// What a worker of each kind needs to start, beyond its channels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum StartParams {
    /// The stage worker.
    #[non_exhaustive]
    Stage {
        /// The scene snapshot to load.
        scene_rev: u64,
        /// The frame size.
        size: UVec2,
        /// The frame rate.
        fps: (u32, u32),
    },
    /// The tracker worker.
    #[non_exhaustive]
    Tracker {
        /// The camera's stable id and the tracker config name.
        source: String,
    },
    /// The audio worker.
    Audio,
    /// An encoder worker.
    #[non_exhaustive]
    Encoder {
        /// What to encode.
        config: EncodeConfig,
        /// The take's frame index it continues from.
        first_index: u64,
    },
    /// An adapter task (never serialized: tasks get their parameters as a value).
    #[non_exhaustive]
    Adapter {
        /// The registered adapter's id.
        adapter: String,
    },
}

impl StartParams {
    /// `Stage { scene_rev, size, fps }`.
    pub fn stage(scene_rev: u64, size: UVec2, fps: (u32, u32)) -> Self {
        Self::Stage { scene_rev, size, fps }
    }

    /// `Tracker { source }`.
    pub fn tracker(source: impl Into<String>) -> Self {
        Self::Tracker { source: source.into() }
    }

    /// `Encoder { config, first_index }`.
    pub fn encoder(config: EncodeConfig, first_index: u64) -> Self {
        Self::Encoder { config, first_index }
    }

    /// `Adapter { adapter }`.
    pub fn adapter(adapter: impl Into<String>) -> Self {
        Self::Adapter { adapter: adapter.into() }
    }

    /// The kind of worker these parameters start.
    pub fn kind(&self) -> WorkerKind {
        match self {
            Self::Stage { .. } => WorkerKind::Renderer,
            Self::Tracker { .. } => WorkerKind::Tracker,
            Self::Audio => WorkerKind::Audio,
            Self::Encoder { .. } => WorkerKind::Encoder,
            Self::Adapter { .. } => WorkerKind::Adapter,
        }
    }
}

/// The shared objects of one worker generation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ChannelNames {
    /// The generation.
    pub generation: u32,
    /// Each object, by role.
    pub entries: Vec<(ChannelRole, ChannelRef)>,
}

impl ChannelNames {
    /// `entries` of `generation`.
    pub fn new(generation: u32, entries: Vec<(ChannelRole, ChannelRef)>) -> Self {
        Self { generation, entries }
    }

    /// The object of `role`, if the generation has one.
    pub fn get(&self, role: ChannelRole) -> Option<&ChannelRef> {
        self.entries.iter().find(|(r, _)| *r == role).map(|(_, c)| c)
    }
}

/// How one object travels: on Unix as a descriptor sent with the same message (its position in that message's
/// descriptor list), on Windows as the section's random name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ChannelRef {
    /// A descriptor of the message.
    #[non_exhaustive]
    Fd {
        /// Its position in the message's descriptors.
        position: u16,
    },
    /// A named section.
    #[non_exhaustive]
    Name {
        /// The name.
        name: String,
    },
}

impl ChannelRef {
    /// `Fd { position }`.
    pub fn fd(position: u16) -> Self {
        Self::Fd { position }
    }

    /// `Name { name }`.
    pub fn name(name: impl Into<String>) -> Self {
        Self::Name { name: name.into() }
    }
}

/// What a shared object carries.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ChannelRole {
    /// Frames: the stage's output slots, or an encoder's input slots.
    Frames,
    /// The studio's preview frames.
    Preview,
    /// Tracker frames.
    Tracker,
    /// Voice cues.
    VoiceCues,
    /// Live inputs from the studio.
    LiveInputs,
    /// Audio samples.
    Audio,
    /// Encoded packets.
    Packets,
}

/// The host's own handle to one shared object, kept while the generation lives (and by an `Attach` latch, which
/// sends a fresh clone on every replay): a descriptor on Unix, the section handle and its name on Windows.
#[derive(Debug)]
pub struct ChannelHandle {
    #[cfg(unix)]
    fd: std::os::fd::OwnedFd,
    #[cfg(windows)]
    handle: std::os::windows::io::OwnedHandle,
    #[cfg(windows)]
    name: String,
}

impl ChannelHandle {
    /// The handle of the object `fd`.
    #[cfg(unix)]
    pub fn from_fd(fd: std::os::fd::OwnedFd) -> Self {
        Self { fd }
    }

    /// The object's descriptor, to send.
    #[cfg(unix)]
    pub fn fd(&self) -> std::os::fd::BorrowedFd<'_> {
        use std::os::fd::AsFd as _;
        self.fd.as_fd()
    }

    /// The handle of the section `handle`, named `name`.
    #[cfg(windows)]
    pub fn from_section(handle: std::os::windows::io::OwnedHandle, name: String) -> Self {
        Self { handle, name }
    }

    /// The section's name, to send.
    #[cfg(windows)]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Another handle to the same object.
    ///
    /// # Errors
    /// The OS's, when it can't duplicate the handle.
    pub fn try_clone(&self) -> std::io::Result<ChannelHandle> {
        #[cfg(unix)]
        {
            Ok(Self { fd: self.fd.try_clone()? })
        }
        #[cfg(windows)]
        {
            Ok(Self { handle: self.handle.try_clone()?, name: self.name.clone() })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
        }
    }
}

fn protocol(what: impl Into<String>) -> StreamError {
    StreamError::Protocol(what.into())
}

fn io(e: &std::io::Error) -> StreamError {
    StreamError::Io(e.to_string())
}

/// The frame of `msg`: header and body.
fn frame<M: Serialize>(msg: &M) -> Result<Vec<u8>, StreamError> {
    let body = postcard::to_stdvec(msg).map_err(|e| protocol(format!("the message did not encode: {e}")))?;
    if body.len() > MAX_CONTROL_MESSAGE {
        return Err(protocol(format!("a {} byte message is over the {MAX_CONTROL_MESSAGE} byte limit", body.len())));
    }
    let len = u32::try_from(body.len()).map_err(|_| protocol("message length"))?;
    let mut out = Vec::with_capacity(HEADER + body.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// The body length a header announces.
fn body_len(header: &[u8; HEADER]) -> Result<usize, StreamError> {
    if header[..4] != MAGIC {
        return Err(protocol(format!("bad magic {:02x?}", &header[..4])));
    }
    let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
    if len > MAX_CONTROL_MESSAGE {
        return Err(protocol(format!("a {len} byte frame is over the {MAX_CONTROL_MESSAGE} byte limit")));
    }
    Ok(len)
}

fn decode<M: DeserializeOwned>(body: &[u8]) -> Result<M, StreamError> {
    postcard::from_bytes(body).map_err(|e| protocol(format!("the message did not decode: {e}")))
}

/// Writes `msg` as one frame.
///
/// # Errors
/// [`StreamError::Protocol`] for a message over the limit; [`StreamError::Io`] when the write fails.
pub fn write_msg<W: Write, M: Serialize>(w: &mut W, msg: &M) -> Result<(), StreamError> {
    let bytes = frame(msg)?;
    w.write_all(&bytes).and_then(|()| w.flush()).map_err(|e| io(&e))
}

/// Reads one frame, reusing `buf` for its body; `None` at the end of the stream between frames.
///
/// # Errors
/// [`StreamError::Protocol`] for a bad magic, an over-long length or a body that does not decode;
/// [`StreamError::Io`] for a read error or a stream that ends inside a frame.
pub fn read_msg<R: Read, M: DeserializeOwned>(r: &mut R, buf: &mut Vec<u8>) -> Result<Option<M>, StreamError> {
    let mut header = [0_u8; HEADER];
    let mut got = 0;
    while got < HEADER {
        match r.read(&mut header[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(StreamError::Io("the stream ended inside a frame header".into())),
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(io(&e)),
        }
    }
    let len = body_len(&header)?;
    buf.clear();
    buf.resize(len, 0);
    r.read_exact(buf).map_err(|e| io(&e))?;
    decode(buf).map(Some)
}

#[cfg(unix)]
pub use unix::{read_msg_fds, write_msg_fds};

#[cfg(unix)]
mod unix {
    use std::io::{IoSlice, IoSliceMut, Write as _};
    use std::mem::MaybeUninit;
    use std::os::fd::{BorrowedFd, OwnedFd};
    use std::os::unix::net::UnixStream;

    use rustix::net::{
        RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, SendAncillaryBuffer,
        SendAncillaryMessage, SendFlags, recvmsg, sendmsg,
    };
    use serde::Serialize;
    use serde::de::DeserializeOwned;

    use super::{HEADER, MAX_FDS, body_len, decode, frame, io, protocol};
    use crate::error::StreamError;

    /// Room for [`MAX_FDS`] descriptors, with headroom for the alignment `align_for_cmsghdr` takes.
    const CMSG_ROOM: usize = rustix::cmsg_space!(ScmRights(MAX_FDS)) + 16;

    /// Writes `msg` as one frame with `fds` attached to its first byte (`SCM_RIGHTS`).
    ///
    /// # Errors
    /// [`StreamError::Config`] for more than [`MAX_FDS`] descriptors; [`StreamError::Protocol`] for a message over the
    /// limit; [`StreamError::Io`] when the write fails.
    pub fn write_msg_fds<M: Serialize>(s: &UnixStream, msg: &M, fds: &[BorrowedFd<'_>]) -> Result<(), StreamError> {
        if fds.len() > MAX_FDS {
            return Err(StreamError::Config(format!("{} descriptors in one message, over {MAX_FDS}", fds.len())));
        }
        let bytes = frame(msg)?;
        let mut rest = &bytes[..];
        if !fds.is_empty() {
            let mut space = [MaybeUninit::<u8>::uninit(); CMSG_ROOM];
            let mut control = SendAncillaryBuffer::new(&mut space);
            if !control.push(SendAncillaryMessage::ScmRights(fds)) {
                return Err(StreamError::Config("the descriptors do not fit the control buffer".into()));
            }
            // The first byte alone carries the descriptors; the rest is a plain write.
            let sent = loop {
                match sendmsg(s, &[IoSlice::new(&bytes[..1])], &mut control, SendFlags::NOSIGNAL) {
                    Err(rustix::io::Errno::INTR) => {}
                    other => break other,
                }
            };
            match sent {
                Ok(1) => rest = &bytes[1..],
                Ok(_) => return Err(StreamError::Io("the descriptor byte was not sent".into())),
                Err(e) => return Err(io(&e.into())),
            }
        }
        let mut w = s;
        w.write_all(rest).and_then(|()| w.flush()).map_err(|e| io(&e))
    }

    /// Reads exactly `out.len()` bytes; descriptors that come with them are appended to `fds`. `Ok(false)` when the
    /// stream ended before the first byte and `eof_ok`.
    fn read_exact_fds(
        s: &UnixStream,
        out: &mut [u8],
        fds: &mut Vec<OwnedFd>,
        eof_ok: bool,
    ) -> Result<bool, StreamError> {
        let mut got = 0;
        while got < out.len() {
            let mut space = [MaybeUninit::<u8>::uninit(); CMSG_ROOM];
            let mut control = RecvAncillaryBuffer::new(&mut space);
            let mut iov = [IoSliceMut::new(&mut out[got..])];
            let msg = match recvmsg(s, &mut iov, &mut control, RecvFlags::CMSG_CLOEXEC) {
                Ok(msg) => msg,
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => return Err(io(&e.into())),
            };
            for message in control.drain() {
                if let RecvAncillaryMessage::ScmRights(received) = message {
                    fds.extend(received);
                }
            }
            if msg.flags.contains(ReturnFlags::CTRUNC) {
                return Err(protocol("more descriptors arrived than a message may carry"));
            }
            if msg.bytes == 0 {
                if got == 0 && eof_ok {
                    return Ok(false);
                }
                return Err(StreamError::Io("the stream ended inside a frame".into()));
            }
            got += msg.bytes;
        }
        Ok(true)
    }

    /// Reads one frame and the descriptors that came with its first byte, in order, reusing `buf` for the body;
    /// `None` at the end of the stream between frames. Every `ChannelRef::Fd` position the message names must name
    /// one of them, and none may be left over.
    ///
    /// # Errors
    /// [`StreamError::Protocol`] for a bad frame, descriptors that arrive anywhere but a frame's first byte, or
    /// descriptors that do not match the message's positions one to one; [`StreamError::Io`] for a read error.
    pub fn read_msg_fds<M: DeserializeOwned + super::FdPositions>(
        s: &UnixStream,
        buf: &mut Vec<u8>,
    ) -> Result<Option<(M, Vec<OwnedFd>)>, StreamError> {
        let mut fds = Vec::new();
        let mut header = [0_u8; HEADER];
        if !read_exact_fds(s, &mut header[..1], &mut fds, true)? {
            return Ok(None);
        }
        let first = fds.len();
        read_exact_fds(s, &mut header[1..], &mut fds, false)?;
        let len = body_len(&header)?;
        buf.clear();
        buf.resize(len, 0);
        read_exact_fds(s, buf, &mut fds, false)?;
        if fds.len() != first {
            return Err(protocol("descriptors arrived after a frame's first byte"));
        }
        let msg: M = decode(buf)?;
        let mut positions = msg.fd_positions();
        positions.sort_unstable();
        let expected: Vec<u16> = (0..u16::try_from(fds.len()).unwrap_or(u16::MAX)).collect();
        if positions != expected {
            return Err(protocol(format!(
                "the message names descriptors {positions:?} but {} arrived with it",
                fds.len()
            )));
        }
        Ok(Some((msg, fds)))
    }
}

/// The descriptor positions a message names (for [`read_msg_fds`]'s check).
pub trait FdPositions {
    /// The positions, in any order.
    fn fd_positions(&self) -> Vec<u16>;
}

impl FdPositions for HostMsg {
    fn fd_positions(&self) -> Vec<u16> {
        HostMsg::fd_positions(self)
    }
}

impl FdPositions for WorkerMsg {
    fn fd_positions(&self) -> Vec<u16> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::{AlphaKind, CodecId, VideoSpec};
    use crate::supervisor::Scope;

    fn config() -> EncodeConfig {
        let video = VideoSpec { size: UVec2::new(64, 32), fps: (30, 1), alpha: AlphaKind::Straight };
        EncodeConfig::new(video, CodecId::Vp9Alpha).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn every_message_round_trips_through_a_frame() -> Result<(), StreamError> {
        let host = [
            HostMsg::start(
                ChannelNames::new(3, vec![(ChannelRole::Frames, ChannelRef::fd(0)), (ChannelRole::Packets, ChannelRef::fd(1))]),
                StartParams::encoder(config(), 120),
            ),
            HostMsg::frame(2, 121, 121),
            HostMsg::Keyframe,
            HostMsg::scene(9),
            HostMsg::source(WorkerId::new(Scope::Host, 5, 1), true),
            HostMsg::attach(ChannelRole::Tracker, ChannelRef::name("Local\\x"), 4),
            HostMsg::Stop,
        ];
        let mut wire = Vec::new();
        for msg in &host {
            write_msg(&mut wire, msg)?;
        }
        let (mut r, mut buf) = (&wire[..], Vec::new());
        for msg in &host {
            assert_eq!(read_msg::<_, HostMsg>(&mut r, &mut buf)?.as_ref(), Some(msg));
        }
        assert_eq!(read_msg::<_, HostMsg>(&mut r, &mut buf)?, None);
        let spec = StreamSpec::video(CodecId::Vp9Alpha, "v", crate::time::Timebase { num: 1, den: 30 }, config().video)?
            .with_extradata(bytes::Bytes::from_static(&[1, 2, 3]));
        let worker = [WorkerMsg::hello(WorkerKind::Encoder, 3), WorkerMsg::beat(5, 4), WorkerMsg::opened(spec)];
        let mut wire = Vec::new();
        for msg in &worker {
            write_msg(&mut wire, msg)?;
        }
        let mut r = &wire[..];
        for msg in &worker {
            assert_eq!(read_msg::<_, WorkerMsg>(&mut r, &mut buf)?.as_ref(), Some(msg));
        }
        Ok(())
    }

    #[test]
    fn malformed_frames_are_protocol_faults() {
        let mut buf = Vec::new();
        let bad_magic = [b'X', b'X', b'X', b'X', 0, 0, 0, 0];
        assert!(matches!(read_msg::<_, HostMsg>(&mut &bad_magic[..], &mut buf), Err(StreamError::Protocol(_))));
        let mut long = MAGIC.to_vec();
        long.extend_from_slice(&(u32::MAX).to_le_bytes());
        assert!(matches!(read_msg::<_, HostMsg>(&mut &long[..], &mut buf), Err(StreamError::Protocol(_))));
        let mut junk = MAGIC.to_vec();
        junk.extend_from_slice(&3_u32.to_le_bytes());
        junk.extend_from_slice(&[0xFF, 0xFF, 0xFF]);
        assert!(matches!(read_msg::<_, HostMsg>(&mut &junk[..], &mut buf), Err(StreamError::Protocol(_))));
        let cut = &MAGIC[..3];
        assert!(matches!(read_msg::<_, HostMsg>(&mut &cut[..], &mut buf), Err(StreamError::Io(_))));
    }

    #[test]
    fn state_messages_are_the_latched_ones() {
        assert!(HostMsg::scene(1).is_state());
        assert!(HostMsg::source(WorkerId::new(Scope::Host, 1, 0), false).is_state());
        assert!(!HostMsg::Keyframe.is_state());
        assert!(!HostMsg::frame(0, 0, 0).is_state());
        assert_eq!(StartParams::encoder(config(), 0).kind(), WorkerKind::Encoder);
    }

    #[cfg(unix)]
    #[test]
    fn descriptors_travel_with_their_frame() -> Result<(), Box<dyn std::error::Error>> {
        use std::io::{Read as _, Seek as _, Write as _};
        use std::os::fd::AsFd as _;
        use std::os::unix::net::UnixStream;
        let (a, b) = UnixStream::pair()?;
        let mut file = tempfile_like()?;
        file.write_all(b"shared")?;
        let msg = HostMsg::start(
            ChannelNames::new(1, vec![(ChannelRole::Frames, ChannelRef::fd(0))]),
            StartParams::encoder(config(), 0),
        );
        write_msg_fds(&a, &HostMsg::Keyframe, &[])?;
        write_msg_fds(&a, &msg, &[file.as_fd()])?;
        write_msg_fds(&a, &HostMsg::Stop, &[])?;
        // A message naming a descriptor that is not sent is refused.
        write_msg_fds(&a, &msg, &[])?;
        drop(a);
        let mut buf = Vec::new();
        let (first, fds) = read_msg_fds::<HostMsg>(&b, &mut buf)?.ok_or("eof")?;
        assert_eq!((first, fds.len()), (HostMsg::Keyframe, 0));
        let (second, fds) = read_msg_fds::<HostMsg>(&b, &mut buf)?.ok_or("eof")?;
        assert_eq!(second, msg);
        let mut shared = std::fs::File::from(fds.into_iter().next().ok_or("no fd")?);
        shared.rewind()?;
        let mut text = String::new();
        shared.read_to_string(&mut text)?;
        assert_eq!(text, "shared");
        assert_eq!(read_msg_fds::<HostMsg>(&b, &mut buf)?.map(|(m, _)| m), Some(HostMsg::Stop));
        assert!(matches!(read_msg_fds::<HostMsg>(&b, &mut buf), Err(StreamError::Protocol(_))));
        Ok(())
    }

    /// An unnamed temporary file: an open descriptor to send.
    #[cfg(unix)]
    fn tempfile_like() -> std::io::Result<std::fs::File> {
        let path = std::env::temp_dir().join(format!("omuzaki-worker-test-{}", std::process::id()));
        let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path)?;
        std::fs::remove_file(&path)?;
        Ok(file)
    }
}
