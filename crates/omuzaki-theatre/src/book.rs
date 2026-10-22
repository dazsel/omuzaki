//! [`TheatreBook`]: what the Build Theatre knows beyond the op log (spec v3 §13): each agent's plan, the steps as cards
//! with their cause, rationale and gate failures, the person's notes for the agents, and how each agent is paused.
//!
//! The book is a service in the app core: the `theatre.*` ops write it ([`super::ops`]), the Theatre's panels read a
//! snapshot of it, and a watcher thread ([`TheatreBook::attach`]) turns the runner's events into steps. The watcher
//! also carries out the pause modes, because a pause is the person's control over a session and has to hold whether or
//! not a Theatre panel is on screen. The book is saved beside the project (`.omuzaki/theatre.json`) when it has one.
//!
//! Ops run under the executor's lock, so nothing an op does here waits: an op changes the book in memory and asks for a
//! pause with [`TheatreBook::defer_pause`], and the watcher, on its own thread, pauses the session and saves the file
//! once the op's transaction has committed. A pause therefore lands a moment after the step that asked for it, before
//! the agent's next request in practice (an agent's request crosses a transport; the watcher wakes on the event).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use omuzaki_core::time::Seconds;
use omuzaki_ops_api::{Actor, Event, Grants, OpRecord, Runner, Sessions, TxId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Where the book is kept, under the project folder.
pub const BOOK_FILE: &str = ".omuzaki/theatre.json";
/// The most steps the book keeps; the oldest go first. The op log keeps everything.
pub const KEPT_STEPS: usize = 2000;

/// Where a plan item stands (TH-03).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    /// Not started.
    #[default]
    Planned,
    /// Being worked on: the agent's steps land under it.
    Running,
    /// Done, waiting for a person to look.
    NeedsReview,
    /// Done.
    Done,
    /// A gate failed on it.
    FailedGate,
}

impl ItemStatus {
    /// Whether the item has finished, one way or another.
    pub const fn finished(self) -> bool {
        matches!(self, Self::NeedsReview | Self::Done | Self::FailedGate)
    }
}

/// One item of an agent's plan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlanItem {
    /// The agent's id for it, unique in its plan.
    pub id: String,
    /// What it is, in plain words ("Make the face more oval").
    pub text: String,
    /// Where it stands.
    pub status: ItemStatus,
    /// The steps made while it was running, oldest first.
    #[serde(default)]
    pub steps: Vec<TxId>,
}

/// An agent's plan: its items in order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Plan {
    /// Whose plan.
    pub owner: Actor,
    /// The items, in the agent's order.
    pub items: Vec<PlanItem>,
}

impl Plan {
    /// The item being worked on.
    pub fn running(&self) -> Option<&PlanItem> {
        self.items.iter().find(|i| i.status == ItemStatus::Running)
    }
}

/// A gate that failed on a step (TH-11, spec §15): which, why, and the picture that shows it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GateFailure {
    /// The gate's name ("likeness", "mesh budget").
    pub gate: String,
    /// What it found, in plain words.
    pub message: String,
    /// The evidence image: an `omuzaki://render/<hash>.png` URI or a hash in the render store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

/// One transaction as a Theatre card (TH-02).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Step {
    /// The transaction.
    pub tx: TxId,
    /// Its label.
    pub label: String,
    /// Who made it.
    pub actor: Actor,
    /// The prompt, chat message or rule that caused it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    /// When its first op ran, on the runner's clock.
    pub started: Seconds,
    /// When its last op ran.
    pub ended: Seconds,
    /// How many ops it holds.
    pub ops: u32,
    /// Its newest op's name (`part.set_param`), for the Theatre's caption.
    #[serde(default)]
    pub op: String,
    /// The character parts it changed (`hair`, `head`), for the viewport's highlight and its follow mode.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<String>,
    /// The plan item that was running when it landed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// The agent's reason for it, in plain words (TH-08).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    /// A gate that failed on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<GateFailure>,
    /// Whether it is undone.
    #[serde(default)]
    pub undone: bool,
}

impl Step {
    /// How long it took, in seconds.
    pub fn seconds(&self) -> f64 {
        (self.ended.0 - self.started.0).max(0.0)
    }
}

/// A region of the viewport a person circled, as fractions of the viewport (0 to 1 from the top left).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Region {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

