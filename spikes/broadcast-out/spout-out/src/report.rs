//! The numbers a run produces, as plain serialisable structs. The receiver process prints its report as JSON on stdout
//! and the sender (or `loopback` parent) reads it back, so the two halves can be run separately or together.

use serde::{Deserialize, Serialize};

use crate::analysis::Case;
use crate::stats::Summary;

/// Marks the report line among any other output.
pub const REPORT_PREFIX: &str = "REPORT ";

/// How the frame reaches Spout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
pub enum Path {
    /// Share the GPU texture: wgpu (D3D12) → `D3D11On12` → Spout's shared texture. No CPU readback.
    Gpu,
    /// Read the frame back to the CPU and hand Spout the pixels (`SendImage`): the baseline and the fallback.
    Cpu,
}

/// How the receiving process reads what the sender shares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
pub enum ReceiverKind {
    /// The Spout SDK's receiver class: waits on the sender's sync event, then reads the newest frame.
    Sdk,
    /// An OBS-style receiver written from the protocol: reads the shared texture on its own clock, no sync, no mutex.
    Raw,
}

/// Whether the sender waits for the GPU between handing the frame to Spout and telling the receiver it is ready.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
pub enum SyncMode {
    /// Trust Spout's own mutex, flush and implicit shared-surface synchronisation.
    None,
    /// Wait for the copy into Spout's shared texture to finish on the GPU (a D3D12 fence on the GPU path, a D3D11 event
    /// query on the CPU path) before signalling the frame.
    Fence,
}

/// What the sender measured about itself.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SenderReport {
    /// Name the sender registered with Spout.
    pub name: String,
    /// GPU the frames were rendered on.
    pub adapter: String,
    /// Backend wgpu used.
    pub backend: String,
    /// Frame size.
    pub size: (u32, u32),
    /// Which hand-off path ran.
    pub path: Path,
    /// Synchronisation mode.
    pub sync: SyncMode,
    /// Target frame rate (0 = unpaced).
    pub fps: f64,
    /// Frames handed to Spout.
    pub sent: u32,
    /// CPU time to encode and submit the render, per frame.
    pub submit_ms: Summary,
    /// CPU time inside the Spout hand-off (acquire, copy, flush, release, optional fence wait), per frame.
    pub handoff_ms: Summary,
    /// Time from one frame's start to the next (pacing accuracy).
    pub period_ms: Summary,
    /// `(frame, FNV-1a hash of every byte)` for every `hash_every`-th frame, from the sender's own readback.
    pub hashes: Vec<(u32, u64)>,
    /// Receiver blend-mode analysis of the premultiplied frame against the GPU ground truth.
    pub composite: Vec<Case>,
    /// The test patches' stored bytes matched what the shader should write (within one level).
    pub patches_ok: bool,
}

/// What the receiver measured.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReceiverReport {
    /// Frames read.
    pub received: u32,
    /// Highest frame number seen.
    pub last_frame: u32,
    /// Frame numbers skipped between received frames (a slower receiver, or frames lost).
    pub skipped: u32,
    /// Frames seen twice or out of order.
    pub repeats: u32,
    /// Reads of the shared texture, including repeats of a frame already seen (every read for a polling receiver).
    pub polls: u32,
    /// Reads whose image was torn, counting repeats; `torn_polls / polls` is the exposure of a receiver that reads on its
    /// own clock.
    pub torn_polls: u32,
    /// Rows where two frames met inside one torn image (the first few).
    pub tear_rows: Vec<u32>,
    /// Frames whose header and footer or complement cells disagree: torn or corrupted.
    pub torn: u32,
    /// Frames whose test patches did not hold the expected bytes.
    pub patch_failures: u32,
    /// Sender frame start to the receiver noticing the frame and issuing its GPU copy.
    pub seen_ms: Summary,
    /// Sender frame start to the pixels being in receiver memory (includes the staging copy and map).
    pub read_ms: Summary,
    /// Time between consecutive received frames.
    pub interval_ms: Summary,
    /// Sender's frame rate as published in Spout shared memory at the end of the run.
    pub sender_fps: f64,
    /// `(frame, hash)` for the sampled frames, to compare with the sender's.
    pub hashes: Vec<(u32, u64)>,
}

/// Both halves of a loopback run, for machine-readable output.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoopbackReport {
    /// The sending process.
    pub sender: SenderReport,
    /// The receiving process.
    pub receiver: ReceiverReport,
}

/// 64-bit FNV-1a, enough to tell two frames apart across processes.
#[must_use]
pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_matches_known_vectors() {
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a(b"foobar"), 0x8594_4171_f739_67e8);
    }
}
