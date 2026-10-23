//! The Theatre's steps (TH-02, `shell.md` section 11 items 4 and 5): the approvals waiting for the person pinned on top,
//! then one card per transaction, nested under the plan item that was running when it landed, newest first.
//!
//! A card is the design system's [`StepCard`]: the step's label, who made it, its cause, how long it took, the character
//! before and after, and, when a gate failed on it (TH-11), a warning border with the gate's evidence. Under it, the
//! agent's reason (TH-08) and the gate's finding. Its buttons:
//!
//! - **Rewind** (TH-07) shows the character just before the step in the viewport and, where app core has
//!   `history.restore`, restores it as one undoable step. Branching from a step waits for branches (#2200).
//! - **Explain** shows the agent's reason, or says none is given yet.
//! - **Adjust** reopens the step's op in the palette with its arguments.
//! - **Approve** and **Reject** (TH-06) show on an agent's newest step while it is paused "after each step": Approve
//!   resumes it; Reject asks what should change, sends that to the agent as a note on the step, and resumes it so it can
//!   act on it.
//!
//! Choosing a card shows it in the compare strip.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use egui::{RichText, ScrollArea, Ui};
use omuzaki_doc_api::DocPath;
use omuzaki_ops_api::{Actor, Grants, SessionInfo, Sessions, TxId};
use omuzaki_theatre::{Book, ItemStatus, PauseMode, Step};
use omuzaki_ui_api::panel::{Panel, PanelCtx};
use serde_json::json;

use super::pictures::{Pictures, Source, Sources};
use super::{Theatre, no_book, nobody, paused, problem, status, text};
use crate::ds::icons;
use crate::ds::theme::{Ds, ds};
use crate::ds::widgets::{Button, StatusChip, StepAction, StepCard, StepState, TextField, Thumb};
use crate::panels::agent::approvals;
use crate::panels::agent::{AgentFeed, Agents};
use crate::panels::io::Io;
use crate::panels::text::say;
use crate::shell::history::{HistoryLog, Preview, Viewing, actor_kind, state_at, step_label, who};
use crate::shell::pictures::{Atlas, HistoryPictures};
use crate::shell::requests::{ShellRequest, ShellRequests};

/// How many cards show before "Show older".
pub const FIRST_CARDS: usize = 60;
/// App core's op that restores a past state as one undoable step (#443).
const OP_RESTORE: &str = "history.restore";

/// What the steps list reads besides the book: the shell's views, each optional.
#[derive(Clone, Copy)]
pub struct Views<'a> {
    /// The agent sessions, for Approve, Reject and the pinned approvals.
    pub sessions: Option<&'a Arc<dyn Sessions>>,
    /// The shell's copy of the sessions and approvals.
    pub feed: Option<&'a AgentFeed>,
    /// The op log, for Rewind and Adjust.
    pub log: Option<&'a HistoryLog>,
    /// The viewport's past state, for Rewind.
    pub preview: Option<&'a Preview>,
    /// History's pictures, for the cards.
    pub pictures: Option<&'a HistoryPictures>,
    /// The shell's request queue, for Adjust.
    pub requests: Option<&'a ShellRequests>,
}

impl<'a> Views<'a> {
    /// The views from a panel's context.
    pub fn from_ctx(ctx: &PanelCtx<'a>) -> Self {
        Self {
            sessions: ctx.view::<Agents>().map(|a| &a.0),
            feed: ctx.view::<AgentFeed>(),
            log: ctx.view::<HistoryLog>(),
            preview: ctx.view::<Preview>(),
            pictures: ctx.view::<HistoryPictures>(),
            requests: ctx.view::<ShellRequests>(),
        }
    }
}

/// The steps list.
#[derive(Default)]
pub struct StepsView {
    /// The book's revision the list was built from, and the book then.
    seen: Option<(u64, Book)>,
    atlas: Atlas,
    pictures: Pictures,
    /// Cards whose Explain was pressed with no reason to show.
    asked: HashSet<TxId>,
    /// The step being rejected, and the comment so far.
    rejecting: Option<(TxId, String)>,
    /// How many cards show.
    shown: usize,
    /// What the last control said when it was refused.
    problem: Option<String>,
    /// What the last control could not do, when it is worth saying (a rewind with no restore in this build).
    note: Option<String>,
}

impl std::fmt::Debug for StepsView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StepsView").field("shown", &self.shown).finish_non_exhaustive()
    }
}

