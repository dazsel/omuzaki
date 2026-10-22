//! The Scripts panel (#1097, `scripting.md` section 8): the project's scripts and macros and My scripts, read through
//! `script.list` once a second. A script opens in the Console; a macro opens in the Macros panel. New starts an empty
//! script; Delete asks once, then runs `script.delete` (undoable in the project) or `script.delete_mine`.

use egui::{RichText, ScrollArea, Sense, Ui, WidgetInfo, WidgetType};
use omuzaki_ops_api::Actor;
use omuzaki_script::Place;
use omuzaki_ui_api::panel::{Panel, PanelCtx};
use serde_json::json;

use super::consent::Author;
use super::console::no_scripting;
use super::registration::{CONSOLE_PANEL, MACROS_PANEL};
use super::{Listed, Opened, Scripting, text};
use crate::ds::a11y;
use crate::ds::icons;
use crate::ds::paint::{self, RingPlacement};
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::radius;
use crate::ds::widgets::{Button, Section};
use crate::panels::text::say;
use crate::shell::requests::{ShellRequest, ShellRequests};

/// How often the list is read again, in seconds.
const READ_EVERY: f64 = 1.0;

/// The Scripts panel's state.
#[derive(Debug, Default)]
pub struct Scripts {
    listed: Vec<Listed>,
    read_at: Option<f64>,
    error: Option<String>,
    /// The item whose Delete was pressed once, waiting for the second press.
    deleting: Option<(String, Place)>,
}

/// What a row asked for.
enum Ask {
    Open(Listed),
    Delete(Listed),
    ConfirmDelete(Listed),
    CancelDelete,
}

impl Scripts {
    /// The items as last read.
    pub fn listed(&self) -> &[Listed] {
        &self.listed
    }

    /// Reads the list now.
    pub fn read(&mut self, scripting: &Scripting, actor: &Actor) {
        match scripting.list(actor) {
            Ok(listed) => {
                self.listed = listed;
                self.error = None;
            }
            Err(error) => self.error = Some(error.message),
        }
    }

    /// Draws the panel.
    pub fn ui(&mut self, ui: &mut Ui, scripting: &Scripting, actor: &Actor, requests: Option<&ShellRequests>) {
        let ds = ds(ui.ctx());
        let now = ui.input(|i| i.time);
        if self.read_at.is_none_or(|at| now - at >= READ_EVERY) {
            self.read_at = Some(now);
            self.read(scripting, actor);
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(READ_EVERY));
        let new = say(&ds, text::NEW_SCRIPT, &[]);
        if ui.add(Button::new(&new).icon(icons::PLUS)).clicked() {
            scripting.open(Opened { saved: None, name: String::new(), source: String::new(), author: Author::Me });
            show(requests, CONSOLE_PANEL);
        }
        if let Some(error) = &self.error {
            ui.label(RichText::new(say(&ds, text::LIST_FAILED, &[("reason", error)])).color(ds.theme.danger));
        }
        let mut asks = Vec::new();
        ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
            for (place, title, empty) in
                [(Place::Project, text::IN_PROJECT, text::NONE_IN_PROJECT), (Place::Mine, text::MINE, text::NONE_MINE)]
            {
                let title = say(&ds, title, &[]);
                Section::new(&title).id_salt(("script.scripts", title.as_ref())).default_open(true).show(ui, |ui| {
                    let items: Vec<&Listed> = self.listed.iter().filter(|l| l.place == place).collect();
                    if items.is_empty() {
                        ui.label(RichText::new(say(&ds, empty, &[])).color(ds.theme.text2));
                    }
                    for item in items {
                        let deleting = self.deleting.as_ref().is_some_and(|(id, p)| *id == item.id && *p == item.place);
                        row(ui, &ds, item, deleting, &mut asks);
                    }
                });
            }
        });
        for ask in asks {
            match ask {
                Ask::Open(item) if item.is_macro() => {
                    scripting.choose_macro(Some((item.id.clone(), item.place)));
                    show(requests, MACROS_PANEL);
                }
                Ask::Open(item) => match scripting.open_item(&item, actor) {
                    Ok(()) => show(requests, CONSOLE_PANEL),
                    Err(error) => self.error = Some(error.message),
                },
                Ask::Delete(item) => self.deleting = Some((item.id, item.place)),
                Ask::CancelDelete => self.deleting = None,
                Ask::ConfirmDelete(item) => {
                    self.deleting = None;
                    let op = match item.place {
                        Place::Project => "script.delete",
                        Place::Mine => "script.delete_mine",
                    };
                    if let Err(error) = super::run(scripting.runner().as_ref(), actor, op, json!({ "id": item.id })) {
                        self.error = Some(error.message);
                    }
                    self.read(scripting, actor);
                }
            }
        }
    }
}

