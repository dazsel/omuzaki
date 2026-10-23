//! The AI performer's core (ADR 0031; `docs/design/ai-performer.md`): everything between what the performer hears
//! and what it says and does, over the seams in `omuzaki-performer-api`.
//!
//! - [`parser`]: the streaming inline-tag parser (§6.2): text in, speech and intents at a character offset out.
//! - [`chunker`]: the sentence chunker (§5.2) that feeds the voice.
//! - [`persona`]: the [`persona::Persona`] TOML (§4), its limits, revision and prompt block.
//! - [`addressing`]: names, backchannels and echo (§5.1, §5.4, §5.5).
//! - [`prompt`]: the request for one turn (§6.3).
//! - [`brain`]: [`brain::LlmBrain`], the brain over any LLM client.
//! - [`decide`]: the decision layer (#2348): the rule floor, the gates and a local-model decider.
//! - [`systemone`]: the System One provider (Jev, by `TypeSafe`), a bring-your-own-key option.
//! - [`eval`]: the decision layer's measure: latency and agreement with a labelled set of moments.
//! - [`realtime`]: [`realtime::RealtimeShow`], the turn loop over a realtime speech-to-speech provider (#2347).
//! - [`show`]: [`show::Show`], the turn loop (§5): the floor, barge-in, private turns, talk limits, intent timing.
//!
//! Callers import by module path (ADR 0054 §2).
#![deny(missing_docs)]

omuzaki_core::define_link! {
    omuzaki_performer_api::link();
    omuzaki_stage_api::link();
}

pub mod addressing;
pub mod brain;
pub mod chunker;
pub mod decide;
pub mod eval;
pub mod parser;
pub mod persona;
pub mod prompt;
pub mod realtime;
pub mod show;
pub mod systemone;
