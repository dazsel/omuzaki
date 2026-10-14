//! Foundations: the palette with its contrast ratios.

use egui::{Align2, Color32, Sense, Stroke, StrokeKind, Ui, vec2};

use crate::ds::contrast::{Rule, contrast_ratio, threshold};
use crate::ds::gallery::Group;
use crate::ds::gallery::layout::{caption, specimen};
use crate::ds::gallery::text::translate;
use crate::ds::paint;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::radius;
use crate::register_story;

#[derive(Default)]
struct State;

register_story! {
    id: "palette", title: "Palette", group: Group::Foundations, order: 10, state: State,
    show: |ui, _| show(ui),
}

/// A swatch with its token name and its contrast against the panel (`bg1`).
///
/// The labels use whichever of the text colour and the window colour reads on the swatch. On a mid-tone swatch neither
/// reaches the ratio the labels themselves are held to (4.5:1, 7:1 in high contrast), so those labels sit on a small
/// plate of the window colour instead of straight on the swatch.
fn swatch(ui: &mut Ui, ds: &Ds, name: &str, colour: Color32, against: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(64.0, 52.0), Sense::hover());
    let t = &ds.theme;
    let painter = ui.painter();
    painter.rect_filled(rect, radius::CONTROL, colour);
    painter.rect_stroke(rect, radius::CONTROL, Stroke::new(1.0, t.line), StrokeKind::Inside);
    let needed = threshold(t, Rule::Text);
    let direct = [t.text, t.bg0].into_iter().find(|ink| contrast_ratio(*ink, colour) >= needed);
    let label = |anchor: Align2, pos: egui::Pos2, text: &str, font: &egui::FontId| {
        let (ink, plate) = direct.map_or((t.text, Some(t.bg0)), |ink| (ink, None));
        if let Some(plate) = plate {
            let size = crate::ds::text::galley(painter.ctx(), text, font, f32::INFINITY).size();
            painter.rect_filled(anchor.anchor_size(pos, size).expand2(vec2(3.0, 1.0)), 3, plate);
        }
        paint::text(painter, pos, anchor, text, font, ink);
    };
    let ratio = format!("{:.1}", contrast_ratio(colour, against));
    label(Align2::LEFT_BOTTOM, rect.left_bottom() + vec2(6.0, -6.0), &translate(ds, name), &ds.text.micro);
    label(Align2::LEFT_TOP, rect.left_top() + vec2(6.0, 5.0), &ratio, &egui::FontId::monospace(10.5));
}

fn show(ui: &mut Ui) {
    let ds = ds(ui.ctx());
    let t = ds.theme;
    let ds = &*ds;
    specimen(ui, ds, "Surfaces", |ui| {
        for (name, c) in [("bg0", t.bg0), ("bg1", t.bg1), ("bg2", t.bg2), ("bg3", t.bg3), ("bg4", t.bg4)] {
            swatch(ui, ds, name, c, t.bg1);
        }
    });
    specimen(ui, ds, "Lines", |ui| {
        for (name, c) in [("line", t.line), ("border", t.border_control)] {
            swatch(ui, ds, name, c, t.bg1);
        }
    });
    specimen(ui, ds, "Text", |ui| {
        for (name, c) in [("text", t.text), ("text2", t.text2), ("text3", t.text3), ("disabled", t.text_disabled)] {
            swatch(ui, ds, name, c, t.bg1);
        }
    });
    specimen(ui, ds, "You", |ui| {
        for (name, c) in
            [("accent", t.accent), ("hover", t.accent_hover), ("press", t.accent_press), ("select", t.selection)]
        {
            swatch(ui, ds, name, c, t.bg1);
        }
    });
    specimen(ui, ds, "Agents", |ui| {
        for (name, c) in [("agent", t.agent), ("glow", t.agent_glow)] {
            swatch(ui, ds, name, c, t.bg1);
        }
    });
    specimen(ui, ds, "Status", |ui| {
        for (name, c) in [("ok", t.ok), ("warn", t.warn), ("danger", t.danger)] {
            swatch(ui, ds, name, c, t.bg1);
        }
    });
    caption(
        ui,
        ds,
        "Numbers are WCAG contrast against bg1. Violet means an agent; gold means you; status always has an icon and a word.",
    );
}
