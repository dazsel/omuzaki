//! `ApprovalCard` (`design-system.md` section 7.9): the unit of human-in-the-loop.
//!
//! It shows who is asking, what for, the exact scope, the cost and whether it can be undone, then offers Approve,
//! Approve for this session and Reject… (which takes a comment that goes back to the agent as the reason). The
//! variants are a *plan* (steps as a checklist), an *elicitation* (a question with options) and a *dangerous* request
//! (`broadcast`, `publish`, `spend`, `native`): a heavier `danger` border and a **two-step confirm** whose second
//! step must be a deliberate press after a short dwell ([`confirm`](crate::ds::confirm)): a double-click or a held
//! Enter cannot arm and confirm in one gesture. There are **no timers, ever**: an approval never accepts or rejects
//! itself, and an armed request waits as long as it takes.
//!
//! Keyboard: the card is one focus group. Enter approves, Ctrl+Enter approves for the session, Backspace opens Reject,
//! Esc moves focus out without deciding.

use std::borrow::Cow;

use egui::{Align, Frame, Id, Key, Label, Layout, Margin, Modifiers, RichText, Sense, Stroke, Ui, vec2};

use super::chip::{ActorChip, ActorKind, CostChip, ScopeBadge, is_risky_scope};
use super::text_field::TextField;
use super::{Button, ButtonKind};
use crate::ds::a11y;
use crate::ds::confirm::Arm;
use crate::ds::icons;
use crate::ds::paint;
use crate::ds::strings::Msg;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{Status, radius, space};

/// One step of a plan.
#[derive(Clone, Copy, Debug)]
pub struct PlanStep<'a> {
    /// What the step does.
    pub label: &'a str,
    /// Inside the granted scope: approving the plan runs it. Steps outside ask again.
    pub in_scope: bool,
}

/// What kind of request this is.
#[derive(Clone, Copy, Debug, Default)]
pub enum Variant<'a> {
    /// An op that needs a decision.
    #[default]
    Request,
    /// A multi-step plan; approving runs the in-scope steps.
    Plan(&'a [PlanStep<'a>]),
    /// A question from the agent with options (MCP elicitation): Accept, Decline or Cancel.
    Question(&'a [&'a str]),
    /// Reaches outside the project: `confirm` names the second click ("Confirm: go live as @name").
    Dangerous {
        /// The label of the armed second-click button.
        confirm: &'a str,
    },
}

/// What the person decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Run it, once.
    Approve,
    /// Run it, and similar requests this session.
    ApproveForSession,
    /// Run every batched similar request.
    ApproveAll,
    /// Don't run it. With a comment, the agent keeps working and reads it as the reason; without, the turn stops.
    Reject {
        /// Why, for the agent.
        comment: Option<String>,
    },
    /// The chosen option of a question.
    Accept(usize),
    /// Decline to answer a question (the agent continues).
    Decline,
    /// Cancel the question (the agent stops asking).
    Cancel,
}

#[derive(Clone, Default)]
struct Phase {
    /// The dangerous Approve was pressed once and waits for a deliberate second press.
    arm: Option<Arm>,
    /// The Reject box is open.
    rejecting: bool,
    /// The comment typed so far.
    comment: String,
    /// The Changes section is open.
    changes_open: bool,
    /// The selected option of a question.
    choice: usize,
}

