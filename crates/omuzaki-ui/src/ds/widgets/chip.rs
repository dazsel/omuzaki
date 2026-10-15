//! Chips: actor, scope, cost, status and gate (`design-system.md` section 7.0).
//!
//! A chip is small, quiet text on a pill. Status chips always carry an icon and a word as well as a colour (P9), and
//! the actor chip always carries the name: an initial alone is not an identity.

use std::borrow::Cow;

use egui::{
    Align2, EventFilter, Rect, Response, Sense, Stroke, StrokeKind, Ui, Widget, WidgetInfo, WidgetType, pos2, vec2,
};

use crate::ds::a11y;
use crate::ds::contrast::{Rule, Table};
use crate::ds::icons;
use crate::ds::paint::{self, RingPlacement};
use crate::ds::strings::Msg;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{Status, mix, radius, space};
use crate::register_contrast;
use omuzaki_ops_api::Scope;

use super::tooltip::{Tip, TipExt as _};

const CHIP_HEIGHT: f32 = 20.0;
const BADGE_HEIGHT: f32 = 18.0;
/// The least room a chip's label keeps when the chip is squeezed: an ellipsis and a letter. Buttons keep the same.
const MIN_LABEL: f32 = 16.0;

/// The room a chip's label gets in a layout that offers `fit` pt, when the chip's other parts take `fixed` pt. A label
/// that needs more than this is cut and its tooltip says the whole of it.
fn label_room(fit: f32, fixed: f32) -> f32 {
    (fit - fixed).max(MIN_LABEL)
}

/// Completes a cut label with its tooltip: the whole text, shown on hover or on keyboard focus. `whole` builds it only
/// when the tooltip is shown, so a chip that stays idle allocates nothing.
///
/// On the frame a chip stops being active (it lost its hover and focus, or its label now fits) the Esc dismissal is
/// cleared directly, not through `tip`: `tip` on an idle chip would show the tooltip of a chip that now fits.
fn cut_tip<'a>(response: Response, cut: bool, whole: impl FnOnce() -> Cow<'a, str>) -> Response {
    let active = cut && (response.hovered() || response.has_focus());
    if cut && response.has_focus() {
        // Escape is the tooltip's to dismiss, so a focused cut chip keeps its focus when Escape is pressed.
        let keep_on_escape = EventFilter { escape: true, ..EventFilter::default() };
        response.ctx.memory_mut(|m| m.set_focus_lock_filter(response.id, keep_on_escape));
    }
    let key = response.id.with("cut_tip_active");
    let was_active = response.ctx.data(|d| d.get_temp::<bool>(key)).unwrap_or(false);
    if active != was_active {
        response.ctx.data_mut(|d| d.insert_temp(key, active));
    }
    if active {
        let whole = whole();
        response.tip(Tip::new(&whole))
    } else {
        if was_active {
            // The key is the one `TipExt::tip` keeps its Esc dismissal under (`tooltip.rs`).
            response.ctx.data_mut(|d| d.remove::<bool>(response.id.with("tip_dismissed")));
        }
        response
    }
}

/// Who did something. Agents are violet, people use the accent, plugins and the system stay neutral.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActorKind {
    /// A person (you or a collaborator).
    Person,
    /// An AI agent: the only thing that is ever violet.
    Agent,
    /// A plugin or connector.
    Plugin,
    /// The app itself (autosave, import).
    System,
}

impl ActorKind {
    /// The message that names the kind to a screen reader.
    const fn word(self) -> Msg {
        match self {
            Self::Person => Msg::ActorPerson,
            Self::Agent => Msg::ActorAgent,
            Self::Plugin => Msg::ActorPlugin,
            Self::System => Msg::ActorSystem,
        }
    }

    /// The fill and the ink of the initial's disc.
    pub(super) fn disc(self, t: &crate::ds::tokens::Theme) -> (egui::Color32, egui::Color32) {
        match self {
            Self::Agent => (t.agent, t.on_agent),
            Self::Person => (t.accent, t.on_accent),
            Self::Plugin | Self::System => (t.bg4, t.text),
        }
    }
}

