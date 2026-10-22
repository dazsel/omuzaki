//! Contract for streaming and recording (spec v3 §25, §39.1; `docs/design/record.md` §5).
//!
//! Pipeline time, encoded packets, stream descriptions, the input tap contract, and the encoder, muxer and output
//! traits. Types and traits only: no codec, no thread, no I/O, so the crate builds for `wasm32-unknown-unknown`.
//!
//! | Module | What |
//! |---|---|
//! | [`time`] | [`Ticks`](time::Ticks) and exact [`Timebase`](time::Timebase) rescaling |
//! | [`packet`] | packets, side data and the [`ByteMeter`](packet::ByteMeter) |
//! | [`encoder`] | stream and encoder specs, [`Frame`](encoder::Frame), the [`Encoder`](encoder::Encoder) traits |
//! | [`mux`] | [`Container`](mux::Container) and the [`Muxer`](mux::Muxer) traits |
//! | [`input`] | the [`InputTap`] trait, [`FrameStamp`] and the tail contract |
//! | [`output`] | the [`Output`](output::Output) trait, its config and status, and the [`OutputKind`](output::OutputKind) registry (`register_output!`) |
//! | [`error`] | [`StreamError`](error::StreamError) |
//! | [`supervisor`] | the live show's supervised workers: ids, kinds, health, restart reasons, events and status rows |
//! | [`worker`] | the worker protocol: host and worker messages, a generation's shared objects, the framing |
//! | [`window`] | the clean output window: the [`WindowHost`](window::WindowHost) and [`PaletteSource`](window::PaletteSource) services and their types |

#![deny(missing_docs)]

pub mod encoder;
pub mod error;
pub mod input;
pub mod mux;
pub mod output;
pub mod packet;
pub mod supervisor;
pub mod time;
pub mod window;
pub mod worker;

pub use input::{FrameStamp, InputTap, TailMode, TailStep};

#[doc(hidden)]
pub use inventory as __inventory;
#[doc(hidden)]
pub use omuzaki_core::__record_registration;
