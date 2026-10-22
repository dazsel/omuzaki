//! Phone and mocap receivers (spec v3 §22 PF-02 to PF-04): the face from an iPhone tracking app or a VMC sender,
//! received over UDP and published as anim-api [`TrackFrame`](omuzaki_anim_api::TrackFrame)s, behind the same
//! track-api [`Tracker`](omuzaki_track_api::tracker::Tracker) seam as the webcam, so the stage, the `input.*` ops and
//! the health meter treat every source alike.
//!
//! | Module | What it does |
//! |---|---|
//! | [`protocol`] | [`Protocol`] and one [`Decoder`](protocol::Decoder) per wire format, plus the OSC reader VMC rides on |
//! | [`receiver`] | [`NetTracker`]: the receiver thread, clock mapping, the lost frame and the latency figures |
//! | [`settings`] | the `input.phone` setting: the phone's address and the ports |
//! | [`names`] | blendshape spellings onto channels |
//!
//! Android tracker apps that send the same protocols (PF-03) are covered by the same receivers. Sending our own solved
//! pose out over VMC (PF-04's "out") and body bones in are later work: [`protocol::osc::Writer`] already builds packets.
#![deny(missing_docs)]
// Product names (iFacialMocap, VTube Studio, Live Link Face, ARKit, OSC) are prose here, not code.
#![allow(clippy::doc_markdown)]

pub mod names;
pub mod protocol;
pub mod receiver;
pub mod settings;

pub use protocol::Protocol;
pub use receiver::NetTracker;
pub use settings::{InputPhone, PhoneSettings};

omuzaki_core::define_link!();