/// The camera a note was drawn under, so an agent can look from the same place.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NoteCamera {
    /// Turn around the character, in degrees.
    pub yaw: f32,
    /// Tilt, in degrees.
    pub pitch: f32,
    /// Distance from the target, in metres.
    pub distance: f32,
    /// What the camera looks at, in metres.
    pub target: [f32; 3],
}

/// A person's note for the agents (TH-09): what they circled and what they want done there.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Note {
    /// Its number, from 1.
    pub id: u64,
    /// Who wrote it.
    pub by: Actor,
    /// What they want ("make this rounder").
    pub text: String,
    /// The circled region.
    pub region: Region,
    /// The part under the region, when one is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
    /// The camera it was drawn under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<NoteCamera>,
    /// The step whose picture shows the character as the person saw it: the screenshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picture: Option<TxId>,
    /// When, on the runner's clock.
    pub at: Seconds,
    /// Whether an agent has taken it up.
    #[serde(default)]
    pub taken: bool,
}

/// When the watcher pauses an agent's session (TH-06).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PauseMode {
    /// Never: the agent works until it is done or paused by hand. The default.
    #[default]
    Off,
    /// After every step that changes the project.
    EveryStep,
    /// After each plan item finishes.
    EachItem,
    /// Only when a gate fails.
    Gates,
}

/// Everything the book holds.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Book {
    /// Each agent's plan, in the order they first posted one.
    pub plans: Vec<Plan>,
    /// The steps, oldest first, at most [`KEPT_STEPS`].
    pub steps: Vec<Step>,
    /// The person's notes, oldest first.
    pub notes: Vec<Note>,
    /// Each agent's pause mode, by agent id.
    #[serde(default)]
    pub modes: BTreeMap<String, PauseMode>,
    /// The reference picture the compare strip shows first (TH-04): a render store URI or hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

impl Book {
    /// `owner`'s plan.
    pub fn plan(&self, owner: &Actor) -> Option<&Plan> {
        self.plans.iter().find(|p| p.owner == *owner)
    }

    fn plan_mut(&mut self, owner: &Actor) -> Option<&mut Plan> {
        self.plans.iter_mut().find(|p| p.owner == *owner)
    }

    /// The step of `tx`.
    pub fn step(&self, tx: TxId) -> Option<&Step> {
        self.steps.iter().rev().find(|s| s.tx == tx)
    }

    fn step_mut(&mut self, tx: TxId) -> Option<&mut Step> {
        self.steps.iter_mut().rev().find(|s| s.tx == tx)
    }

    /// `actor`'s newest step.
    pub fn last_step_of(&self, actor: &Actor) -> Option<&Step> {
        self.steps.iter().rev().find(|s| s.actor == *actor)
    }

    /// An agent's pause mode.
    pub fn mode(&self, agent: &Actor) -> PauseMode {
        agent_id(agent).and_then(|id| self.modes.get(id).copied()).unwrap_or_default()
    }
}

/// The id an agent's mode is kept under.
pub fn agent_id(actor: &Actor) -> Option<&str> {
    match actor {
        Actor::Agent { id, .. } => Some(id),
        _ => None,
    }
}

/// A step still open: its ops so far.
#[derive(Clone, Debug)]
struct Open {
    actor: Actor,
    cause: Option<String>,
    started: Seconds,
    ended: Seconds,
    ops: u32,
    changes: bool,
    /// A reason given while the step was still open.
    rationale: Option<String>,
    op: String,
    parts: Vec<String>,
}

/// The part a document path is under: `characters/<id>/parts/<part>/...`.
pub fn part_of(path: &omuzaki_doc_api::DocPath) -> Option<&str> {
    match path.segments() {
        [characters, _, parts, part, ..] if characters == "characters" && parts == "parts" => Some(part),
        _ => None,
    }
}

/// The book as a service. Clones share one book.
#[derive(Clone, Debug, Default)]
pub struct TheatreBook {
    book: Arc<Mutex<Book>>,
    open: Arc<Mutex<BTreeMap<TxId, Open>>>,
    /// Bumped on every change, so a panel rebuilds its view only when something moved.
    revision: Arc<AtomicU64>,
    /// Where it is saved, when the project has a folder.
    file: Arc<Mutex<Option<PathBuf>>>,
    /// Agents to pause after their next step whatever their mode: the person's Step.
    once: Arc<Mutex<Vec<Actor>>>,
    /// Agents an op asked to pause, paused by the watcher after the op's transaction commits.
    deferred: Arc<Mutex<Vec<Actor>>>,
    /// The revision last saved.
    saved: Arc<AtomicU64>,
}

