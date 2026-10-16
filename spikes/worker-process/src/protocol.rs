//! The fixed-size messages a worker posts to its host over the pipe (the worker's standard output).

/// Bytes in one message.
pub const MSG_BYTES: usize = 32;

const KIND_READY: u32 = 1;
const KIND_FRAME: u32 = 2;

/// What a worker tells its host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Msg {
    /// The device and pipeline exist; the first frame is next.
    Ready {
        /// Nanoseconds spent creating the instance, adapter and device.
        gpu_ns: u64,
        /// Nanoseconds spent building the scene's pipeline.
        scene_ns: u64,
    },
    /// Frame `index` is complete in `slot`.
    Frame {
        /// The slot (shared-memory ring entry, or shared texture) holding the finished frame.
        slot: u32,
        /// The worker's frame counter, from 0 for each worker.
        index: u64,
        /// Nanoseconds the worker spent drawing the frame and getting it off the GPU.
        render_ns: u64,
        /// Nanoseconds the worker spent copying the frame into the slot.
        write_ns: u64,
    },
}

/// A message that is not one of ours.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("unknown message kind {0}")]
pub struct ProtocolError(pub u32);

impl Msg {
    /// The wire form: kind and slot as little-endian u32, then three little-endian u64.
    #[must_use]
    pub fn encode(self) -> [u8; MSG_BYTES] {
        let (kind, slot, a, b, c) = match self {
            Self::Ready { gpu_ns, scene_ns } => (KIND_READY, 0, 0, gpu_ns, scene_ns),
            Self::Frame { slot, index, render_ns, write_ns } => (KIND_FRAME, slot, index, render_ns, write_ns),
        };
        let mut out = [0u8; MSG_BYTES];
        out[0..4].copy_from_slice(&kind.to_le_bytes());
        out[4..8].copy_from_slice(&slot.to_le_bytes());
        out[8..16].copy_from_slice(&a.to_le_bytes());
        out[16..24].copy_from_slice(&b.to_le_bytes());
        out[24..32].copy_from_slice(&c.to_le_bytes());
        out
    }

    /// Reads a message back.
    ///
    /// # Errors
    /// Fails when the kind is not [`Msg::Ready`] or [`Msg::Frame`].
    pub fn decode(bytes: &[u8; MSG_BYTES]) -> Result<Self, ProtocolError> {
        let u32_at = |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
        let u64_at = |at: usize| {
            let mut word = [0u8; 8];
            word.copy_from_slice(&bytes[at..at + 8]);
            u64::from_le_bytes(word)
        };
        match u32_at(0) {
            KIND_READY => Ok(Self::Ready { gpu_ns: u64_at(16), scene_ns: u64_at(24) }),
            KIND_FRAME => {
                Ok(Self::Frame { slot: u32_at(4), index: u64_at(8), render_ns: u64_at(16), write_ns: u64_at(24) })
            }
            other => Err(ProtocolError(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        for msg in [
            Msg::Ready { gpu_ns: 7, scene_ns: u64::MAX },
            Msg::Frame { slot: 2, index: 1 << 40, render_ns: 5, write_ns: 6 },
        ] {
            assert_eq!(Msg::decode(&msg.encode()), Ok(msg));
        }
    }

    #[test]
    fn unknown_kind_is_an_error() {
        assert_eq!(Msg::decode(&[0u8; MSG_BYTES]), Err(ProtocolError(0)));
    }
}
