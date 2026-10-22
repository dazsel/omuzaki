//! Sending the performed character out to other apps (spec v3 PF-04's "out"): the solved pose and face, after the
//! retarget and the life layer, so a `VMC` receiver (a game engine, another avatar app, a recorder) moves with the character.
//!
//! The App hands a [`PoseOutput`] (`omuzaki-track-net`'s `VMC` sender) to the input; starting it gives a [`PoseTap`],
//! which the perform thread publishes each step into. The tap is a newest-wins slot, so the perform thread never waits
//! on the network: the output's own thread sends the newest pose at its own rate.

use std::net::SocketAddr;
use std::sync::Arc;

use omuzaki_anim_api::HumanoidPose;
use omuzaki_core::time::Seconds;
use serde::Serialize;

/// Where the perform thread leaves each step's pose for an output to send.
pub trait PoseTap: Send + Sync {
    /// Leaves `pose` and `channels` (the bus's values; the first 52 are the `ARKit` channels), performed at `time`, as
    /// the newest. **Hot path:** one caller at a time (the perform thread), never blocks, locks or allocates.
    fn publish(&self, pose: &HumanoidPose, channels: &[f32], time: Seconds);
}

/// What an output is doing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct PoseOutStatus {
    /// Where it sends, while it runs.
    pub target: Option<SocketAddr>,
    /// Poses sent since it started.
    pub sent: u64,
}

impl PoseOutStatus {
    /// Sending to `target`, with `sent` poses sent so far.
    pub const fn sending(target: SocketAddr, sent: u64) -> Self {
        Self { target: Some(target), sent }
    }
}

/// An output that sends the performed pose to another app. Cold: starting and stopping open and close a socket and a
/// thread.
pub trait PoseOutput: Send {
    /// Starts sending to `target`, stopping a run to another target first. Returns the tap to publish into.
    ///
    /// # Errors
    /// The socket or the thread could not be opened.
    fn start(&mut self, target: SocketAddr) -> std::io::Result<Arc<dyn PoseTap>>;

    /// Stops sending. Idempotent.
    fn stop(&mut self);

    /// What it is doing now.
    fn status(&self) -> PoseOutStatus;
}
