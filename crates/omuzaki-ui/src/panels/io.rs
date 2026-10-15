//! What a panel reads and runs each frame, and the two things every edit panel does with it: notice that the document
//! changed, and run an op as the person.
//!
//! [`Io`] carries what `omuzaki_ui_api::panel::PanelCtx` carries (the runner, the actor, this frame's events), so a
//! panel's `ui` is written once and its `Panel` impl only builds an `Io` from the context.

use omuzaki_char_api::{CharId, CharacterDef, paths};
use omuzaki_doc_api::{DocVersion, Value};
use omuzaki_ops_api::{Actor, Cause, CauseKind, Event, Grants, Mode, OpError, Outcome, Request, Runner, TxId};

/// What a panel may read and run during one frame.
#[derive(Clone, Copy)]
pub struct Io<'a> {
    /// Every change to the document is an op run through it.
    pub runner: &'a dyn Runner,
    /// The person the panel acts for.
    pub actor: &'a Actor,
    /// What the runner sent since the previous frame, oldest first. Reading them does not consume them.
    pub events: &'a [Event],
}

impl<'a> Io<'a> {
    /// A frame that runs ops through `runner` as `actor`, with `events`.
    pub const fn new(runner: &'a dyn Runner, actor: &'a Actor, events: &'a [Event]) -> Self {
        Self { runner, actor, events }
    }

    /// Runs `op` with `args` as the person, in `tx` when one is open (else as its own one-step transaction).
    ///
    /// # Errors
    /// What the runner refuses, as it says it.
    pub fn run(&self, op: &str, args: Value, tx: Option<TxId>) -> Result<Outcome, OpError> {
        self.runner.run(Request {
            op: op.to_owned(),
            args,
            mode: Mode::Apply,
            actor: self.actor.clone(),
            grants: Grants::person(),
            cause: Some(Cause { kind: CauseKind::Direct, text: None }),
            tx,
            base: None,
        })
    }

    /// The query `op`'s answer.
    ///
    /// # Errors
    /// What the runner refuses, or `internal` when the op answered nothing.
    pub fn ask(&self, op: &str, args: Value) -> Result<Value, OpError> {
        let outcome = self.run(op, args, None)?;
        outcome.plan.output.ok_or_else(|| OpError::internal(format!("`{op}` answered nothing")))
    }

    /// Character `id` as the document holds it, or `None` when there is no such character.
    ///
    /// # Errors
    /// What the runner refuses, or `invalid` when the node doesn't read as a character.
    pub fn character(&self, id: &CharId) -> Result<Option<CharacterDef>, OpError> {
        let Some(node) = self.runner.get(&paths::character(id), &Grants::person())? else { return Ok(None) };
        CharacterDef::from_node(&node)
            .map(Some)
            .map_err(|e| OpError::invalid("character", format!("character '{id}' doesn't read as a character: {e}")))
    }
}

/// Notices document changes from the runner's events, so a panel rebuilds its view of the document when it changed
/// and never per frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct DocWatch {
    /// The newest version the panel built from; `None` before the first build.
    built: Option<DocVersion>,
    /// The newest version the events named.
    seen: Option<DocVersion>,
    /// Something outside the events asked for a rebuild (the panel opened another character).
    forced: bool,
}

impl DocWatch {
    /// Reads `events`; true when the panel's view is stale and should be rebuilt. Reading the same events twice (egui
    /// may run a frame twice) answers the same.
    pub fn stale(&mut self, events: &[Event]) -> bool {
        for event in events {
            if let Some(version) = version_of(event) {
                self.seen = Some(self.seen.map_or(version, |seen| seen.max(version)));
            }
        }
        self.forced || self.built.is_none() || self.seen > self.built
    }

    /// Records that the panel rebuilt from the newest version it has seen.
    pub fn built(&mut self) {
        self.built = Some(self.seen.unwrap_or(DocVersion(0)));
        self.forced = false;
    }

    /// Makes the next [`stale`](Self::stale) true.
    pub fn force(&mut self) {
        self.forced = true;
    }

    /// The newest document version the events named, if any.
    pub fn version(&self) -> Option<DocVersion> {
        self.seen
    }
}

/// The document version an event leaves behind, for the events that change the document.
fn version_of(event: &Event) -> Option<DocVersion> {
    match event {
        Event::OpApplied { record, .. } => Some(record.version),
        Event::TxRolledBack { version, .. } | Event::Undone { version, .. } | Event::Redone { version, .. } => {
            Some(*version)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_watch_is_stale_until_built_and_again_after_a_newer_version() {
        let mut watch = DocWatch::default();
        assert!(watch.stale(&[]), "nothing built yet");
        watch.built();
        assert!(!watch.stale(&[]));
        let undone = Event::tx_rolled_back(TxId(1), Actor::System, DocVersion(4));
        assert!(watch.stale(std::slice::from_ref(&undone)));
        assert!(watch.stale(std::slice::from_ref(&undone)), "the same events read twice answer the same");
        watch.built();
        assert!(!watch.stale(std::slice::from_ref(&undone)), "an old version is not news");
        watch.force();
        assert!(watch.stale(&[]));
    }
}