/// "● Designer": a circle with the initial plus the name. Always includes the name.
///
/// It caps its width at the room the layout gives, cuts a long name with an ellipsis, and its tooltip shows the whole name on hover or keyboard focus.
#[derive(Clone, Copy, Debug)]
#[must_use = "a chip does nothing until it is added to a Ui"]
pub struct ActorChip<'a> {
    name: &'a str,
    kind: ActorKind,
}

impl<'a> ActorChip<'a> {
    /// A chip for `name`.
    pub fn new(name: &'a str, kind: ActorKind) -> Self {
        a11y::require_label(name);
        Self { name, kind }
    }
}

impl Widget for ActorChip<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let ds = ds(ui.ctx());
        let t = &ds.theme;
        let ctx = ui.ctx().clone();
        let fixed = 16.0 + space::S6 + space::S4;
        let name_full = paint::text_width(&ctx, self.name, &ds.text.small);
        let room = label_room(paint::fit_width(ui), fixed);
        let name_w = name_full.min(room);
        let (rect, response) = ui.allocate_exact_size(vec2(fixed + name_w, CHIP_HEIGHT), Sense::hover());
        if ui.is_rect_visible(rect) {
            let (fill, ink) = self.kind.disc(t);
            let painter = ui.painter();
            let dot = pos2(rect.left() + 8.0, rect.center().y);
            painter.circle_filled(dot, 8.0, fill);
            let initial = self.name.chars().next().map_or([0u8; 4], |c| {
                let mut buf = [0u8; 4];
                c.to_uppercase().next().unwrap_or(c).encode_utf8(&mut buf);
                buf
            });
            let len = initial.iter().position(|&b| b == 0).unwrap_or(4);
            if let Ok(letter) = std::str::from_utf8(&initial[..len]) {
                paint::text(painter, dot, Align2::CENTER_CENTER, letter, &ds.text.micro, ink);
            }
            let area =
                Rect::from_min_size(pos2(rect.left() + 16.0 + space::S6, rect.top()), vec2(name_w, rect.height()));
            paint::text_in(painter, area, Align2::LEFT_CENTER, self.name, &ds.text.small, t.text2);
        }
        a11y::describe(
            &response,
            || WidgetInfo::labeled(WidgetType::Label, true, self.name),
            |node| {
                node.set_description(ds.text(self.kind.word()).into_owned());
            },
        );
        cut_tip(response, name_full > room, || Cow::Borrowed(self.name))
    }
}

/// `edit:character`: what an op or an agent may touch. `broadcast`, `publish`, `spend`, `native`, `input` and `process` get a warning icon.
///
/// It caps its width at the room the layout gives, cuts a long name with an ellipsis, and its tooltip shows the whole name on hover or keyboard focus.
#[derive(Clone, Copy, Debug)]
#[must_use = "a chip does nothing until it is added to a Ui"]
pub struct ScopeBadge<'a> {
    scope: &'a str,
}

/// Whether a scope name (such as `"broadcast"`) reaches outside the project, so it carries the warning icon
/// (`shell.md` section 10). The risk itself is defined by [`Scope::is_risky`]. A name that does not parse is treated
/// as risky: an unknown scope should never look safe.
pub fn is_risky_scope(name: &str) -> bool {
    // Looked up here rather than with `parse`: its error for an unknown name is a formatted `String`, and this runs
    // every frame for every badge.
    Scope::ALL.into_iter().find(|sc| sc.name() == name).is_none_or(Scope::is_risky)
}

impl<'a> ScopeBadge<'a> {
    /// A badge for `scope`, such as `"edit:character"`.
    pub fn new(scope: &'a str) -> Self {
        a11y::require_label(scope);
        Self { scope }
    }

    fn is_risky(&self) -> bool {
        is_risky_scope(self.scope)
    }
}

