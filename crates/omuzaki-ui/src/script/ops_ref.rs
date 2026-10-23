//! The Ops reference panel (#1097, `scripting.md` section 8): every op a script can call, searched by name or summary,
//! by area, with its scope and summary. Insert puts a call to it at the console's cursor, with its required fields
//! filled with an empty value of their kind.

use egui::{RichText, ScrollArea, Ui};
use omuzaki_ui_api::panel::{Panel, PanelCtx};

use super::console::{call_template, no_scripting};
use super::registration::CONSOLE_PANEL;
use super::{Scripting, text};
use crate::ds::icons;
use crate::ds::theme::ds;
use crate::ds::tokens::{Status as Tone, space};
use crate::ds::widgets::{Button, StatusChip, TextField};
use crate::panels::text::say;
use crate::shell::requests::{ShellRequest, ShellRequests};

/// The Ops reference's state.
#[derive(Debug, Default)]
pub struct OpsReference {
    query: String,
}

impl OpsReference {
    /// Draws the panel.
    pub fn ui(&mut self, ui: &mut Ui, scripting: &Scripting, requests: Option<&ShellRequests>) {
        let ds = ds(ui.ctx());
        let search = say(&ds, text::SEARCH_OPS, &[]);
        ui.add(TextField::new(&search, &mut self.query).search().hint(&search).id_salt("script.ops.search"));
        ui.add_space(space::S4);
        let query = self.query.trim().to_lowercase();
        let callables = scripting.callables();
        let found: Vec<_> = callables
            .iter()
            .filter(|op| {
                query.is_empty() || op.info.name.contains(&query) || op.info.summary.to_lowercase().contains(&query)
            })
            .collect();
        if found.is_empty() {
            ui.label(RichText::new(say(&ds, text::NO_OPS_FOUND, &[])).color(ds.theme.text2));
            return;
        }
        let mut insert = None;
        ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
            let mut area = "";
            for op in found {
                if op.area != area {
                    area = &op.area;
                    ui.add_space(space::S4);
                    ui.label(RichText::new(area).font(ds.text.strong.clone()).color(ds.theme.text));
                }
                ui.horizontal(|ui| {
                    let call = format!("{}::{}", op.area, op.verb);
                    ui.label(RichText::new(call).font(ds.text.mono.clone()).color(ds.theme.text));
                    ui.add(StatusChip::new(Tone::Info, &op.info.scope.to_string()));
                    let label = say(&ds, text::INSERT_CALL, &[("op", op.info.name)]);
                    let insert_word = say(&ds, text::INSERT, &[]);
                    let button = Button::new(&insert_word).ghost().icon(icons::PLUS).accessible_label(&label);
                    if ui.add(button).clicked() {
                        insert = Some(call_template(op));
                    }
                });
                ui.label(RichText::new(op.info.summary).font(ds.text.small.clone()).color(ds.theme.text2));
            }
        });
        if let Some(call) = insert {
            scripting.insert(call);
            if let Some(requests) = requests {
                requests.push(ShellRequest::ShowPanel(CONSOLE_PANEL));
            }
        }
    }
}

/// The Ops reference, registered.
#[derive(Debug, Default)]
pub struct OpsPanel(pub OpsReference);

impl Panel for OpsPanel {
    fn ui(&mut self, ui: &mut Ui, ctx: &PanelCtx<'_>) {
        match ctx.view::<Scripting>() {
            Some(scripting) => super::padded(ui, |ui| self.0.ui(ui, scripting, ctx.view::<ShellRequests>())),
            None => no_scripting(ui),
        }
    }
}
