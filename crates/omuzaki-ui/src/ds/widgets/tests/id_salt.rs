//! `.id_salt()`: the state of a widget follows its salt when rows are reordered, and without a salt stays in its place.
//!
//! Each test shows two rows of one widget, "Alpha" and "Beta", sets some state in the first, swaps the order of the
//! rows and asks which row holds that state now. A salted widget is named by its salt, so the state moves with it. An
//! unsalted one is named by its place in the `Ui`, so the state stays in the first slot and lands on the other row;
//! every widget is run both ways, so a test that passes without the salt would be caught.

use egui::Ui;
use egui_kittest::Harness;

use super::*;

/// Two rows of one widget in a harness, and whether they are shown in the other order.
struct Rows<T> {
    swapped: bool,
    alpha: T,
    beta: T,
}

/// Draws one row: its label, its salt (none when the rows are not salted) and its value.
type Row<T> = fn(&mut Ui, &'static str, Option<&'static str>, &mut T);

fn salt_of(name: &str) -> &'static str {
    if name == "Alpha" { "alpha" } else { "beta" }
}

fn rows<T: 'static>(salted: bool, alpha: T, beta: T, row: Row<T>, height: f32) -> Harness<'static, Rows<T>> {
    harness_state(Settings::default(), vec2(360.0, height), Rows { swapped: false, alpha, beta }, move |ui, rows| {
        let Rows { swapped, alpha, beta } = rows;
        let (first, second) =
            if *swapped { (("Beta", &mut *beta), ("Alpha", &mut *alpha)) } else { (("Alpha", alpha), ("Beta", beta)) };
        for (name, value) in [first, second] {
            row(ui, name, salted.then(|| salt_of(name)), value);
        }
    })
}

/// Puts "Beta" above "Alpha" and lets the frames settle.
fn swap<T>(h: &mut Harness<'_, Rows<T>>) {
    h.state_mut().swapped = true;
    h.run_steps(4);
}

/// The names, among "Alpha" and "Beta", for which `holds_state` is true.
fn holders(mut holds_state: impl FnMut(&str) -> bool) -> Vec<&'static str> {
    ["Alpha", "Beta"].into_iter().filter(|name| holds_state(name)).collect()
}

// ---- ParamSlider -------------------------------------------------------------------------------------------------

fn slider(ui: &mut Ui, name: &'static str, salt: Option<&'static str>, value: &mut f64) {
    let mut slider = ParamSlider::new(name, value, 0.0..=1.0);
    if let Some(salt) = salt {
        slider = slider.id_salt(salt);
    }
    slider.show(ui);
}

/// Starts typing a number in "Alpha", swaps the rows, and says which sliders are being typed in.
fn sliders_being_typed_in(salted: bool) -> Vec<&'static str> {
    let mut h = rows(salted, 0.2, 0.8, slider, 200.0);
    h.get_by_role_and_label(Role::Button, "Edit Alpha value").click();
    h.run_steps(3);
    swap(&mut h);
    holders(|name| h.query_by_role_and_label(Role::TextInput, &format!("{name} value")).is_some())
}

#[test]
fn a_typed_number_stays_with_its_slider_when_the_rows_swap() {
    assert_eq!(sliders_being_typed_in(true), ["Alpha"]);
}

#[test]
fn without_a_salt_a_typed_number_stays_in_its_slot() {
    assert_eq!(sliders_being_typed_in(false), ["Beta"]);
}

#[test]
fn a_salted_slider_that_is_typed_in_moves_down_with_its_row() {
    let mut h = rows(true, 0.2, 0.8, slider, 200.0);
    h.get_by_role_and_label(Role::Button, "Edit Alpha value").click();
    h.run_steps(3);
    let before = h.get_by_role_and_label(Role::TextInput, "Alpha value").rect();
    swap(&mut h);
    let after = h.get_by_role_and_label(Role::TextInput, "Alpha value").rect();
    let beta = h.get_by_role_and_label(Role::Button, "Edit Beta value").rect();
    assert!(after.top() > before.top(), "Alpha is the second row now: {before:?} then {after:?}");
    assert!(beta.bottom() <= after.top(), "and Beta, resting, is above it: {beta:?}");
}