impl Widget for ScopeBadge<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let ds = ds(ui.ctx());
        let t = &ds.theme;
        let ctx = ui.ctx().clone();
        let risky = self.is_risky();
        let font = egui::FontId::monospace(11.0);
        let text_full = paint::text_width(&ctx, self.scope, &font);
        let icon_w = if risky { 12.0 + space::S4 } else { 0.0 };
        let fixed = icon_w + 2.0 * space::S6 + 2.0;
        let room = label_room(paint::fit_width(ui), fixed);
        let text_w = text_full.min(room);
        let (rect, response) = ui.allocate_exact_size(vec2(fixed + text_w, BADGE_HEIGHT), Sense::hover());
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            // Neutral outlines are quiet; the warning ones are full strength so the icon and the border agree.
            let outline = if risky { t.warn } else { t.line };
            painter.rect_stroke(rect, radius::CONTROL, Stroke::new(t.control_stroke, outline), StrokeKind::Inside);
            let mut x = rect.left() + space::S6 + 1.0;
            if risky {
                paint::icon(painter, &ds, pos2(x + 6.0, rect.center().y), icons::WARNING, 12.0, t.warn, true);
                x += icon_w;
            }
            let area = Rect::from_min_size(pos2(x, rect.top()), vec2(text_w, rect.height()));
            paint::text_in(painter, area, Align2::LEFT_CENTER, self.scope, &font, t.text2);
        }
        a11y::describe(
            &response,
            || WidgetInfo::labeled(WidgetType::Label, true, self.scope),
            |node| {
                node.set_description(ds.text(if risky { Msg::ScopeOutside } else { Msg::ScopeInside }).into_owned());
            },
        );
        cut_tip(response, text_full > room, || Cow::Borrowed(self.scope))
    }
}

/// `~40 s · 1.2 GB · $0.12`: what running an op will cost, before it runs (P2). Omit it when the cost is trivial.
///
/// It caps its width at the room the layout gives, cuts a long cost with an ellipsis, and its tooltip shows the whole cost on hover or keyboard focus.
#[derive(Clone, Copy, Debug)]
#[must_use = "a chip does nothing until it is added to a Ui"]
pub struct CostChip<'a> {
    parts: &'a [&'a str],
}

impl<'a> CostChip<'a> {
    /// A chip for the cost parts, in the order time, memory, money.
    pub fn new(parts: &'a [&'a str]) -> Self {
        Self { parts }
    }
}

/// The cost parts' font: mono, a little under the body.
fn cost_font() -> egui::FontId {
    egui::FontId::monospace(11.5)
}

/// The separator between cost parts: a middle dot with room around it. Punctuation, not a word.
const COST_SEPARATOR: &str = " · ";

/// The width the cost `parts` take when painted by [`paint_cost`].
pub(super) fn cost_width(ctx: &egui::Context, parts: &[&str]) -> f32 {
    let font = cost_font();
    let sep_w = paint::text_width(ctx, COST_SEPARATOR, &font);
    parts
        .iter()
        .enumerate()
        .map(|(i, part)| paint::text_width(ctx, part, &font) + if i > 0 { sep_w } else { 0.0 })
        .sum()
}

/// Paints the cost `parts` from `at`, the left end of their centre line: `~40 s · 1.2 GB · $0.12`. Shared by the chip
/// and the rows that show a cost beside other things (a job row), which place it themselves.
pub(super) fn paint_cost(painter: &egui::Painter, ds: &Ds, at: egui::Pos2, parts: &[&str]) {
    paint_cost_in(painter, ds, at, parts, f32::INFINITY);
}

/// [`paint_cost`] cut at `room` pt from `at`. A cost that does not fit keeps room for its ellipsis at the end: the run
/// that runs over is cut with one, and a cut that falls between two runs is marked with a bare ellipsis.
fn paint_cost_in(painter: &egui::Painter, ds: &Ds, at: egui::Pos2, parts: &[&str], room: f32) {
    let font = cost_font();
    let limit = if cost_width(painter.ctx(), parts) > room { at.x + room } else { f32::INFINITY };
    let limit = limit - paint::text_width(painter.ctx(), ELLIPSIS, &font);
    let mut x = at.x;
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            let Some(right) = cost_run(painter, x, at.y, limit, COST_SEPARATOR, &font, ds.theme.text3) else { return };
            x = right;
        }
        let Some(right) = cost_run(painter, x, at.y, limit, part, &font, ds.theme.text2) else { return };
        x = right;
    }
}

