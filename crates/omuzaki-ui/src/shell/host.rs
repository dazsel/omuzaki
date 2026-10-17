//! The panels the shell has built, and how one is drawn (`shell.md` sections 3 and 4).
//!
//! The registry holds a build function per panel; the shell builds a panel the first time it is shown and keeps it for
//! the session, so a panel's own state (a scroll position, a half-typed field) survives a tab switch, a fold or a
//! workspace change. [`Bodies::draw`] draws one panel's body, or its registered empty state while it cannot work.

use std::borrow::Cow;
use std::collections::HashMap;

use egui::{ScrollArea, Ui, WidgetInfo, WidgetType, accesskit};
use omuzaki_ui_api::panel::{Panel, PanelCtx};
use omuzaki_ui_api::registry::PanelDef;

use super::text::{Word, say, title};
use crate::ds::a11y;
use crate::ds::widgets::EmptyState;

/// The panel instances built so far, by registered id.
#[derive(Default)]
pub struct Bodies {
    built: HashMap<&'static str, Box<dyn Panel>>,
}

impl Bodies {
    /// The tab title of `def`: the panel's own when it has one (an inspector naming its selection), else the
    /// registered English one. Not yet translated: the caller passes it through [`title`].
    pub fn title(&self, def: &PanelDef) -> Cow<'static, str> {
        self.built.get(def.id).and_then(|panel| panel.title()).unwrap_or(Cow::Borrowed(def.title))
    }

    /// Draws the body of `def` into `ui`, building the panel first if this is its first frame. While the panel says it
    /// cannot work, the registered empty state is drawn instead: a headline saying why and a line saying what to do.
    pub fn draw(&mut self, ui: &mut Ui, def: &'static PanelDef, ctx: &PanelCtx<'_>) {
        let panel = self.built.entry(def.id).or_insert_with(|| (def.build)(ctx));
        if !panel.available(ctx) {
            unavailable(ui, def);
            return;
        }
        let Some(label) = panel.accessibility_label() else {
            panel.ui(ui, ctx);
            return;
        };
        // The panel says more to a screen reader than its title does: name the region its body is.
        let response = ui.scope(|ui| panel.ui(ui, ctx)).response;
        a11y::describe(&response, || WidgetInfo::labeled(WidgetType::Other, true, label.as_ref()), |node| {
            node.set_role(accesskit::Role::Region);
        });
    }

    /// How many panels have been built.
    #[cfg(test)]
    pub fn built(&self) -> usize {
        self.built.len()
    }
}

/// The empty state of a panel that cannot work: the registered words, or a generic notice when it registered none.
fn unavailable(ui: &mut Ui, def: &PanelDef) {
    let ctx = ui.ctx().clone();
    let name = title(&ctx, def.title);
    let (headline, line) = match def.empty {
        Some(empty) => (title(&ctx, empty.reason).into_owned(), title(&ctx, empty.fix).into_owned()),
        None => (say(&ctx, Word::PanelUnavailable, &[("panel", &name)]), say(&ctx, Word::PanelUnavailableLine, &[])),
    };
    ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
        EmptyState::new(def.icon, &headline, &line).show(ui);
    });
}
