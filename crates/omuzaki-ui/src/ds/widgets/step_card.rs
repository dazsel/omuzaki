//! `StepCard` (`design-system.md` 7.10): one card per transaction, in History, the Agent panel and the Theatre.
//!
//! ```text
//!  [before] → [after]                                    4 s
//!   Before     After                       ✋ Pending review
//!  Rounder face profile
//!  ● Designer  "make the face more oval"
//!  [✓ likeness +0.03] [⚠ spacing +0.02]
//!  [ Rewind ] [ Explain ] [ Adjust ]
//! ```
//!
//! The states are four, and each is a shape and a word as well as a colour (P9). **Applied** is the plain card.
//! **Pending review** has an accent border and "Pending review", and swaps the actions for Approve and Reject.
//! **Rejected** strikes the label through and offers Restore. **Failed gate** has a `warn` border, a triangle, "Gate
//! failed" and the evidence image beside the pictures. The pictures are lazy ([`Thumb`]): the card says which of them
//! are on screen and still missing ([`Wants`]).
//!
//! The card is painted and its buttons are placed by hand ([`Button::show_at`]), so a card costs no child `Ui` and
//! nothing allocates once the text is cached. Its buttons are ordinary Tab stops, left to right and top to bottom;
//! each is named for what it does to which step ("Rewind: Rounder face profile").

use egui::{
    Align2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Widget, WidgetInfo, WidgetType, accesskit, pos2, vec2,
};

use super::button::Button;
use super::chip::ActorKind;
use super::ids;
use super::thumbnail::{Thumb, paint_thumb};
use super::tooltip::{Tip, TipExt as _};
use crate::ds::a11y;
use crate::ds::contrast::{Rule, Table};
use crate::ds::icons;
use crate::ds::paint;
use crate::ds::strings::Msg;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{Status, Theme, radius, space};
use crate::register_contrast;

/// What a step card's salt is namespaced by (see [`ids`]).
const KIND: &str = "ds.step_card";

/// What a metric did to the result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Change {
    /// For the better: a check mark and the `ok` colour.
    Better,
    /// For the worse: a warning triangle and the `warn` colour.
    Worse,
    /// Neither, or not known: no mark.
    Neutral,
}

/// A metric delta as a chip: `likeness +0.03`.
#[derive(Clone, Copy, Debug)]
pub struct Metric<'a> {
    /// The metric's name ("likeness").
    pub name: &'a str,
    /// The signed change, formatted by the host ("+0.03").
    pub delta: &'a str,
    /// Whether that is an improvement. The sign alone cannot say: a falling error is a good thing.
    pub change: Change,
}

/// Where a step is in review.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StepState {
    /// Applied, nothing to decide.
    #[default]
    Applied,
    /// Waiting for a person's review.
    Pending,
    /// A person rejected it; it can be restored.
    Rejected,
    /// A gate failed on it; the evidence shows beside the pictures.
    FailedGate,
}

/// What a card's buttons ask for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StepAction {
    /// Restore the project to just before this step (or branch from it).
    Rewind,
    /// Ask the agent for its rationale in plain words.
    Explain,
    /// Reopen the op's form with its arguments.
    Adjust,
    /// Accept a pending step.
    Approve,
    /// Reject a pending step.
    Reject,
    /// Bring a rejected step back.
    Restore,
}

impl StepAction {
    const fn label(self) -> Msg {
        match self {
            Self::Rewind => Msg::StepRewind,
            Self::Explain => Msg::StepExplain,
            Self::Adjust => Msg::StepAdjust,
            Self::Approve => Msg::StepApprove,
            Self::Reject => Msg::StepReject,
            Self::Restore => Msg::StepRestore,
        }
    }

    const fn icon(self) -> &'static str {
        match self {
            Self::Rewind => icons::ARROW_U_UP_LEFT,
            Self::Explain => icons::CHAT_CIRCLE_TEXT,
            Self::Adjust => icons::SLIDERS_HORIZONTAL,
            Self::Approve => icons::CHECK,
            Self::Reject => icons::X,
            Self::Restore => icons::ARROW_COUNTER_CLOCKWISE,
        }
    }
}

