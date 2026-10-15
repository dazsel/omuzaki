//! A headless host for `omuzaki-ui-api` panels, a conformance check, and a probe panel and workspace that register
//! themselves.
//!
//! [`MockHost`] collects the registry and draws any registered panel for one frame the way the shell does (its body,
//! or its registered empty state while it is unavailable) in an egui context with no window or GPU, so panel crates
//! test their panels without the app. [`MockHost::check`] is the conformance check every panel crate runs on its
//! panels. [`ProbePanel`] and the `mock.probe` workspace give registry tests a registration from a crate of their own.
#![deny(missing_docs)]

use std::borrow::Cow;
use std::panic::{self, AssertUnwindSafe};

use egui::{Context, Pos2, RawInput, Rect, Ui, vec2};
use omuzaki_ui_api::layout::Node;
use omuzaki_ui_api::panel::{PaletteEntry, Panel, PanelCtx};
use omuzaki_ui_api::registry::{DefKind, Group, PanelDef, Region, Registry};

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

/// Draws registered panels headless, one frame at a time, as the shell would.
pub struct MockHost {
    egui: Context,
    registry: Registry,
}

impl Default for MockHost {
    fn default() -> Self {
        Self::new()
    }
}

impl MockHost {
    /// A host over every registration linked into this binary, with a fresh egui context.
    pub fn new() -> Self {
        Self { egui: accessible_context(), registry: Registry::collect() }
    }

    /// What the host found registered.
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Builds the panel registered as `id`, as the shell does when it first shows it.
    pub fn open(&self, id: &str, ctx: &PanelCtx<'_>) -> Option<(&'static PanelDef, Box<dyn Panel>)> {
        let def = self.registry.panel(id)?;
        Some((def, (def.build)(ctx)))
    }

    /// Draws one frame of `panel`, registered as `def`, in a window of `size` points. Like the shell, it keeps egui's
    /// default pass count, so `Panel::ui` may run more than once when the panel asks egui to discard a pass.
    pub fn frame(&self, panel: &mut dyn Panel, def: &PanelDef, ctx: &PanelCtx<'_>, size: [f32; 2]) -> Frame {
        draw(&self.egui, panel, def, ctx, size)
    }

    /// The conformance check for the panel registered as `id`; `ctx` should be over an empty document. Returns one
    /// line per problem, so `assert!(problems.is_empty(), "{problems:#?}")` names them all:
    ///
    /// - the id is registered once and is stable: non-empty, lower-case ASCII letters, digits, `.`, `_` or `-`;
    /// - it draws with no document: its body, or (when unavailable) a registered empty state;
    /// - it has a non-empty title (the panel's override, else the registered one) and, when it overrides the
    ///   screen-reader label, a non-empty label; its frame gives AccessKit something to announce;
    /// - nothing panics at its minimum size, at half of it, or at 1 by 1 points (each size on a freshly built panel,
    ///   as the shell builds one at whatever size it first shows it).
    pub fn check(&self, id: &str, ctx: &PanelCtx<'_>) -> Vec<String> {
        let mut problems = Vec::new();
        let valid = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-');
        if id.is_empty() || !id.chars().all(valid) {
            problems.push(format!("id {id:?} is not lower-case ASCII letters, digits, '.', '_' or '-'"));
        }
        problems.extend(
            self.registry
                .duplicates()
                .iter()
                .filter(|d| d.kind == DefKind::Panel && d.id == id)
                .map(ToString::to_string),
        );
        let Some((def, mut panel)) = self.open(id, ctx) else {
            problems.push(format!("no panel is registered as {id:?}"));
            return problems;
        };
        // The title the tab shows, and (when the panel overrides it) the label a screen reader gets, each on its own.
        let title = panel.title().unwrap_or(Cow::Borrowed(def.title));
        if title.trim().is_empty() {
            problems.push(format!("{id}: the title is empty"));
        }
        if panel.accessibility_label().is_some_and(|label| label.trim().is_empty()) {
            problems.push(format!("{id}: the screen-reader label is empty"));
        }
        let [w, h] = def.min_size;
        // A fresh context, so a panic can't leave state behind for the next draw.
        let first = panic::catch_unwind(AssertUnwindSafe(|| {
            draw(&accessible_context(), panel.as_mut(), def, ctx, def.min_size)
        }));
        match first {
            Ok(frame) => {
                if !frame.body && def.empty.is_none() {
                    problems.push(format!("{id}: unavailable with no document, and registers no empty state"));
                }
                if frame.labels.is_empty() {
                    problems.push(format!("{id}: its frame gives AccessKit nothing to announce"));
                }
            }
            Err(_) => problems.push(format!("{id}: panics at {w} by {h} points")),
        }
        for size in [[w / 2.0, h / 2.0], [1.0, 1.0]] {
            // A fresh context and a freshly built panel each time: the shell builds a panel at whatever size it first
            // shows it, so state set by an earlier draw must not hide a panic on this size's first draw.
            let egui = accessible_context();
            let drawn =
                panic::catch_unwind(AssertUnwindSafe(|| draw(&egui, (def.build)(ctx).as_mut(), def, ctx, size)));
            if drawn.is_err() {
                problems.push(format!("{id}: panics at {w} by {h} points", w = size[0], h = size[1]));
            }
        }
        problems
    }
}

/// What one [`MockHost::frame`] drew.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    /// Whether the panel's body was drawn; `false` means it was unavailable and its empty state was shown.
    pub body: bool,
    /// How many shapes egui produced.
    pub shapes: usize,
    /// What a screen reader can announce from the frame, sorted, each text once: each AccessKit node's label, or its value when it
    /// has no label (egui puts a text label's words in the value).
    pub labels: Vec<String>,
}

fn accessible_context() -> Context {
    let egui = Context::default();
    egui.enable_accesskit();
    egui
}

fn draw(egui: &Context, panel: &mut dyn Panel, def: &PanelDef, ctx: &PanelCtx<'_>, size: [f32; 2]) -> Frame {
    let input =
        RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(size[0], size[1]))), ..RawInput::default() };
    let body = panel.available(ctx);
    let output = egui.run_ui(input, |ui| match (body, def.empty) {
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
    let mut labels: Vec<String> = output
        .platform_output
        .accesskit_update
        .iter()
        .flat_map(|update| {
            update.nodes.iter().filter_map(|(_, node)| node.label().or_else(|| node.value()).map(str::to_owned))
        })
        .filter(|label| !label.trim().is_empty())
        .collect();
    labels.sort_unstable();
    // egui gives a label's words to its node and to the node of its text run.
    labels.dedup();
    // No GPU here: the texture uploads are dropped on purpose (egui panics on an unhandled delta otherwise).
    output.drop_without_applying_deltas();
    Frame { body, shapes, labels }
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
