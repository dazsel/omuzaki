//! `JobRow` (`design-system.md` 7.0, "Progress / job row"): one long job in a list: its title, a bar, what it costs, how
//! long it has run, **Cancel**; and when it fails, the error and **Retry**.
//!
//! "A progress bar with no cost estimate or cancel" is an anti-pattern (`principles.md`), so a running job always has a
//! Cancel, and the cost the person saw before it started (`CostHint`, P2) stays on the row.
//!
//! The bar is determinate when the host knows the fraction and **static** when it does not: a hatched fill over the
//! whole rail and the word "Working", never an animation, so nothing loops (WCAG 2.2.2) and an idle list costs no
//! frames. Every state carries an icon and a word as well as a colour (P9).
//!
//! The row is a fixed height (the control row plus 48 pt), so a list of them can be virtualised. It is painted, with
//! its two buttons placed by hand ([`Button::show_at`]): a row costs no child `Ui`. The host formats the elapsed time
//! and repaints once a second while a job runs; the row never asks for a frame itself.

use std::fmt::Write as _;

use egui::{
    Align2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Widget, WidgetInfo, WidgetType, accesskit, pos2, vec2,
};

use super::button::Button;
use super::channel_meter::hatch;
use super::chip::{cost_width, paint_cost};
use super::tooltip::{Tip, TipExt as _};
use crate::ds::a11y;
use crate::ds::contrast::{Rule, Table};
use crate::ds::icons;
use crate::ds::paint;
use crate::ds::strings::Msg;
use crate::ds::text::NumberBuf;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{Theme, radius, space};
use crate::register_contrast;

/// Where a job is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JobState<'a> {
    /// Waiting for its turn.
    Queued,
    /// Running. `progress` is the fraction done (0 to 1) when the host knows it, `None` when it does not.
    Running {
        /// How far along it is, if known.
        progress: Option<f32>,
    },
    /// It failed: the error, in the host's words, and a Retry.
    Failed {
        /// What went wrong and what to do next (`OpError`'s message and hint).
        error: &'a str,
    },
    /// It finished.
    Done,
}

/// A job's row.
#[must_use = "a job row does nothing until it is shown"]
pub struct JobRow<'a> {
    title: &'a str,
    state: JobState<'a>,
    cost: &'a [&'a str],
    elapsed: Option<&'a str>,
    salt: Option<egui::Id>,
}

/// The bar's thickness.
const BAR: f32 = 6.0;
/// Height of the line the bar (or the error) sits on.
const BAR_LINE: f32 = 14.0;
/// Height of the line of facts under it.
const META_LINE: f32 = 18.0;
/// The icon at the start of the title.
const ICON: f32 = 16.0;

impl<'a> JobRow<'a> {
    /// A row for the job `title` in `state`.
    pub fn new(title: &'a str, state: JobState<'a>) -> Self {
        a11y::require_label(title);
        Self { title, state, cost: &[], elapsed: None, salt: None }
    }