/// A group of cards: a plan item's steps, or the steps under none.
struct Group {
    title: Option<(String, ItemStatus)>,
    steps: Vec<usize>,
}

impl StepsView {
    /// Draws the list.
    pub fn ui(&mut self, ui: &mut Ui, theatre: &Theatre, io: Io<'_>, views: Views<'_>) {
        let ds = ds(ui.ctx());
        if self.shown == 0 {
            self.shown = FIRST_CARDS;
        }
        let revision = theatre.book.revision();
        if self.seen.as_ref().is_none_or(|(seen, _)| *seen != revision) {
            self.seen = Some((revision, theatre.book.snapshot()));
        }
        let Some((_, book)) = self.seen.take() else { return };
        let approvals = views.feed.map_or(&[][..], AgentFeed::approvals);
        if book.steps.is_empty() && approvals.is_empty() {
            nobody(ui, &ds);
            self.seen = Some((revision, book));
            return;
        }
        ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
            self.approvals(ui, &ds, views, io.actor);
            problem(ui, &ds, self.problem.as_deref());
            if let Some(note) = &self.note {
                ui.add(egui::Label::new(RichText::new(note).color(ds.theme.text2)).wrap());
            }
            Self::filter_line(ui, &ds, theatre, &book);
            let groups = groups(&book, theatre);
            let mut drawn = 0;
            let total: usize = groups.iter().map(|g| g.steps.len()).sum();
            for group in &groups {
                if drawn >= self.shown {
                    break;
                }
                if let Some((title, item_status)) = &group.title {
                    group_heading(ui, &ds, title, *item_status);
                } else if groups.len() > 1 {
                    ui.label(RichText::new(say(&ds, text::OTHER_STEPS, &[])).font(ds.text.strong.clone()));
                }
                for &at in &group.steps {
                    if drawn >= self.shown {
                        break;
                    }
                    self.card(ui, &ds, theatre, io, views, &book, at);
                    drawn += 1;
                }
                ui.add_space(8.0);
            }
            if total > drawn {
                let more = (total - drawn).min(FIRST_CARDS).to_string();
                if ui.add(Button::new(&say(&ds, text::OLDER, &[("count", &more)])).ghost()).clicked() {
                    self.shown += FIRST_CARDS;
                }
            }
        });
        self.seen = Some((revision, book));
    }

    /// The approvals waiting for the person, pinned above the steps.
    fn approvals(&mut self, ui: &mut Ui, ds: &Ds, views: Views<'_>, person: &Actor) {
        let (Some(feed), Some(sessions)) = (views.feed, views.sessions) else { return };
        for batch in approvals::batches(feed.approvals()) {
            let session = feed.session(batch.first.session);
            if let Some(approvals::Answer::Decide(ids, decision)) =
                approvals::card(ui, ds, &batch, session, None, "theatre")
            {
                self.problem =
                    approvals::resolve_all(sessions.as_ref(), &ids, &decision, person).err().map(|e| e.message);
            }
            ui.add_space(6.0);
        }
    }

    /// While the list is filtered to one item: which, and the way back.
    fn filter_line(ui: &mut Ui, ds: &Ds, theatre: &Theatre, book: &Book) {
        let Some((owner, id)) = theatre.looking().item else { return };
        let item = book.plan(&owner).and_then(|p| p.items.iter().find(|i| i.id == id)).map(|i| i.text.clone());
        let Some(item) = item else {
            theatre.look(|l| l.item = None);
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(say(ds, text::SHOWING_ITEM, &[("item", &item)])).color(ds.theme.text));
            if ui.add(Button::new(&say(ds, text::SHOW_ALL, &[])).ghost().icon(icons::X)).clicked() {
                theatre.look(|l| l.item = None);
            }
        });
    }

    /// One step's card, its lines and its controls.
    #[allow(clippy::too_many_arguments, reason = "one card: what it shows and what its buttons reach")]
    fn card(&mut self, ui: &mut Ui, ds: &Ds, theatre: &Theatre, io: Io<'_>, views: Views<'_>, book: &Book, at: usize) {
        let step = &book.steps[at];
        let actor = who(ui.ctx(), &step.actor);
        let session = views.feed.and_then(|f| f.live().find(|s| s.actor == step.actor));
        let pending = session.is_some_and(|s| awaits_review(book, s, step));
        let state = if pending {
            StepState::Pending
        } else if step.gate.is_some() {
            StepState::FailedGate
        } else {
            StepState::Applied
        };
        let previous = at.checked_sub(1).map(|p| book.steps[p].tx);
        let before = previous.map_or(Thumb::Missing, |tx| self.thumb(ui, views, tx));
        let after = self.thumb(ui, views, step.tx);
        let age = say(ds, text::SECONDS, &[("seconds", &format!("{:.1}", step.seconds()))]);
        let mut card = StepCard::new(&step.label, &actor, actor_kind(&step.actor))
            .age(&age)
            .state(state)
            .pictures(before, after)
            .id_salt(("theatre-step", step.tx));
        if let Some(cause) = &step.cause {
            card = card.cause(cause);
        }
        if let Some(evidence) = step.gate.as_ref().and_then(|g| g.evidence.as_deref()) {
            let sources = Sources { steps: views.pictures, renders: theatre.renders.as_ref() };
            let thumb = Source::parse(evidence)
                .and_then(|source| self.pictures.get(ui.ctx(), sources, &source))
                .map_or(Thumb::Missing, |p| Thumb::Ready(p.texture.id()));
            card = card.evidence(thumb);
        }
        let out = card.show(ui);
        if out.response.clicked() {
            theatre.look(|l| l.step = Some(step.tx));
        }
        self.lines(ui, ds, step);
        match out.action {
            Some(StepAction::Rewind) => self.rewind(ds, io, views, step, previous),
            Some(StepAction::Explain) => {
                if !self.asked.insert(step.tx) {
                    self.asked.remove(&step.tx);
                }
            }
            Some(StepAction::Adjust) => Self::adjust(views, step.tx),
            Some(StepAction::Approve) => self.resume(views, session, io.actor),
            Some(StepAction::Reject) => self.rejecting = Some((step.tx, String::new())),
            _ => {}
        }
        if self.rejecting.as_ref().is_some_and(|(tx, _)| *tx == step.tx) {
            self.reject_form(ui, ds, io, views, step, session);
        }
        ui.add_space(6.0);
    }

    /// The reason, the gate's finding and "undone", under a card.
    fn lines(&self, ui: &mut Ui, ds: &Ds, step: &Step) {
        let t = &ds.theme;
        let line = |ui: &mut Ui, text: &str, colour| {
            ui.add(egui::Label::new(RichText::new(text).font(ds.text.small.clone()).color(colour)).wrap());
        };
        if let Some(gate) = &step.gate {
            line(ui, &say(ds, text::GATE_LINE, &[("gate", &gate.gate), ("message", &gate.message)]), t.warn);
        }
        match &step.rationale {
            Some(reason) => line(ui, &say(ds, text::BECAUSE, &[("reason", reason)]), t.text2),
            None if self.asked.contains(&step.tx) => line(ui, &say(ds, text::NO_REASON, &[]), t.text3),
            None => {}
        }
        if step.undone {
            line(ui, &say(ds, text::UNDONE, &[]), t.text3);
        }
    }

    /// A step's picture as a card thumbnail.
    fn thumb(&mut self, ui: &Ui, views: Views<'_>, tx: TxId) -> Thumb {
        let Some(pictures) = views.pictures else { return Thumb::Missing };
        self.atlas.texture(ui.ctx(), pictures, tx, Some(Instant::now())).map_or(Thumb::Loading, Thumb::Ready)
    }

    /// Rewind: the character just before the step in the viewport, then app core's restore when it has one.
    fn rewind(&mut self, ds: &Ds, io: Io<'_>, views: Views<'_>, step: &Step, previous: Option<TxId>) {
        (self.problem, self.note) = (None, None);
        let Some(log) = views.log else { return };
        let Some(first) = log.records.iter().find(|r| r.tx == step.tx) else { return };
        let before = first.seq.saturating_sub(1);
        if let Some(preview) = views.preview {
            let current = io.runner.get(&DocPath::root(), &Grants::person()).map(Option::unwrap_or_default);
            match current.map_err(|e| e.message).and_then(|doc| state_at(doc, log, before).map_err(|e| e.to_string())) {
                Ok(doc) => {
                    let label = log.records.iter().rev().find(|r| r.seq <= before).map(step_label).unwrap_or_default();
                    preview.view(Viewing { seq: before, label, doc: Arc::new(doc) });
                }
                Err(why) => self.problem = Some(why),
            }
        }
        if io.runner.ops().iter().any(|op| op.name == OP_RESTORE)
            && let Some(previous) = previous
        {
            self.problem = io.run(OP_RESTORE, json!({ "to": { "tx": previous } }), None).err().map(|e| e.message);
        } else {
            self.note = Some(say(ds, text::NO_RESTORE, &[]).into_owned());
        }
    }

    /// Adjust: the step's newest op in the palette, with its arguments.
    fn adjust(views: Views<'_>, tx: TxId) {
        let (Some(log), Some(requests)) = (views.log, views.requests) else { return };
        if let Some(record) = log.records.iter().rev().find(|r| r.tx == tx) {
            requests.push(ShellRequest::Adjust { op: record.op.clone(), args: record.args.clone() });
        }
    }

    fn resume(&mut self, views: Views<'_>, session: Option<&SessionInfo>, person: &Actor) {
        let (Some(sessions), Some(session)) = (views.sessions, session) else { return };
        self.problem = sessions.resume(session.id, person).err().map(|e| e.message);
    }

    /// Reject's comment: what should change, sent as a note on the step, then the agent resumes to act on it.
    fn reject_form(
        &mut self,
        ui: &mut Ui,
        ds: &Ds,
        io: Io<'_>,
        views: Views<'_>,
        step: &Step,
        session: Option<&SessionInfo>,
    ) {
        let Some((_, comment)) = self.rejecting.as_mut() else { return };
        let label = say(ds, text::REJECT_WHY, &[]);
        ui.add(TextField::new(&label, comment).id_salt(("theatre-reject", step.tx)));
        let send = say(ds, text::REJECT_SEND, &[]);
        let ready = !comment.trim().is_empty();
        let button = Button::new(&send).primary().icon(icons::CHAT_CIRCLE_TEXT);
        let button = if ready { button } else { button.disabled(&label) };
        if ui.add(button).clicked() {
            let note = say(ds, text::REJECTED_NOTE, &[("label", &step.label), ("comment", comment.trim())]);
            match io.run("theatre.direct", json!({ "text": note, "step": step.tx }), None) {
                Ok(_) => {
                    self.rejecting = None;
                    self.resume(views, session, io.actor);
                }
                Err(e) => self.problem = Some(e.message),
            }
        }
    }
}

