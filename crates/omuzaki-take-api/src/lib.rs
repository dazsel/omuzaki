//! Contract for takes: the recorded inputs beside a recording or a replay clip (SH-01; ADR 0052; design in
//! `docs/design/takes.md`, section 5).
//!
//! A take's `<stem>.input` file is a header, then one self-describing checksummed chunk per frame period, then an
//! index and trailer. This crate holds only the types, constants and traits that every user of the format shares:
//! the codec is `omuzaki-take`, the scripted oracle is `omuzaki-take-mock`. Subsystems with live state add it to each
//! `State` with [`register_take_state!`] next to the code that owns it.
#![deny(missing_docs)]

mod chunk;
mod error;
mod header;
mod record;
mod source;
mod state;

pub use chunk::{
    CHUNK_HEADER_LEN, CHUNK_MAGIC, ChunkFlags, ChunkHeader, FILE_MAGIC, MAJOR, MINOR, RECORD_HEADER_LEN, RecordKind,
};
pub use error::TakeError;
pub use header::{SnapshotAt, TakeHeader, TakeKind};
pub use record::{EventRecord, Incomplete, Record, TakeEntry};
pub use source::{ChunkView, TakeReport, TakeSource};
pub use state::{
    CastEntry, SourceEntry, StateProvider, StateRecord, VoiceEntry, Withheld, WithheldCause, state_providers,
};

#[doc(hidden)]
pub use inventory as __inventory;
#[doc(hidden)]
pub use omuzaki_core::__record_registration;

omuzaki_core::define_link!();
