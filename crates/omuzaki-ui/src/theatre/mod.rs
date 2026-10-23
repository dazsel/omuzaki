//! The Build Theatre (spec v3 §13, `shell.md` section 11): a workspace for watching agents build the character and
//! steering them while they do.
//!
//! - **Plan board** ([`plan`], TH-03): each agent's plan as a checklist, each item with its status and its steps;
//!   choosing one filters the steps.
//! - **Stage** ([`stage`], TH-01, TH-06, TH-09, TH-10): the controls strip (the agent, Pause, Step, Resume, the pause
//!   mode, Draw to direct, the budget meter and its limits, Stop) over the live viewport, which names the current op,
//!   highlights the part being edited, follows the agent's work and pops out into its own window; drawing on it leaves
//!   the agents a note with the region, the part under it, the camera and a picture.
//! - **Compare strip** ([`compare`], TH-04): the reference, the previous step and the current one at the camera every
//!   step is pictured from, with how much changed.
//! - **Steps** ([`steps`], TH-02, TH-06, TH-07, TH-08, TH-11): the approvals waiting for the person, then one card per
//!   transaction nested under its plan item, with its pictures, cause, duration and reason, gate cards with their
//!   evidence, Rewind, Explain, Adjust, and Approve or Reject with a comment in "after each step" mode.
//!
//! Everything the Theatre shows beyond the op log is in the app core's [`TheatreBook`], which the agents write through
//! the `theatre.*` ops; every control here acts through an op (`theatre.pause_mode`, `theatre.direct`,
//! `theatre.reference`) or as the person through the session registry (pause, resume, the budget), never by writing
//! the book or the document itself.

pub mod compare;
pub mod pictures;
pub mod plan;
pub mod registration;
pub mod stage;
pub mod steps;
pub mod text;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use egui::{RichText, Ui};
use omuzaki_ops_api::{Actor, SessionInfo, SessionState, TxId};
use omuzaki_store_api::Store;
use omuzaki_theatre::{Book, ItemStatus, TheatreBook};
use omuzaki_ui_api::panel::PanelCtx;

use crate::ds::icons;
use crate::ds::theme::Ds;
use crate::ds::tokens::Status as Tone;
use crate::ds::widgets::EmptyState;
use crate::panels::agent::AgentFeed;
use crate::panels::text::say;

/// The Theatre as the app shares it with its panels: the core's book, the store the agents' renders are kept in, and
/// what the person is looking at. Clones share one of each.
#[derive(Clone)]
pub struct Theatre {
    /// The app core's book.
    pub book: TheatreBook,
    /// Where `omuzaki://render/` pictures (evidence, references) are kept, when the app has a store.
    pub renders: Option<Arc<dyn Store>>,
    looking: Rc<RefCell<Looking>>,
}

impl std::fmt::Debug for Theatre {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Theatre").field("book", &self.book).finish_non_exhaustive()
    }
}

/// What the person is looking at, shared by the Theatre's panels.
#[derive(Clone, Debug)]
pub struct Looking {
    /// The agent the controls act on; `None` follows the newest working one.
    pub agent: Option<Actor>,
    /// The plan item the steps are filtered to: its owner and id.
    pub item: Option<(Actor, String)>,
    /// The step the compare strip shows; `None` for the newest.
    pub step: Option<TxId>,
    /// Draw to direct is on.
    pub pen: bool,
    /// The viewport keeps the agent's work in view.
    pub follow: bool,
    /// The stage is in a window of its own.
    pub popped: bool,
}

impl Default for Looking {
    fn default() -> Self {
        Self { agent: None, item: None, step: None, pen: false, follow: true, popped: false }
    }
}

impl Theatre {
    /// The Theatre over `book`, with the render store when the app has one.
    pub fn new(book: TheatreBook, renders: Option<Arc<dyn Store>>) -> Self {
        Self { book, renders, looking: Rc::default() }
    }

    /// What the person is looking at.
    pub fn looking(&self) -> Looking {
        self.looking.borrow().clone()
    }

    /// Changes what the person is looking at.
    pub fn look(&self, f: impl FnOnce(&mut Looking)) {
        f(&mut self.looking.borrow_mut());
    }

    /// The agent the controls act on: the one chosen while its session lives, else the newest live session that made a
    /// step, else the newest live session.
    pub fn agent<'a>(&self, feed: &'a AgentFeed, book: &Book) -> Option<&'a SessionInfo> {
        let live: Vec<&SessionInfo> = feed.live().collect();
        let chosen = self.looking.borrow().agent.clone();
        if let Some(chosen) = chosen
            && let Some(session) = live.iter().find(|s| s.actor == chosen)
        {
            return Some(session);
        }
        let stepped = book.steps.iter().rev().find_map(|step| live.iter().find(|s| s.actor == step.actor));
        stepped.or_else(|| live.last()).copied()
    }

    /// The Theatre from a panel's context.
    pub fn from_ctx<'a>(ctx: &PanelCtx<'a>) -> Option<&'a Self> {
        ctx.view::<Self>()
    }
}

/// A plan item's status as a tone, an icon and words (never the colour alone).
pub fn status(ds: &Ds, status: ItemStatus) -> (Tone, &'static str, String) {
    let (tone, icon, word) = match status {
        ItemStatus::Planned => (Tone::Info, icons::CIRCLE, text::PLANNED),
        ItemStatus::Running => (Tone::Info, icons::CIRCLE_DASHED, text::RUNNING),
        ItemStatus::NeedsReview => (Tone::Warn, icons::EYE, text::NEEDS_REVIEW),
        ItemStatus::Done => (Tone::Ok, icons::CHECK_CIRCLE, text::DONE),
        ItemStatus::FailedGate => (Tone::Danger, icons::WARNING, text::FAILED_GATE),
    };
    (tone, icon, say(ds, word, &[]).into_owned())
}

/// Whether `session` is paused.
pub const fn paused(session: &SessionInfo) -> bool {
    matches!(session.state, SessionState::Paused { .. })
}

/// The panel body without the Theatre's book.
pub fn no_book(ui: &mut Ui, ds: &Ds) {
    EmptyState::new(icons::BINOCULARS, &say(ds, text::NO_BOOK, &[]), &say(ds, text::NO_BOOK_HOW, &[])).show(ui);
}

/// The panel body while no agent has done anything.
pub fn nobody(ui: &mut Ui, ds: &Ds) {
    EmptyState::new(icons::BINOCULARS, &say(ds, text::NOBODY, &[]), &say(ds, text::NOBODY_HOW, &[])).show(ui);
}

/// A refusal under the controls, until the next control.
pub fn problem(ui: &mut Ui, ds: &Ds, problem: Option<&str>) {
    if let Some(problem) = problem {
        let line = say(ds, text::REFUSED, &[("reason", problem)]);
        ui.add(egui::Label::new(RichText::new(line).color(ds.theme.danger)).wrap());
    }
}