/// Two sliders with the same label, told apart only by their salts (or by nothing).
fn twin_sliders(salted: bool) -> Harness<'static, Rows<f64>> {
    fn twin(ui: &mut Ui, _name: &'static str, salt: Option<&'static str>, value: &mut f64) {
        let mut slider = ParamSlider::new("Width", value, 0.0..=1.0);
        if let Some(salt) = salt {
            slider = slider.id_salt(salt);
        }
        slider.show(ui);
    }
    rows(salted, 0.2, 0.8, twin, 200.0)
}

/// The nodes ordered from the top of the screen down: the tree does not promise an order.
fn from_the_top<'a>(nodes: impl Iterator<Item = egui_kittest::Node<'a>>) -> Vec<egui_kittest::Node<'a>> {
    let mut nodes: Vec<_> = nodes.collect();
    nodes.sort_by(|a, b| a.rect().top().total_cmp(&b.rect().top()));
    nodes
}

/// The top of the typed-in field, when exactly one of the two same-named sliders is being typed in.
fn typing_top(h: &Harness<'_, Rows<f64>>) -> f32 {
    let mut fields = h.query_all_by_role_and_label(Role::TextInput, "Width value");
    let top = fields.next().map(|field| field.rect().top());
    assert!(fields.next().is_none(), "only one of the two sliders is being typed in");
    top.unwrap_or(f32::NAN)
}

#[test]
fn two_sliders_with_the_same_label_in_one_parent_do_not_share_state() {
    for salted in [false, true] {
        let mut h = twin_sliders(salted);
        let edit_buttons = from_the_top(h.get_all_by_role_and_label(Role::Button, "Edit Width value"));
        assert_eq!(edit_buttons.len(), 2, "two rows named alike (salted: {salted})");
        let (first, second) = (edit_buttons[0].rect().top(), edit_buttons[1].rect().top());
        edit_buttons[0].click();
        h.run_steps(3);
        assert!(
            (typing_top(&h) - first).abs() < 6.0,
            "the first row is typed in, the second is not (salted: {salted})"
        );
        assert_eq!(h.query_all_by_role_and_label(Role::Button, "Edit Width value").count(), 1, "the second rests");
        assert!(second > first);
    }
}

#[test]
fn same_labelled_sliders_are_told_apart_by_their_salts_when_the_rows_swap() {
    let typed_in_first_row = |salted| {
        let mut h = twin_sliders(salted);
        if let Some(first) = from_the_top(h.get_all_by_role_and_label(Role::Button, "Edit Width value")).first() {
            first.click();
        }
        h.run_steps(3);
        let before = typing_top(&h);
        swap(&mut h);
        (before, typing_top(&h))
    };
    let (before, after) = typed_in_first_row(true);
    assert!(after > before, "the typed-in slider is the second row now, as its salt says: {before} then {after}");
    let (before, after) = typed_in_first_row(false);
    assert!((after - before).abs() < 6.0, "without salts there is nothing to tell the rows apart but place");
}

// ---- Toggle ------------------------------------------------------------------------------------------------------

fn toggle(ui: &mut Ui, name: &'static str, salt: Option<&'static str>, value: &mut bool) {
    let mut toggle = Toggle::switch(name, value);
    if let Some(salt) = salt {
        toggle = toggle.id_salt(salt);
    }
    ui.add(toggle);
}

fn toggles_with_focus(salted: bool) -> Vec<&'static str> {
    let mut h = rows(salted, false, false, toggle, 120.0);
    h.get_by_role_and_label(Role::Switch, "Alpha").focus();
    h.run();
    swap(&mut h);
    holders(|name| h.get_by_role_and_label(Role::Switch, name).is_focused())
}

