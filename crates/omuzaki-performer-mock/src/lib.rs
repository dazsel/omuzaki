//! The AI performer's fake harness (`ai-performer.md` §9): every seam of `omuzaki-performer-api` faked on one
//! `FakeClock`, deterministic, and the golden-transcript writer and differ.
//!
//! - [`ScriptedEars`] releases timed `Heard` events.
//! - [`FakeVoice`]: synthesis is instant, 60 ms of audio per character, chunks back to back, a cut lands one 5 ms
//!   block later; it records speaks and cuts.
//! - [`RecordingStage`] checks intents against fixed inventories and records them with their `at`.
//! - [`FakeDuplex`] plays a realtime speech-to-speech session (#2347): what the person says, canned replies given when
//!   asked or by themselves, cancels that drop the rest; [`FakePcmVoice`] plays its audio back to back.
//! - [`ScriptedBrain`] plays scripted `BrainEvent`s, for the turn machine's tests without an LLM.
//! - [`FakePerception`], [`FakeCatalog`], [`FakeWallet`], [`RecordingLead`], [`FakeCut`] and [`RestPlanner`] fill
//!   the other seams.
//! - [`Transcript`] writes JSON Lines and diffs them against a golden file.
//!
//! The LLM side is `omuzaki-llm-mock`'s `FakeLlm` (a mock may use another mock only in its tests, so it is a
//! dev-dependency here): `tests/five_turns.rs` drives the §10 exchange through it and compares the result with
//! `goldens/five-turns.jsonl`. Every fake passes the contract's own checks
//! (`omuzaki_performer_api::conformance`) in `tests/conformance.rs`.
#![deny(missing_docs)]

omuzaki_core::define_link! {
    omuzaki_performer_api::link();
    omuzaki_stage_api::link();
}

mod brain;
mod duplex;
mod ears;
mod other;
mod stage;
mod transcript;
mod voice;

pub use brain::{ScriptedBrain, ScriptedTurn};
pub use duplex::{
    CannedReply, DELTA_S, DUPLEX_RATE, DuplexCall, FakeDuplex, FakePcmVoice, Played, STREAM_SPEEDUP, realtime_prices,
    reply_events,
};
pub use ears::ScriptedEars;
pub use other::{FakePerception, FakeWallet};
pub use stage::{Applied, RecordingStage, RestPlanner};
pub use transcript::Transcript;
pub use voice::{BLOCK_S, FakeCatalog, FakeCut, FakeVoice, LeadCall, MS_PER_CHAR, RecordingLead, Spoken};
