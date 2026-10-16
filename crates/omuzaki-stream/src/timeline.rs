//! The A/V timeline (spec v3 §25): one clock for every stream, and the audio reads every later task goes through.
//!
//! Filled by the A/V timeline task. This file starts with the one trait the rest of the crate reads audio through,
//! implemented for the engine's tap reader.

use omuzaki_audio_api::ring::{Consumer, TapRead};

/// Where recording and streaming read audio from: the engine's tap, or a scripted source in tests.
pub trait AudioSource: Send {
    /// Moves the next contiguous run of samples into the front of `out` and says where it sits on the stream.
    ///
    /// The run ends early at a gap or a flag change, so callers read again for the rest.
    fn read(&mut self, out: &mut [f32]) -> TapRead;
}

impl AudioSource for Consumer {
    fn read(&mut self, out: &mut [f32]) -> TapRead {
        // The inherent `Consumer::read` is the real reader; the trait only names it for the crates above.
        Consumer::read(self, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omuzaki_audio_api::ring::ring;
    use omuzaki_audio_api::tap::DropPolicy;

    #[test]
    fn the_tap_reader_is_an_audio_source() {
        let (mut producer, mut consumer) = ring(16, DropPolicy::DropNewest, 0);
        assert_eq!(producer.push(&[1.0, 2.0, 3.0]), 3);
        let source: &mut dyn AudioSource = &mut consumer;
        let run = source.read(&mut [0.0; 8]);
        assert_eq!((run.len, run.start_sample, run.gap_before), (3, 0, 0));
    }
}