#[test]
fn a_focused_toggle_keeps_the_focus_when_the_rows_swap() {
    assert_eq!(toggles_with_focus(true), ["Alpha"]);
}

#[test]
fn without_a_salt_the_focus_stays_in_its_slot() {
    assert_eq!(toggles_with_focus(false), ["Beta"]);
}

// ---- Segmented ---------------------------------------------------------------------------------------------------

fn segmented(ui: &mut Ui, name: &'static str, salt: Option<&'static str>, value: &mut u8) {
    let (left, right) = if name == "Alpha" { ("Alpha left", "Alpha right") } else { ("Beta left", "Beta right") };
    let segments = [Segment::new(0u8, left), Segment::new(1u8, right)];
    let mut control = Segmented::new(name, value, &segments);
    if let Some(salt) = salt {
        control = control.id_salt(salt);
    }
    ui.add(control);
}

fn segmented_with_focus(salted: bool) -> Vec<&'static str> {
    let mut h = rows(salted, 0, 0, segmented, 120.0);
    h.get_by_role_and_label(Role::RadioButton, "Alpha left").focus();
    h.run();
    swap(&mut h);
    holders(|name| h.get_by_role_and_label(Role::RadioButton, &format!("{name} left")).is_focused())
}

#[test]
fn a_focused_segment_keeps_the_focus_when_the_rows_swap() {
    assert_eq!(segmented_with_focus(true), ["Alpha"]);
}

#[test]
fn without_a_salt_the_focus_stays_with_the_slot_of_the_segments() {
    assert_eq!(segmented_with_focus(false), ["Beta"]);
}

// ---- TextField ---------------------------------------------------------------------------------------------------

fn text(ui: &mut Ui, name: &'static str, salt: Option<&'static str>, value: &mut String) {
    let mut field = TextField::new(name, value);
    if let Some(salt) = salt {
        field = field.id_salt(salt);
    }
    ui.add(field);
}

fn text_fields_with_focus(salted: bool) -> Vec<&'static str> {
    let mut h = rows(salted, "one".to_owned(), "two".to_owned(), text, 120.0);
    h.get_by_role_and_label(Role::TextInput, "Alpha").focus();
    h.run();
    swap(&mut h);
    holders(|name| h.get_by_role_and_label(Role::TextInput, name).is_focused())
}

#[test]
fn a_focused_text_field_keeps_the_focus_when_the_rows_swap() {
    assert_eq!(text_fields_with_focus(true), ["Alpha"]);
}

#[test]
fn without_a_salt_a_text_fields_focus_stays_in_its_slot() {
    assert_eq!(text_fields_with_focus(false), ["Beta"]);
}

// ---- ColourField -------------------------------------------------------------------------------------------------

fn colour(ui: &mut Ui, name: &'static str, salt: Option<&'static str>, value: &mut Colour) {
    let mut field = ColourField::new(name, value);
    if let Some(salt) = salt {
        field = field.id_salt(salt);
    }
    field.show(ui);
}

/// How far under its field the first thing in the popover is: 4 pt to the popover, 12 pt of its margin.
const POPOVER_OFFSET: f32 = 16.0;

/// Opens "Alpha"'s popover, swaps the rows, and says which field the popover hangs under.
fn colour_fields_with_popover(salted: bool) -> Vec<&'static str> {
    let mut h = rows(salted, Colour::rgb(0x8A, 0x6F, 0xD1), Colour::rgb(0x11, 0x22, 0x33), colour, 620.0);
    h.get_by_role_and_label(Role::ColorWell, "Alpha").click();
    h.run_steps(3);
    swap(&mut h);
    let first_in_popover = h.get_by_role_and_label(Role::Slider, "Saturation and value").rect().top();
    let gap = |name: &str| {
        (h.get_by_role_and_label(Role::ColorWell, name).rect().bottom() + POPOVER_OFFSET - first_in_popover).abs()
    };
    ["Alpha", "Beta"].into_iter().min_by(|a, b| gap(a).total_cmp(&gap(b))).into_iter().collect()
}

