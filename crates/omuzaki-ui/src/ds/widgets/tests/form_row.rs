//! A form row: the label is the control's accessible name, in a column shared with the slider row.

use egui::vec2;

use super::*;
use crate::ds::testing::unlabelled_interactive;

#[test]
fn the_label_the_row_draws_is_the_name_the_control_gets_and_the_controls_line_up() {
    let h = state(String::from("Hair"), |ui, text| {
        let (mut side, mut other, mut v) = (0u8, 1u8, 0.3);
        FormRow::new("Part name").show(ui, |ui, label| ui.add(TextField::new(label, text)));
        FormRow::new("Side").modified(true).show(ui, |ui, label| {
            ui.add(Segmented::new(label, &mut side, &[Segment::new(0u8, "Left"), Segment::new(1u8, "Right")]))
        });
        FormRow::new("Other").show(ui, |ui, label| {
            ui.add(Segmented::new(label, &mut other, &[Segment::new(0u8, "Left"), Segment::new(1u8, "Right")]))
        });
        ParamSlider::new("Width", &mut v, 0.0..=1.0).show(ui);
    });
    let (side, other) =
        (h.get_by_role_and_label(Role::RadioGroup, "Side"), h.get_by_role_and_label(Role::RadioGroup, "Other"));
    assert!((side.rect().left() - other.rect().left()).abs() < 0.5, "the controls of two rows share one column");
    let slider = h.get_by_role_and_label(Role::Slider, "Width");
    assert!((slider.rect().left() - side.rect().left()).abs() < 0.5, "and it is the column a slider's track starts in");
    assert_eq!(unlabelled_interactive(&h), Vec::<String>::new());
    // The label is painted, not a node: the control carries the one name.
    assert_eq!(h.query_all_by_label("Part name").count(), 1);
}

#[test]
fn the_label_column_is_38_percent_between_96_and_160_and_gives_way_to_the_control_in_a_narrow_row() {
    let column = |width: f32| {
        let h = harness_state(Settings::default(), vec2(width + 16.0, 60.0), String::new(), |ui, text| {
            ui.set_max_width(width);
            FormRow::new("Name").show(ui, |ui, label| ui.add(Button::new(label)));
            let _ = &text;
        });
        // The harness and its frame each keep 8 pt of margin.
        h.get_by_role_and_label(Role::Button, "Name").rect().left() - 16.0
    };
    // gutter 12 + label column + gap 6
    assert!((column(400.0) - (12.0 + (388.0_f32 * 0.38).clamp(96.0, 160.0) + 6.0)).abs() < 1.0);
    assert!((column(900.0) - (12.0 + 160.0 + 6.0)).abs() < 1.0, "never wider than 160 pt");
    assert!(column(150.0) < 12.0 + 96.0 + 6.0, "a narrow row takes the room from the label first");
}

#[test]
fn a_label_wider_than_its_column_ends_in_an_ellipsis_and_the_tooltip_says_it_all() {
    use crate::ds::audit::painted;
    let long = "A label long enough that it cannot fit in its column";
    let mut h = state(String::new(), move |ui, text| {
        FormRow::new(long).show(ui, |ui, label| ui.add(TextField::new(label, text)));
    });
    assert!(painted(h.output()).iter().any(|p| p.text == long && p.elided));
    h.hover_at(egui::pos2(60.0, 22.0));
    h.run_steps(10);
    assert!(painted(h.output()).iter().any(|p| p.text == long && !p.elided), "hovering shows the whole label");
}
