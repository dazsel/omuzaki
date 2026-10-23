//! The Agent panel (`shell.md` section 10): the compact view of agent work.
//!
//! - **Sessions.** One row per live session: its name and model, the person it works for, how it is connected, where
//!   it stands (working, idle, waiting for you, paused), its open transaction, its steps and its spend against its
//!   budget. An icon tells the built-in assistant from an external agent. Stop is always there while a session runs;
//!   Pause or Resume, Undo its steps and Revoke sit beside it, and Grants opens the grants popover ([`grants`]).
//! - **Approvals.** Every request waiting for the person, batched, as approval cards ([`approvals`]), decided here or
//!   in the stack at the top right of the window.
//!
//! It reads the sessions from the [`AgentFeed`], the studio's copy kept current from the runner's events, never by
//! asking the registry each frame. Every control acts as the person through the session registry ([`Agents`]), which
//! checks the person is the session's principal; nothing here changes the document.

pub mod approvals;
pub mod feed;
pub mod fences;
pub mod grants;
pub mod review;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use egui::{Frame, Id, Margin, Rect, RichText, ScrollArea, Ui, vec2};
use omuzaki_ops_api::{
    Actor, EndReason, ErrorCode, Grants, OpError, PauseReason, Scope, SessionId, SessionInfo, SessionState, Sessions,
    Transport,
};
use omuzaki_ui_api::panel::{Panel, PanelCtx};

pub use self::feed::AgentFeed;
use self::feed::{Status, has_credential};
use self::grants::{GrantChange, GrantsState, grants_ui};
use self::review::Reviews;
use super::text::{self, say};
use crate::ds::icons;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{Status as Tone, radius};
use crate::ds::widgets::chip::StatusChip;
use crate::ds::widgets::dialog::{Dialog, DialogAction};
use crate::ds::widgets::{Button, EmptyState, Toggle};

/// The agent sessions of the app's executor, shared with panels as a view model.
#[derive(Clone)]
pub struct Agents(pub Arc<dyn Sessions>);

/// Remembers a session's grants on its credential, for this project (`agent-sessions.md` section 3.3): an HTTP token's
/// stored record, the built-in assistant's settings. The app shares one ([`Credentials`]) when its transports keep
/// credentials; without one, "Grant for this project" is not offered.
pub trait CredentialGrants {
    /// Remembers `grants` for `session`'s credential.
    ///
    /// # Errors
    /// Whatever stopped it: the credential is gone, or the grants exceed the person's.
    fn remember(&self, session: &SessionInfo, grants: &Grants) -> Result<(), OpError>;
}

/// Where agents connect to this studio: the app core's loopback MCP address, shared with panels as a view model once
/// the port is open. The connect card names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpEndpoint(pub String);

/// The credential store, shared with panels as a view model.
#[derive(Clone)]
pub struct Credentials(pub Arc<dyn CredentialGrants>);

/// The Agent panel's id, as it registers.
pub const ID: &str = "agent";

/// A control that waits for the person's two-step confirm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Confirm {
    /// End the session, and with `credential` its credential too.
    Revoke { id: SessionId, credential: bool },
    /// Undo every step of the session.
    Undo { id: SessionId },
    /// Grant a scope that reaches outside the project.
    Grant { id: SessionId, scope: Scope },
}

/// The Agent panel.
#[derive(Debug, Default)]
pub struct AgentPanel {
    /// What the last control said when it was refused, shown until the next one.
    problem: Option<String>,
    /// What the last control did, when it is worth saying (where an export went).
    note: Option<String>,
    /// The session whose grants popover is open, and the button it opened from.
    grants: Option<(SessionId, Rect)>,
    /// Each session's popover fields.
    grants_state: HashMap<SessionId, GrantsState>,
    /// The control waiting for its confirm.
    confirm: Option<Confirm>,
    /// The panel's own copy of the sessions, when the shell shares none (the panel alone, in a test or a plugin host).
    own: Option<AgentFeed>,
    /// The egui frame `own` last followed, so a pass egui runs twice sees the events once.
    followed: Option<u64>,
}