#[test]
fn an_open_popover_stays_with_its_colour_field_when_the_rows_swap() {
    assert_eq!(colour_fields_with_popover(true), ["Alpha"]);
}

#[test]
fn without_a_salt_the_popover_stays_in_its_slot() {
    assert_eq!(colour_fields_with_popover(false), ["Beta"]);
}

// ---- Section -----------------------------------------------------------------------------------------------------

/// A section titled "Advanced" whatever the row, with a button in it named for the row.
fn section(ui: &mut Ui, name: &'static str, salt: Option<&'static str>, _value: &mut ()) {
    let mut section = Section::new("Advanced");
    if let Some(salt) = salt {
        section = section.id_salt(salt);
    }
    let label = format!("In {name}");
    section.show(ui, |ui| {
        ui.add(Button::new(&label));
    });
}

/// Opens the first "Advanced" section, optionally swaps the rows, and says which rows' bodies are showing.
fn sections_open(salted: bool, swapped: bool) -> Vec<&'static str> {
    let mut h = rows(salted, (), (), section, 200.0);
    if let Some(first) = from_the_top(h.get_all_by_role_and_label(Role::DisclosureTriangle, "Advanced")).first() {
        first.click();
    }
    h.run_steps(8);
    if swapped {
        swap(&mut h);
    }
    holders(|name| h.query_by_role_and_label(Role::Button, &format!("In {name}")).is_some())
}

#[test]
fn two_sections_with_the_same_title_open_one_at_a_time_with_or_without_a_salt() {
    assert_eq!(sections_open(true, false), ["Alpha"]);
    assert_eq!(sections_open(false, false), ["Alpha"], "a second 'Advanced' used to open with the first");
}

#[test]
fn an_open_section_stays_with_its_salt_when_the_rows_swap() {
    assert_eq!(sections_open(true, true), ["Alpha"]);
}

#[test]
fn without_a_salt_the_open_section_stays_in_its_slot() {
    assert_eq!(sections_open(false, true), ["Beta"]);
}

// ---- Different widgets, one salt ---------------------------------------------------------------------------------

#[test]
fn widgets_of_different_kinds_can_share_a_salt_in_one_parent() {
    type Shared = (bool, u8, f64, String, Colour, u8, usize, f64, Curve);
    let mut h = state(
        (false, 0u8, 0.5, String::new(), Colour::rgb(1, 2, 3), 0u8, 0usize, 0.5, Curve::linear()),
        |ui, (on, mode, gain, name, tint, choice, tab, count, curve): &mut Shared| {
            let segments = [Segment::new(0u8, "Mode left"), Segment::new(1u8, "Mode right")];
            let choices = [Choice::new(0u8, "One"), Choice::new(1u8, "Two")];
            let tabs = [TabItem::new("Tab one"), TabItem::new("Tab two")];
            ui.add(Toggle::switch("Mute", on).id_salt("x"));
            ui.add(Segmented::new("Mode", mode, &segments).id_salt("x"));
            ParamSlider::new("Gain", gain, 0.0..=1.0).id_salt("x").show(ui);
            ui.add(TextField::new("Name", name).id_salt("x"));
            ColourField::new("Tint", tint).id_salt("x").show(ui);
            ui.add(Dropdown::new("Choice", choice, &choices).id_salt("x"));
            ui.add(TabBar::new("Tabs", tab, &tabs).id_salt("x"));
            ui.add(NumberField::new("Count", count, 0.0..=1.0).id_salt("x"));
            CurveEditor::new("Curve", curve).id_salt("x").show(ui);
            StepCard::new("Step", "Designer", ActorKind::Agent).state(StepState::Pending).id_salt("x").show(ui);
            JobRow::new("Job", JobState::Running { progress: Some(0.4) }).id_salt("x").show(ui);
        },
    );
    let focusable = [
        (Role::Switch, "Mute"),
        (Role::RadioButton, "Mode left"),
        (Role::Slider, "Gain"),
        (Role::TextInput, "Name"),
        (Role::ColorWell, "Tint"),
        (Role::ComboBox, "Choice"),
        (Role::Tab, "Tab one"),
        (Role::SpinButton, "Count"),
        (Role::Slider, "Curve, point 1 of 2"),
        (Role::Button, "Approve: Step"),
        (Role::Button, "Cancel Job"),
    ];
    for &(role, label) in &focusable {
        h.get_by_role_and_label(role, label).focus();
        h.run();
        let focused: Vec<_> = focusable
            .iter()
            .filter(|(role, label)| h.get_by_role_and_label(*role, label).is_focused())
            .map(|(_, label)| *label)
            .collect();
        assert_eq!(focused, [label], "focusing one of five widgets that share the salt \"x\" focuses only it");
    }
    h.get_by_role_and_label(Role::RadioGroup, "Mode");
}

