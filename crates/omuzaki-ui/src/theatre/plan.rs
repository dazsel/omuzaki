//! The plan board (TH-03, `shell.md` section 11 item 1): each agent's plan as a checklist. An item shows its status as
//! an icon and a word (planned, running, needs review, done, gate failed) and how many steps it holds; choosing it
//! filters the Steps list to them, and choosing it again shows every step.

use egui::{RichText, ScrollArea, Sense, Ui, WidgetInfo, WidgetType};
use omuzaki_theatre::{Book, PlanItem};
use omuzaki_ui_api::panel::{Panel, PanelCtx};

use super::{Theatre, no_book, status, text};
use crate::ds::a11y;
use crate::ds::icons;
use crate::ds::paint::{self, RingPlacement};
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::radius;
use crate::ds::widgets::EmptyState;
use crate::panels::text::say;
use crate::shell::history::who;

/// The plan board.
#[derive(Debug, Default)]
pub struct PlanBoard {
    /// The book's revision the board was drawn from, and the book then.
    seen: Option<(u64, Book)>,
}

impl PlanBoard {
    /// Draws the board for `theatre`.
    pub fn ui(&mut self, ui: &mut Ui, theatre: &Theatre) {
        let ds = ds(ui.ctx());
        let revision = theatre.book.revision();
        if self.seen.as_ref().is_none_or(|(seen, _)| *seen != revision) {
            self.seen = Some((revision, theatre.book.snapshot()));
        }
        let Some((_, book)) = &self.seen else { return };
        if book.plans.is_empty() {
            let (title, line) = (say(&ds, text::NO_PLAN, &[]), say(&ds, text::NO_PLAN_HOW, &[]));
            EmptyState::new(icons::LIST_CHECKS, &title, &line).show(ui);
            return;
        }
        let looking = theatre.looking();
        ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
            for plan in &book.plans {
                let agent = who(ui.ctx(), &plan.owner);
                let heading = say(&ds, text::PLAN_OF, &[("agent", &agent)]);
                ui.label(RichText::new(heading).font(ds.text.strong.clone()).color(ds.theme.text));
                ui.add_space(4.0);
                for item in &plan.items {
                    let chosen = looking.item.as_ref().is_some_and(|(o, id)| *o == plan.owner && *id == item.id);
                    if item_row(ui, &ds, item, chosen) {
                        theatre.look(|l| {
                            l.item = if chosen { None } else { Some((plan.owner.clone(), item.id.clone())) };
                        });
                    }
                }
                ui.add_space(12.0);
            }
        });
    }
}

/// One item's row; true when it was chosen this frame.
fn item_row(ui: &mut Ui, ds: &Ds, item: &PlanItem, chosen: bool) -> bool {
    let t = &ds.theme;
    let (tone, icon, word) = status(ds, item.status);
    let steps = match item.steps.len() {
        0 => None,
        1 => Some(say(ds, text::ITEM_ONE_STEP, &[]).into_owned()),
        n => Some(say(ds, text::ITEM_STEPS, &[("count", &n.to_string())]).into_owned()),
    };
    let width = ui.available_width();
    let wrap = width - 40.0;
    let galley = ui.painter().layout(item.text.clone(), ds.text.body.clone(), t.text, wrap.max(40.0));
    let detail = match &steps {
        Some(steps) => format!("{word} \u{b7} {steps}"),
        None => word.clone(),
    };
    let height = galley.size().y + 22.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), Sense::click());
    let name = say(ds, text::ITEM_NAME, &[("item", &item.text), ("status", &word)]);
    a11y::describe(&response, || WidgetInfo::selected(WidgetType::Button, true, chosen, name.as_ref()), |_| {});
    let fill = if chosen {
        t.bg3
    } else if response.hovered() {
        t.bg2
    } else {
        egui::Color32::TRANSPARENT
    };
    let painter = ui.painter();
    painter.rect_filled(rect, radius::CONTROL, fill);
    paint::focus_ring(painter, t, &response, rect, radius::CONTROL, RingPlacement::Inside);
    let colour = t.status(tone);
    let glyph_at = rect.left_top() + egui::vec2(16.0, 4.0 + galley.size().y * 0.5);
    painter.text(glyph_at, egui::Align2::CENTER_CENTER, icon, ds.text.icon(16.0, false), colour);
    let text_at = rect.left_top() + egui::vec2(32.0, 4.0);
    let line_at = text_at + egui::vec2(0.0, galley.size().y + 2.0);
    painter.galley(text_at, galley, t.text);
    painter.text(line_at, egui::Align2::LEFT_TOP, detail, ds.text.small.clone(), t.text2);
    response.clicked()
}

/// [`PlanBoard`] as a registered panel.
#[derive(Debug, Default)]
pub struct PlanPanel(PlanBoard);

impl Panel for PlanPanel {
    fn ui(&mut self, ui: &mut Ui, ctx: &PanelCtx<'_>) {
        match Theatre::from_ctx(ctx) {
            Some(theatre) => self.0.ui(ui, theatre),
            None => no_book(ui, &ds(ui.ctx())),
        }
    }
}
