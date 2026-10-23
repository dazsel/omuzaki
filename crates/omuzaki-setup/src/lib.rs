//! The first-run setup flow (SE-01, `docs/design/first-run.md`).
//!
//! A small pure machine: [`SetupFlow`] takes [`Probes`] (what the machine can do) and [`Event`]s (what the person or
//! the host did), and answers with [`Effect`]s for the host to carry out through the same ops the panels call, and a
//! [`View`] for the panel to draw. It reads no clock (the host passes `now_ms`), does no I/O and draws nothing, so
//! every case in the design is a table test and the crate builds for the web.
//!
//! - [`probe`]: what the host learned about cameras, the tracker model, outputs and language.
//! - [`progress`]: [`Step`], the budgets, the persisted [`Progress`], the [`SetupStore`] contract and the [`Timers`].
//! - [`flow`]: the machine itself.
//! - [`ops`]: the `setup.*` ops over the session's progress, and [`SessionSetupStore`], the [`SetupStore`] the studio
//!   gives the flow, so the panel and an agent read and write the same progress.

pub mod flow;
pub mod ops;
pub mod probe;
pub mod progress;
mod request;

pub use flow::{CharacterChoice, Current, Effect, Event, InputChoice, Note, SetupFlow, View};
pub use ops::SessionSetupStore;
pub use probe::{Cameras, IPhone, LanguageOffer, OutputKind, Permission, Probes, TrackerModel};
pub use progress::{MemoryStore, Progress, SetupStore, Step, StoreError, TOTAL_BUDGET_S, Timers};

omuzaki_core::define_link!();