/// A request for a decision.
#[must_use = "an approval card does nothing until it is shown"]
pub struct ApprovalCard<'a> {
    id: Id,
    agent: &'a str,
    what: &'a str,
    scope: &'a str,
    scope_meaning: Option<&'a str>,
    cost: &'a [&'a str],
    reversible: Option<bool>,
    changes: Option<(&'a str, &'a [&'a str])>,
    similar: usize,
    variant: Variant<'a>,
}

impl<'a> ApprovalCard<'a> {
    /// A card keyed by `id` (the request's id): `agent` needs a decision about `what` ("Render 24 contact sheets…").
    pub fn new(id: impl std::hash::Hash + std::fmt::Debug, agent: &'a str, what: &'a str) -> Self {
        a11y::require_label(agent);
        a11y::require_label(what);
        Self {
            id: Id::new(id),
            agent,
            what,
            scope: "read",
            scope_meaning: None,
            cost: &[],
            reversible: None,
            changes: None,
            similar: 0,
            variant: Variant::Request,
        }
    }

    /// The scope the request needs (`render`, `edit:character`, `broadcast`…).
    pub const fn scope(mut self, scope: &'a str) -> Self {
        self.scope = scope;
        self
    }

    /// What the scope means in a sentence ("Allows rendering previews. Doesn't change the project.").
    pub const fn scope_meaning(mut self, meaning: &'a str) -> Self {
        self.scope_meaning = Some(meaning);
        self
    }

    /// The cost parts: time, memory, money.
    pub const fn cost(mut self, parts: &'a [&'a str]) -> Self {
        self.cost = parts;
        self
    }

    /// Whether the effect can be undone.
    pub const fn reversible(mut self, reversible: bool) -> Self {
        self.reversible = Some(reversible);
        self
    }

    /// A one-line patch summary that expands to its lines.
    pub const fn changes(mut self, summary: &'a str, lines: &'a [&'a str]) -> Self {
        self.changes = Some((summary, lines));
        self
    }

    /// How many identical requests are batched with this one.
    pub const fn similar(mut self, count: usize) -> Self {
        self.similar = count;
        self
    }

    /// The kind of request.
    pub const fn variant(mut self, variant: Variant<'a>) -> Self {
        self.variant = variant;
        self
    }

    fn dangerous(&self) -> bool {
        matches!(self.variant, Variant::Dangerous { .. }) || is_risky_scope(self.scope)
    }

    /// Shows the card and returns the decision, if one was made this frame.
    pub fn show(self, ui: &mut Ui) -> Option<Decision> {
        let ds = ds(ui.ctx());
        let t = &ds.theme;
        let ctx = ui.ctx().clone();
        let mut phase: Phase = ctx.data(|d| d.get_temp(self.id)).unwrap_or_default();
        let dangerous = self.dangerous();
        let mut decision = None;
        let mut focus_within = false;

        // Card keys act while focus was inside the card last frame and not in the comment box. They are consumed
        // before the buttons are drawn, or a focused Approve would take Ctrl+Enter as its own Enter.
        let was_within = ctx.data(|d| d.get_temp::<bool>(self.id.with("focus"))).unwrap_or(false);
        if was_within && !phase.rejecting {
            let key = ctx.input_mut(|i| {
                if i.consume_key(Modifiers::CTRL, Key::Enter) {
                    Some(Key::Enter)
                } else if i.consume_key(Modifiers::NONE, Key::Backspace) {
                    Some(Key::Backspace)
                } else {
                    None
                }
            });
            match key {
                Some(Key::Enter) if !dangerous && matches!(self.variant, Variant::Request | Variant::Plan(_)) => {
                    decision = Some(Decision::ApproveForSession);
                }
                Some(Key::Backspace) => phase.rejecting = true,
                _ => {}
            }
        }

        let border = if dangerous {
            Stroke::new(2.0, t.danger)
        } else {
            Stroke::new(1.0, if t.control_stroke > 1.0 { t.border_control } else { t.line })
        };
        let response = Frame::new()
            .fill(t.bg2)
            .stroke(border)
            .corner_radius(radius::CARD)
            .inner_margin(Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing = vec2(space::S8, space::S8);
                self.header(ui, &ds, dangerous);
                ui.label(RichText::new(self.what).font(ds.text.strong.clone()).color(t.text));
                self.risk_line(ui, &ds);
                self.cost_row(ui, &ds);
                self.variant_body(ui, &ds, &mut phase);
                if let Some((summary, lines)) = self.changes {
                    Self::changes_section(ui, &ds, &mut phase, summary, lines);
                }
                let chosen = if phase.rejecting {
                    self.reject_box(ui, &ds, &mut phase, &mut focus_within)
                } else {
                    self.buttons(ui, &ds, &mut phase, &mut focus_within, dangerous)
                };
                decision = decision.take().or(chosen);
            })
            .response;

        // Esc moves focus out of the card without deciding.
        if focus_within && !phase.rejecting && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            ctx.memory_mut(|m| {
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
            focus_within = false;
        }
        ctx.data_mut(|d| d.insert_temp(self.id.with("focus"), focus_within));

        if decision.is_some() {
            phase = Phase::default();
        }
        ctx.data_mut(|d| d.insert_temp(self.id, phase));

        let group = ui.interact(response.rect, self.id.with("card"), Sense::hover());
        let label = ds.text_with(Msg::ApprovalGroup, &[("agent", self.agent), ("what", self.what)]);
        let risk = self.risk_meaning();
        a11y::describe(
            &group,
            || egui::WidgetInfo::labeled(egui::WidgetType::Other, true, label.as_str()),
            |node| {
                node.set_role(egui::accesskit::Role::Group);
                node.set_live(egui::accesskit::Live::Polite);
                // The explanation is part of the card's own accessible text, not only of its visible line.
                if let Some(meaning) = risk {
                    node.set_description(meaning);
                }
            },
        );
        decision
    }

    fn header(&self, ui: &mut Ui, ds: &Ds, dangerous: bool) {
        let t = &ds.theme;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::S6;
            if dangerous {
                let (rect, _) = ui.allocate_exact_size(vec2(16.0, 18.0), Sense::hover());
                // The danger status icon (not the warning triangle): the card's danger border and this icon are a pair, so the
                // colour is never alone (design-system.md section 10).
                paint::icon(ui.painter(), ds, rect.center(), icons::status(Status::Danger), 16.0, t.danger, true);
            }
            ui.add(ActorChip::new(self.agent, ActorKind::Agent));
            ui.label(RichText::new(ds.text(Msg::ApprovalNeedsYou)).font(ds.text.small.clone()).color(t.text2));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let badge = ui.add(ScopeBadge::new(self.scope));
                if let Some(meaning) = self.scope_meaning {
                    badge.on_hover_text(meaning);
                }
            });
        });
    }

    /// The scope's explanation, when the scope is risky (it reaches outside the project) and the caller gave one.
    fn risk_meaning(&self) -> Option<&'a str> {
        self.scope_meaning.filter(|_| is_risky_scope(self.scope))
    }

    /// The explanation of a risky scope, on a line under the action title in the warning colour, the way "Can't be
    /// undone" sits under a broadcast. It is visible without hovering, so keyboard, touch and screen-reader users see
    /// it too; the badge's tooltip repeats it.
    fn risk_line(&self, ui: &mut Ui, ds: &Ds) {
        let Some(meaning) = self.risk_meaning() else { return };
        let t = &ds.theme;
        // The icon has its own column, so a wrapped second line hangs under the text, not under the icon.
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = space::S6;
            let (rect, _) = ui.allocate_exact_size(vec2(12.0, ds.text.small.size * 1.3), Sense::hover());
            paint::icon(ui.painter(), ds, rect.center(), icons::WARNING, 12.0, t.warn, true);
            ui.add(Label::new(RichText::new(meaning).font(ds.text.small.clone()).color(t.warn)).wrap());
        });
    }

    fn cost_row(&self, ui: &mut Ui, ds: &Ds) {
        let t = &ds.theme;
        if self.cost.is_empty() && self.reversible.is_none() {
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = space::S8;
            if !self.cost.is_empty() {
                ui.add(CostChip::new(self.cost));
            }
            match self.reversible {
                Some(true) => {
                    ui.label(
                        RichText::new(ds.text(Msg::ApprovalReversible))
                            .font(egui::FontId::monospace(11.5))
                            .color(t.text2),
                    );
                }
                Some(false) => {
                    ui.label(RichText::new(icons::WARNING).font(ds.text.icon(12.0, true)).color(t.warn));
                    ui.label(
                        RichText::new(ds.text(Msg::ApprovalIrreversible)).font(ds.text.small.clone()).color(t.warn),
                    );
                }
                None => {}
            }
        });
    }

    fn variant_body(&self, ui: &mut Ui, ds: &Ds, phase: &mut Phase) {
        let t = &ds.theme;
        match self.variant {
            Variant::Plan(steps) => {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = space::S4;
                    for step in steps {
                        ui.horizontal(|ui| {
                            let (rect, _) = ui.allocate_exact_size(vec2(16.0, 18.0), Sense::hover());
                            let (glyph, colour) =
                                if step.in_scope { (icons::CHECK_CIRCLE, t.ok) } else { (icons::WARNING, t.warn) };
                            paint::icon(ui.painter(), ds, rect.center(), glyph, 14.0, colour, true);
                            ui.label(RichText::new(step.label).font(ds.text.body.clone()).color(t.text));
                            if !step.in_scope {
                                ui.label(
                                    RichText::new(ds.text(Msg::ApprovalAsksAgain))
                                        .font(ds.text.small.clone())
                                        .color(t.text3),
                                );
                            }
                        });
                    }
                });
            }
            Variant::Question(options) => {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = space::S2;
                    for (i, option) in options.iter().enumerate() {
                        if radio_row(ui, ds, option, phase.choice == i) {
                            phase.choice = i;
                        }
                    }
                });
            }
            Variant::Request | Variant::Dangerous { .. } => {}
        }
    }

    fn changes_section(ui: &mut Ui, ds: &Ds, phase: &mut Phase, summary: &str, lines: &[&str]) {
        let t = &ds.theme;
        let ctx = ui.ctx().clone();
        let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
        if response.clicked() {
            phase.changes_open = !phase.changes_open;
        }
        let painter = ui.painter();
        paint::icon(
            painter,
            ds,
            egui::pos2(rect.left() + 6.0, rect.center().y),
            if phase.changes_open { icons::CARET_DOWN } else { icons::CARET_RIGHT },
            12.0,
            t.text2,
            false,
        );
        let hot = paint::hot(&response);
        let word = paint::text(
            painter,
            egui::pos2(rect.left() + 18.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            &ds.text(Msg::ApprovalChanges),
            &ds.text.small,
            if hot { t.text } else { t.text2 },
        );
        paint::text(
            painter,
            egui::pos2(word.right() + space::S4, rect.center().y),
            egui::Align2::LEFT_CENTER,
            summary,
            &ds.text.small,
            t.text,
        );
        paint::focus_ring(painter, t, &response, rect, radius::CONTROL, paint::RingPlacement::Inside);
        a11y::describe(
            &response,
            || egui::WidgetInfo::labeled(egui::WidgetType::CollapsingHeader, true, ds.text(Msg::ApprovalChangesName)),
            |node| {
                node.set_expanded(phase.changes_open);
                node.set_value(summary);
            },
        );
        let _ = ctx;
        if phase.changes_open {
            Frame::new().fill(t.bg3).corner_radius(radius::CONTROL).inner_margin(Margin::same(8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                for line in lines {
                    ui.label(RichText::new(*line).font(ds.text.mono.clone()).color(t.text2));
                }
            });
        }
    }

    fn buttons(
        &self,
        ui: &mut Ui,
        ds: &Ds,
        phase: &mut Phase,
        focus_within: &mut bool,
        dangerous: bool,
    ) -> Option<Decision> {
        let mut decision = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = space::S8;
            decision = if let Variant::Question(options) = self.variant {
                self.question_buttons(ui, ds, phase, options.len(), focus_within)
            } else {
                self.request_buttons(ui, ds, phase, focus_within, dangerous)
            };
        });
        if self.similar > 1 && !matches!(self.variant, Variant::Question(_)) {
            // Identical pending requests collapse into one card: one Approve all instead of N prompts.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::S8;
                let count = self.similar.to_string();
                ui.label(
                    RichText::new(ds.text_with(Msg::ApprovalSimilar, &[("count", &count)]))
                        .font(ds.text.small.clone())
                        .color(ds.theme.text3),
                );
                let (label, name) = (
                    ds.text_with(Msg::ApprovalAll, &[("count", &count)]),
                    ds.text_with(Msg::ApprovalAllName, &[("count", &count)]),
                );
                let all = ui.add(Button::new(&label).ghost().accessible_label(&name));
                *focus_within |= all.has_focus();
                if all.clicked() {
                    decision = Some(Decision::ApproveAll);
                }
            });
        }
        decision
    }

    /// Accept, Decline and Cancel for a question (MCP elicitation).
    fn question_buttons(
        &self,
        ui: &mut Ui,
        ds: &Ds,
        phase: &Phase,
        options: usize,
        focus_within: &mut bool,
    ) -> Option<Decision> {
        let mut decision = None;
        let name = ds.text_with(Msg::ApprovalAcceptName, &[("agent", self.agent)]);
        let (accept_label, enter) = (ds.text(Msg::ApprovalAccept), ds.text(Msg::KeyEnter));
        let accept = ui.add(Button::new(&accept_label).primary().shortcut(&enter).accessible_label(&name));
        *focus_within |= accept.has_focus();
        if accept.clicked() {
            decision = Some(Decision::Accept(phase.choice.min(options.saturating_sub(1))));
        }
        let decline = ui.add(Button::new(&ds.text(Msg::ApprovalDecline)));
        *focus_within |= decline.has_focus();
        if decline.clicked() {
            decision = Some(Decision::Decline);
        }
        let cancel = ui.add(Button::new(&ds.text(Msg::ApprovalCancel)).ghost());
        *focus_within |= cancel.has_focus();
        if cancel.clicked() {
            decision = Some(Decision::Cancel);
        }
        decision
    }

    /// Approve, Approve for this session and Reject… for a request or plan; a dangerous request has the two-step
    /// confirm and no session approval.
    fn request_buttons(
        &self,
        ui: &mut Ui,
        ds: &Ds,
        phase: &mut Phase,
        focus_within: &mut bool,
        dangerous: bool,
    ) -> Option<Decision> {
        let mut decision = None;
        let agent = self.agent;
        let armed = phase.arm.is_some();
        let label = match (dangerous, armed, self.variant) {
            (true, true, Variant::Dangerous { confirm }) => Cow::Borrowed(confirm),
            (true, true, _) => ds.text(Msg::ApprovalConfirm),
            (true, false, _) => ds.text(Msg::ApprovalApproveAsk),
            (false, _, Variant::Plan(_)) => ds.text(Msg::ApprovalApprovePlan),
            (false, _, _) => ds.text(Msg::ApprovalApprove),
        };
        let action =
            |name: &str| ds.text_with(Msg::ApprovalAction, &[("action", name), ("what", self.what), ("agent", agent)]);
        let a11y_label = action(&label);
        let kind = if dangerous && armed { ButtonKind::Danger } else { ButtonKind::Primary };
        let enter = ds.text(Msg::KeyEnter);
        let mut approve = Button::new(&label).kind(kind).accessible_label(&a11y_label);
        if !dangerous {
            approve = approve.shortcut(&enter);
        }
        let approve = ui.add(approve);
        *focus_within |= approve.has_focus();
        match (dangerous, phase.arm.as_mut()) {
            // First step of the two-step confirm. No timeout: it waits until you decide.
            (true, None) => {
                if crate::ds::input::is_press(&approve) {
                    phase.arm = Some(Arm::start(ui.ctx()));
                }
            }
            // Second step: only a deliberate press, after the dwell, counts.
            (true, Some(arm)) => {
                if arm.confirmed(&approve) {
                    decision = Some(Decision::Approve);
                }
            }
            (false, _) => {
                if approve.clicked() {
                    decision = Some(Decision::Approve);
                }
            }
        }
        if !dangerous {
            let session_label = ds.text(Msg::ApprovalSession);
            let session_name = action(&session_label);
            let session_keys = ds.text(Msg::KeysApproveSession);
            let session =
                ui.add(Button::new(&session_label).shortcut_hint(&session_keys).accessible_label(&session_name));
            *focus_within |= session.has_focus();
            if session.clicked() {
                decision = Some(Decision::ApproveForSession);
            }
        }
        let (reject_label, reject_keys) = (ds.text(Msg::ApprovalReject), ds.text(Msg::KeyBackspace));
        let reject_name = action(&ds.text(Msg::ApprovalRejectName));
        let reject = ui.add(Button::new(&reject_label).shortcut_hint(&reject_keys).accessible_label(&reject_name));
        *focus_within |= reject.has_focus();
        if reject.clicked() {
            phase.rejecting = true;
        }
        if phase.arm.is_some() {
            let cancel = ui.add(Button::new(&ds.text(Msg::ApprovalCancel)).ghost());
            *focus_within |= cancel.has_focus();
            if cancel.clicked() {
                phase.arm = None;
            }
        }
        decision
    }

    /// The Reject comment box: the comment goes back to the agent as the reason and it keeps working; a bare Reject
    /// stops the turn.
    fn reject_box(&self, ui: &mut Ui, ds: &Ds, phase: &mut Phase, focus_within: &mut bool) -> Option<Decision> {
        let mut decision = None;
        let hint = ds.text_with(Msg::ApprovalReasonHint, &[("agent", self.agent)]);
        let field = ui.add(TextField::new(&ds.text(Msg::ApprovalReasonName), &mut phase.comment).hint(&hint));
        *focus_within |= field.has_focus();
        if !field.has_focus() && !field.lost_focus() && phase.comment.is_empty() {
            field.request_focus();
        }
        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::S8;
            let with_comment = !phase.comment.trim().is_empty();
            let send_label = ds.text_with(Msg::ApprovalRejectTell, &[("agent", self.agent)]);
            let first = ds.text(Msg::ApprovalReasonFirst);
            let send = ui.add(Button::new(&send_label).danger().disabled_if(!with_comment, &first));
            if send.clicked() || (enter && with_comment) {
                decision = Some(Decision::Reject { comment: Some(phase.comment.trim().to_owned()) });
            }
            if ui.add(Button::new(&ds.text(Msg::ApprovalRejectStop))).clicked() {
                decision = Some(Decision::Reject { comment: None });
            }
            if ui.add(Button::new(&ds.text(Msg::ApprovalBack)).ghost()).clicked() {
                phase.rejecting = false;
                phase.comment.clear();
            }
        });
        decision
    }
}

/// One option of a question: a radio dot and its label on a full-width row. Returns true when chosen.
fn radio_row(ui: &mut Ui, ds: &Ds, label: &str, selected: bool) -> bool {
    let t = &ds.theme;
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ds.metrics.control_row), Sense::click());
    let hover = paint::fade(ui.ctx(), ds, response.id, paint::hot(&response));
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, radius::CONTROL, t.bg4);
    } else if hover > 0.0 {
        painter.rect_filled(rect, radius::CONTROL, t.bg3.gamma_multiply(hover));
    }
    let dot = egui::pos2(rect.left() + 16.0, rect.center().y);
    painter.circle_stroke(dot, 7.0, Stroke::new(1.5, if selected { t.accent } else { t.border_control }));
    if selected {
        painter.circle_filled(dot, 3.5, t.accent);
    }
    paint::text(
        painter,
        egui::pos2(rect.left() + 32.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        if selected { &ds.text.strong } else { &ds.text.body },
        t.text,
    );
    paint::focus_ring(painter, t, &response, rect, radius::CONTROL, paint::RingPlacement::Inside);
    a11y::describe(
        &response,
        || egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, selected, label),
        |_| {},
    );
    response.clicked()
}