/// One item's row: its glyph and name (a click opens it) and Delete, which asks once.
fn row(ui: &mut Ui, ds: &Ds, item: &Listed, deleting: bool, asks: &mut Vec<Ask>) {
    let t = &ds.theme;
    if let Some(problem) = &item.problem {
        let line = say(ds, text::UNREADABLE, &[("id", &item.id), ("problem", problem)]);
        ui.label(RichText::new(line).color(t.text2));
        return;
    }
    ui.horizontal(|ui| {
        let glyph = if item.is_macro() { icons::RECORD } else { icons::FILE_CODE };
        let kind = if item.is_macro() { say(ds, text::KIND_MACRO, &[]) } else { say(ds, text::KIND_SCRIPT, &[]) };
        let width = (ui.available_width() - 36.0).max(80.0);
        let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 26.0), Sense::click());
        let name = say(ds, text::OPEN_ITEM, &[("name", item.title()), ("kind", &kind)]);
        a11y::describe(&response, || WidgetInfo::labeled(WidgetType::Button, true, name.as_ref()), |_| {});
        if response.hovered() {
            ui.painter().rect_filled(rect, radius::CONTROL, t.bg3);
        }
        paint::focus_ring(ui.painter(), t, &response, rect, radius::CONTROL, RingPlacement::Inside);
        let icon_colour = if item.is_macro() { t.danger } else { t.text2 };
        let at = rect.left_center() + egui::vec2(6.0, 0.0);
        ui.painter().text(at, egui::Align2::LEFT_CENTER, glyph, ds.text.icon(14.0, false), icon_colour);
        let label = ui.painter().layout_no_wrap(item.title().to_owned(), ds.text.body.clone(), t.text);
        ui.painter().galley(rect.left_center() + egui::vec2(26.0, -label.size().y / 2.0), label, t.text);
        if response.clicked() {
            asks.push(Ask::Open(item.clone()));
        }
        if deleting {
            let sure = say(ds, text::DELETE_SURE, &[]);
            if ui.add(Button::new(&sure).danger()).clicked() {
                asks.push(Ask::ConfirmDelete(item.clone()));
            }
            let keep = say(ds, text::KEEP, &[]);
            if ui.add(Button::new(&keep).ghost()).clicked() {
                asks.push(Ask::CancelDelete);
            }
        } else {
            let delete = say(ds, text::DELETE_ITEM, &[("name", item.title())]);
            let word = say(ds, text::DELETE_SURE, &[]);
            let button = Button::new(&word).ghost().icon(icons::TRASH).accessible_label(&delete);
            if ui.add(button).clicked() {
                asks.push(Ask::Delete(item.clone()));
            }
        }
    });
}

/// Shows the panel `id` through the shell, when there is one.
fn show(requests: Option<&ShellRequests>, id: &'static str) {
    if let Some(requests) = requests {
        requests.push(ShellRequest::ShowPanel(id));
    }
}

/// The Scripts panel, registered.
#[derive(Debug, Default)]
pub struct ScriptsPanel(pub Scripts);

impl Panel for ScriptsPanel {
    fn ui(&mut self, ui: &mut Ui, ctx: &PanelCtx<'_>) {
        match ctx.view::<Scripting>() {
            Some(scripting) => {
                super::padded(ui, |ui| self.0.ui(ui, scripting, ctx.actor(), ctx.view::<ShellRequests>()));
            }
            None => no_scripting(ui),
        }
    }
}
