//! The replay buffer (`docs/design/record.md` D3 to D5): the last seconds of the program kept as encoded packets,
//! saved as a clip on request.
pub mod arena;
pub mod control;
pub mod ring;
pub mod runtime;
pub mod segment;
pub mod settings;
