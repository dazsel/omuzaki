//! Controls: `CurveEditor`, with a preset, a live input, a signed range and the keyboard path.

use egui::Ui;

use crate::ds::gallery::Group;
use crate::ds::gallery::layout::caption;
use crate::ds::gallery::text::tr;
use crate::ds::theme::ds;
use crate::ds::widgets::{Curve, CurveCause, CurveEditor, CurveEvent, CurvePoint, Preset};
use crate::register_story;

struct State {
    response: Curve,
    signed: Curve,
    locked: Curve,
    last: Option<CurveCause>,
}

impl Default for State {
    fn default() -> Self {
        let response = Curve::unit(Preset::SCurve);
        let signed = Curve::new(
            -1.0..=1.0,
            -1.0..=1.0,
            vec![
                CurvePoint::new(-1.0, -1.0),
                CurvePoint::new(-0.2, 0.0),
                CurvePoint::new(0.2, 0.0),
                CurvePoint::new(1.0, 1.0),
            ],
        )
        .unwrap_or_else(|_| Curve::linear());
        Self { response, signed, locked: Curve::unit(Preset::EaseOut), last: None }
    }
}

register_story! {
    id: "curve_editor", title: "Curve editor", group: Group::Controls, order: 90, state: State,
    show: show,
}

fn show(ui: &mut Ui, s: &mut State) {
    let ds = ds(ui.ctx());
    let ds = &*ds;
    let out = CurveEditor::new(tr!(ds, "Jaw response"), &mut s.response).live_input(Some(0.42)).id_salt("jaw").show(ui);
    if let Some(CurveEvent::Commit { cause, .. }) = out.event {
        s.last = Some(cause);
    }
    ui.add_space(12.0);
    CurveEditor::new(tr!(ds, "Head tilt response"), &mut s.signed).plot_height(120.0).id_salt("tilt").show(ui);
    ui.add_space(12.0);
    CurveEditor::new(tr!(ds, "Locked response"), &mut s.locked)
        .plot_height(90.0)
        .disabled(tr!(ds, "Held by Designer"))
        .id_salt("locked")
        .show(ui);
    ui.add_space(8.0);
    let note = s.last.map_or("Drag a point, double-click to add one, drag one off to remove it. Tab picks a point; arrows nudge it; Enter types its numbers.".to_owned(), |c| format!("Committed: {c:?}"));
    caption(ui, ds, &note);
}