/// Whether `step` is `session`'s newest and waits for the person: the session is paused, and its mode pauses it after
/// every step.
fn awaits_review(book: &Book, session: &SessionInfo, step: &Step) -> bool {
    paused(session)
        && book.mode(&session.actor) == PauseMode::EveryStep
        && book.last_step_of(&session.actor).is_some_and(|last| last.tx == step.tx)
}

/// The cards in order: each plan's items with steps, in plan order (the list filtered to one item when the person
/// chose one), then the steps under no item; newest first within each.
fn groups(book: &Book, theatre: &Theatre) -> Vec<Group> {
    let filter = theatre.looking().item;
    let index = |tx: TxId| book.steps.iter().rposition(|s| s.tx == tx);
    let mut out = Vec::new();
    let mut placed = HashSet::new();
    for plan in &book.plans {
        for item in &plan.items {
            let chosen = filter.as_ref().is_none_or(|(o, id)| *o == plan.owner && *id == item.id);
            let steps: Vec<usize> = item.steps.iter().rev().filter_map(|tx| index(*tx)).collect();
            placed.extend(steps.iter().copied());
            if chosen && !steps.is_empty() {
                out.push(Group { title: Some((item.text.clone(), item.status)), steps });
            }
        }
    }
    if filter.is_none() {
        let rest: Vec<usize> = (0..book.steps.len()).rev().filter(|at| !placed.contains(at)).collect();
        if !rest.is_empty() {
            out.push(Group { title: None, steps: rest });
        }
    }
    out
}

/// A plan item's heading over its cards.
fn group_heading(ui: &mut Ui, ds: &Ds, title: &str, item_status: ItemStatus) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(title).font(ds.text.strong.clone()).color(ds.theme.text));
        let (tone, _, word) = status(ds, item_status);
        ui.add(StatusChip::new(tone, &word));
    });
}

/// [`StepsView`] as a registered panel.
#[derive(Debug, Default)]
pub struct StepsPanel(StepsView);

impl Panel for StepsPanel {
    fn ui(&mut self, ui: &mut Ui, ctx: &PanelCtx<'_>) {
        match Theatre::from_ctx(ctx) {
            Some(theatre) => self.0.ui(ui, theatre, Io::from_ctx(ctx), Views::from_ctx(ctx)),
            None => no_book(ui, &ds(ui.ctx())),
        }
    }
}