/// The actions each state offers, in Tab order.
const fn actions(state: StepState) -> &'static [StepAction] {
    match state {
        StepState::Applied | StepState::FailedGate => &[StepAction::Rewind, StepAction::Explain, StepAction::Adjust],
        StepState::Pending => &[StepAction::Approve, StepAction::Reject, StepAction::Explain],
        StepState::Rejected => &[StepAction::Restore, StepAction::Explain],
    }
}

/// A transaction's card.
#[must_use = "a step card does nothing until it is shown"]
pub struct StepCard<'a> {
    label: &'a str,
    actor: &'a str,
    actor_kind: ActorKind,
    cause: Option<&'a str>,
    metrics: &'a [Metric<'a>],
    age: Option<&'a str>,
    state: StepState,
    before: Thumb,
    after: Thumb,
    evidence: Option<Thumb>,
    compact: bool,
    salt: Option<egui::Id>,
}

/// Which pictures are on screen and not yet there: the host starts loading these.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Wants {
    /// The "before" picture.
    pub before: bool,
    /// The "after" picture.
    pub after: bool,
    /// The failed gate's evidence image.
    pub evidence: bool,
}

impl Wants {
    /// Whether any picture is wanted.
    pub const fn any(self) -> bool {
        self.before || self.after || self.evidence
    }
}

/// Width of a before or after picture: 76 pt, 56 when compact (4:3).
const fn thumb_width(compact: bool) -> f32 {
    if compact { 56.0 } else { 76.0 }
}
/// Height of the small caption under a picture.
const CAPTION: f32 = 14.0;
/// Height of the label, actor and chip lines.
const LINE: f32 = 20.0;
/// Space inside the card.
const PAD: f32 = space::S12;

impl<'a> StepCard<'a> {
    /// A card for the step `label` ("Rounder face profile"), made by `actor`.
    pub fn new(label: &'a str, actor: &'a str, actor_kind: ActorKind) -> Self {
        a11y::require_label(label);
        a11y::require_label(actor);
        Self {
            label,
            actor,
            actor_kind,
            cause: None,
            metrics: &[],
            age: None,
            state: StepState::Applied,
            before: Thumb::Loading,
            after: Thumb::Loading,
            evidence: None,
            compact: false,
            salt: None,
        }
    }

    /// The prompt, chat message or rule that caused the step, as the host wants it quoted; cut with an ellipsis.
    pub const fn cause(mut self, cause: &'a str) -> Self {
        self.cause = Some(cause);
        self
    }

