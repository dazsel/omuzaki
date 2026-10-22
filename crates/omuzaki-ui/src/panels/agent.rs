//! The Agent panel (`shell.md` section 10): the compact view of agent work. One row per agent session (who, its state,
//! how many steps it made), with Pause or Resume, Stop and Undo its work; and every approval waiting for the person as
//! an approval card, decided here.
//!
//! It reads the executor's session registry ([`Agents`], a view model the app shares), so an agent connected over
//! MCP, the built-in assistant and a plugin look the same. Every control acts as the person through that registry,
//! which checks the person is the session's principal; nothing here changes the document directly.

use std::sync::Arc;

use egui::{RichText, ScrollArea, Ui};
use omuzaki_ops_api::{Actor, ApprovalRequest, Decision, Need, PendingApproval, SessionInfo, SessionState, Sessions};
use omuzaki_ui_api::panel::{Panel, PanelCtx};

use super::text::{self, say};
use crate::ds::icons;
use crate::ds::theme::{Ds, ds};
use crate::ds::widgets::approval_card::{ApprovalCard, Decision as CardDecision};
use crate::ds::widgets::{Button, EmptyState};
use crate::shell::history::HistoryLog;

/// The agent sessions of the app's executor, shared with panels as a view model.
#[derive(Clone)]
pub struct Agents(pub Arc<dyn Sessions>);

/// The Agent panel's id, as it registers.
pub const ID: &str = "agent";

/// The Agent panel.
#[derive(Debug, Default)]
pub struct AgentPanel {
    /// What the last control said when it was refused, shown until the next one.
    problem: Option<String>,
}

impl Panel for AgentPanel {
    fn ui(&mut self, ui: &mut Ui, ctx: &PanelCtx<'_>) {
        let ds = ds(ui.ctx());
        let Some(Agents(sessions)) = ctx.view::<Agents>() else {
            return connect(ui, &ds);
        };
        let all = sessions.sessions();
        let live: Vec<&SessionInfo> = all.iter().filter(|s| !matches!(s.state, SessionState::Ended { .. })).collect();
        let approvals = sessions.approvals();
        if live.is_empty() && approvals.is_empty() {
            return connect(ui, &ds);
        }
        let person = ctx.actor();
        let steps = |actor: &Actor| {
            ctx.view::<HistoryLog>().map_or(0, |log| log.records.iter().filter(|r| r.actor == *actor).count())
        };
        ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
            for approval in &approvals {
                if let Some(decision) = approval_card(ui, &ds, approval) {
                    self.report(sessions.resolve(approval.id, decision, person).map(drop));
                }
            }
            for session in &live {
                self.session_row(ui, &ds, sessions.as_ref(), session, steps(&session.actor), person);
            }
            if let Some(problem) = &self.problem {
                ui.label(RichText::new(problem).color(ds.theme.danger));
            }
        });
    }
}

impl AgentPanel {
    fn report(&mut self, result: Result<(), omuzaki_ops_api::OpError>) {
        self.problem = result.err().map(|e| e.message);
    }

    fn session_row(
        &mut self,
        ui: &mut Ui,
        ds: &Ds,
        sessions: &dyn Sessions,
        session: &SessionInfo,
        steps: usize,
        person: &Actor,
    ) {
        let name = match &session.model {
            Some(model) => format!("{} ({model})", session.client),
            None => session.client.clone(),
        };
        let (state, paused) = match &session.state {
            SessionState::Active if session.open_tx.is_some() => (say(ds, text::AGENT_WORKING, &[]), false),
            SessionState::Active => (say(ds, text::AGENT_IDLE, &[]), false),
            SessionState::Paused { .. } => (say(ds, text::AGENT_PAUSED, &[]), true),
            SessionState::Ended { .. } => (say(ds, text::AGENT_ENDED, &[]), false),
        };
        let state = state.into_owned();
        let count = steps.to_string();
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(icons::ROBOT).font(ds.text.icon(16.0, false)).color(ds.theme.agent));
                ui.label(RichText::new(&name).font(ds.text.strong.clone()));
            });
            let line = say(ds, text::AGENT_STATE, &[("state", &state), ("steps", &count)]);
            ui.label(RichText::new(line).color(ds.theme.text2));
            ui.horizontal_wrapped(|ui| {
                let (pause, resume) = (say(ds, text::AGENT_PAUSE, &[]), say(ds, text::AGENT_RESUME, &[]));
                if paused {
                    if ui.add(Button::new(&resume).icon(icons::PLAY)).clicked() {
                        self.report(sessions.resume(session.id, person).map(drop));
                    }
                } else if ui.add(Button::new(&pause).icon(icons::PAUSE)).clicked() {
                    self.report(sessions.pause(session.id, person).map(drop));
                }
                if ui.add(Button::new(&say(ds, text::AGENT_STOP, &[])).icon(icons::STOP)).clicked() {
                    self.report(sessions.stop(session.id, person).map(drop));
                }
                let undo = say(ds, text::AGENT_UNDO_ALL, &[]);
                if steps > 0 && ui.add(Button::new(&undo).ghost().icon(icons::ARROW_COUNTER_CLOCKWISE)).clicked() {
                    self.report(sessions.undo_session(session.id, person).map(drop));
                }
            });
        });
        ui.add_space(4.0);
    }
}

/// The card for one approval; the person's decision, in the executor's terms, when they made one.
fn approval_card(ui: &mut Ui, ds: &Ds, approval: &PendingApproval) -> Option<Decision> {
    let agent = match &approval.actor {
        Actor::Agent { id, .. } | Actor::Plugin { id } => id.clone(),
        other => format!("{other:?}"),
    };
    let what = match (&approval.request, &approval.need) {
        (Some(ApprovalRequest { op, .. }), _) => say(ds, text::APPROVAL_RUN, &[("op", op)]),
        (None, Need::Pairing { client }) => say(ds, text::APPROVAL_PAIR, &[("client", client)]),
        (None, _) => say(ds, text::APPROVAL_OTHER, &[]),
    }
    .into_owned();
    let scope = match &approval.need {
        Need::Scope { scope } => Some(scope.to_string()),
        _ => None,
    };
    let mut card = ApprovalCard::new(("agent_approval", approval.id), &agent, &what);
    if let Some(scope) = scope.as_deref() {
        card = card.scope(scope);
    }
    let decision = card.show(ui)?;
    Some(match decision {
        CardDecision::Approve | CardDecision::ApproveAll | CardDecision::Accept(_) => Decision::Approve,
        CardDecision::ApproveForSession => Decision::ApproveForSession,
        CardDecision::Reject { comment } => Decision::Reject { comment },
        CardDecision::Decline | CardDecision::Cancel => Decision::Reject { comment: None },
    })
}

/// No agent is connected: how to connect one.
fn connect(ui: &mut Ui, ds: &Ds) {
    let (headline, line) = (say(ds, text::AGENT_NONE, &[]), say(ds, text::AGENT_NONE_HOW, &[]));
    ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
        EmptyState::new(icons::ROBOT, &headline, &line).show(ui);
    });
}

omuzaki_ui_api::register_panel! {
    id: "agent", title: "Agent", icon: icons::ROBOT, region: omuzaki_ui_api::registry::Region::Right,
    min_size: [288.0, 160.0], order: 20,
    build: |_| Box::new(AgentPanel::default()),
}