impl Panel for AgentPanel {
    fn ui(&mut self, ui: &mut Ui, ctx: &PanelCtx<'_>) {
        let ds = ds(ui.ctx());
        let Some(Agents(sessions)) = ctx.view::<Agents>() else {
            return connect(ui, &ds, ctx.view::<McpEndpoint>());
        };
        let shared = ctx.view::<AgentFeed>();
        if shared.is_none() {
            let frame = ui.ctx().cumulative_frame_nr();
            let own = self.own.get_or_insert_with(|| AgentFeed::load(sessions.as_ref()));
            if self.followed != Some(frame) {
                own.follow(ctx.events());
                self.followed = Some(frame);
            }
        }
        let Some(feed) = shared.or(self.own.as_ref()) else { return };
        let feed = feed.clone();
        let now = Instant::now();
        if let Some(after) = feed.next_change(now) {
            ui.ctx().request_repaint_after(after);
        }
        let live: Vec<&SessionInfo> = feed.live().collect();
        if live.is_empty() && feed.approvals().is_empty() {
            return connect(ui, &ds, ctx.view::<McpEndpoint>());
        }
        let person = ctx.actor();
        let desk = ctx.view::<Reviews>().map(|r| r.0.as_ref());
        let credentials = ctx.view::<Credentials>();
        ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
            for batch in approvals::batches(feed.approvals()) {
                let session = feed.session(batch.first.session);
                if let Some(answer) = approvals::card(ui, &ds, &batch, session, desk, "panel") {
                    self.answer(sessions.as_ref(), &feed, desk, answer, person);
                }
                ui.add_space(6.0);
            }
            for session in &live {
                self.session_row(ui, &ds, sessions.as_ref(), &feed, session, person, now);
            }
            if let Some(problem) = &self.problem {
                ui.label(RichText::new(problem).color(ds.theme.danger));
            }
            if let Some(note) = &self.note {
                ui.label(RichText::new(note).color(ds.theme.text2));
            }
        });
        self.grants_popover(ui, &ds, sessions.as_ref(), &feed, credentials, person);
        self.confirm_dialog(ui, &ds, sessions.as_ref(), &feed, person);
    }
}

impl AgentPanel {
    fn report<T>(&mut self, result: Result<T, OpError>) {
        self.problem = result.err().map(|e| e.message);
        self.note = None;
    }