/// The mark a cut leaves: a single character, so it is the same width in every locale.
const ELLIPSIS: &str = "\u{2026}";

/// Paints one run of cost text from `x`, or the part of it that fits before `end`. Returns where it ends, or `None`
/// once it has been cut, so the caller draws nothing after it. `end` leaves room for the ellipsis of a cut run.
fn cost_run(
    painter: &egui::Painter,
    x: f32,
    cy: f32,
    end: f32,
    text: &str,
    font: &egui::FontId,
    colour: egui::Color32,
) -> Option<f32> {
    if x + paint::text_width(painter.ctx(), text, font) <= end {
        return Some(paint::text(painter, pos2(x, cy), Align2::LEFT_CENTER, text, font, colour).right());
    }
    if end - x > paint::text_width(painter.ctx(), ELLIPSIS, font) {
        let area = Rect::from_min_max(pos2(x, cy - BADGE_HEIGHT / 2.0), pos2(end, cy + BADGE_HEIGHT / 2.0));
        paint::text_in(painter, area, Align2::LEFT_CENTER, text, font, colour);
    } else {
        // Too little room for any letter, but the room reserved for the ellipsis still holds one.
        paint::text(painter, pos2(x, cy), Align2::LEFT_CENTER, ELLIPSIS, font, colour);
    }
    None
}

impl Widget for CostChip<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let ds = ds(ui.ctx());
        let whole = cost_width(ui.ctx(), self.parts);
        let room = label_room(paint::fit_width(ui), 0.0);
        let truncated = whole > room;
        let width = whole.min(room);
        let (rect, response) = ui.allocate_exact_size(vec2(width.max(1.0), BADGE_HEIGHT), Sense::hover());
        if ui.is_rect_visible(rect) {
            let cut = if truncated { rect.width() } else { f32::INFINITY };
            paint_cost_in(ui.painter(), &ds, rect.left_center(), self.parts, cut);
        }
        // A label's text is its value, so the name and the parts are one message ("Estimated cost: ~40 s, 1.2 GB").
        // Built inside the closure: it only runs when a screen reader is listening, so a frame pays nothing.
        a11y::describe(
            &response,
            || {
                let parts = self.parts.join(&ds.text(Msg::ListSeparator));
                WidgetInfo::labeled(WidgetType::Label, true, ds.text_with(Msg::CostEstimate, &[("parts", &parts)]))
            },
            |_| {},
        );
        // The whole cost is joined only while the chip is hovered or focused: a cut chip would otherwise allocate every frame.
        cut_tip(response, truncated, || Cow::Owned(self.parts.join(COST_SEPARATOR)))
    }
}

/// The resting and hovered fills of a status pill: the status colour at 10% and 18% over a raised surface.
fn pill_fills(t: &crate::ds::tokens::Theme, status: Status) -> [egui::Color32; 2] {
    [t.status_tint(status, t.bg2), mix(t.status(status), t.bg2, 18)]
}

/// Draws the pill frame shared by status and gate chips and returns the rectangle to put content in.
fn pill(ui: &Ui, ds: &Ds, rect: egui::Rect, status: Status, hover: f32) -> egui::Rect {
    let t = &ds.theme;
    let [base, lifted] = pill_fills(t, status);
    let fill = paint::lerp(base, lifted, hover);
    let painter = ui.painter();
    painter.rect_filled(rect, radius::FULL, fill);
    painter.rect_stroke(rect, radius::FULL, Stroke::new(1.0, mix(t.status(status), t.bg2, 45)), StrokeKind::Inside);
    rect.shrink2(vec2(space::S8, 0.0))
}