    /// What the job was estimated to cost before it started (time, memory, money), as the person saw it.
    pub const fn cost(mut self, parts: &'a [&'a str]) -> Self {
        self.cost = parts;
        self
    }

    /// How long it has run, formatted by the host ("0:42").
    pub const fn elapsed(mut self, elapsed: &'a str) -> Self {
        self.elapsed = Some(elapsed);
        self
    }

    /// Keys the row by `salt` instead of by position.
    pub fn id_salt(mut self, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.salt = Some(egui::Id::new(salt));
        self
    }

    /// Shows the row at full width and reports Cancel and Retry.
    pub fn show(self, ui: &mut Ui) -> JobOutput {
        let ds = ds(ui.ctx());
        let m = &ds.metrics;
        let height = 2.0 * space::S8 + m.control_row + BAR_LINE + META_LINE;
        let (rect, outer) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
        let id = self.salt.map_or(outer.id.with("job"), |salt| ui.make_persistent_id(salt));
        let rect = paint::snap(ui.ctx(), rect);
        let inner = rect.shrink2(vec2(space::S8, space::S8));
        let title_line = Rect::from_min_size(inner.min, vec2(inner.width(), m.control_row));
        let bar_line = Rect::from_min_size(pos2(inner.left(), title_line.bottom()), vec2(inner.width(), BAR_LINE));
        let meta_line = Rect::from_min_size(pos2(inner.left(), bar_line.bottom()), vec2(inner.width(), META_LINE));

        let plan = self.button_plan(ui.ctx(), &ds, title_line);
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let t = &ds.theme;
            painter.rect_filled(rect, radius::CARD, t.bg2);
            painter.rect_stroke(rect, radius::CARD, Stroke::new(1.0, border(t)), StrokeKind::Inside);
            let cut_title = self.paint_title(painter, &ds, title_line, plan.as_ref().map(|(_, rect)| rect.left()));
            if let Some(area) = cut_title {
                let _ = ui.interact(area, id.with("title"), Sense::hover()).tip(Tip::new(self.title));
            }
            match self.state {
                JobState::Failed { error } => {
                    let cut = Self::paint_error(painter, &ds, bar_line, error);
                    if cut {
                        let _ = ui.interact(bar_line, id.with("error"), Sense::hover()).tip(Tip::new(error));
                    }
                }
                _ => paint_bar(painter, &ds, bar_line, self.state),
            }
            self.paint_meta(painter, &ds, meta_line);
        }
        // The buttons go on top of the card they sit on, so they are made after it is painted.
        let pressed = plan.map(|(msg, rect)| self.button(ui, &ds, id, msg, rect));

        let response = ui.interact(rect, id, Sense::hover());
        self.accessibility(&response, &ds);
        let (cancel, retry) = match (pressed, self.state) {
            (Some(true), JobState::Failed { .. }) => (false, true),
            (Some(true), _) => (true, false),
            _ => (false, false),
        };
        JobOutput { response, cancel, retry }
    }

    /// The button the state offers and where it goes: Cancel while the job can still be stopped, Retry once it failed,
    /// right-aligned on the title line.
    fn button_plan(&self, ctx: &egui::Context, ds: &Ds, line: Rect) -> Option<(Msg, Rect)> {
        let msg = match self.state {
            JobState::Queued | JobState::Running { .. } => Msg::JobCancel,
            JobState::Failed { .. } => Msg::JobRetry,
            JobState::Done => return None,
        };
        let label = ds.text(msg);
        let width = self.make_button(&label).width(ctx).min(line.width() * 0.5);
        Some((msg, Rect::from_min_max(pos2(line.right() - width, line.top()), line.right_bottom())))
    }

    /// Cancel is quiet and has an X; Retry is a plain button with the arrow that means "again".
    fn make_button<'b>(&self, label: &'b str) -> Button<'b> {
        match self.state {
            JobState::Failed { .. } => Button::new(label).icon(icons::ARROW_COUNTER_CLOCKWISE),
            _ => Button::new(label).ghost().icon(icons::X),
        }
    }

    /// Shows the button in its place; true when it was pressed.
    fn button(&self, ui: &mut Ui, ds: &Ds, id: egui::Id, msg: Msg, rect: Rect) -> bool {
        let label = ds.text(msg);
        // Cancel and Retry have ids of their own: when the host turns a failed job into a running one, the focus must not
        // carry over from the one button to the other, or a held Enter would retry and then cancel.
        let (button_id, named) = if msg == Msg::JobRetry {
            (id.with("retry"), Msg::JobRetryName)
        } else {
            (id.with("cancel"), Msg::JobCancelName)
        };
        let response = self.make_button(&label).show_at(ui, button_id, rect);
        self.name_for_readers(ui.ctx(), ds, &response, named);
        crate::ds::input::is_press(&response)
    }

    /// Names a button for a screen reader as what it does to which job ("Cancel Render 24 sheets"). Built inside the
    /// closure, which runs only when a screen reader is listening, so a frame without one allocates nothing.
    fn name_for_readers(&self, ctx: &egui::Context, ds: &Ds, button: &Response, msg: Msg) {
        ctx.accesskit_node_builder(button.id, |node| node.set_label(ds.text_with(msg, &[("title", self.title)])));
    }

    /// The state's icon and the title, up to the button.
    /// The state's icon and the title, up to the button. The title's area when it had to be cut, for its tooltip.
    fn paint_title(&self, painter: &egui::Painter, ds: &Ds, line: Rect, button_left: Option<f32>) -> Option<Rect> {
        let t = &ds.theme;
        let (glyph, ink, filled) = state_icon(t, self.state);
        paint::icon(painter, ds, pos2(line.left() + ICON / 2.0, line.center().y), glyph, ICON, ink, filled);
        let left = line.left() + ICON + space::S6;
        let right = button_left.map_or(line.right(), |left| left - space::S8);
        let area = Rect::from_min_max(pos2(left, line.top()), pos2(right.max(left), line.bottom()));
        let shown = paint::text_in(painter, area, Align2::LEFT_CENTER, self.title, &ds.text.strong, t.text);
        (paint::text_width(painter.ctx(), self.title, &ds.text.strong) > shown.width() + 0.5).then_some(area)
    }

    /// "Failed" and the error under the title; true when the error had to be cut.
    fn paint_error(painter: &egui::Painter, ds: &Ds, line: Rect, error: &str) -> bool {
        let t = &ds.theme;
        let word = ds.text(Msg::JobFailed);
        let drawn = paint::text(painter, line.left_center(), Align2::LEFT_CENTER, &word, &ds.text.small, t.danger);
        let area = Rect::from_min_max(pos2(drawn.right() + space::S6, line.top()), line.max);
        let shown = paint::text_in(painter, area, Align2::LEFT_CENTER, error, &ds.text.small, t.text2);
        paint::text_width(painter.ctx(), error, &ds.text.small) > shown.width() + 0.5
    }

    /// The percent or the word for the state, the cost, and the elapsed time at the right end.
    fn paint_meta(&self, painter: &egui::Painter, ds: &Ds, line: Rect) {
        let t = &ds.theme;
        let mut x = line.left();
        match self.state {
            JobState::Running { progress: Some(fraction) } => {
                let mut percent = NumberBuf::new(f64::from(fraction.clamp(0.0, 1.0)) * 100.0, 0);
                let _ = percent.write_str("%");
                x = paint::text(
                    painter,
                    pos2(x, line.center().y),
                    Align2::LEFT_CENTER,
                    percent.as_str(),
                    &ds.text.mono,
                    t.text,
                )
                .right();
            }
            JobState::Running { progress: None } => {
                x = paint::text(
                    painter,
                    pos2(x, line.center().y),
                    Align2::LEFT_CENTER,
                    &ds.text(Msg::JobWorking),
                    &ds.text.small,
                    t.text2,
                )
                .right();
            }
            JobState::Queued => {
                x = paint::text(
                    painter,
                    pos2(x, line.center().y),
                    Align2::LEFT_CENTER,
                    &ds.text(Msg::JobQueued),
                    &ds.text.small,
                    t.text2,
                )
                .right();
            }
            JobState::Done => {
                x = paint::text(
                    painter,
                    pos2(x, line.center().y),
                    Align2::LEFT_CENTER,
                    &ds.text(Msg::JobDone),
                    &ds.text.small,
                    t.text2,
                )
                .right();
            }
            JobState::Failed { .. } => {}
        }
        let right = self.elapsed.map_or(line.right(), |elapsed| {
            let width = paint::text_width(painter.ctx(), elapsed, &ds.text.mono) + ICON + space::S4;
            let left = line.right() - width;
            paint::icon(painter, ds, pos2(left + 7.0, line.center().y), icons::CLOCK, 14.0, t.text3, false);
            paint::text(
                painter,
                pos2(left + ICON + space::S4, line.center().y),
                Align2::LEFT_CENTER,
                elapsed,
                &ds.text.mono,
                t.text3,
            );
            left - space::S8
        });
        let cost_x = x + if x > line.left() { space::S12 } else { 0.0 };
        if !self.cost.is_empty() && cost_x + cost_width(painter.ctx(), self.cost) <= right {
            paint_cost(painter, ds, pos2(cost_x, line.center().y), self.cost);
        }
    }

    fn accessibility(&self, response: &Response, ds: &Ds) {
        let state = self.state;
        let (title, elapsed, cost) = (self.title, self.elapsed, self.cost);
        a11y::describe(
            response,
            || WidgetInfo::labeled(WidgetType::ProgressIndicator, true, title),
            |node| {
                node.set_role(accesskit::Role::ProgressIndicator);
                if let JobState::Running { progress: Some(fraction) } = state {
                    node.set_min_numeric_value(0.0);
                    node.set_max_numeric_value(1.0);
                    node.set_numeric_value(f64::from(fraction.clamp(0.0, 1.0)));
                }
                let word = match state {
                    JobState::Queued => ds.text(Msg::JobQueued),
                    JobState::Running { .. } => ds.text(Msg::JobWorking),
                    JobState::Failed { .. } => ds.text(Msg::JobFailed),
                    JobState::Done => ds.text(Msg::JobDone),
                };
                // "Working, 40%, ~40 s, 1.2 GB, 0:12 elapsed": the facts a person would read, in the table's list style.
                let separator = ds.text(Msg::ListSeparator);
                let mut parts: Vec<String> = vec![word.into_owned()];
                match state {
                    JobState::Running { progress: Some(f) } => {
                        parts.push(format!("{}%", NumberBuf::new(f64::from(f.clamp(0.0, 1.0)) * 100.0, 0).as_str()));
                    }
                    JobState::Failed { error } => parts.push(error.to_owned()),
                    _ => {}
                }
                parts.extend(cost.iter().map(|c| (*c).to_owned()));
                if let Some(elapsed) = elapsed {
                    parts.push(ds.text_with(Msg::JobElapsed, &[("time", elapsed)]));
                }
                node.set_value(parts.join(&separator));
            },
        );
    }
}

