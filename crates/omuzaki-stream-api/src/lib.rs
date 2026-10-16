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
//! | [`output`] | the [`Output`](output::Output) trait and its config and status |
//! | [`error`] | [`StreamError`](error::StreamError) |

#![deny(missing_docs)]

pub mod encoder;
pub mod error;
pub mod input;
pub mod mux;
pub mod output;
pub mod packet;
pub mod time;

pub use input::{FrameStamp, InputTap, TailMode, TailStep};