    /// The metric deltas as chips.
    pub const fn metrics(mut self, metrics: &'a [Metric<'a>]) -> Self {
        self.metrics = metrics;
        self
    }

    /// How long ago, formatted by the host ("4 s").
    pub const fn age(mut self, age: &'a str) -> Self {
        self.age = Some(age);
        self
    }

    /// Where the step is in review.
    pub const fn state(mut self, state: StepState) -> Self {
        self.state = state;
        self
    }

    /// The before and after pictures.
    pub const fn pictures(mut self, before: Thumb, after: Thumb) -> Self {
        self.before = before;
        self.after = after;
        self
    }

    /// The failed gate's evidence image (shown only in the failed-gate state).
    pub const fn evidence(mut self, evidence: Thumb) -> Self {
        self.evidence = Some(evidence);
        self
    }

    /// Smaller pictures (56 pt) for a dense list.
    pub const fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }

    /// Keys the card by `salt` instead of by position.
    pub fn id_salt(mut self, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.salt = Some(egui::Id::new(salt));
        self
    }

    /// Shows the card at full width.
    pub fn show(self, ui: &mut Ui) -> StepOutput {
        let ds = ds(ui.ctx());
        let ctx = ui.ctx().clone();
        let width = ui.available_width();
        let layout = self.layout(&ctx, &ds, width);
        let (rect, outer) = ui.allocate_exact_size(vec2(width, layout.height), Sense::hover());
        let id = self.salt.map_or(outer.id.with("step"), |salt| ids::salted(ui, KIND, salt));
        let rect = paint::snap(&ctx, rect);
        let origin = rect.min;
        let on_screen = ui.is_rect_visible(rect);

        let mut wants = Wants::default();
        if on_screen {
            self.paint_card(ui, &ds, rect);
            wants = self.paint_pictures(ui, &ds, origin, &layout);
            self.paint_text(ui, &ds, rect, id, &layout);
        }
        let action = self.buttons(ui, &ds, id, origin, &layout);
        let response = ui.interact(rect, id, Sense::hover());
        self.accessibility(&response, &ds);
        StepOutput { response, action, wants }
    }

    /// Where everything goes, from the card's width: heights are known before anything is drawn.
    fn layout(&self, ctx: &egui::Context, ds: &Ds, width: f32) -> Layout {
        let inner = width - 2.0 * PAD;
        let thumb_w = thumb_width(self.compact);
        let thumb_h = (thumb_w * 0.75).round();
        let pictures_h = thumb_h + CAPTION;
        let label_h = LINE;
        let actor_h = LINE;
        let (chip_rows, chip_h) = self.chip_rows(ctx, ds, inner);
        let (button_rows, buttons_h) = self.button_rows(ctx, ds, inner);
        let height = PAD
            + pictures_h
            + space::S8
            + label_h
            + actor_h
            + if chip_rows > 0 { space::S4 + chip_h } else { 0.0 }
            + space::S8
            + buttons_h
            + PAD;
        Layout { height, thumb_w, thumb_h, button_rows, inner }
    }

    /// How many rows the metric chips need at `inner` width, and how tall they are together.
    fn chip_rows(&self, ctx: &egui::Context, ds: &Ds, inner: f32) -> (usize, f32) {
        let mut rows = 0_usize;
        let mut x = 0.0_f32;
        for metric in self.metrics {
            let w = chip_width(ctx, ds, metric);
            if rows == 0 || x + w > inner {
                rows += 1;
                x = 0.0;
            }
            x += w + space::S6;
        }
        #[allow(clippy::cast_precision_loss, reason = "a handful of rows")]
        let height = rows as f32 * LINE + rows.saturating_sub(1) as f32 * space::S4;
        (rows, height)
    }

    /// How many rows the action buttons need at `inner` width, and how tall they are together.
    fn button_rows(&self, ctx: &egui::Context, ds: &Ds, inner: f32) -> (usize, f32) {
        let mut rows = 1_usize;
        let mut x = 0.0_f32;
        for action in actions(self.state) {
            let w = action_button(&ds.text(action.label()), *action).width(ctx);
            if x > 0.0 && x + w > inner {
                rows += 1;
                x = 0.0;
            }
            x += w + space::S8;
        }
        #[allow(clippy::cast_precision_loss, reason = "a handful of rows")]
        let height = rows as f32 * ds.metrics.control_row + rows.saturating_sub(1) as f32 * space::S4;
        (rows, height)
    }

    /// The fill and border of the state.
    fn paint_card(&self, ui: &Ui, ds: &Ds, rect: Rect) {
        let t = &ds.theme;
        let painter = ui.painter();
        painter.rect_filled(rect, radius::CARD, t.bg2);
        let stroke = match self.state {
            StepState::Pending => Stroke::new(2.0, t.accent),
            StepState::FailedGate => Stroke::new(2.0, t.warn),
            StepState::Applied | StepState::Rejected => Stroke::new(1.0, border(t)),
        };
        painter.rect_stroke(rect, radius::CARD, stroke, StrokeKind::Inside);
    }

    /// Before, an arrow, after, and the evidence of a failed gate; reports which are on screen and missing.
    fn paint_pictures(&self, ui: &Ui, ds: &Ds, origin: egui::Pos2, layout: &Layout) -> Wants {
        let t = &ds.theme;
        let painter = ui.painter();
        let dim = self.state != StepState::Rejected;
        let slot = |x: f32| {
            Rect::from_min_size(pos2(origin.x + PAD + x, origin.y + PAD), vec2(layout.thumb_w, layout.thumb_h))
        };
        let before = slot(0.0);
        let after = slot(layout.thumb_w + 20.0);
        let mut wants = Wants::default();
        for (rect, thumb, caption, flag) in [
            (before, self.before, Msg::StepBefore, &mut wants.before),
            (after, self.after, Msg::StepAfter, &mut wants.after),
        ] {
            paint_thumb(painter, ds, rect, thumb, radius::CONTROL, dim);
            painter.rect_stroke(
                rect,
                radius::CONTROL,
                Stroke::new(t.control_stroke, paint::dim(dim, t.border_control)),
                StrokeKind::Inside,
            );
            let word = ds.text(caption);
            let area = Rect::from_min_size(pos2(rect.left(), rect.bottom() + 1.0), vec2(rect.width(), CAPTION));
            paint::text_in(painter, area, Align2::CENTER_TOP, &word, &ds.text.micro, t.text3);
            *flag = ui.is_rect_visible(rect) && thumb == Thumb::Loading;
        }
        paint::icon(
            painter,
            ds,
            pos2(before.right() + 10.0, before.center().y),
            icons::ARROW_RIGHT,
            14.0,
            t.text3,
            false,
        );
        if self.state == StepState::FailedGate
            && let Some(evidence) = self.evidence
        {
            let rect = slot(2.0 * (layout.thumb_w + 20.0));
            paint_thumb(painter, ds, rect, evidence, radius::CONTROL, true);
            painter.rect_stroke(rect, radius::CONTROL, Stroke::new(2.0, t.warn), StrokeKind::Inside);
            let word = ds.text(Msg::StepEvidence);
            let area = Rect::from_min_size(pos2(rect.left(), rect.bottom() + 1.0), vec2(rect.width(), CAPTION));
            paint::text_in(painter, area, Align2::CENTER_TOP, &word, &ds.text.micro, t.text3);
            wants.evidence = ui.is_rect_visible(rect) && evidence == Thumb::Loading;
        }
        wants
    }

    /// Age and state word at the top right, then the label, the actor and cause, and the metric chips.
    fn paint_text(&self, ui: &Ui, ds: &Ds, rect: Rect, id: egui::Id, layout: &Layout) {
        let origin = rect.min;
        let t = &ds.theme;
        let painter = ui.painter();
        let left = origin.x + PAD;
        let right = rect.right() - PAD;
        let top = origin.y + PAD;

        // Top right: how long ago, and where the step is in review.
        let mut y = top;
        if let Some(age) = self.age {
            paint::text(painter, pos2(right, y + 8.0), Align2::RIGHT_CENTER, age, &ds.text.small, t.text3);
            y += LINE;
        }
        self.paint_state_word(painter, ds, pos2(right, y + 8.0));

        let mut line_y = top + layout.thumb_h + CAPTION + space::S8;
        // The label, struck through when rejected.
        let area = Rect::from_min_max(pos2(left, line_y), pos2(right, line_y + LINE));
        let (font, ink) =
            if self.state == StepState::Rejected { (&ds.text.body, t.text3) } else { (&ds.text.strong, t.text) };
        let drawn = paint::text_in(painter, area, Align2::LEFT_CENTER, self.label, font, ink);
        if self.state == StepState::Rejected {
            painter.line_segment([drawn.left_center(), drawn.right_center()], Stroke::new(1.0, ink));
        }
        line_y += LINE;

        // The actor, then the cause after it.
        let chip = paint_actor(painter, ds, pos2(left, line_y + LINE / 2.0), self.actor, self.actor_kind);
        if let Some(cause) = self.cause {
            let area = Rect::from_min_max(pos2(chip + space::S8, line_y), pos2(right, line_y + LINE));
            let shown = paint::text_in(painter, area, Align2::LEFT_CENTER, cause, &ds.text.small, t.text2);
            if paint::text_width(painter.ctx(), cause, &ds.text.small) > shown.width() + 0.5 {
                let _ = ui.interact(area, id.with("cause"), Sense::hover()).tip(Tip::new(cause));
            }
        }
        line_y += LINE + space::S4;

        // The metric chips, wrapped.
        let mut x = left;
        let mut row = 0.0_f32;
        for metric in self.metrics {
            let w = chip_width(painter.ctx(), ds, metric);
            if x > left && x + w > right {
                x = left;
                row += 1.0;
            }
            paint_chip(
                painter,
                ds,
                Rect::from_min_size(pos2(x, line_y + row * (LINE + space::S4)), vec2(w, LINE)),
                metric,
            );
            x += w + space::S6;
        }
    }

    /// "Pending review", "Rejected" or "Gate failed": an icon and a word, right-aligned; nothing when applied.
    fn paint_state_word(&self, painter: &egui::Painter, ds: &Ds, at: egui::Pos2) {
        let t = &ds.theme;
        let (glyph, msg, ink) = match self.state {
            StepState::Applied => return,
            StepState::Pending => (icons::HAND_PALM, Msg::StepPending, t.accent_text),
            StepState::Rejected => (icons::X_CIRCLE, Msg::StepRejected, t.text2),
            StepState::FailedGate => (icons::WARNING, Msg::StepGateFailed, t.warn),
        };
        let word = ds.text(msg);
        let drawn = paint::text(painter, at, Align2::RIGHT_CENTER, &word, &ds.text.small, ink);
        paint::icon(painter, ds, pos2(drawn.left() - 10.0, at.y), glyph, 14.0, ink, true);
    }

    /// The action buttons, left to right, wrapping at the card's edge. Returns the one pressed.
    fn buttons(&self, ui: &mut Ui, ds: &Ds, id: egui::Id, origin: egui::Pos2, layout: &Layout) -> Option<StepAction> {
        let ctx = ui.ctx().clone();
        let row_h = ds.metrics.control_row;
        let top = origin.y + layout.height
            - PAD
            - (layout.button_rows as f32) * row_h
            - (layout.button_rows.saturating_sub(1) as f32) * space::S4;
        let (left, right) = (origin.x + PAD, origin.x + PAD + layout.inner);
        let (mut x, mut y) = (left, top);
        let mut pressed = None;
        for action in actions(self.state) {
            let label = ds.text(action.label());
            let button = action_button(&label, *action);
            let w = button.width(&ctx);
            if x > left && x + w > right {
                x = left;
                y += row_h + space::S4;
            }
            let rect = Rect::from_min_size(pos2(x, y), vec2(w, row_h));
            let response = button.show_at(ui, id.with(("action", *action as usize)), rect);
            // "Rewind: Rounder face profile": what it does, to which step. Built only when a screen reader asks.
            ctx.accesskit_node_builder(response.id, |node| {
                let label = ds.text(action.label());
                node.set_label(ds.text_with(Msg::StepActionName, &[("action", &label), ("label", self.label)]));
            });
            if response.clicked() {
                pressed = Some(*action);
            }
            x += w + space::S8;
        }
        pressed
    }

    fn accessibility(&self, response: &Response, ds: &Ds) {
        let (actor, label, state) = (self.actor, self.label, self.state);
        a11y::describe(
            response,
            || {
                WidgetInfo::labeled(
                    WidgetType::Other,
                    true,
                    ds.text_with(Msg::StepGroup, &[("actor", actor), ("label", label)]),
                )
            },
            |node| {
                node.set_role(accesskit::Role::Group);
                let word = match state {
                    StepState::Applied => Msg::StepApplied,
                    StepState::Pending => Msg::StepPending,
                    StepState::Rejected => Msg::StepRejected,
                    StepState::FailedGate => Msg::StepGateFailed,
                };
                node.set_value(ds.text(word).as_ref());
            },
        );
    }
}

