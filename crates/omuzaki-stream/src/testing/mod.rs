//! The testing kit: a fake encoder, a memory muxer and a scripted audio source, for tests in this and other crates.
//!
//! Behind `cfg(any(test, feature = "testing"))`, so release builds carry none of it.

pub mod encoder;
pub mod muxer;
pub mod source;

pub use encoder::{FakeEncoder, FakeEncoderConfig, FakeEncoderFactory};
pub use muxer::{MemoryMuxer, MemoryMuxerFactory, read_test_container};
pub use source::ScriptedSource;