/// A status as an icon, a word and a colour: "✓ Connected".
///
/// It caps its width at the room the layout gives, cuts a long text with an ellipsis, and its tooltip shows the whole text on hover or keyboard focus.
#[derive(Clone, Copy, Debug)]
#[must_use = "a chip does nothing until it is added to a Ui"]
pub struct StatusChip<'a> {
    status: Status,
    text: &'a str,
}

impl<'a> StatusChip<'a> {
    /// A chip saying `text` with `status`'s icon and colour.
    pub fn new(status: Status, text: &'a str) -> Self {
        a11y::require_label(text);
        Self { status, text }
    }
}

impl Widget for StatusChip<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let ds = ds(ui.ctx());
        let ctx = ui.ctx().clone();
        let fixed = 2.0 * space::S8 + 14.0 + space::S4;
        let text_full = paint::text_width(&ctx, self.text, &ds.text.small);
        let room = label_room(paint::fit_width(ui), fixed);
        let text_w = text_full.min(room);
        let (rect, response) = ui.allocate_exact_size(vec2(fixed + text_w, CHIP_HEIGHT), Sense::hover());
        if ui.is_rect_visible(rect) {
            let inner = pill(ui, &ds, rect, self.status, 0.0);
            let colour = ds.theme.status(self.status);
            paint::icon(
                ui.painter(),
                &ds,
                pos2(inner.left() + 7.0, inner.center().y),
                icons::status(self.status),
                14.0,
                colour,
                true,
            );
            let area =
                Rect::from_min_size(pos2(inner.left() + 14.0 + space::S4, inner.top()), vec2(text_w, inner.height()));
            paint::text_in(ui.painter(), area, Align2::LEFT_CENTER, self.text, &ds.text.small, colour);
        }
        a11y::describe(
            &response,
            || WidgetInfo::labeled(WidgetType::Label, true, self.text),
            |node| {
                node.set_description(ds.status_word(self.status).into_owned());
            },
        );
        cut_tip(response, text_full > room, || Cow::Borrowed(self.text))
    }
}

/// A measured number next to what it checks: `✓ likeness 0.88` or `▲ likeness 0.81 < 0.85`. Click opens the evidence.
///
/// It caps its width at the room the layout gives and cuts a long metric with an ellipsis (the value and the limit stay whole); its tooltip shows the whole metric on hover or keyboard focus.
#[derive(Clone, Copy, Debug)]
#[must_use = "a chip does nothing until it is added to a Ui"]
pub struct GateChip<'a> {
    metric: &'a str,
    value: &'a str,
    threshold: Option<&'a str>,
}

impl<'a> GateChip<'a> {
    /// A passing gate: `metric` reads `value`.
    pub fn passed(metric: &'a str, value: &'a str) -> Self {
        a11y::require_label(metric);
        Self { metric, value, threshold: None }
    }

    /// A failing gate: `value` is below (or above) `threshold`.
    pub fn failed(metric: &'a str, value: &'a str, threshold: &'a str) -> Self {
        a11y::require_label(metric);
        Self { metric, value, threshold: Some(threshold) }
    }
}