// ---- Dropdown ----------------------------------------------------------------------------------------------------

fn dropdown(ui: &mut Ui, name: &'static str, salt: Option<&'static str>, value: &mut u8) {
    let choices = [Choice::new(0u8, "One"), Choice::new(1u8, "Two")];
    let mut dropdown = Dropdown::new(name, value, &choices);
    if let Some(salt) = salt {
        dropdown = dropdown.id_salt(salt);
    }
    ui.add(dropdown);
}

/// Opens "Alpha"'s list, swaps the rows, and says which dropdown's list is open.
fn dropdowns_open(salted: bool) -> Vec<&'static str> {
    let mut h = rows(salted, 0, 1, dropdown, 200.0);
    h.get_by_role_and_label(Role::ComboBox, "Alpha").click();
    h.run_steps(3);
    swap(&mut h);
    holders(|name| h.query_by_role_and_label(Role::ListBox, name).is_some())
}

#[test]
fn an_open_dropdown_stays_with_its_salt_when_the_rows_swap() {
    assert_eq!(dropdowns_open(true), ["Alpha"]);
}

#[test]
fn without_a_salt_the_open_dropdown_stays_in_its_slot() {
    assert_eq!(dropdowns_open(false), ["Beta"]);
}

// ---- TabBar ------------------------------------------------------------------------------------------------------

fn tab_bar(ui: &mut Ui, name: &'static str, salt: Option<&'static str>, value: &mut usize) {
    let (one, two) = if name == "Alpha" { ("Alpha one", "Alpha two") } else { ("Beta one", "Beta two") };
    let tabs = [TabItem::new(one), TabItem::new(two)];
    let mut bar = TabBar::new(name, value, &tabs);
    if let Some(salt) = salt {
        bar = bar.id_salt(salt);
    }
    ui.add(bar);
}

fn tabs_with_focus(salted: bool) -> Vec<&'static str> {
    let mut h = rows(salted, 0, 0, tab_bar, 120.0);
    h.get_by_role_and_label(Role::Tab, "Alpha one").focus();
    h.run();
    swap(&mut h);
    holders(|name| h.get_by_role_and_label(Role::Tab, &format!("{name} one")).is_focused())
}

#[test]
fn a_focused_tab_keeps_the_focus_when_the_rows_swap() {
    assert_eq!(tabs_with_focus(true), ["Alpha"]);
}

#[test]
fn without_a_salt_the_focused_tab_stays_in_its_slot() {
    assert_eq!(tabs_with_focus(false), ["Beta"]);
}

// ---- NumberField -------------------------------------------------------------------------------------------------

fn number(ui: &mut Ui, name: &'static str, salt: Option<&'static str>, value: &mut f64) {
    let mut field = NumberField::new(name, value, 0.0..=1.0);
    if let Some(salt) = salt {
        field = field.id_salt(salt);
    }
    field.show(ui);
}

/// Starts typing a number in "Alpha", swaps the rows, and says which fields are being typed in.
fn numbers_being_typed_in(salted: bool) -> Vec<&'static str> {
    let mut h = rows(salted, 0.2, 0.8, number, 200.0);
    h.get_by_role_and_label(Role::SpinButton, "Alpha").click();
    h.run_steps(3);
    swap(&mut h);
    holders(|name| h.query_by_role_and_label(Role::TextInput, &format!("{name} value")).is_some())
}