    /// Carries out what the person did on an approval card.
    fn answer(
        &mut self,
        sessions: &dyn Sessions,
        feed: &AgentFeed,
        desk: Option<&dyn review::ReviewDesk>,
        answer: approvals::Answer,
        person: &Actor,
    ) {
        match answer {
            approvals::Answer::Decide(ids, decision) => {
                self.report(approvals::resolve_all(sessions, &ids, &decision, person));
            }
            approvals::Answer::Export(id) => {
                let approval = feed.approvals().iter().find(|a| a.id == id);
                let exported = match (desk, approval) {
                    (Some(desk), Some(approval)) => desk.export(approval),
                    _ => Err(OpError::unavailable("review")),
                };
                match exported {
                    Ok(place) => (self.problem, self.note) = (None, Some(place)),
                    Err(e) => self.report::<()>(Err(e)),
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments, reason = "one row of the panel: what it shows and what its controls call")]
    fn session_row(
        &mut self,
        ui: &mut Ui,
        ds: &Ds,
        sessions: &dyn Sessions,
        feed: &AgentFeed,
        session: &SessionInfo,
        person: &Actor,
        now: Instant,
    ) {
        let t = &ds.theme;
        let status = feed.status(session, now);
        let frame = Frame::new()
            .fill(t.bg2)
            .stroke(egui::Stroke::new(1.0, if status == Status::Waiting { t.agent } else { t.line }))
            .corner_radius(radius::CARD)
            .inner_margin(Margin::same(10));
        frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.horizontal(|ui| {
                let glyph = if session.transport == Transport::InApp { icons::SPARKLE } else { icons::ROBOT };
                ui.label(RichText::new(glyph).font(ds.text.icon(16.0, false)).color(t.agent));
                ui.label(RichText::new(&session.client).font(ds.text.strong.clone()));
                if let Some(model) = &session.model {
                    ui.label(RichText::new(model).font(ds.text.small.clone()).color(t.text2));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = say(ds, text::AGENT_GRANTS, &[]);
                    let name = say(ds, text::AGENT_GRANTS_NAME, &[("name", &session.client)]);
                    let button = ui.add(Button::new(&label).ghost().icon(icons::SHIELD_CHECK).accessible_label(&name));
                    if button.clicked() {
                        let open = self.grants.is_some_and(|(id, _)| id == session.id);
                        self.grants = if open { None } else { Some((session.id, button.rect)) };
                    }
                });
            });
            let who = person_name(ds, &session.on_behalf_of);
            let how = transport(ds, &session.transport);
            let line = say(ds, text::AGENT_FOR, &[("person", &who), ("transport", &how)]);
            ui.label(RichText::new(line).font(ds.text.small.clone()).color(t.text2));
            if let Some(role) = &session.role {
                ui.label(RichText::new(say(ds, text::AGENT_ROLE, &[("role", role)])).color(t.text2));
            }
            ui.horizontal_wrapped(|ui| {
                let (tone, word) = status_words(ds, status);
                ui.add(StatusChip::new(tone, &word));
                let steps = session.steps.to_string();
                let word = one_or_many(session.steps, text::AGENT_ONE_STEP, text::AGENT_STEPS);
                ui.label(RichText::new(say(ds, word, &[("steps", &steps)])).color(t.text2));
                let spend = spend(ds, session);
                if !spend.is_empty() {
                    ui.label(RichText::new(spend).color(t.text2));
                }
            });
            if let Some(tx) = &session.open_tx {
                ui.label(RichText::new(say(ds, text::AGENT_OPEN_TX, &[("label", &tx.label)])).color(t.text));
            }
            ui.horizontal_wrapped(|ui| self.controls(ui, ds, sessions, session, status, person));
        });
        ui.add_space(6.0);
    }

    /// Stop, Pause or Resume, Undo its steps, Revoke.
    fn controls(
        &mut self,
        ui: &mut Ui,
        ds: &Ds,
        sessions: &dyn Sessions,
        session: &SessionInfo,
        status: Status,
        person: &Actor,
    ) {
        let name = session.client.as_str();
        let named = |text: &str| say(ds, text::AGENT_CONTROL_NAME, &[("action", text), ("name", name)]).into_owned();
        // Stop is always there while the session runs.
        let stop = say(ds, text::AGENT_STOP, &[]);
        if ui.add(Button::new(&stop).danger().icon(icons::STOP).accessible_label(&named(&stop))).clicked() {
            // Stop pauses and cancels the session's jobs; an executor without Stop yet pauses.
            let stopped = match sessions.stop(session.id, person) {
                Err(e) if e.code == ErrorCode::Unsupported => sessions.pause(session.id, person).map(drop),
                other => other.map(drop),
            };
            self.report(stopped);
        }
        if matches!(status, Status::Paused(_)) {
            let resume = say(ds, text::AGENT_RESUME, &[]);
            if ui.add(Button::new(&resume).icon(icons::PLAY).accessible_label(&named(&resume))).clicked() {
                self.report(sessions.resume(session.id, person));
            }
        } else {
            let pause = say(ds, text::AGENT_PAUSE, &[]);
            if ui.add(Button::new(&pause).icon(icons::PAUSE).accessible_label(&named(&pause))).clicked() {
                self.report(sessions.pause(session.id, person));
            }
        }
        if session.steps > 0 {
            let steps = session.steps.to_string();
            let word = one_or_many(session.steps, text::AGENT_UNDO_ONE, text::AGENT_UNDO_ALL);
            let undo = say(ds, word, &[("steps", &steps)]);
            let name = named(&undo);
            let button = Button::new(&undo).ghost().icon(icons::ARROW_COUNTER_CLOCKWISE).accessible_label(&name);
            if ui.add(button).clicked() {
                self.confirm = Some(Confirm::Undo { id: session.id });
            }
        }
        let revoke = say(ds, text::AGENT_REVOKE, &[]);
        if ui.add(Button::new(&revoke).ghost().icon(icons::X_CIRCLE).accessible_label(&named(&revoke))).clicked() {
            self.confirm = Some(Confirm::Revoke { id: session.id, credential: has_credential(&session.transport) });
        }
    }

    /// The grants popover beside the button that opened it.
    fn grants_popover(
        &mut self,
        ui: &Ui,
        ds: &Ds,
        sessions: &dyn Sessions,
        feed: &AgentFeed,
        credentials: Option<&Credentials>,
        person: &Actor,
    ) {
        let Some((id, anchor)) = self.grants else { return };
        let Some(session) = feed.session(id).filter(|s| !matches!(s.state, SessionState::Ended { .. })) else {
            self.grants = None;
            return;
        };
        let ctx = ui.ctx().clone();
        let project = credentials.filter(|_| has_credential(&session.transport));
        let state = self.grants_state.entry(id).or_default();
        let screen = ctx.content_rect();
        let (pivot, at) = if anchor.left() - grants::WIDTH - 24.0 >= screen.left() {
            (egui::Align2::RIGHT_TOP, anchor.left_bottom() + vec2(anchor.width(), 4.0))
        } else {
            (egui::Align2::LEFT_TOP, anchor.left_bottom() + vec2(0.0, 4.0))
        };
        let mut change = None;
        let area = egui::Area::new(Id::new(("agent_grants", id)))
            .order(egui::Order::Foreground)
            .pivot(pivot)
            .fixed_pos(at)
            .constrain(true)
            .show(&ctx, |ui| {
                Frame::popup(ui.style()).fill(ds.theme.bg2).show(ui, |ui| {
                    change = grants_ui(ui, ds, session, &Grants::person(), state, project.is_some());
                });
            });
        let cap = state.cap_micros();
        // A press outside the popover and its button closes it; the press's own position decides, not the hover.
        let pressed_outside = ctx.input(|i| {
            i.pointer.any_pressed()
                && i.pointer.interact_pos().is_some_and(|p| !area.response.rect.contains(p) && !anchor.contains(p))
        });
        if pressed_outside && self.confirm.is_none() || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.grants = None;
        }
        match change {
            Some(GrantChange::Grants(grants)) => self.report(sessions.set_grants(id, grants, person)),
            Some(GrantChange::Budget(budget)) => self.report(sessions.set_budget(id, budget, person)),
            Some(GrantChange::Risky(scope)) => {
                // Spend needs its cap set first: the confirm grants it against the cap as typed.
                if scope == Scope::Spend
                    && let Some(micros) = cap.filter(|c| *c != session.budget.usd_micros)
                {
                    let budget = omuzaki_ops_api::Budget { usd_micros: micros, ..session.budget };
                    self.report(sessions.set_budget(id, budget, person));
                }
                self.confirm = Some(Confirm::Grant { id, scope });
            }
            Some(GrantChange::Project(grants)) => {
                if let Some(Credentials(store)) = project {
                    self.report(store.remember(session, &grants));
                }
            }
            None => {}
        }
    }

    /// The two-step confirm for Revoke, Undo its steps and a grant that reaches outside the project.
    fn confirm_dialog(&mut self, ui: &Ui, ds: &Ds, sessions: &dyn Sessions, feed: &AgentFeed, person: &Actor) {
        let Some(confirm) = self.confirm else { return };
        let id = match confirm {
            Confirm::Revoke { id, .. } | Confirm::Undo { id } | Confirm::Grant { id, .. } => id,
        };
        let Some(session) = feed.session(id) else {
            self.confirm = None;
            return;
        };
        let ctx = ui.ctx().clone();
        let name = session.client.as_str();
        let steps = session.steps.to_string();
        let (title, primary, armed, body) = match confirm {
            Confirm::Revoke { .. } => (
                say(ds, text::REVOKE_TITLE, &[("name", name)]),
                say(ds, text::REVOKE_PRIMARY, &[("name", name)]),
                say(ds, text::REVOKE_CONFIRM, &[("name", name)]),
                say(ds, text::REVOKE_BODY, &[]),
            ),
            Confirm::Undo { .. } => (
                say(ds, text::UNDO_TITLE, &[("name", name)]),
                say(ds, one_or_many(session.steps, text::UNDO_PRIMARY_ONE, text::UNDO_PRIMARY), &[("steps", &steps)]),
                say(ds, one_or_many(session.steps, text::UNDO_CONFIRM_ONE, text::UNDO_CONFIRM), &[("steps", &steps)]),
                say(ds, one_or_many(session.steps, text::UNDO_BODY_ONE, text::UNDO_BODY), &[("steps", &steps)]),
            ),
            Confirm::Grant { scope, .. } => (
                say(ds, text::GRANT_TITLE, &[("name", name), ("scope", scope.name())]),
                say(ds, text::GRANT_PRIMARY, &[("scope", scope.name())]),
                say(ds, text::GRANT_CONFIRM, &[("scope", scope.name())]),
                say(ds, approvals::scope_meaning(scope), &[]),
            ),
        };
        let others: Vec<String> = feed.same_credential(session).iter().map(|s| s.client.clone()).collect();
        let mut credential = matches!(confirm, Confirm::Revoke { credential: true, .. });
        let offers_credential = has_credential(&session.transport);
        let action = Dialog::new(("agent_confirm", id), &title, &primary).dangerous(&armed).show(&ctx, |ui| {
            ui.add(egui::Label::new(RichText::new(body.as_ref()).color(ds.theme.text)).wrap());
            if let Confirm::Revoke { .. } = confirm
                && offers_credential
            {
                let label = say(ds, text::REVOKE_CREDENTIAL, &[]);
                ui.add(Toggle::checkbox(&label, &mut credential).id_salt(("revoke_credential", id)));
                if credential && !others.is_empty() {
                    let names = others.join(", ");
                    let line = say(ds, text::REVOKE_OTHERS, &[("names", &names)]);
                    ui.add(egui::Label::new(RichText::new(line).color(ds.theme.warn)).wrap());
                }
            }
        });
        if let Confirm::Revoke { id, .. } = confirm {
            self.confirm = Some(Confirm::Revoke { id, credential });
        }
        match action {
            Some(DialogAction::Primary) => {
                self.confirm = None;
                match confirm {
                    Confirm::Revoke { id, .. } => {
                        let reason = EndReason::Revoked { by: person.clone(), credential };
                        self.report(sessions.end(id, reason));
                    }
                    Confirm::Undo { id } => self.report(sessions.undo_session(id, person)),
                    Confirm::Grant { id, scope } => {
                        self.report(sessions.set_grants(id, session.grants.clone().with(scope), person));
                    }
                }
            }
            Some(DialogAction::Cancel) => self.confirm = None,
            None => {}
        }
    }
}

