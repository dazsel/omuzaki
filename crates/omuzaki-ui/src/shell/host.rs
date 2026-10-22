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
    /// The panels drawn in the egui frame `drawn_in`, so overlays can tell what is on screen.
    drawn: Vec<&'static str>,
    drawn_in: u64,
}

impl Bodies {
    /// Whether the panel registered as `id` was drawn in this egui frame (it is on screen, not behind a tab).
    pub fn on_screen(&self, ctx: &egui::Context, id: &str) -> bool {
        self.drawn_in == ctx.cumulative_frame_nr() && self.drawn.contains(&id)
    }

    /// Whether the panel registered as `id` was on screen in the frame before the docks were drawn: what the top bar,
    /// drawn first, asks.
    pub fn on_screen_before(&self, ctx: &egui::Context, id: &str) -> bool {
        self.drawn_in.saturating_add(1) >= ctx.cumulative_frame_nr() && self.drawn.contains(&id)
    }

    /// The tab title of `def`: the panel's own when it has one (an inspector naming its selection), else the
    /// registered English one. Not yet translated: the caller passes it through [`title`].
    pub fn title(&self, def: &PanelDef) -> Cow<'static, str> {
        self.built.get(def.id).and_then(|panel| panel.title()).unwrap_or(Cow::Borrowed(def.title))
    }

    /// Draws the body of `def` into `ui`, building the panel first if this is its first frame. While the panel says it
    /// cannot work, the registered empty state is drawn instead: a headline saying why and a line saying what to do.
    ///
    /// Either way the body is one AccessKit region named by [`Panel::accessibility_label`] or, when the panel gives
    /// none, by its title (the API's promise). A maximised panel has no tab to name it, and an unavailable one still
    /// answers to its explicit label, so the name belongs to the body and not to the tab.
    pub fn draw(&mut self, ui: &mut Ui, def: &'static PanelDef, ctx: &PanelCtx<'_>) {
        let egui_ctx = ui.ctx().clone();
        let frame = egui_ctx.cumulative_frame_nr();
        if self.drawn_in != frame {
            self.drawn.clear();
            self.drawn_in = frame;
        }
        if !self.drawn.contains(&def.id) {
            self.drawn.push(def.id);
        }
        let panel = self.built.entry(def.id).or_insert_with(|| (def.build)(ctx));
        let label = match panel.accessibility_label() {
            Some(label) => label,
            None => title(&egui_ctx, &panel.title().unwrap_or(Cow::Borrowed(def.title))).into_owned().into(),
        };
        let response = ui
            .scope(|ui| {
                if panel.available(ctx) {
                    panel.ui(ui, ctx);
                } else {
                    unavailable(ui, def);
                }
            })
            .response;
        a11y::describe(
            &response,
            || WidgetInfo::labeled(WidgetType::Other, true, label.as_ref()),
            |node| {
                node.set_role(accesskit::Role::Region);
            },
        );
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
