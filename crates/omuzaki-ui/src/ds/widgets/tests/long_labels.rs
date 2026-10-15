//! Long labels (German, or the pseudo-locale, which is 40% longer than English) must not push a control out of its
//! panel: they end in an ellipsis and the tooltip says the whole thing.

use super::*;
use std::sync::Arc;

use crate::ds::audit::painted;
use crate::ds::strings::{English, Pseudo};
use crate::ds::theme::set_strings;

const GERMAN: &str = "Donaudampfschifffahrtsgesellschaftskapitän bestätigen und fortfahren";
/// An inspector column is 288 pt with 8 pt of padding on each side.
const PANEL: f32 = 288.0;

fn panel(add: impl FnMut(&mut egui::Ui) + 'static) -> egui_kittest::Harness<'static> {
    crate::ds::testing::harness(Settings::default(), vec2(PANEL, 400.0), add)
}

fn right_edges(h: &egui_kittest::Harness<'_>) -> Vec<(Role, f32)> {
    h.root()
        .children_recursive()
        .filter(|n| n.accesskit_node().bounding_box().is_some())
        .filter(|n| {
            matches!(
                n.accesskit_node().role(),
                Role::Button | Role::Switch | Role::CheckBox | Role::RadioGroup | Role::RadioButton
            )
        })
        .map(|n| (n.accesskit_node().role(), n.rect().right()))
        .collect()
}

#[test]
fn a_long_label_does_not_push_a_button_a_toggle_or_a_segmented_control_out_of_the_panel() {
    let h = panel(|ui| {
        ui.add(Button::new(GERMAN));
        ui.add(Button::new(GERMAN).icon(crate::ds::icons::PLUS).primary().shortcut("Ctrl Enter"));
        ui.add(Toggle::switch(GERMAN, &mut true));
        ui.add(Toggle::checkbox(GERMAN, &mut false));
        let mut v = 0u8;
        ui.add(Segmented::new("Mode", &mut v, &[Segment::new(0u8, GERMAN), Segment::new(1u8, "Zwei")]));
    });
    let edges = right_edges(&h);
    assert!(edges.len() >= 7, "found {edges:?}");
    for (role, right) in &edges {
        assert!(
            *right <= PANEL - 8.0 + 1.0,
            "{role:?} reaches {right}, past the panel's 280 pt content edge: {edges:?}"
        );
    }
}

#[test]
fn a_wrapping_row_still_wraps_a_button_instead_of_squeezing_it() {
    let h = panel(|ui| {
        ui.horizontal_wrapped(|ui| {
            for label in ["Approve", "Reject", "Details", "Delete 3 parts"] {
                ui.add(Button::new(label));
            }
        });
    });
    for label in ["Approve", "Reject", "Details", "Delete 3 parts"] {
        let node = h.get_by_role_and_label(Role::Button, label);
        let natural = crate::ds::paint::text_width(&h.ctx, label, &crate::ds::theme::ds(&h.ctx).text.strong) + 24.0;
        assert!((node.rect().width() - natural).abs() < 1.0, "{label} was squeezed to {}", node.rect().width());
    }
}

#[test]
fn the_tooltip_of_a_cut_label_says_it_all() {
    let mut h = panel(|ui| {
        ui.add(Button::new(GERMAN));
    });
    let painted_now = |h: &egui_kittest::Harness<'_>| painted(h.output());
    let before = painted_now(&h);
    assert!(before.iter().any(|p| p.text == GERMAN), "the label is painted");
    assert!(before.iter().filter(|p| p.text == GERMAN).all(|p| p.elided), "the button label is cut, and says so");
    h.get_by_role_and_label(Role::Button, GERMAN).hover();
    h.run_steps(8);
    assert!(
        painted_now(&h).iter().any(|p| p.text == GERMAN && !p.elided),
        "hovering shows the whole label in a tooltip"
    );
}

#[test]
fn a_label_that_fits_is_not_cut_and_has_no_tooltip() {
    let mut h = panel(|ui| {
        ui.add(Button::new("Approve"));
        ui.add(Toggle::switch("Outlines", &mut true));
    });
    h.get_by_role_and_label(Role::Button, "Approve").hover();
    h.run_steps(8);
    let shown = painted(h.output());
    assert!(shown.iter().all(|p| !p.elided), "nothing is cut");
    assert_eq!(shown.iter().filter(|p| p.text == "Approve").count(), 1, "and no tooltip repeats it");
}

#[test]
fn an_icon_only_segment_is_completed_by_its_tooltip() {
    let mut h = panel(|ui| {
        let mut v = 0u8;
        ui.add(Segmented::new(
            "Camera view",
            &mut v,
            &[Segment::new(0u8, "Front").icon(crate::ds::icons::USER).icon_only(), Segment::new(1u8, "Side")],
        ));
    });
    h.get_by_role_and_label(Role::RadioButton, "Front").hover();
    h.run_steps(8);
    assert!(painted(h.output()).iter().any(|p| p.text == "Front"), "the label appears in a tooltip");
}

/// A name of 70 characters, the length the chips are checked at: a German compound with an umlaut and an exclamation.
const NAME70: &str = "Donaudampfschifffahrtsgesellschaftskapitän bestätigen und fortfahren!!";

/// Every chip in a 288 pt panel, each with [`NAME70`] as its name. The gate's value and limit are short numbers.
fn chips() -> egui_kittest::Harness<'static> {
    let mut h = panel(|ui| {
        ui.add(ActorChip::new(NAME70, ActorKind::Agent));
        ui.add(StatusChip::new(Status::Warn, NAME70));
        ui.add(GateChip::failed(NAME70, "0.81", "0.85"));
        ui.add(ScopeBadge::new(NAME70));
        ui.add(CostChip::new(&[NAME70]));
    });
    // The pseudo-locale: the component's own words are the ones that change, so the cut has to hold with them too.
    set_strings(&h.ctx, Arc::new(Pseudo(English)));
    h.run_steps(3);
    h
}