impl TheatreBook {
    /// An empty book, kept in memory.
    pub fn new() -> Self {
        Self::default()
    }

    /// The book of the project in `dir`: read from [`BOOK_FILE`] when there is one, and saved there after each change.
    pub fn for_project(dir: &Path) -> Self {
        let file = dir.join(BOOK_FILE);
        let book = std::fs::read(&file)
            .ok()
            .and_then(|bytes| match serde_json::from_slice::<Book>(&bytes) {
                Ok(book) => Some(book),
                Err(error) => {
                    tracing::warn!(%error, "the Theatre's book did not read; it starts empty");
                    None
                }
            })
            .unwrap_or_default();
        let this = Self::default();
        *this.lock() = book;
        *this.file.lock().unwrap_or_else(PoisonError::into_inner) = Some(file);
        this
    }

    /// A copy of the book now.
    pub fn snapshot(&self) -> Book {
        self.lock().clone()
    }

    /// Changes since the book was made: a panel compares it with the one it built its view for.
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    /// Reads the book under its lock.
    pub fn read<R>(&self, f: impl FnOnce(&Book) -> R) -> R {
        f(&self.lock())
    }

    /// Changes the book and tells the panels. The file is saved by [`flush`](Self::flush), which the watcher calls.
    pub fn write<R>(&self, f: impl FnOnce(&mut Book) -> R) -> R {
        let out = f(&mut self.lock());
        self.revision.fetch_add(1, Ordering::AcqRel);
        out
    }