/// The person a session works for, as the row names them.
fn person_name(ds: &Ds, actor: &Actor) -> String {
    match actor {
        Actor::Person { name } if name == "you" => say(ds, text::AGENT_YOU, &[]).into_owned(),
        Actor::Person { name } => name.clone(),
        Actor::Agent { id, .. } | Actor::Plugin { id } => id.clone(),
        Actor::System => String::new(),
    }
}

/// How a session is connected, in words.
fn transport(ds: &Ds, transport: &Transport) -> String {
    match transport {
        Transport::Stdio => say(ds, text::TRANSPORT_STDIO, &[]),
        Transport::Http { token } => say(ds, text::TRANSPORT_HTTP, &[("token", token)]),
        Transport::InApp => say(ds, text::TRANSPORT_IN_APP, &[]),
        Transport::Plugin => say(ds, text::TRANSPORT_PLUGIN, &[]),
        Transport::Automation => say(ds, text::TRANSPORT_AUTOMATION, &[]),
        Transport::Replay => say(ds, text::TRANSPORT_REPLAY, &[]),
    }
    .into_owned()
}

/// A status as the chip shows it: its tone and its words (never the colour alone).
fn status_words(ds: &Ds, status: Status) -> (Tone, String) {
    let (tone, word) = match status {
        Status::Working => (Tone::Ok, text::AGENT_WORKING),
        Status::Idle => (Tone::Info, text::AGENT_IDLE),
        Status::Waiting => (Tone::Warn, text::AGENT_WAITING),
        Status::Paused(PauseReason::Budget) => (Tone::Warn, text::AGENT_PAUSED_BUDGET),
        Status::Paused(PauseReason::Pairing) => (Tone::Warn, text::AGENT_PAUSED_PAIRING),
        Status::Paused(PauseReason::Undo) => (Tone::Info, text::AGENT_PAUSED_UNDO),
        Status::Paused(_) => (Tone::Info, text::AGENT_PAUSED),
        Status::Ended => (Tone::Info, text::AGENT_ENDED),
    };
    (tone, say(ds, word, &[]).into_owned())
}

