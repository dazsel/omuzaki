//! The four wire protocols, each a [`Decoder`] from one UDP datagram into a [`TrackFrame`].
//!
//! | Protocol | Sender | Port (listen) | Handshake | Sender time |
//! |---|---|---|---|---|
//! | [`ifacialmocap`] | the iFacialMocap iPhone app | 49983 | a fixed string to the phone's 49983 | none |
//! | [`vtube_studio`] | VTube Studio on an iPhone | 21412 | a JSON request to the phone's 21412, renewed every second | `Timestamp` (ms) |
//! | [`live_link`] | Epic's Live Link Face | 11111 | none: the phone is set to send to this computer | frame number and rate |
//! | [`vmc`] | VMC senders (VSeeFace, XR Animator, mocopi, VMagicMirror) | 39539 | none | `/VMC/Ext/T` |
//!
//! Decoding allocates nothing: numbers are parsed from borrowed text, JSON is read into fixed fields with borrowed
//! keys, and the binary layouts are read in place.
//!
//! **Axes.** Each decoder turns its sender's head and eye angles into anim-api's convention (right-handed, +Y up, the
//! face looking along +Z; positive yaw turns toward the performer's left, positive pitch nods down, positive roll tilts
//! toward the right shoulder). The mapping per protocol is one small table ([`Axes`]) so a sign found wrong on a real
//! phone is a one-line fix; the retarget's `mirror` setting flips yaw and roll for a mirrored preview on top.

pub mod ifacialmocap;
pub mod live_link;
pub mod osc;
pub mod vmc;
pub mod vtube_studio;

use omuzaki_anim_api::{DeviceProfile, TrackFrame};
use omuzaki_core::time::Seconds;
use omuzaki_track_api::body::BodyFrame;
use omuzaki_track_api::camera::DeviceTime;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Which protocol a [`NetTracker`](crate::NetTracker) speaks.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    /// iFacialMocap (iPhone).
    Ifacialmocap,
    /// VTube Studio's iPhone face tracking.
    VtubeStudio,
    /// Epic's Live Link Face (iPhone).
    LiveLinkFace,
    /// The VMC protocol (OSC over UDP), from mocap and tracking apps.
    Vmc,
}

impl Protocol {
    /// Every protocol, in the order a picker lists them.
    pub const ALL: [Self; 4] = [Self::Ifacialmocap, Self::VtubeStudio, Self::LiveLinkFace, Self::Vmc];

    /// The UDP port this computer listens on unless the setting says otherwise: each app's own default.
    pub const fn default_port(self) -> u16 {
        match self {
            Self::Ifacialmocap => ifacialmocap::PORT,
            Self::VtubeStudio => vtube_studio::PORT,
            Self::LiveLinkFace => live_link::PORT,
            Self::Vmc => vmc::PORT,
        }
    }

    /// Whether this computer has to ask the phone for data, so the phone's address must be known.
    pub const fn needs_phone_address(self) -> bool {
        matches!(self, Self::Ifacialmocap | Self::VtubeStudio)
    }

    /// The stable id the picker and the `input.*` ops use, `net:<protocol>`.
    pub const fn device_id(self) -> &'static str {
        match self {
            Self::Ifacialmocap => "net:ifacialmocap",
            Self::VtubeStudio => "net:vtube_studio",
            Self::LiveLinkFace => "net:live_link_face",
            Self::Vmc => "net:vmc",
        }
    }

    /// The protocol a [`device_id`](Self::device_id) names.
    pub fn from_device_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.device_id() == id)
    }

    /// The name a picker shows.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ifacialmocap => "iPhone: iFacialMocap",
            Self::VtubeStudio => "iPhone: VTube Studio",
            Self::LiveLinkFace => "iPhone: Live Link Face",
            Self::Vmc => "VMC (mocap and tracking apps)",
        }
    }

    /// A fresh decoder for one session.
    pub fn decoder(self) -> Box<dyn Decoder> {
        match self {
            Self::Ifacialmocap => Box::new(ifacialmocap::IfacialmocapDecoder),
            Self::VtubeStudio => Box::new(vtube_studio::VtubeStudioDecoder),
            Self::LiveLinkFace => Box::new(live_link::LiveLinkDecoder),
            Self::Vmc => Box::new(vmc::VmcDecoder::new()),
        }
    }
}

/// What a decoded packet says about its own timing.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Stamp {
    /// The sender's capture time, when the protocol carries one: mapped onto the stage clock by `ClockMap`.
    pub sent: Option<DeviceTime>,
    /// The sender's frame counter, when the protocol carries one: gaps count as dropped frames.
    pub seq: Option<u64>,
}

/// Why a datagram was refused. The receiver counts it and carries on.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DecodeError {
    /// The datagram does not have the protocol's layout.
    #[error("not a {protocol:?} packet: {why}")]
    Format {
        /// The protocol that refused it.
        protocol: Protocol,
        /// What was wrong, for the log.
        why: &'static str,
    },
    /// An OSC packet (VMC) did not parse.
    #[error("VMC: {0}")]
    Osc(#[from] osc::OscError),
}

impl DecodeError {
    /// A [`DecodeError::Format`].
    pub const fn format(protocol: Protocol, why: &'static str) -> Self {
        Self::Format { protocol, why }
    }
}

/// One protocol's packet reader. A session owns one; it runs on the receiver thread.
pub trait Decoder: Send {
    /// Reads `packet`. When it completes a frame, `out` is overwritten with it (its `seq` and `time` are the
    /// receiver's to set) and the frame's [`Stamp`] is returned; `None` means the packet completed nothing (a VMC
    /// bundle before its `Apply`, a status packet). Hot on the receiver thread: allocates nothing.
    ///
    /// # Errors
    /// [`DecodeError`] for a datagram that is not this protocol's.
    fn decode(&mut self, packet: &[u8], out: &mut TrackFrame) -> Result<Option<Stamp>, DecodeError>;

    /// The body the sender has sent so far (PF-04, PF-07), read after [`decode`](Self::decode) completed a frame:
    /// fills `out`'s bones, mask and confidences (its `seq` and `time` are the receiver's to set) and returns whether
    /// the sender sends a body at all. Face-only protocols send none. Allocates nothing.
    fn body(&self, out: &mut BodyFrame) -> bool {
        let _ = out;
        false
    }
}

/// The frame every phone protocol starts from: `ARKit`-scaled values, tracking, nothing present yet.
pub(crate) fn blank() -> TrackFrame {
    TrackFrame::empty(0, Seconds(0.0), DeviceProfile::Arkit)
}

/// Where a sender's three angles go, and with which sign: `yaw = sign[0] * angles[index[0]]` and so on.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Axes {
    /// Which of the sender's three angles is yaw, pitch and roll.
    pub index: [usize; 3],
    /// The sign each one takes.
    pub sign: [f32; 3],
}

impl Axes {
    /// Yaw, pitch and roll from the sender's three angles, in radians.
    pub fn apply(&self, angles: [f32; 3]) -> [f32; 3] {
        [0, 1, 2].map(|i| self.sign[i] * angles[self.index[i]])
    }
}
