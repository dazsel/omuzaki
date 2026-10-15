//! Contract for streaming and recording (spec v3 §25, §39.1; `docs/design/record.md` §5).
//!
//! The base types of recording and streaming: pipeline time, encoded packets and the shared byte meter. Types only: no
//! codec, no thread, no I/O, so the crate builds for `wasm32-unknown-unknown`. Encoders, muxers and outputs build on them.
//!
//! | Module | What |
//! |---|---|
//! | [`time`] | [`Ticks`](time::Ticks) and exact [`Timebase`](time::Timebase) rescaling |
//! | [`packet`] | packets, side data and the [`ByteMeter`](packet::ByteMeter) |
//! | [`error`] | [`StreamError`](error::StreamError) |

#![deny(missing_docs)]

pub mod error;
pub mod packet;
pub mod time;