/// The session's spend against its budget ("$0.40 of $2.00 · 1200 tokens"), or nothing when it has neither.
fn spend(ds: &Ds, session: &SessionInfo) -> String {
    let mut parts = Vec::new();
    if session.budget.usd_micros > 0 || session.spent.usd_micros > 0 {
        let (spent, cap) =
            (approvals::dollars(session.spent.usd_micros), approvals::dollars(session.budget.usd_micros));
        parts.push(say(ds, text::AGENT_SPEND, &[("spent", &spent), ("budget", &cap)]).into_owned());
    }
    if session.spent.tokens > 0 {
        let tokens = session.spent.tokens.to_string();
        parts.push(say(ds, text::AGENT_TOKENS, &[("count", &tokens)]).into_owned());
    }
    parts.join(" \u{b7} ")
}

/// No agent is connected: how to connect one, and where when the studio's MCP port is open.
fn connect(ui: &mut Ui, ds: &Ds, endpoint: Option<&McpEndpoint>) {
    let headline = say(ds, text::AGENT_NONE, &[]);
    let mut line = say(ds, text::AGENT_NONE_HOW, &[]).into_owned();
    if let Some(McpEndpoint(url)) = endpoint {
        line.push(' ');
        line.push_str(&say(ds, text::AGENT_NONE_WHERE, &[("url", url)]));
    }
    ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
        EmptyState::new(icons::ROBOT, &headline, &line).show(ui);
    });
}

omuzaki_ui_api::register_panel! {
    id: "agent", title: "Agent", icon: icons::ROBOT, region: omuzaki_ui_api::registry::Region::Right,
    min_size: [288.0, 160.0], order: 20,
    build: |_| Box::new(AgentPanel::default()),
}

/// The words for a count: `one` for exactly one, else `many` (English plurals until the catalog selects them itself).
fn one_or_many(count: u32, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 { one } else { many }
}
