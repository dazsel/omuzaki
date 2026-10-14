//! The scripted HTTP transport: the one fake every caller of `omuzaki-http-api` tests against (connectors D13, ADR 0030
//! item 13). Connectors, `net.http` and the LLM providers all test against [`FakeHttp`] rather than each writing a
//! fake of their own.
//!
//! - [`FakeHttp`] answers each request from its routes, in order, and records what it was asked. Routes are built in
//!   code with [`FakeHttp::route`] and [`FakeHttp::json`], or loaded from a hand-written cassette with
//!   [`FakeHttp::cassette`].
//! - [`Reply`] is one scripted answer: a response, an error, a hang or a stream of [`Chunk`]s.
//! - Nothing here opens a socket, starts a thread or sleeps. A hang or a stall polls its cancel handle a fixed number
//!   of times instead, so tests stay deterministic and fast.
#![deny(missing_docs)]

mod cassette;
mod fake;
mod stream;

pub use cassette::CassetteError;
pub use fake::{FakeHttp, Logged, Reply};
pub use stream::{Chunk, StreamScript};