impl Widget for GateChip<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let ds = ds(ui.ctx());
        let t = &ds.theme;
        let ctx = ui.ctx().clone();
        let status = if self.threshold.is_some() { Status::Warn } else { Status::Ok };
        let mono = egui::FontId::monospace(11.5);
        let metric_full = paint::text_width(&ctx, self.metric, &ds.text.small);
        let value_w = paint::text_width(&ctx, self.value, &mono);
        let limit_w = self.threshold.map_or(0.0, |th| {
            space::S6 + paint::text_width(&ctx, "<", &mono) + space::S4 + paint::text_width(&ctx, th, &mono)
        });
        // The metric is the label that gives way: the value and the limit are the numbers, and stay whole.
        let fixed = 2.0 * space::S8 + 14.0 + space::S4 + space::S6 + value_w + limit_w;
        let room = label_room(paint::fit_width(ui), fixed);
        let metric_w = metric_full.min(room);
        let (rect, response) = ui.allocate_exact_size(vec2(fixed + metric_w, CHIP_HEIGHT), Sense::click());
        if ui.is_rect_visible(rect) {
            let hover = paint::fade(&ctx, &ds, response.id, paint::hot(&response));
            let inner = pill(ui, &ds, rect, status, hover);
            let colour = t.status(status);
            let painter = ui.painter();
            let cy = inner.center().y;
            paint::icon(
                painter,
                &ds,
                pos2(inner.left() + 7.0, cy),
                if status == Status::Ok { icons::SHIELD_CHECK } else { icons::WARNING },
                14.0,
                colour,
                true,
            );
            let mut x = inner.left() + 14.0 + space::S4;
            let area = Rect::from_min_size(pos2(x, inner.top()), vec2(metric_w, inner.height()));
            x = paint::text_in(painter, area, Align2::LEFT_CENTER, self.metric, &ds.text.small, t.text).right()
                + space::S6;
            x = paint::text(painter, pos2(x, cy), Align2::LEFT_CENTER, self.value, &mono, colour).right();
            if let Some(th) = self.threshold {
                x = paint::text(painter, pos2(x + space::S6, cy), Align2::LEFT_CENTER, "<", &mono, t.text2).right();
                paint::text(painter, pos2(x + space::S4, cy), Align2::LEFT_CENTER, th, &mono, t.text2);
            }
            paint::focus_ring(painter, t, &response, rect, radius::FULL, RingPlacement::Outside);
        }
        a11y::describe(
            &response,
            || WidgetInfo::labeled(WidgetType::Button, true, self.metric),
            |node| {
                node.set_value(match self.threshold {
                    Some(threshold) => {
                        ds.text_with(Msg::GateFailed, &[("value", self.value), ("threshold", threshold)])
                    }
                    None => ds.text_with(Msg::GatePassed, &[("value", self.value)]),
                });
                node.set_description(ds.text(Msg::GateOpens).into_owned());
            },
        );
        cut_tip(response, metric_full > room, || Cow::Borrowed(self.metric))
    }
}

register_contrast!("chip", pairs);

/// The ink of every chip on the fill it is drawn on, including the hovered pill.
fn pairs(table: &mut Table<'_>) {
    let t = *table.theme();
    for kind in [ActorKind::Person, ActorKind::Agent, ActorKind::Plugin, ActorKind::System] {
        let (fill, ink) = kind.disc(&t);
        table.add(format!("actor chip {kind:?} initial on its disc"), ink, fill, Rule::Text);
    }
    // A status chip only rests; a gate chip, which can be clicked, also hovers, and it is Ok or Warn.
    for status in [Status::Info, Status::Ok, Status::Warn, Status::Danger] {
        let hovers = matches!(status, Status::Ok | Status::Warn);
        for (state, fill) in ["rest", "hover"].into_iter().zip(pill_fills(&t, status)) {
            if state == "hover" && !hovers {
                continue;
            }
            let name = format!("{status:?} pill {state}");
            table.add(format!("{name} status text"), t.status(status), fill, Rule::Text);
            table.add(format!("{name} gate metric"), t.text, fill, Rule::Text);
            table.add(format!("{name} gate limit"), t.text2, fill, Rule::Text);
        }
    }
}

#[cfg(test)]
mod risk_tests {
    use super::is_risky_scope;

    #[test]
    fn risky_names_are_the_ones_that_reach_outside_the_project() {
        for name in ["broadcast", "publish", "spend", "native", "input", "process"] {
            assert!(is_risky_scope(name), "{name} should be risky");
        }
        for name in ["read", "edit:character", "edit:scene", "edit:library", "render", "perform", "net", "fs"] {
            assert!(!is_risky_scope(name), "{name} should not be risky");
        }
    }

    #[test]
    fn a_scope_name_that_does_not_parse_is_risky() {
        assert!(is_risky_scope("edit:everything"));
        assert!(is_risky_scope(""));
        assert!(is_risky_scope("Input"));
    }
}
