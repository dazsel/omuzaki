//! The Build Theatre's book (spec v3 §13, "watch it being built"): what the studio shows of an agent's work beyond the
//! op log.
//!
//! The op log already says what changed and who changed it. The Theatre adds what an agent means to do (its plan, each
//! item with its status and its steps), why it did a step (the rationale), what a quality gate found (the gate-failure
//! card with its evidence), what the person wants changed where (notes drawn on the viewport) and how much rope the
//! agent has (the pause modes). An agent writes its part through the `theatre.*` ops, over MCP like any other op; the
//! person writes theirs through the same ops from the studio's Theatre panels.
//!
//! | Module | Holds |
//! |---|---|
//! | [`book`] | [`TheatreBook`](book::TheatreBook), the service, and the [`Book`](book::Book) it keeps |
//! | [`ops`] | `theatre.plan`, `theatre.item`, `theatre.explain`, `theatre.gate`, `theatre.direct`, `theatre.notes`, `theatre.take`, `theatre.status`, `theatre.pause_mode`, `theatre.reference` |
//! | [`text`] | the cleaning of text people and agents write into the book |
#![deny(missing_docs)]

pub mod book;
pub mod ops;
pub mod text;

omuzaki_core::define_link!();

pub use book::{Book, GateFailure, ItemStatus, Note, NoteCamera, PauseMode, Plan, PlanItem, Region, Step, TheatreBook};