impl Widget for JobRow<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        self.show(ui).response
    }
}

/// What a [`JobRow`] reports.
#[derive(Debug)]
pub struct JobOutput {
    /// The row as a whole: a progress indicator with the job's facts as its value.
    pub response: Response,
    /// Cancel was pressed: stop the job.
    pub cancel: bool,
    /// Retry was pressed: run it again.
    pub retry: bool,
}

/// The card's outline: the hairline `line`, or the control border in high contrast.
fn border(t: &Theme) -> egui::Color32 {
    if t.control_stroke > 1.0 { t.border_control } else { t.line }
}

/// The icon at the start of the title, its colour, and whether it is the Fill weight: the shape half of the state.
fn state_icon(t: &Theme, state: JobState<'_>) -> (&'static str, egui::Color32, bool) {
    match state {
        JobState::Queued => (icons::CLOCK, t.text3, false),
        JobState::Running { .. } => (icons::PLAY, t.accent, true),
        JobState::Failed { .. } => (icons::WARNING, t.danger, true),
        JobState::Done => (icons::CHECK_CIRCLE, t.ok, true),
    }
}

/// The rail and its fill: accent to the fraction done; hatched over the whole rail when unknown; green when done.
fn paint_bar(painter: &egui::Painter, ds: &Ds, line: Rect, state: JobState<'_>) {
    let t = &ds.theme;
    let rail = Rect::from_center_size(line.center(), vec2(line.width(), BAR));
    painter.rect_filled(rail, radius::FULL, t.bg4);
    let fill = |fraction: f32, colour| {
        // At least a dot wide, so a job that has only just started still shows it has.
        let width = (rail.width() * fraction.clamp(0.0, 1.0)).max(BAR).min(rail.width());
        let part = Rect::from_min_size(rail.min, vec2(width, rail.height()));
        painter.rect_filled(part, radius::FULL, colour);
        part
    };
    match state {
        JobState::Running { progress: Some(fraction) } if fraction > 0.0 => {
            fill(fraction, t.accent);
        }
        JobState::Running { progress: None } => {
            let whole = fill(1.0, t.accent);
            hatch(painter, whole, t.bg4);
        }
        JobState::Done => {
            fill(1.0, t.ok);
        }
        JobState::Queued | JobState::Running { .. } | JobState::Failed { .. } => {}
    }
}

register_contrast!("job_row", pairs);

/// The title, the state words, the error and the facts on the card, and the bar's fills on its rail.
fn pairs(table: &mut Table<'_>) {
    let t: Theme = *table.theme();
    table.add("title", t.text, t.bg2, Rule::Text);
    table.add("state word and error text", t.text2, t.bg2, Rule::Text);
    table.add("failed word", t.danger, t.bg2, Rule::Text);
    table.add("cost and elapsed", t.text3, t.bg2, Rule::Text);
    table.add("percent", t.text, t.bg2, Rule::Text);
    for (name, ink) in [("running icon and fill", t.accent), ("done icon and fill", t.ok)] {
        table.add(format!("{name} on the card"), ink, t.bg2, Rule::NonText);
        table.add(format!("{name} on the rail"), ink, t.bg4, Rule::NonText);
    }
    table.add("failed icon", t.danger, t.bg2, Rule::NonText);
    table.add("queued icon", t.text3, t.bg2, Rule::NonText);
}