#[test]
fn a_number_being_typed_in_stays_with_its_salt_when_the_rows_swap() {
    assert_eq!(numbers_being_typed_in(true), ["Alpha"]);
}

#[test]
fn without_a_salt_a_number_being_typed_in_stays_in_its_slot() {
    assert_eq!(numbers_being_typed_in(false), ["Beta"]);
}

// ---- CurveEditor -------------------------------------------------------------------------------------------------

fn curve(ui: &mut Ui, name: &'static str, salt: Option<&'static str>, value: &mut Curve) {
    let mut editor = CurveEditor::new(name, value);
    if let Some(salt) = salt {
        editor = editor.id_salt(salt);
    }
    editor.show(ui);
}

/// Focuses the first point of "Alpha"'s curve, swaps the rows, and says which curve's first point has the focus.
fn curve_points_with_focus(salted: bool) -> Vec<&'static str> {
    let mut h = rows(salted, Curve::linear(), Curve::linear(), curve, 480.0);
    h.get_by_role_and_label(Role::Slider, "Alpha, point 1 of 2").focus();
    h.run();
    swap(&mut h);
    holders(|name| h.get_by_role_and_label(Role::Slider, &format!("{name}, point 1 of 2")).is_focused())
}

#[test]
fn a_focused_curve_point_keeps_the_focus_when_the_rows_swap() {
    assert_eq!(curve_points_with_focus(true), ["Alpha"]);
}

#[test]
fn without_a_salt_the_focused_curve_point_stays_in_its_slot() {
    assert_eq!(curve_points_with_focus(false), ["Beta"]);
}

// ---- StepCard ----------------------------------------------------------------------------------------------------

fn step(ui: &mut Ui, name: &'static str, salt: Option<&'static str>, _value: &mut ()) {
    let mut card = StepCard::new(name, "Designer", ActorKind::Agent).state(StepState::Pending);
    if let Some(salt) = salt {
        card = card.id_salt(salt);
    }
    card.show(ui);
}

fn steps_with_focus(salted: bool) -> Vec<&'static str> {
    let mut h = rows(salted, (), (), step, 260.0);
    h.get_by_role_and_label(Role::Button, "Approve: Alpha").focus();
    h.run();
    swap(&mut h);
    holders(|name| h.get_by_role_and_label(Role::Button, &format!("Approve: {name}")).is_focused())
}

#[test]
fn a_focused_step_action_keeps_the_focus_when_the_rows_swap() {
    assert_eq!(steps_with_focus(true), ["Alpha"]);
}

#[test]
fn without_a_salt_the_focused_step_action_stays_in_its_slot() {
    assert_eq!(steps_with_focus(false), ["Beta"]);
}

// ---- JobRow ------------------------------------------------------------------------------------------------------

fn job(ui: &mut Ui, name: &'static str, salt: Option<&'static str>, _value: &mut ()) {
    let mut row = JobRow::new(name, JobState::Running { progress: Some(0.4) });
    if let Some(salt) = salt {
        row = row.id_salt(salt);
    }
    row.show(ui);
}

fn jobs_with_focus(salted: bool) -> Vec<&'static str> {
    let mut h = rows(salted, (), (), job, 120.0);
    h.get_by_role_and_label(Role::Button, "Cancel Alpha").focus();
    h.run();
    swap(&mut h);
    holders(|name| h.get_by_role_and_label(Role::Button, &format!("Cancel {name}")).is_focused())
}

#[test]
fn a_focused_job_action_keeps_the_focus_when_the_rows_swap() {
    assert_eq!(jobs_with_focus(true), ["Alpha"]);
}

#[test]
fn without_a_salt_the_focused_job_action_stays_in_its_slot() {
    assert_eq!(jobs_with_focus(false), ["Beta"]);
}
