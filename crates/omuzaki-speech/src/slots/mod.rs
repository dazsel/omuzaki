//! The speech slots (ML-01, ADR 0017): `vad@1` (voice activity) and `asr@1` (speech to text), with the evaluations every
//! model in them must pass, and the public-domain speech the evaluations run on.
//!
//! The registry opens a session of each slot (Silero VAD on the `onnx` runtime, Whisper on [`crate::whisper_runtime`])
//! and the app hands both to [`crate::SpeechHost::open`]. A person's own model can take either slot once it passes the
//! slot's smoke test.

pub mod asr;
pub mod clips;
pub mod vad;

use omuzaki_ai_api::{SlotId, SlotRef};

pub use asr::{AsrSlot, asr_signature, asr_slot};
pub use vad::{VadSlot, vad_signature, vad_slot};

/// `id@1` for a literal slot id.
fn slot_ref(id: &'static str) -> SlotRef {
    SlotRef::new(SlotId::new(id).unwrap_or_else(|_| unreachable!("{id} is a literal slug")), 1)
}
