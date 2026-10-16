//! The AI performer's contract (ADR 0031, ADR 0041; `docs/design/ai-performer.md` and
//! `docs/design/ai-performer-voice-face.md`).
//!
//! The performer is an agent session that holds `perform` only: it listens, reads chat, asks an LLM for a reply,
//! speaks it chunk by chunk and acts through [`intent::Intent`]s on its own cast member. This crate holds the seams
//! between the parts, so implementations never link each other and the App wires them:
//!
//! - [`brain`]: [`brain::Brain`] turns a [`brain::TurnInput`] into a non-blocking [`brain::TurnStream`] of
//!   [`brain::BrainEvent`]s (speech, intents at a character offset, style, `[pass]`, dropped tags).
//! - [`speech`]: [`speech::Voice`] speaks chunks and returns a [`speech::SpeechHandle`] the performer can cut.
//! - [`ears`]: [`ears::Ears`] reports [`ears::Heard`], including the early `Held` that drives barge-in.
//! - [`stage`]: [`stage::Stage`] applies intents and reports the member's [`stage::Inventory`].
//! - [`perception`]: opt-in [`perception::Glance`]s.
//! - [`intent`]: the vocabulary, with [`intent::GazeTarget`], [`intent::PlaceId`] and the closed
//!   [`intent::StyleTag`] set.
//! - [`event`]: [`event::PerformerEvent`], the JSON Lines [`event::TranscriptLine`] and [`event::AiLine`].
//! - [`spend`]: the one [`spend::SpendGate`] every paid call goes through.
//! - [`voice`]: voice assignments, the catalog, and the mouth drive's [`voice::VoiceLead`] and [`voice::CutProbe`].
//! - [`moves`]: the per-frame [`moves::BigMovePlanner`] and its inputs.
//! - [`limits`]: every limit, as constants. [`ids`]: voice ids, persona revisions, connectors.
//! - [`conformance`]: the checks every implementation of a seam must pass.
//!
//! `AiLine` is defined but not registered as a live event kind, and the brain's input has no chat yet: both wait for
//! `omuzaki-live-api` (#1915). Callers import by module path (ADR 0054 §2).
#![deny(missing_docs)]

pub mod brain;
pub mod conformance;
pub mod ears;
pub mod event;
pub mod ids;
pub mod intent;
pub mod limits;
pub mod moves;
pub mod perception;
pub mod speech;
pub mod spend;
pub mod stage;
pub mod voice;