impl Widget for StepCard<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        self.show(ui).response
    }
}

/// What a [`StepCard`] reports.
#[derive(Debug)]
pub struct StepOutput {
    /// The card as a group named for the actor and the step, with its state as its value.
    pub response: Response,
    /// The button pressed this frame.
    pub action: Option<StepAction>,
    /// The pictures that are on screen and still missing.
    pub wants: Wants,
}

/// The card's measurements for its width.
struct Layout {
    height: f32,
    thumb_w: f32,
    thumb_h: f32,
    button_rows: usize,
    inner: f32,
}

/// The button of an action: Approve is the primary one, Explain and Adjust are quiet. The spoken name is set where the
/// button is placed.
fn action_button(label: &str, action: StepAction) -> Button<'_> {
    let button = Button::new(label).icon(action.icon());
    match action {
        StepAction::Approve => button.primary(),
        StepAction::Rewind | StepAction::Reject | StepAction::Restore => button,
        StepAction::Explain | StepAction::Adjust => button.ghost(),
    }
}

/// The card's outline: the hairline `line`, or the control border in high contrast.
fn border(t: &Theme) -> egui::Color32 {
    if t.control_stroke > 1.0 { t.border_control } else { t.line }
}

/// Draws the actor as a disc with the initial and the name; returns where it ends.
fn paint_actor(painter: &egui::Painter, ds: &Ds, at: egui::Pos2, name: &str, kind: ActorKind) -> f32 {
    let t = &ds.theme;
    let (fill, ink) = kind.disc(t);
    painter.circle_filled(pos2(at.x + 8.0, at.y), 8.0, fill);
    if let Some(initial) = name.chars().next() {
        let mut buf = [0u8; 4];
        let upper = initial.to_uppercase().next().unwrap_or(initial);
        paint::text(
            painter,
            pos2(at.x + 8.0, at.y),
            Align2::CENTER_CENTER,
            upper.encode_utf8(&mut buf),
            &ds.text.micro,
            ink,
        );
    }
    paint::text(painter, pos2(at.x + 16.0 + space::S6, at.y), Align2::LEFT_CENTER, name, &ds.text.small, t.text2)
        .right()
}