    /// Saves the book to the project's file if it changed since the last save. Never call it from an op: it writes a
    /// file, and ops run under the executor's lock.
    pub fn flush(&self) {
        let revision = self.revision();
        if self.saved.swap(revision, Ordering::AcqRel) == revision {
            return;
        }
        let Some(file) = self.file.lock().unwrap_or_else(PoisonError::into_inner).clone() else { return };
        let saved = serde_json::to_vec_pretty(&*self.lock());
        let written = match saved {
            Ok(bytes) => {
                file.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&file, bytes))
            }
            Err(error) => Err(std::io::Error::other(error)),
        };
        if let Err(error) = written {
            tracing::warn!(%error, "the Theatre's book was not saved");
        }
    }

    /// Asks the watcher to pause `agent` once the running op's transaction commits: for ops, which may not call the
    /// sessions themselves.
    pub fn defer_pause(&self, agent: &Actor) {
        self.deferred.lock().unwrap_or_else(PoisonError::into_inner).push(agent.clone());
    }

    /// Pauses `agent` after its next step, whatever its mode: the person's Step after a Resume.
    pub fn pause_after_next(&self, agent: &Actor) {
        self.once.lock().unwrap_or_else(PoisonError::into_inner).push(agent.clone());
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Book> {
        // Every write leaves the book whole, so a panic elsewhere can't leave it half-written.
        self.book.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Follows `runner`'s events on a thread of its own: each committed transaction that changed the project becomes a
    /// step under the plan item running then, and each agent is paused as its mode says, through `sessions`. The
    /// thread ends when the runner closes its event stream.
    ///
    /// # Errors
    /// The runner refused the subscription, or the thread did not start.
    pub fn attach(&self, runner: &Arc<dyn Runner>, sessions: Arc<dyn Sessions>) -> Result<(), String> {
        let events = runner.subscribe(&Grants::person()).map_err(|e| e.to_string())?;
        let book = self.clone();
        std::thread::Builder::new()
            .name("theatre-book".to_owned())
            .spawn(move || {
                for event in events {
                    for agent in book.take(&event) {
                        book.pause(sessions.as_ref(), &agent);
                    }
                    book.flush();
                }
            })
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Takes one event into the book; the agents to pause because of it.
    pub fn take(&self, event: &Event) -> Vec<Actor> {
        match event {
            Event::OpApplied { record, .. } => self.applied(record),
            Event::TxCommitted { tx, label, actor, .. } => {
                let mut pause: Vec<Actor> =
                    std::mem::take(&mut *self.deferred.lock().unwrap_or_else(PoisonError::into_inner));
                pause.extend(self.committed(*tx, label, actor));
                pause.dedup();
                return pause;
            }
            Event::TxRolledBack { tx, .. } => {
                self.open.lock().unwrap_or_else(PoisonError::into_inner).remove(tx);
            }
            Event::Undone { tx, .. } => self.mark_undone(*tx, true),
            Event::Redone { tx, .. } => self.mark_undone(*tx, false),
            _ => {}
        }
        Vec::new()
    }

    fn applied(&self, record: &OpRecord) {
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        let step = open.entry(record.tx).or_insert_with(|| Open {
            actor: record.actor.clone(),
            cause: record.cause.as_ref().and_then(|c| c.text.clone()),
            started: record.at,
            ended: record.at,
            ops: 0,
            changes: false,
            rationale: None,
            op: String::new(),
            parts: Vec::new(),
        });
        step.ended = record.at;
        step.ops += 1;
        step.op.clone_from(&record.op);
        for change in &record.patch.changes {
            if let Some(part) = part_of(&change.path)
                && !step.parts.iter().any(|p| p == part)
            {
                step.parts.push(part.to_owned());
            }
        }
        step.changes |= !record.patch.changes.is_empty();
        if step.cause.is_none() {
            step.cause = record.cause.as_ref().and_then(|c| c.text.clone());
        }
    }

    fn committed(&self, tx: TxId, label: &str, actor: &Actor) -> Option<Actor> {
        let open = self.open.lock().unwrap_or_else(PoisonError::into_inner).remove(&tx)?;
        if !open.changes {
            // A query or a Theatre note: nothing to show as a step.
            return None;
        }
        let once = {
            let mut once = self.once.lock().unwrap_or_else(PoisonError::into_inner);
            let at = once.iter().position(|a| a == actor);
            at.map(|at| once.remove(at)).is_some()
        };
        self.write(|book| {
            if let Some(step) = book.step_mut(tx) {
                // A coalescing op landed in a step already committed: it grows.
                step.ended = open.ended;
                step.ops += open.ops;
                step.op = open.op;
                for part in open.parts {
                    if !step.parts.contains(&part) {
                        step.parts.push(part);
                    }
                }
                return None;
            }
            let item = book.plan_mut(actor).and_then(|plan| {
                let running = plan.items.iter_mut().find(|i| i.status == ItemStatus::Running)?;
                running.steps.push(tx);
                Some(running.id.clone())
            });
            book.steps.push(Step {
                tx,
                label: label.to_owned(),
                actor: actor.clone(),
                cause: open.cause,
                started: open.started,
                ended: open.ended,
                ops: open.ops,
                op: open.op,
                parts: open.parts,
                item,
                rationale: open.rationale,
                gate: None,
                undone: false,
            });
            let excess = book.steps.len().saturating_sub(KEPT_STEPS);
            book.steps.drain(..excess);
            (once || book.mode(actor) == PauseMode::EveryStep).then(|| actor.clone())
        })
    }

    fn mark_undone(&self, tx: TxId, undone: bool) {
        if self.read(|book| book.step(tx).is_some()) {
            self.write(|book| {
                if let Some(step) = book.step_mut(tx) {
                    step.undone = undone;
                }
            });
        }
    }

    /// Pauses `agent`'s session as its principal, for the person's pause mode.
    pub fn pause(&self, sessions: &dyn Sessions, agent: &Actor) {
        let Some(session) = sessions.session_of(agent) else { return };
        if let Err(error) = sessions.pause(session.id, &session.on_behalf_of) {
            tracing::warn!(%error, "the Theatre could not pause the agent");
        }
    }

    /// Sets `owner`'s plan to `items`, keeping the steps of items that stay. Returns the agent to pause when an item
    /// finished under the "each item" mode.
    pub fn set_plan(&self, owner: &Actor, items: Vec<(String, String, Option<ItemStatus>)>) -> Option<Actor> {
        self.write(|book| {
            let old = book.plan(owner).cloned();
            let finished_before = |id: &str| {
                old.as_ref().and_then(|p| p.items.iter().find(|i| i.id == id)).is_some_and(|i| i.status.finished())
            };
            let mut finished = false;
            let items: Vec<PlanItem> = items
                .into_iter()
                .map(|(id, text, status)| {
                    let kept = old.as_ref().and_then(|p| p.items.iter().find(|i| i.id == id));
                    let status = status.or(kept.map(|i| i.status)).unwrap_or_default();
                    finished |= status.finished() && !finished_before(&id);
                    PlanItem { steps: kept.map(|i| i.steps.clone()).unwrap_or_default(), id, text, status }
                })
                .collect();
            match book.plan_mut(owner) {
                Some(plan) => plan.items = items,
                None => book.plans.push(Plan { owner: owner.clone(), items }),
            }
            (finished && book.mode(owner) == PauseMode::EachItem).then(|| owner.clone())
        })
    }

    /// Sets one item's status. Returns the agent to pause, as [`set_plan`](Self::set_plan) does.
    ///
    /// # Errors
    /// No such item in `owner`'s plan.
    pub fn set_item(&self, owner: &Actor, id: &str, status: ItemStatus) -> Result<Option<Actor>, String> {
        self.write(|book| {
            let mode = book.mode(owner);
            let plan = book.plan_mut(owner).ok_or_else(|| "post a plan with theatre.plan first".to_owned())?;
            if status == ItemStatus::Running {
                // One item runs at a time: starting one pauses the one before.
                for item in plan.items.iter_mut().filter(|i| i.status == ItemStatus::Running) {
                    item.status = ItemStatus::Planned;
                }
            }
            let item = plan.items.iter_mut().find(|i| i.id == id).ok_or_else(|| format!("no plan item {id}"))?;
            let finished = status.finished() && !item.status.finished();
            item.status = status;
            Ok((finished && mode == PauseMode::EachItem).then(|| owner.clone()))
        })
    }

    /// Stores `text` as the reason for `tx`; without one, for `actor`'s step still open (its transaction has changed
    /// the project and not committed yet), else for its newest step.
    ///
    /// # Errors
    /// There is no such step.
    pub fn explain(&self, actor: &Actor, tx: Option<TxId>, text: &str) -> Result<TxId, String> {
        if tx.is_none() {
            let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((tx, step)) = open.iter_mut().rev().find(|(_, s)| s.actor == *actor && s.changes) {
                step.rationale = Some(text.to_owned());
                return Ok(*tx);
            }
        }
        let tx = match tx {
            Some(tx) => tx,
            None => self
                .read(|book| book.last_step_of(actor).map(|s| s.tx))
                .ok_or_else(|| "no step to explain yet: pass its tx".to_owned())?,
        };
        self.write(|book| {
            let step = book.step_mut(tx).ok_or_else(|| format!("no step {} in the Theatre", tx.0))?;
            step.rationale = Some(text.to_owned());
            Ok(tx)
        })
    }

    /// Records a failed gate on `tx` (or `actor`'s newest step) and marks the running item failed. Returns the agent to
    /// pause under the "gates" mode (and "each item", since the item ended).
    ///
    /// # Errors
    /// There is no such step.
    pub fn gate_failed(&self, actor: &Actor, tx: Option<TxId>, failure: GateFailure) -> Result<Option<Actor>, String> {
        let tx = match tx {
            Some(tx) => tx,
            None => self
                .read(|book| book.last_step_of(actor).map(|s| s.tx))
                .ok_or_else(|| "no step for the gate yet: pass its tx".to_owned())?,
        };
        self.write(|book| {
            let step = book.step_mut(tx).ok_or_else(|| format!("no step {} in the Theatre", tx.0))?;
            step.gate = Some(failure);
            let (owner, item) = (step.actor.clone(), step.item.clone());
            if let (Some(plan), Some(item)) = (book.plan_mut(&owner), item)
                && let Some(item) = plan.items.iter_mut().find(|i| i.id == item)
            {
                item.status = ItemStatus::FailedGate;
            }
            let mode = book.mode(&owner);
            Ok(matches!(mode, PauseMode::Gates | PauseMode::EachItem | PauseMode::EveryStep).then_some(owner))
        })
    }

    /// Adds a person's note; returns its number.
    pub fn add_note(&self, mut note: Note) -> u64 {
        self.write(|book| {
            note.id = book.notes.last().map_or(1, |n| n.id + 1);
            let id = note.id;
            book.notes.push(note);
            id
        })
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_doc_api::DocPath;

    use super::part_of;

    #[test]
    fn a_part_is_read_from_its_path() {
        let path = |s: &str| DocPath::parse(s).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(part_of(&path("characters/kit/parts/hair/params/locks")), Some("hair"));
        assert_eq!(part_of(&path("characters/kit/parts/hair")), Some("hair"));
        assert_eq!(part_of(&path("characters/kit/face")), None);
        assert_eq!(part_of(&path("scenes/main/parts/x")), None);
    }
}
