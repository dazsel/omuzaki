//! Approval cards (`shell.md` section 10, `agent-sessions.md` section 7): each pending approval as an
//! [`ApprovalCard`], in the Agent panel and as a stack at the top right of the window.
//!
//! - **Batching.** Approvals with the same batch key (session, op, need) collapse into one card ("6 similar
//!   requests: Approve all 6"); a decision on the card is given to each of them, exactly once.
//! - **Decisions per need** (design 7.1): a missing scope offers Approve, for the session and, for a session with a
//!   credential, for the project; a person's confirmation offers Approve with the op's reason in full; a budget offers
//!   Approve once and Raise the budget; a pairing offers Pair. Every card has Reject with a comment. `broadcast`,
//!   `publish`, `spend` and `native` take the card's two-step confirm.
//! - **Nothing auto-resolves.** No timer here decides anything; an approval waits until the person answers or its
//!   session ends.

use egui::{Context, Id, Order, Rect, ScrollArea, Ui};
use omuzaki_ops_api::{
    Actor, ApprovalId, Budget, Decision, Need, OpError, PendingApproval, Scope, SessionInfo, Sessions, Spend,
};

use super::super::text::{self, say};
use super::feed::{AgentFeed, has_credential};
use super::review::{ReviewDesk, review_batch};
use crate::ds::theme::{Ds, ds};
use crate::ds::widgets::Button;
use crate::ds::widgets::approval_card::{ApprovalCard, Decision as CardDecision};

/// Width of the stack at the top right of the window.
pub const STACK_WIDTH: f32 = 380.0;
/// How many cards the stack shows before it points to the Agent panel.
const STACK_CARDS: usize = 3;

/// Approvals that collapse into one card.
#[derive(Clone, Debug)]
pub struct Batch<'a> {
    /// The oldest of them, which the card describes.
    pub first: &'a PendingApproval,
    /// Every approval in the batch, oldest first.
    pub ids: Vec<ApprovalId>,
}

/// The pending approvals grouped by batch key, in the order their oldest arrived.
pub fn batches(approvals: &[PendingApproval]) -> Vec<Batch<'_>> {
    let mut out: Vec<Batch<'_>> = Vec::new();
    for approval in approvals {
        match out.iter_mut().find(|b| b.first.batch == approval.batch) {
            Some(batch) => batch.ids.push(approval.id),
            None => out.push(Batch { first: approval, ids: vec![approval.id] }),
        }
    }
    out
}

/// Gives `decision` to every approval in `ids`, each exactly once, as `person`.
///
/// # Errors
/// The first refusal, after which the rest are left waiting.
pub fn resolve_all(
    sessions: &dyn Sessions,
    ids: &[ApprovalId],
    decision: &Decision,
    person: &Actor,
) -> Result<(), OpError> {
    for id in ids {
        sessions.resolve(*id, decision.clone(), person)?;
    }
    Ok(())
}

/// What the person did on a card.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    /// A decision, for these approvals.
    Decide(Vec<ApprovalId>, Decision),
    /// Export the request for a reviewer elsewhere.
    Export(ApprovalId),
}

