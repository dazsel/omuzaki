//! The input tap contract: what the host asks a tap for each frame, and how a take's tail is copied out
//! (`docs/design/takes.md` §5, ADR 0052, `docs/design/record.md` D4 and D5).
//!
//! A tap writes the input record for one frame period into a buffer the host owns; the host admits its length to the
//! replay ring and copies the bytes. The buffer is reused across frames, so a tap appends into it rather than
//! allocating its own.

use omuzaki_core::time::Seconds;
use omuzaki_history_api::Lsn;

/// The frame a tap writes its chunk for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameStamp {
    /// The video frame index.
    pub index: i64,
    /// The frame's time on the performance clock.
    pub tick: Seconds,
    /// The time the frame period ends: the chunk covers everything up to here.
    pub until: Seconds,
    /// True for the first frame of a group, the one a keyframe starts. The chunk then carries the start state (takes.md
    /// T2), so the group the ring evicts is always self-contained.
    pub group_start: bool,
}

/// Which copy of the queue a tail is written from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TailMode {
    /// A stop: the tail drains the live queue, since nothing is recording after it.
    Stop,
    /// A save: the tail copies the queue through `last_frame` and the journal position `upto_lsn`, and leaves the live
    /// queue as it is, so recording continues.
    #[non_exhaustive]
    Save {
        /// The last frame the save covers.
        last_frame: i64,
        /// The last journal entry the save covers.
        upto_lsn: Lsn,
    },
}

impl TailMode {
    /// A save through `last_frame` and the journal position `upto_lsn`.
    pub fn save(last_frame: i64, upto_lsn: Lsn) -> Self {
        Self::Save { last_frame, upto_lsn }
    }
}

/// The result of one [`InputTap::tail`] step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TailStep {
    /// One tail chunk was appended to `out`; the host calls again.
    Chunk,
    /// The feed has not yet written through the boundary the tail needs. The host calls again after the next frame,
    /// or, at a stop, waits up to 2 s and then accepts an `Incomplete` marker in place of the rest.
    Pending,
    /// The tail is complete; nothing more is written.
    Done,
}

/// A source of per-frame input records (`docs/design/takes.md` §5).
///
/// The host calls [`take`](Self::take) once per frame, on the frame thread, and calls [`tail`](Self::tail)
/// between frames when a take stops or a replay is saved.
///
/// # Examples
///
/// A tap that writes the same 64 bytes for every frame:
///
/// ```
/// use omuzaki_stream_api::input::{FrameStamp, InputTap};
///
/// struct Fixed;
///
/// impl InputTap for Fixed {
///     fn take(&mut self, _stamp: FrameStamp, out: &mut Vec<u8>) -> bool {
///         out.extend_from_slice(&[7; 64]);
///         true
///     }
/// }
///
/// let mut tap = Fixed;
/// let mut out = Vec::new();
/// let stamp = FrameStamp {
///     index: 0,
///     tick: omuzaki_core::time::Seconds(0.0),
///     until: omuzaki_core::time::Seconds(1.0 / 30.0),
///     group_start: true,
/// };
/// assert!(tap.take(stamp, &mut out));
/// assert_eq!(out.len(), 64);
/// ```
///
/// A tap whose tail yields `Chunk, Pending, Chunk, Done`, which the host must wait through:
///
/// ```
/// use omuzaki_history_api::Lsn;
/// use omuzaki_stream_api::input::{FrameStamp, InputTap, TailMode, TailStep};
///
/// struct Staged {
///     step: usize,
/// }
///
/// impl InputTap for Staged {
///     fn take(&mut self, _stamp: FrameStamp, _out: &mut Vec<u8>) -> bool {
///         false
///     }
///
///     fn tail(&mut self, _mode: &TailMode, cursor: &mut u64, out: &mut Vec<u8>) -> TailStep {
///         let step = self.step;
///         self.step += 1;
///         match step {
///             0 | 2 => {
///                 out.push(step as u8);
///                 *cursor += 1;
///                 TailStep::Chunk
///             }
///             1 => TailStep::Pending,
///             _ => TailStep::Done,
///         }
///     }
/// }
///
/// let mut tap = Staged { step: 0 };
/// let mode = TailMode::save(10, Lsn(42));
/// let mut cursor = 0;
/// let mut out = Vec::new();
/// let mut steps = Vec::new();
/// loop {
///     out.clear();
///     let step = tap.tail(&mode, &mut cursor, &mut out);
///     steps.push(step);
///     if step == TailStep::Done {
///         break;
///     }
/// }
/// assert_eq!(steps, [TailStep::Chunk, TailStep::Pending, TailStep::Chunk, TailStep::Done]);
/// assert_eq!(cursor, 2);
/// ```
pub trait InputTap: Send {
    /// Appends the chunk for `stamp` to `out` and returns true, or returns false when the frame has no input.
    ///
    /// `out` is cleared by the host before the call and is never shrunk, so a tap appends one whole chunk and leaves
    /// earlier bytes alone. The host then admits `out.len()` to the ring (record.md D4).
    fn take(&mut self, stamp: FrameStamp, out: &mut Vec<u8>) -> bool;

    /// Runs one tail step: appends one whole TAIL chunk to `out` and returns [`TailStep::Chunk`], or reports
    /// [`TailStep::Pending`] or [`TailStep::Done`].
    ///
    /// The host calls this until it returns `Done`. `cursor` is the host's position in the copy and starts at 0;
    /// the tap moves it past the bytes it copies, so a save never disturbs the live queue. The default writes no tail.
    fn tail(&mut self, _mode: &TailMode, _cursor: &mut u64, _out: &mut Vec<u8>) -> TailStep {
        TailStep::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_builds_the_struct_variant() {
        let mode = TailMode::save(9, Lsn(3));
        assert_eq!(mode, TailMode::Save { last_frame: 9, upto_lsn: Lsn(3) });
    }

    #[test]
    fn the_default_tail_writes_nothing_and_is_done() {
        struct Silent;
        impl InputTap for Silent {
            fn take(&mut self, _stamp: FrameStamp, _out: &mut Vec<u8>) -> bool {
                false
            }
        }
        let mut tap = Silent;
        let mut cursor = 0;
        let mut out = Vec::new();
        assert_eq!(tap.tail(&TailMode::Stop, &mut cursor, &mut out), TailStep::Done);
        assert_eq!(out, Vec::<u8>::new());
        assert_eq!(cursor, 0);
    }
}
