//! The compare strip (TH-04, `shell.md` section 11 item 3): the reference, the previous step and the current one side
//! by side, at the camera every step is pictured from, refreshed with each step. Under them, how much of the character
//! changed since the previous step and how far it is from the reference, counted pixel by pixel.
//!
//! The current step is the one chosen in the steps list, else the newest. The reference is what the book names: a
//! step's own picture (Use as reference) or an agent's render, such as a character sheet (`theatre.reference`).

use egui::{Align2, Rect, RichText, Ui, vec2};
use omuzaki_theatre::Book;
use omuzaki_ui_api::panel::{Panel, PanelCtx};
use serde_json::json;

use super::pictures::{Picture, Pictures, Source, Sources, changed};
use super::{Theatre, no_book, nobody, problem, text};
use crate::ds::icons;
use crate::ds::theme::{Ds, ds};
use crate::ds::widgets::Button;
use crate::panels::io::Io;
use crate::panels::text::say;
use crate::shell::pictures::HistoryPictures;

/// The largest a picture is drawn, in points.
const LARGEST: f32 = 320.0;
/// Space between the pictures.
const GAP: f32 = 12.0;

/// The compare strip.
#[derive(Debug, Default)]
pub struct CompareStrip {
    pictures: Pictures,
    seen: Option<(u64, Book)>,
    problem: Option<String>,
}

/// One column: its caption and its picture, if any.
struct Column {
    caption: String,
    source: Option<Source>,
}

impl CompareStrip {
    /// Draws the strip.
    pub fn ui(&mut self, ui: &mut Ui, theatre: &Theatre, io: Io<'_>, steps: Option<&HistoryPictures>) {
        let ds = ds(ui.ctx());
        let revision = theatre.book.revision();
        if self.seen.as_ref().is_none_or(|(seen, _)| *seen != revision) {
            self.seen = Some((revision, theatre.book.snapshot()));
        }
        let Some((_, book)) = &self.seen else { return };
        let chosen = theatre.looking().step;
        let at = chosen.and_then(|tx| book.steps.iter().rposition(|s| s.tx == tx)).or(book.steps.len().checked_sub(1));
        let Some(at) = at else { return nobody(ui, &ds) };
        let current = book.steps[at].tx;
        let previous = at.checked_sub(1).map(|p| book.steps[p].tx);
        let reference = book.reference.as_deref().and_then(Source::parse);
        if steps.is_none() {
            ui.label(RichText::new(say(&ds, text::NO_PICTURES, &[])).color(ds.theme.text2));
        }
        let columns = [
            Column { caption: say(&ds, text::REFERENCE, &[]).into_owned(), source: reference },
            Column { caption: say(&ds, text::PREVIOUS, &[]).into_owned(), source: previous.map(Source::Step) },
            Column { caption: say(&ds, text::CURRENT, &[]).into_owned(), source: Some(Source::Step(current)) },
        ];
        let sources = Sources { steps, renders: theatre.renders.as_ref() };
        let loaded: Vec<Option<Picture>> =
            columns.iter().map(|c| c.source.as_ref().and_then(|s| self.pictures.get(ui.ctx(), sources, s))).collect();
        // Room under the pictures for their captions, the two lines and the buttons.
        let below = 7.0 * ui.text_style_height(&egui::TextStyle::Body);
        let side = ((ui.available_width() - 2.0 * GAP) / 3.0).min(ui.available_height() - below).clamp(48.0, LARGEST);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            for (column, picture) in columns.iter().zip(&loaded) {
                tile(ui, &ds, column, picture.as_ref(), side);
            }
        });
        ui.add_space(6.0);
        let share = |a: Option<&Picture>, b: Option<&Picture>| a.zip(b).and_then(|(a, b)| changed(a, b));
        let percent = |share: f32| format!("{:.0}%", share * 100.0);
        if let Some(moved) = share(loaded[1].as_ref(), loaded[2].as_ref()) {
            let line = say(&ds, text::CHANGED_SINCE, &[("share", &percent(moved))]);
            ui.label(RichText::new(line).color(ds.theme.text));
        }
        if let Some(off) = share(loaded[0].as_ref(), loaded[2].as_ref()) {
            let line = say(&ds, text::FROM_REFERENCE, &[("share", &percent(off))]);
            ui.label(RichText::new(line).color(ds.theme.text));
        }
        ui.label(RichText::new(say(&ds, text::MATCHED, &[])).font(ds.text.small.clone()).color(ds.theme.text3));
        ui.horizontal_wrapped(|ui| {
            let use_it = say(&ds, text::USE_AS_REFERENCE, &[]);
            if ui.add(Button::new(&use_it).icon(icons::FLAG)).clicked() {
                let picture = Source::Step(current).reference();
                self.problem =
                    io.run("theatre.reference", json!({ "picture": picture }), None).err().map(|e| e.message);
            }
            if book.reference.is_some() {
                let clear = say(&ds, text::CLEAR_REFERENCE, &[]);
                if ui.add(Button::new(&clear).ghost()).clicked() {
                    self.problem = io.run("theatre.reference", json!({}), None).err().map(|e| e.message);
                }
            }
        });
        problem(ui, &ds, self.problem.as_deref());
    }
}

/// One picture with its caption above it; the empty square says why there is none.
fn tile(ui: &mut Ui, ds: &Ds, column: &Column, picture: Option<&Picture>, side: f32) {
    ui.vertical(|ui| {
        ui.set_width(side);
        ui.label(RichText::new(&column.caption).font(ds.text.small.clone()).color(ds.theme.text2));
        let (rect, response) = ui.allocate_exact_size(vec2(side, side), egui::Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, 4.0, ds.theme.bg2);
        if let Some(picture) = picture {
            #[allow(clippy::cast_precision_loss, reason = "a picture's side, at most 1024")]
            let [w, h] = picture.size.map(|v| v as f32);
            let scale = (side / w.max(1.0)).min(side / h.max(1.0));
            let shown = Rect::from_center_size(rect.center(), vec2(w * scale, h * scale));
            egui::Image::new((picture.texture.id(), shown.size())).corner_radius(4.0).paint_at(ui, shown);
            response.on_hover_text(&column.caption);
        } else {
            let why = if column.source.is_none() { text::NO_REFERENCE } else { text::PICTURE_LOADING };
            let why = say(ds, why, &[]);
            painter.text(rect.center(), Align2::CENTER_CENTER, why, ds.text.small.clone(), ds.theme.text3);
        }
    });
}

/// [`CompareStrip`] as a registered panel.
#[derive(Debug, Default)]
pub struct ComparePanel(CompareStrip);

impl Panel for ComparePanel {
    fn ui(&mut self, ui: &mut Ui, ctx: &PanelCtx<'_>) {
        match Theatre::from_ctx(ctx) {
            Some(theatre) => self.0.ui(ui, theatre, Io::from_ctx(ctx), ctx.view::<HistoryPictures>()),
            None => no_book(ui, &ds(ui.ctx())),
        }
    }
}