/// Draws the card for `batch` and returns what the person did. `salt` keeps the stack's cards apart from the panel's.
pub fn card(
    ui: &mut Ui,
    ds: &Ds,
    batch: &Batch<'_>,
    session: Option<&SessionInfo>,
    desk: Option<&dyn ReviewDesk>,
    salt: &str,
) -> Option<Answer> {
    let approval = batch.first;
    let agent = agent_name(approval, session);
    if let Need::Review { .. } = approval.need {
        return review_batch(ui, approval, &agent, desk, salt);
    }
    let what = what(ds, approval);
    let scope_name = match &approval.need {
        Need::Scope { scope } => Some(scope.name()),
        _ => None,
    };
    let meaning = scope_name.and_then(|s| Scope::ALL.into_iter().find(|sc| sc.name() == s)).map(scope_meaning);
    let meaning = meaning.map(|m| say(ds, m, &[]).into_owned());
    let preview = approval.request.as_ref().map(|r| &r.preview);
    let lines: Vec<String> = preview
        .map(|p| p.patch.changes.iter().map(|c| format!("{}: {}", c.path, short(c.after.as_ref()))).collect())
        .unwrap_or_default();
    let line_refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let summary = text::changes(ds, lines.len());
    let cost = preview.and_then(|p| p.cost.as_ref()).map(cost_parts).unwrap_or_default();
    let cost_refs: Vec<&str> = cost.iter().map(String::as_str).collect();
    let credential = session.is_some_and(|s| has_credential(&s.transport));

    let mut card = ApprovalCard::new(("agent_approval", salt, approval.id), &agent, &what).similar(batch.ids.len());
    card = card.scope(scope_name.unwrap_or(""));
    if let Some(meaning) = meaning.as_deref() {
        card = card.scope_meaning(meaning);
    }
    if !line_refs.is_empty() {
        card = card.changes(&summary, &line_refs);
    }
    if !cost_refs.is_empty() {
        card = card.cost(&cost_refs);
    }
    let pair = say(ds, text::APPROVAL_PAIR_BUTTON, &[]);
    let once = say(ds, text::APPROVAL_ONCE, &[]);
    let raise_to = session.map(|s| raised(&s.budget, &approval.need));
    let raise_label = raise_to.map(|b| say(ds, text::APPROVAL_RAISE, &[("budget", &dollars(b.usd_micros))]));
    card = match &approval.need {
        Need::Scope { .. } => card.offers(true, credential),
        Need::Person { reason } => card.offers(false, false).reason(reason),
        Need::Budget { .. } => {
            let card = card.offers(false, false).approve_label(&once);
            match raise_label.as_deref() {
                Some(label) => card.raise(label),
                None => card,
            }
        }
        Need::Pairing { .. } => card.offers(false, false).approve_label(&pair),
        Need::Review { .. } => card,
    };
    let card_decision = card.show(ui)?;
    // "Approve all" answers the whole batch; every other decision answers the card's own request, and the rest of the
    // batch stays on the card.
    let ids = if card_decision == CardDecision::ApproveAll { batch.ids.clone() } else { vec![approval.id] };
    let decision = match card_decision {
        CardDecision::Approve | CardDecision::ApproveAll | CardDecision::Accept(_) => Decision::Approve,
        CardDecision::ApproveForSession => Decision::ApproveForSession,
        CardDecision::ApproveForProject => Decision::ApproveForProject,
        CardDecision::RaiseBudget => Decision::RaiseBudget { budget: raise_to? },
        CardDecision::Export => return Some(Answer::Export(approval.id)),
        CardDecision::Reject { comment } => Decision::Reject { comment },
        CardDecision::Decline | CardDecision::Cancel => Decision::Reject { comment: None },
    };
    Some(Answer::Decide(ids, decision))
}

/// The approvals as a stack at the top right of the window, under `top` (the top bar's bottom edge), when the Agent
/// panel is not on screen. Returns what the person did, and whether they asked for the Agent panel.
pub fn stack(ctx: &Context, top: f32, feed: &AgentFeed, desk: Option<&dyn ReviewDesk>) -> (Option<Answer>, bool) {
    let all = batches(feed.approvals());
    if all.is_empty() {
        return (None, false);
    }
    let ds = ds(ctx);
    let screen = ctx.content_rect();
    let width = STACK_WIDTH.min(screen.width() - 32.0);
    let mut answer = None;
    let mut open_panel = false;
    egui::Area::new(Id::new("agent_approval_stack"))
        .order(Order::Foreground)
        .pivot(egui::Align2::RIGHT_TOP)
        .fixed_pos(egui::pos2(screen.right() - 12.0, top + 12.0))
        .constrain(true)
        .show(ctx, |ui| {
            // A backing behind the cards, so the panel under the stack never shows between them.
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(width);
                // The area's room is last frame's size, so without a floor the list would never grow past the
                // height it first had: it takes up to 70 % of the window, then scrolls.
                let tall = screen.height() * 0.7;
                ScrollArea::vertical().max_height(tall).min_scrolled_height(tall).show(ui, |ui| {
                    for batch in all.iter().take(STACK_CARDS) {
                        let session = feed.session(batch.first.session);
                        if let Some(a) = card(ui, &ds, batch, session, desk, "stack") {
                            answer = Some(a);
                        }
                        ui.add_space(6.0);
                    }
                });
                if all.len() > STACK_CARDS {
                    let more = (all.len() - STACK_CARDS).to_string();
                    let label = say(&ds, text::APPROVAL_MORE, &[("count", &more)]);
                    open_panel = ui.add(Button::new(&label).ghost()).clicked();
                }
            });
        });
    (answer, open_panel)
}

