//! Spike: premultiplied-alpha frames from wgpu to OBS (Spout2), measured end to end.
//!
//! The pure parts (frame tag, composite analysis, statistics, the wgpu scene) build and test anywhere. The Spout hand-off
//! (the C shim over the Spout2 SDK, the D3D12 to D3D11 bridge, the sender, receiver and loopback runs) is Windows-only.

pub mod analysis;
pub mod clock;
pub mod frame_tag;
pub mod gpu;
pub mod report;
pub mod scene;
pub mod stats;
pub mod tally;

#[cfg(windows)]
mod ffi;
#[cfg(windows)]
pub mod interop;
#[cfg(windows)]
pub mod loopback;
#[cfg(windows)]
pub mod raw_receiver;
#[cfg(windows)]
pub mod receiver;
#[cfg(windows)]
pub mod sender;
#[cfg(windows)]
pub mod spout;