/// The width of a metric chip: padding, the mark, the name and the delta.
fn chip_width(ctx: &egui::Context, ds: &Ds, metric: &Metric<'_>) -> f32 {
    let mark = if metric.change == Change::Neutral { 0.0 } else { 12.0 + space::S4 };
    2.0 * space::S8
        + mark
        + paint::text_width(ctx, metric.name, &ds.text.small)
        + space::S4
        + paint::text_width(ctx, metric.delta, &ds.text.mono)
}

/// The ink and fill of a metric chip.
fn chip_colours(t: &Theme, change: Change) -> (egui::Color32, egui::Color32) {
    match change {
        Change::Better => (t.ok, t.status_tint(Status::Ok, t.bg2)),
        Change::Worse => (t.warn, t.status_tint(Status::Warn, t.bg2)),
        Change::Neutral => (t.text2, t.bg3),
    }
}

/// One metric chip: a pill with the mark, the name and the delta.
fn paint_chip(painter: &egui::Painter, ds: &Ds, rect: Rect, metric: &Metric<'_>) {
    let t = &ds.theme;
    let (ink, fill) = chip_colours(t, metric.change);
    painter.rect_filled(rect, radius::FULL, fill);
    let mut x = rect.left() + space::S8;
    let cy = rect.center().y;
    match metric.change {
        Change::Better => {
            paint::icon(painter, ds, pos2(x + 6.0, cy), icons::CHECK, 12.0, ink, false);
            x += 12.0 + space::S4;
        }
        Change::Worse => {
            paint::icon(painter, ds, pos2(x + 6.0, cy), icons::WARNING, 12.0, ink, true);
            x += 12.0 + space::S4;
        }
        Change::Neutral => {}
    }
    let name = paint::text(painter, pos2(x, cy), Align2::LEFT_CENTER, metric.name, &ds.text.small, ink);
    paint::text(painter, pos2(name.right() + space::S4, cy), Align2::LEFT_CENTER, metric.delta, &ds.text.mono, ink);
}

register_contrast!("step_card", pairs);

/// The card's text and states on its surface, the chips on their tints, the state words and the borders.
fn pairs(table: &mut Table<'_>) {
    let t: Theme = *table.theme();
    table.add("label", t.text, t.bg2, Rule::Text);
    table.add("rejected label", t.text3, t.bg2, Rule::Text);
    table.add("cause and actor", t.text2, t.bg2, Rule::Text);
    table.add("age and captions", t.text3, t.bg2, Rule::Text);
    table.add("pending word", t.accent_text, t.bg2, Rule::Text);
    table.add("gate failed word", t.warn, t.bg2, Rule::Text);
    table.add("pending border", t.accent, t.bg2, Rule::NonText);
    table.add("gate failed border", t.warn, t.bg2, Rule::NonText);
    for (name, change) in [("better", Change::Better), ("worse", Change::Worse), ("neutral", Change::Neutral)] {
        let (ink, fill) = chip_colours(&t, change);
        table.add(format!("{name} chip"), ink, fill, Rule::Text);
    }
}