/// The rectangle the stack takes, for tests and for keeping it off other overlays.
pub fn stack_rect(ctx: &Context) -> Option<Rect> {
    ctx.memory(|m| m.area_rect(Id::new("agent_approval_stack")))
}

/// The agent's name as the card shows it: the session's client name, else the actor's id.
fn agent_name(approval: &PendingApproval, session: Option<&SessionInfo>) -> String {
    match (session, &approval.actor) {
        (Some(s), _) => s.client.clone(),
        (None, Actor::Agent { id, .. } | Actor::Plugin { id }) => id.clone(),
        (None, Actor::Person { name }) => name.clone(),
        (None, _) => String::new(),
    }
}

/// What the request asks, as the card's title.
fn what(ds: &Ds, approval: &PendingApproval) -> String {
    match (&approval.request, &approval.need) {
        (Some(request), _) => say(ds, text::APPROVAL_RUN, &[("op", &request.op)]),
        (None, Need::Pairing { client }) => say(ds, text::APPROVAL_PAIR, &[("client", client)]),
        (None, Need::Budget { .. }) => say(ds, text::APPROVAL_BUDGET, &[]),
        (None, _) => say(ds, text::APPROVAL_OTHER, &[]),
    }
    .into_owned()
}

/// The budget a "raise" sets: the current one plus what the request needs beyond what is left, in each unit.
fn raised(budget: &Budget, need: &Need) -> Budget {
    let Need::Budget { needed, left } = need else { return *budget };
    let more = |n: u64, l: u64| n.saturating_sub(l);
    let Spend { usd_micros, tokens, seconds } = *needed;
    Budget {
        usd_micros: budget.usd_micros.saturating_add(more(usd_micros, left.usd_micros)),
        tokens: budget.tokens.map(|t| t.saturating_add(more(tokens, left.tokens))),
        seconds: budget.seconds.map(|s| s.saturating_add(more(seconds, left.seconds))),
    }
}

/// `$2.40` from micro-dollars.
pub fn dollars(micros: u64) -> String {
    let cents = micros.div_ceil(10_000);
    format!("${}.{:02}", cents / 100, cents % 100)
}

/// A value, cut to a short line for the card's change list.
fn short(value: Option<&serde_json::Value>) -> String {
    let full = value.map_or_else(|| "\u{2014}".to_owned(), ToString::to_string);
    if full.chars().count() > 48 { format!("{}\u{2026}", full.chars().take(47).collect::<String>()) } else { full }
}

/// The parts of a cost estimate: time, memory, money.
fn cost_parts(cost: &omuzaki_ops_api::CostHint) -> Vec<String> {
    let mut parts = Vec::new();
    if let Some(s) = cost.seconds {
        parts.push(format!("~{s:.0} s"));
    }
    if let Some(mib) = cost.gpu_mib {
        parts.push(format!("{:.1} GB", f64::from(mib) / 1024.0));
    }
    if let Some(usd) = cost.usd {
        parts.push(format!("${usd:.2}"));
    }
    parts
}

/// What a scope allows, in a sentence (the card's risk line and the grants popover's explanations).
pub const fn scope_meaning(scope: Scope) -> &'static str {
    match scope {
        Scope::Read => text::SCOPE_READ,
        Scope::EditCharacter => text::SCOPE_EDIT_CHARACTER,
        Scope::EditScene => text::SCOPE_EDIT_SCENE,
        Scope::EditLibrary => text::SCOPE_EDIT_LIBRARY,
        Scope::EditSettings => text::SCOPE_EDIT_SETTINGS,
        Scope::Render => text::SCOPE_RENDER,
        Scope::Perform => text::SCOPE_PERFORM,
        Scope::Broadcast => text::SCOPE_BROADCAST,
        Scope::Publish => text::SCOPE_PUBLISH,
        Scope::Spend => text::SCOPE_SPEND,
        Scope::Net => text::SCOPE_NET,
        Scope::Fs => text::SCOPE_FS,
        Scope::Native => text::SCOPE_NATIVE,
        Scope::Input => text::SCOPE_INPUT,
        Scope::Process => text::SCOPE_PROCESS,
        Scope::Script => text::SCOPE_SCRIPT,
        Scope::Review => text::SCOPE_REVIEW,
    }
}
