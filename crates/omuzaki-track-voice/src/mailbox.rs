//! The newest-wins hand-off of [`VoiceFrame`]s from the analysing thread to the perform thread: a sequence lock over
//! atomics, so neither side ever blocks, locks or allocates.
//!
//! The writer bumps a version to odd, stores the fields, and bumps it to even; the reader reads the version, the
//! fields and the version again, and keeps the read only if both versions are the same even number. A reader that
//! loses the race [`MAX_RETRIES`] times returns nothing this time and gets the frame on its next poll: a frame is
//! 10 ms, a poll is once per display frame, so nothing is lost but a few microseconds. Onsets are a running count in
//! the frame, so a frame the reader never sees loses none.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};

use crate::analyzer::VoiceFrame;

/// How many torn reads [`VoiceReader::poll`] retries before giving up until the next poll.
pub const MAX_RETRIES: usize = 4;

/// The fields of a [`VoiceFrame`] after `seq`, as bits: speaking and voiced, loudness, five visemes, onsets, two
/// formants.
const WORDS: usize = 11;

struct Shared {
    /// Twice the number of frames written, plus one while a write is under way.
    version: AtomicU64,
    seq: AtomicU64,
    words: [AtomicU32; WORDS],
}

/// The writing end. Owned by the analysing thread.
pub struct VoiceWriter {
    shared: Arc<Shared>,
}

/// The reading end. Owned by the perform thread.
pub struct VoiceReader {
    shared: Arc<Shared>,
    /// The version of the last frame returned.
    seen: u64,
}

/// A connected writer and reader.
pub fn voice_mailbox() -> (VoiceWriter, VoiceReader) {
    let shared = Arc::new(Shared {
        version: AtomicU64::new(0),
        seq: AtomicU64::new(0),
        words: std::array::from_fn(|_| AtomicU32::new(0)),
    });
    (VoiceWriter { shared: Arc::clone(&shared) }, VoiceReader { shared, seen: 0 })
}

fn pack(frame: &VoiceFrame) -> [u32; WORDS] {
    let v = frame.visemes;
    [
        u32::from(frame.speaking) | u32::from(frame.voiced) << 1,
        frame.loudness.to_bits(),
        v[0].to_bits(),
        v[1].to_bits(),
        v[2].to_bits(),
        v[3].to_bits(),
        v[4].to_bits(),
        frame.onsets,
        frame.formants[0].to_bits(),
        frame.formants[1].to_bits(),
        0,
    ]
}

fn unpack(seq: u64, w: [u32; WORDS]) -> VoiceFrame {
    VoiceFrame {
        seq,
        speaking: w[0] & 1 != 0,
        voiced: w[0] & 2 != 0,
        loudness: f32::from_bits(w[1]),
        visemes: [2, 3, 4, 5, 6].map(|i| f32::from_bits(w[i])),
        onsets: w[7],
        formants: [f32::from_bits(w[8]), f32::from_bits(w[9])],
    }
}

impl VoiceWriter {
    /// Publishes `frame`, replacing one not read yet. Wait-free; allocates nothing.
    pub fn publish(&mut self, frame: &VoiceFrame) {
        let s = &*self.shared;
        let v = s.version.load(Ordering::Relaxed);
        s.version.store(v + 1, Ordering::Relaxed);
        fence(Ordering::Release);
        s.seq.store(frame.seq, Ordering::Relaxed);
        for (slot, word) in s.words.iter().zip(pack(frame)) {
            slot.store(word, Ordering::Relaxed);
        }
        s.version.store(v + 2, Ordering::Release);
    }
}

impl VoiceReader {
    /// The newest frame published since the last call, or `None`. Never blocks, locks or allocates.
    pub fn poll(&mut self) -> Option<VoiceFrame> {
        let s = &*self.shared;
        for _ in 0..MAX_RETRIES {
            let before = s.version.load(Ordering::Acquire);
            if before == self.seen {
                return None;
            }
            if before % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let seq = s.seq.load(Ordering::Relaxed);
            let words: [u32; WORDS] = std::array::from_fn(|i| s.words[i].load(Ordering::Relaxed));
            fence(Ordering::Acquire);
            if s.version.load(Ordering::Relaxed) == before {
                self.seen = before;
                return Some(unpack(seq, words));
            }
        }
        None
    }

    /// Whether the writer is gone (its thread stopped): the reader then only ever returns what is already there.
    pub fn writer_gone(&self) -> bool {
        Arc::strong_count(&self.shared) == 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(seq: u64, loudness: f32) -> VoiceFrame {
        VoiceFrame {
            seq,
            speaking: true,
            voiced: seq.is_multiple_of(2),
            loudness,
            visemes: [0.1, 0.2, 0.3, 0.0, 0.4],
            onsets: 7,
            formants: [700.0, 1200.0],
        }
    }

    #[test]
    fn newest_wins_and_a_frame_is_read_once() {
        let (mut writer, mut reader) = voice_mailbox();
        assert_eq!(reader.poll(), None);
        writer.publish(&frame(1, 0.1));
        writer.publish(&frame(2, 0.2));
        assert_eq!(reader.poll(), Some(frame(2, 0.2)));
        assert_eq!(reader.poll(), None);
        assert!(!reader.writer_gone());
        drop(writer);
        assert!(reader.writer_gone());
    }

    #[test]
    fn a_racing_reader_never_sees_a_torn_frame() {
        let (mut writer, mut reader) = voice_mailbox();
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = Arc::clone(&done);
        let thread = std::thread::spawn(move || {
            let mut seq = 0u64;
            while !stop.load(Ordering::Relaxed) {
                seq += 1;
                // Every field derives from seq, so a mix of two frames is detectable.
                #[allow(clippy::cast_precision_loss)]
                writer.publish(&frame(seq, seq as f32));
            }
            seq
        });
        let mut seen = 0;
        while seen < 10_000 {
            if let Some(f) = reader.poll() {
                #[allow(clippy::cast_precision_loss)]
                let expected = frame(f.seq, f.seq as f32);
                assert_eq!(f, expected);
                seen += 1;
            }
        }
        done.store(true, Ordering::Relaxed);
        assert!(thread.join().unwrap() > 0);
    }
}
