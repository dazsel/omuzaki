//! Tooltips (`design-system.md` section 7.0): name, shortcut, then the op name in mono; 500 ms hover delay,
//! immediate on keyboard focus, at most 320 pt wide, Esc dismisses.

use egui::{Key, Popup, Response, RichText, Sense, Tooltip, Ui, vec2};

use super::keycap::{CapStyle, KeyCaps, Shortcut};
use crate::ds::focus;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::size;

/// What a tooltip says. An icon-only button must have one: it is also the control's accessible name.
#[derive(Clone, Copy, Debug)]
#[must_use = "a tip does nothing until it is attached to a response"]
pub struct Tip<'a> {
    /// What the control does, as a verb phrase ("Frame selection").
    pub name: &'a str,
    /// Its keyboard shortcut.
    pub shortcut: Option<Shortcut>,
    /// The op it runs, in mono ("viewport.frame"): the vocabulary agents use too (P8).
    pub op: Option<&'a str>,
    /// A second line of detail: why a control is disabled, or who holds a lock.
    pub detail: Option<&'a str>,
}

impl<'a> Tip<'a> {
    /// A tooltip with just a name.
    pub const fn new(name: &'a str) -> Self {
        Self { name, shortcut: None, op: None, detail: None }
    }

    /// Adds the shortcut.
    pub fn shortcut(mut self, shortcut: impl Into<Shortcut>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// Adds the op name.
    pub const fn op(mut self, op: &'a str) -> Self {
        self.op = Some(op);
        self
    }

    /// Adds a line of detail, such as why the control is disabled.
    pub const fn detail(mut self, detail: &'a str) -> Self {
        self.detail = Some(detail);
        self
    }

    fn show(&self, ui: &mut Ui, ds: &Ds) {
        let t = &ds.theme;
        ui.set_max_width(size::TOAST_WIDTH);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            ui.label(RichText::new(self.name).font(ds.text.strong.clone()).color(t.text));
            if let Some(shortcut) = self.shortcut {
                let caps = KeyCaps::new(shortcut);
                let (rect, _) = ui.allocate_exact_size(vec2(caps.width(ui.ctx()), 18.0), Sense::hover());
                let style = CapStyle { fill: t.bg3, stroke: t.line, text: t.text2 };
                caps.paint(ui.painter(), ds, rect.left_center(), style);
            }
        });
        if let Some(op) = self.op {
            ui.label(RichText::new(op).font(ds.text.mono.clone()).color(t.text3));
        }
        if let Some(detail) = self.detail {
            ui.label(RichText::new(detail).font(ds.text.small.clone()).color(t.text2));
        }
    }
}

/// Attaches a [`Tip`] to a response.
pub trait TipExt: Sized {
    /// Shows `tip` on hover after the delay, and at once on keyboard focus until Esc.
    #[must_use]
    fn tip(self, tip: Tip<'_>) -> Self;
}

impl TipExt for Response {
    fn tip(self, tip: Tip<'_>) -> Self {
        let ds = ds(&self.ctx);
        let dismissed_id = self.id.with("tip_dismissed");
        if self.has_focus() && focus::visible(&self.ctx) {
            if self.ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.ctx.data_mut(|d| d.insert_temp(dismissed_id, true));
            }
            if !self.ctx.data(|d| d.get_temp::<bool>(dismissed_id)).unwrap_or(false) {
                let popup = egui::Popup::from_response(&self).kind(egui::PopupKind::Tooltip);
                still(popup).show(|ui| tip.show(ui, &ds));
            }
            return self;
        }
        // Focus moved on: the next focus gets its tooltip again.
        if self.ctx.data(|d| d.get_temp::<bool>(dismissed_id)).is_some() {
            self.ctx.data_mut(|d| d.remove::<bool>(dismissed_id));
        }
        let mut tooltip = if self.enabled() { Tooltip::for_enabled(&self) } else { Tooltip::for_disabled(&self) };
        tooltip.popup = still(tooltip.popup);
        tooltip.show(|ui| tip.show(ui, &ds));
        self
    }
}

/// A tooltip that takes no input. egui's popups sense clicks by default, which makes every tooltip a clickable node
/// with no name of its own: a screen reader would offer it as a control.
fn still(popup: Popup<'_>) -> Popup<'_> {
    popup.interactable(false).sense(Sense::hover())
}
