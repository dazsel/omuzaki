//! Test doubles for connectors (spec v3 §14 "a mock connector exists for every capability, so tests need no
//! accounts", §39 rule 6; `docs/design/connectors.md` D11).
//!
//! - [`FakeConnector`]: one fake for every capability. A [`Generator`] makes deterministic bytes in code (PNG images
//!   and textures, a one-triangle GLB, WAV speech and music, BVH motion, a DCC JSON echo); a [`Script`] adds latency
//!   (through [`FakePace`], never sleeping), a price per output and a failure at an index.
//! - Eight registered fakes, `mock-image`, `mock-image-paid`, `mock-texture`, `mock-3d`, `mock-voice`, `mock-music`,
//!   `mock-motion` and `mock-dcc`, with manifests under `connectors/`, so the host and the link gate see them.
//! - The run context's doubles: [`MemoryInputs`], [`MemoryConfig`], [`RecordingMeter`], [`FakePace`].
//! - [`conformance`]: the checks every connector's own tests run.
//!
//! HTTP is faked by `omuzaki-http-mock`'s `FakeHttp` (design D13), which a connector's tests pass to the suite. LLM
//! and vision are faked by `omuzaki-llm-mock`. No fake opens a socket or sleeps.
#![deny(missing_docs)]

mod config;
pub mod conformance;
mod fake;
mod gen_audio;
mod gen_image;
mod gen_mesh;
mod gen_motion;
mod gen_texture;
mod inputs;
mod meter;
mod pace;
mod registered;
mod script;

pub use config::MemoryConfig;
pub use fake::{Counters, FakeConnector, Generator};
pub use inputs::MemoryInputs;
pub use meter::RecordingMeter;
pub use pace::FakePace;
pub use registered::{Mock3d, MockDcc, MockImage, MockImagePaid, MockMotion, MockMusic, MockTexture, MockVoice};
pub use script::Script;

/// The shapes of the generated outputs, for tests that check them.
pub mod shapes {
    pub use crate::gen_audio::{MAX_MUSIC_SECONDS, MAX_VOICE_SECONDS, MUSIC_RATE, VOICE_RATE, VOICE_SAMPLES_PER_CHAR};
    pub use crate::gen_image::MAX_SIDE;
    pub use crate::gen_motion::{FPS, MAX_SECONDS as MAX_MOTION_SECONDS};
}

omuzaki_core::define_link!();