/// The chips' nodes, in the order they were added: those that carry [`NAME70`] as their label or value.
fn chip_nodes<'a>(h: &'a egui_kittest::Harness<'_>) -> Vec<egui_kittest::Node<'a>> {
    h.root()
        .children_recursive()
        .filter(|n| {
            let node = n.accesskit_node();
            node.label().is_some_and(|l| l == NAME70) || node.value().is_some_and(|v| v.contains(NAME70))
        })
        .collect()
}

#[test]
fn a_70_character_name_in_each_chip_stays_inside_the_panel() {
    let h = chips();
    let nodes = chip_nodes(&h);
    assert_eq!(nodes.len(), 5, "the five chips are all there");
    for node in &nodes {
        let right = node.rect().right();
        let role = node.accesskit_node().role();
        assert!(right <= PANEL - 8.0 + 1.0, "{role:?} reaches {right}, past the panel's 280 pt content edge");
    }
}

#[test]
fn each_chip_cuts_its_name_with_an_ellipsis_and_its_tooltip_carries_the_whole_name() {
    let mut h = chips();
    let cut = |h: &egui_kittest::Harness<'_>| painted(h.output()).iter().filter(|p| p.text == NAME70).count();
    assert_eq!(cut(&h), 5, "each chip paints its name");
    assert!(painted(h.output()).iter().filter(|p| p.text == NAME70).all(|p| p.elided), "and every one is cut");

    let centres: Vec<egui::Pos2> = chip_nodes(&h).iter().map(|n| n.rect().center()).collect();
    for (i, at) in centres.into_iter().enumerate() {
        h.hover_at(at);
        h.run_steps(8);
        assert!(
            painted(h.output()).iter().any(|p| p.text == NAME70 && !p.elided),
            "chip {i} shows the whole name in its tooltip"
        );
        h.hover_at(pos2(PANEL - 1.0, 390.0));
        h.run_steps(8);
    }
}

#[test]
fn a_chip_whose_name_fits_is_not_cut_and_has_no_tooltip() {
    let mut h = panel(|ui| {
        ui.add(ActorChip::new("Ada", ActorKind::Person));
        ui.add(StatusChip::new(Status::Ok, "Connected"));
    });
    let ada = h.root().children_recursive().find(|n| n.accesskit_node().value().is_some_and(|v| v == "Ada"));
    let at = ada.map(|n| n.rect().center());
    assert!(at.is_some(), "the Ada chip is in the panel");
    h.hover_at(at.unwrap_or(pos2(0.0, 0.0)));
    h.run_steps(8);
    let shown = painted(h.output());
    assert!(shown.iter().all(|p| !p.elided), "nothing is cut");
    assert_eq!(shown.iter().filter(|p| p.text == "Ada").count(), 1, "and no tooltip repeats the name");
}

#[test]
fn a_focused_cut_gate_chip_shows_its_whole_name_without_a_pointer() {
    let mut h = chips();
    // The gate chip is the only focusable chip in the panel, so one Tab lands on it.
    press(&mut h, Modifiers::NONE, Key::Tab);
    h.run_steps(8);
    assert!(
        painted(h.output()).iter().any(|p| p.text == NAME70 && !p.elided),
        "keyboard focus shows the whole name in the tooltip"
    );
}

