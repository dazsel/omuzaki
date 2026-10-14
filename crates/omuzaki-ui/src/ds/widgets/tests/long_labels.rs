//! Long labels (German, or the pseudo-locale, which is 40% longer than English) must not push a control out of its
//! panel: they end in an ellipsis and the tooltip says the whole thing.

use super::*;
use crate::ds::audit::painted;

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
