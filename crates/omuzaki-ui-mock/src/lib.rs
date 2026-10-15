//! A headless host for `omuzaki-ui-api` panels, and a probe panel and workspace that register themselves.
//!
//! [`MockHost`] draws a panel for one frame the way the shell does (its body, or its registered empty state while
//! it is unavailable) in an egui context with no window, so panel crates test their panels without the app.
//! [`ProbePanel`] and the `mock.probe` workspace give registry tests a registration from a crate of their own.
#![deny(missing_docs)]

use std::borrow::Cow;

use egui::{Context, RawInput, Ui};
use omuzaki_ui_api::layout::Node;
use omuzaki_ui_api::panel::{PaletteEntry, Panel, PanelCtx};
use omuzaki_ui_api::registry::{Group, PanelDef, Region};

omuzaki_core::define_link!();

/// The id of the probe panel and of the probe workspace.
pub const PROBE: &str = "mock.probe";

/// What the shell writes in an unavailable panel that registered no empty state.
pub const NO_EMPTY_STATE: &str = "This panel can't work right now.";

/// A panel that counts the frames and events it saw. Its tab title reports the frame count, so a test that holds it
/// only as `dyn Panel` can still read it.
#[derive(Debug, Default)]
pub struct ProbePanel {
    frames: u32,
    events: usize,
    resets: u32,
}

impl ProbePanel {
    /// The palette entry that resets the counts.
    pub const RESET: PaletteEntry = PaletteEntry::new("mock.probe.reset", "Reset probe");

    /// Frames drawn so far.
    pub fn frames(&self) -> u32 {
        self.frames
    }

    /// Events seen so far, over every frame.
    pub fn events(&self) -> usize {
        self.events
    }

    /// How often [`ProbePanel::RESET`] ran.
    pub fn resets(&self) -> u32 {
        self.resets
    }
}

impl Panel for ProbePanel {
    fn ui(&mut self, ui: &mut Ui, ctx: &PanelCtx<'_>) {
        self.frames += 1;
        self.events += ctx.events().len();
        ui.label(format!("{} frames, {} events", self.frames, self.events));
    }

    fn title(&self) -> Option<Cow<'static, str>> {
        Some(Cow::Owned(format!("Probe ({} frames)", self.frames)))
    }

    fn palette_entries(&self) -> &[PaletteEntry] {
        std::slice::from_ref(&Self::RESET)
    }

    fn run_palette_entry(&mut self, id: &str, _ctx: &PanelCtx<'_>) -> bool {
        if id != Self::RESET.id {
            return false;
        }
        (self.frames, self.events) = (0, 0);
        self.resets += 1;
        true
    }
}

omuzaki_ui_api::register_panel! {
    id: "mock.probe", title: "Probe", icon: "", region: Region::Right, min_size: [288.0, 160.0], order: 900,
    build: |_| Box::new(ProbePanel::default()),
}

omuzaki_ui_api::register_workspace! {
    id: "mock.probe", title: "Probe", icon: "", order: 900, group: Group::Watch,
    default_layout: || Node::pane(PROBE),
}

/// Draws panels headless, one frame at a time, as the shell would.
#[derive(Default)]
pub struct MockHost {
    egui: Context,
}

impl MockHost {
    /// A host with a fresh egui context.
    pub fn new() -> Self {
        Self::default()
    }

    /// Draws one frame of `panel`, registered as `def`, filling the window.
    pub fn frame(&self, panel: &mut dyn Panel, def: &PanelDef, ctx: &PanelCtx<'_>) -> Frame {
        let body = panel.available(ctx);
        let output = self.egui.run_ui(RawInput::default(), |ui| match (body, def.empty) {
            (true, _) => panel.ui(ui, ctx),
            (false, Some(empty)) => {
                ui.label(empty.reason);
                ui.label(empty.fix);
            }
            (false, None) => {
                ui.label(NO_EMPTY_STATE);
            }
        });
        let shapes = output.shapes.len();
        // No GPU here: the texture uploads are dropped on purpose (egui panics on an unhandled delta otherwise).
        output.drop_without_applying_deltas();
        Frame { body, shapes }
    }
}

/// What one [`MockHost::frame`] drew.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Frame {
    /// Whether the panel's body was drawn; `false` means it was unavailable and its empty state was shown.
    pub body: bool,
    /// How many shapes egui produced.
    pub shapes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reset_entry_clears_the_counts_and_other_ids_are_refused() {
        let mut probe = ProbePanel { frames: 3, events: 2, resets: 0 };
        let runner = omuzaki_ops_mock::MockRunner::new(omuzaki_doc_mock::MemDoc::new());
        let actor = omuzaki_ops_api::Actor::System;
        let ctx = PanelCtx::new(&runner, &actor);
        assert!(!probe.run_palette_entry("other", &ctx));
        assert!(probe.run_palette_entry(ProbePanel::RESET.id, &ctx));
        assert_eq!((probe.frames(), probe.events(), probe.resets()), (0, 0, 1));
    }
}
