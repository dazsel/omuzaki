//! Deterministic stand-ins for the model contract (`omuzaki-ai-api`, spec v3 §39 rule 6), so the registry, the slot
//! owners and the stream consumers build and test with no weights, no ONNX Runtime, no GPU and no network:
//!
//! - [`FakeSession`]: an affine function of input 0, a delay line (a streaming model with its own latency), or
//!   replayed outputs; can crash after N runs.
//! - [`FakeRuntime`] (registered as `fake`): loads a small TOML "model" naming its ports, so probing, slot matching,
//!   `io` renames, refusals and crash fallback are testable.
//! - [`MemFetcher`]: serves bytes per URL, counts requests, goes offline on demand.
//! - [`EchoSlot`] (`test_echo@1`) and [`StreamEchoSlot`] (`test_stream@1`), with smoke tests that pass only for the
//!   identity, and the shipped model `echo-fake` pinned to [`ECHO_MODEL`] ([`fixtures_fetcher`] serves it).
//! - [`InlineStream`]: the reference stream, running the session inside `push`, with late blocks on demand.
//! - [`FakeTts`] (`tts@1`) and [`FakeVoiceToFace`] (`voice_to_face@1`): the AI performer's voice and mouth with the
//!   hand-worked numbers of `ai-performer-voice-face.md` §6, scripted latency, failures and a gate.
//!
//! - [`CatalogueRecorder`]: a stand-in for `ModelRegistry::sync_catalogue`, for the updater's tests.
//!
//! There is no mock of the rest of the registry: the `model.*` ops live with the registry in `omuzaki-ai` and test
//! against it with a temporary folder and these fakes.
#![deny(missing_docs)]

mod catalogue;
mod fetch;
mod runtime;
mod session;
mod slot;
mod stream;
mod voice;

pub use catalogue::{CatalogueRecorder, SyncCall};
pub use fetch::MemFetcher;
pub use runtime::{FakeModel, FakeRuntime};
pub use session::{FakeSession, fake_model};
pub use slot::{ECHO_MODEL, ECHO_MODEL_URL, EchoSlot, StreamEchoSlot, echo_signature, fixtures_fetcher};
pub use stream::InlineStream;
pub use voice::{
    ARKIT, CONSONANT, FACE_BLOCK, FACE_RATE, FAKE_TTS_RATE, FakeTts, FakeVoiceToFace, JAW_OPEN, SAMPLES_PER_CHAR,
    TONE_HZ, VOWEL, amplitude, fake_speech, jaw_open, tts_signature, voice_to_face_signature,
};

omuzaki_core::define_link!();