#[test]
fn a_cut_cost_marks_its_cut_when_a_part_ends_at_the_edge_of_the_room() {
    // The room ends one point past the first part and its ellipsis, so the separator starts with too little room for
    // a letter. The cut must still show an ellipsis, and nothing after the separator is drawn.
    let mut h = crate::ds::testing::harness_state(Settings::default(), vec2(PANEL, 400.0), 0.0_f32, |ui, room| {
        ui.set_max_width(*room);
        ui.add(CostChip::new(&["~40 s", "1.2 GB"]));
    });
    let mono = egui::FontId::monospace(11.5);
    let first = crate::ds::paint::text_width(&h.ctx, "~40 s", &mono);
    let ellipsis = crate::ds::paint::text_width(&h.ctx, "\u{2026}", &mono);
    *h.state_mut() = first + ellipsis + 1.0;
    h.run_steps(3);
    let shown = painted(h.output());
    assert!(shown.iter().any(|p| p.text == "\u{2026}"), "the cut is marked with an ellipsis: {shown:?}");
    assert!(shown.iter().all(|p| p.text != "1.2 GB"), "nothing after the cut is drawn");
}

#[test]
fn a_tooltip_dismissed_with_escape_comes_back_on_the_next_focus() {
    let shows_whole = |h: &egui_kittest::Harness<'_>| painted(h.output()).iter().any(|p| p.text == NAME70 && !p.elided);
    let mut h = panel(|ui| {
        ui.add(Button::new("Approve"));
        ui.add(GateChip::failed(NAME70, "0.81", "0.85"));
    });
    // Tab to the button, then to the gate chip, which shows its whole name on focus.
    press(&mut h, Modifiers::NONE, Key::Tab);
    press(&mut h, Modifiers::NONE, Key::Tab);
    h.run_steps(8);
    assert!(shows_whole(&h), "focus shows the whole name");
    press(&mut h, Modifiers::NONE, Key::Escape);
    h.run_steps(8);
    assert!(!shows_whole(&h), "Escape dismisses the tooltip");
    // Tab away (round to the button) and back to the chip: the dismissal must not outlive the focus it belonged to.
    press(&mut h, Modifiers::NONE, Key::Tab);
    h.run_steps(8);
    press(&mut h, Modifiers::NONE, Key::Tab);
    h.run_steps(8);
    assert!(shows_whole(&h), "the next focus shows the tooltip again");
}

#[test]
fn a_hovered_cut_chip_whose_label_comes_to_fit_shows_no_tooltip() {
    let mut h = crate::ds::testing::harness_state(Settings::default(), vec2(PANEL, 400.0), 120.0_f32, |ui, room| {
        ui.set_max_width(*room);
        ui.add(StatusChip::new(Status::Ok, NAME70));
    });
    h.run_steps(3);
    let at = h.root().children_recursive().find(|n| n.accesskit_node().value().is_some_and(|v| v == NAME70));
    let at = at.map(|n| n.rect().center());
    assert!(at.is_some(), "the chip is in the panel");
    h.hover_at(at.unwrap_or(pos2(0.0, 0.0)));
    h.run_steps(8);
    assert!(painted(h.output()).iter().filter(|p| p.text == NAME70).count() >= 2, "cut, it shows the tooltip");
    // The room grows, so the label fits: the chip is still hovered, but the tooltip must go.
    *h.state_mut() = 600.0;
    h.run_steps(8);
    let shown = painted(h.output());
    assert_eq!(shown.iter().filter(|p| p.text == NAME70).count(), 1, "only the chip's own label is drawn");
    assert!(shown.iter().all(|p| !(p.text == NAME70 && p.elided)), "and it is not cut");
}

#[test]
fn a_focused_cut_chip_whose_label_comes_to_fit_shows_no_tooltip() {
    let mut h = crate::ds::testing::harness_state(Settings::default(), vec2(PANEL, 400.0), 120.0_f32, |ui, room| {
        ui.set_max_width(*room);
        ui.add(GateChip::failed(NAME70, "0.81", "0.85"));
    });
    h.run_steps(3);
    press(&mut h, Modifiers::NONE, Key::Tab);
    h.run_steps(8);
    assert!(
        painted(h.output()).iter().filter(|p| p.text == NAME70).count() >= 2,
        "cut and focused, it shows the tooltip"
    );
    // The room grows and the label fits: the chip keeps its focus, but its tooltip must go.
    *h.state_mut() = 600.0;
    h.run_steps(8);
    let shown = painted(h.output());
    assert_eq!(shown.iter().filter(|p| p.text == NAME70).count(), 1, "only the chip's own label is drawn");
}
