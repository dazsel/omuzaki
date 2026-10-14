//! Streaming slots: models fed one block at a time from a real-time thread (voice conversion, ML-14; streaming
//! speech to text; voice to face, ML-05), where the thread that produces the blocks must never wait for the model.
//!
//! ```text
//!  real-time thread                 the slot's worker thread (registry)          real-time thread (often the same)
//!  BlockSink::push(block k) ──▶ bounded queue (`queue` blocks) ──▶ Session::run ──▶ results ──▶ BlockSource::take(k)
//!     never blocks; when full,          a block whose deadline passed                 Ready: the model's block
//!     the oldest waiting block          is skipped, not run                           Missed: use your fallback
//!     is dropped                                                                         (the dry voice)
//! ```
//!
//! **The deadline of block `k` is the `take(k)` call.** A consumer takes block `k` in the call that pushes block
//! `k + queue`, so the model has `queue` block periods to answer, and the output trails the input by
//! [`BlockSource::latency`]. Whatever isn't ready by then is `Missed`: the consumer plays its fallback for that block
//! (the time-aligned dry voice for voice conversion) and the model's late answer is thrown away. Deadlines count
//! blocks, not clock time, so a stream is deterministic under test; only [`StreamStats::run_p95_us`] reads a clock
//! (in the worker).

use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{RunError, TensorBuf, TensorRef};

/// How a streaming model takes its audio, declared in its manifest (`[stream]`). The caller converts to this rate and
/// block size; the slot's signature leaves the block dimension open (`any`) and the stream fixes it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StreamSpec {
    /// The model's sample rate in Hz (mono).
    pub sample_rate: u32,
    /// Samples per block, at `sample_rate`.
    pub block: u32,
    /// The model's own algorithmic delay in samples (look-ahead, context), on top of the queue: its output for input
    /// sample `n` is output sample `n + latency`.
    #[serde(default)]
    pub latency: u32,
}

impl StreamSpec {
    /// The longest block allowed: one second. A streaming model that needs more is a request/response model.
    pub const MAX_BLOCK_S: u32 = 1;

    /// One block's duration.
    pub fn block_duration(&self) -> Duration {
        samples(self.block, self.sample_rate)
    }

    /// How far the output trails the input when the consumer takes block `k` while pushing block `k + queue`:
    /// `queue` blocks plus the model's own delay. This is what feeds the A/V delay (PF-12) and `voice.vc` (spec §10).
    pub fn added_latency(&self, queue: u32) -> Duration {
        samples(self.block.saturating_mul(queue).saturating_add(self.latency), self.sample_rate)
    }

    /// Whether the numbers make sense: 8 to 192 kHz, a block of 1 sample to one second, a delay under ten seconds.
    pub fn is_valid(&self) -> bool {
        (8_000..=192_000).contains(&self.sample_rate)
            && (1..=self.sample_rate.saturating_mul(Self::MAX_BLOCK_S)).contains(&self.block)
            && self.latency < self.sample_rate.saturating_mul(10)
    }
}

/// `n` samples at `rate` Hz; zero for a zero rate.
fn samples(n: u32, rate: u32) -> Duration {
    if rate == 0 { Duration::ZERO } else { Duration::from_secs_f64(f64::from(n) / f64::from(rate)) }
}

/// What [`BlockSource::take`] found.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[must_use = "a missed block must be replaced by the fallback"]
pub enum Block {
    /// The model's output for the block is in `out`.
    Ready,
    /// No output for this block: it was late, dropped from a full queue, or the slot has no working model. `out` is
    /// untouched; play the fallback for this block.
    Missed,
}

/// Counters for the health meter and the latency readout. `Copy`, so reading them never allocates.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StreamStats {
    /// Blocks pushed.
    pub pushed: u64,
    /// Blocks taken with the model's output.
    pub ready: u64,
    /// Blocks taken without it (late, dropped or no model).
    pub missed: u64,
    /// Of the missed, how many were dropped from a full queue before the model saw them.
    pub dropped: u64,
    /// 95th percentile of the model's compute time per block over the last few seconds, in microseconds.
    pub run_p95_us: u32,
    /// True once the slot gave up on the model for this stream: `stall_blocks` missed in a row
    /// ([`SlotMode::Stream`](crate::SlotMode::Stream)). Every block is then missed (or served by a fallback model) and
    /// the health meter says so.
    pub stalled: bool,
}

/// The producing end of a stream, owned by the real-time thread. Implementations neither block, lock, allocate nor
/// call the model: they copy into a preallocated slot of the bounded queue.
pub trait BlockSink: Send {
    /// Queues one block: the slot signature's inputs for one block, where input 0 is the audio, shape
    /// `[`[`StreamSpec::block`]`]`, and any others are per-block controls (a pitch shift). Returns the block's
    /// sequence number (0, 1, 2, ...). When `queue` blocks are already waiting for the model, the oldest is dropped
    /// (it would be late anyway) and counted.
    ///
    /// # Errors
    /// [`RunError::Mismatch`] when the block doesn't match the signature and the stream's block size.
    fn push(&mut self, inputs: &[TensorRef<'_>]) -> Result<u64, RunError>;

    /// Starts a new stream: drops queued blocks and resets the model's state
    /// ([`Session::reset`](crate::Session::reset)) before the next block. Sequence numbers keep counting.
    fn restart(&mut self);
}

/// The consuming end of a stream, owned by the thread that needs the results by a deadline. Never blocks, locks or
/// allocates once `out` has grown to size.
pub trait BlockSource: Send {
    /// Copies block `seq`'s outputs into `out` (one buffer per signature output) if the model finished it, and
    /// marks every block up to `seq` as due: the worker skips any it hasn't started. Call it for each block in order,
    /// `queue` blocks after pushing it.
    fn take(&mut self, seq: u64, out: &mut [TensorBuf]) -> Block;

    /// How many blocks the model has between `push(k)` and `take(k)`.
    fn queue(&self) -> u32;

    /// How far the output trails the input: [`StreamSpec::added_latency`] for this stream's queue.
    fn latency(&self) -> Duration;

    /// The counters so far.
    fn stats(&self) -> StreamStats;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_is_the_queue_plus_the_models_own_delay() {
        // A Beatrice-class model: 24 kHz, 10 ms blocks, 20 ms look-ahead, two blocks in flight.
        let spec = StreamSpec { sample_rate: 24_000, block: 240, latency: 480 };
        assert_eq!(spec.block_duration(), Duration::from_millis(10));
        assert_eq!(spec.added_latency(2), Duration::from_millis(40));
        assert!(spec.is_valid());
    }

    #[test]
    fn nonsense_specs_are_refused() {
        let ok = StreamSpec { sample_rate: 48_000, block: 480, latency: 0 };
        assert!(ok.is_valid());
        assert!(!StreamSpec { sample_rate: 4_000, ..ok }.is_valid());
        assert!(!StreamSpec { block: 0, ..ok }.is_valid());
        assert!(!StreamSpec { block: 48_001, ..ok }.is_valid());
        assert!(!StreamSpec { latency: 480_000, ..ok }.is_valid());
        assert_eq!(StreamSpec { sample_rate: 0, ..ok }.block_duration(), Duration::ZERO);
    }
}
