//! [`Arena`]: audio chunk bytes cut from a few large shared buffers, each reused once the ring has let go of every chunk
//! cut from it, so a warm replay buffer allocates nothing for its audio (#1019).
//!
//! A chunk is a [`Bytes`] sharing its buffer: cutting one bumps a reference count. The ring evicts oldest first, so the
//! buffers come free in the order they were filled, and the oldest retired one is the one to try.

use std::collections::VecDeque;

use bytes::{BufMut, Bytes, BytesMut};

/// The bytes of one buffer: a second of 48 kHz stereo `f32` audio, more than one frame of every track together.
pub const ARENA_BYTES: usize = 384 * 1024;
/// The most retired buffers remembered: a 300 s ring of four such tracks, with room for a save that pins some.
const RETIRED_MAX: usize = 1536;

/// Cuts chunks from shared buffers and reuses each buffer when nothing holds its chunks any more.
#[derive(Debug)]
pub struct Arena {
    /// The buffer being cut, once there is one; a spent buffer stays here (at no spare capacity) until the next cut.
    current: Option<BytesMut>,
    /// Filled buffers, oldest first, waiting for the chunks cut from them to go.
    retired: VecDeque<BytesMut>,
}

impl Default for Arena {
    fn default() -> Self {
        Self::new()
    }
}

impl Arena {
    /// An empty arena; its first cut allocates the first buffer.
    pub fn new() -> Self {
        Self { current: None, retired: VecDeque::with_capacity(RETIRED_MAX) }
    }

    /// `len` bytes, written by `fill` (which must put exactly `len`), as a chunk sharing a buffer.
    pub fn cut(&mut self, len: usize, fill: impl FnOnce(&mut BytesMut)) -> Bytes {
        let current = match self.current.take() {
            Some(buffer) if buffer.capacity() - buffer.len() >= len => buffer,
            spent => self.next(spent, len),
        };
        let buffer = self.current.insert(current);
        fill(buffer);
        let n = buffer.len();
        buffer.split_to(n).freeze()
    }

    /// `len` zero bytes, as a chunk.
    pub fn zeros(&mut self, len: usize) -> Bytes {
        self.cut(len, |b| b.put_bytes(0, len))
    }

    /// Moves on to a buffer with room for `len`: the oldest retired one when nothing holds its chunks, else a new one.
    fn next(&mut self, spent: Option<BytesMut>, len: usize) -> BytesMut {
        let want = ARENA_BYTES.max(len);
        let free = self.retired.front_mut().is_some_and(|oldest| oldest.try_reclaim(want));
        let reused = if free { self.retired.pop_front() } else { None };
        if let Some(spent) = spent {
            if self.retired.len() == RETIRED_MAX {
                // Its chunks free it as they go; it is only forgotten here.
                self.retired.pop_front();
            }
            self.retired.push_back(spent);
        }
        reused.unwrap_or_else(|| BytesMut::with_capacity(want))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_buffer_is_reused_once_its_chunks_are_gone() {
        let mut arena = Arena::new();
        let first = arena.zeros(ARENA_BYTES);
        let start = first.as_ptr();
        let second = arena.zeros(16);
        assert_ne!(second.as_ptr(), start, "the first buffer is full");
        drop(first);
        let third = arena.zeros(ARENA_BYTES);
        assert_eq!(third.as_ptr(), start, "the freed buffer comes back");
        assert_eq!(&third[..4], &[0, 0, 0, 0]);
    }

    #[test]
    fn a_held_buffer_is_left_alone() {
        let mut arena = Arena::new();
        let held = arena.cut(8, |b| b.put_slice(&[1; 8]));
        let _rest = arena.zeros(ARENA_BYTES - 8);
        let next = arena.zeros(ARENA_BYTES);
        assert_ne!(next.as_ptr(), held.as_ptr());
        assert_eq!(&held[..], &[1; 8], "a held chunk is never written over");
    }
}
