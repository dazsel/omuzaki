//! An inline editor drawn inside a row must not move the layout cursor.
//!
//! `Ui::scope_builder` moves the parent's cursor to the bottom of the child's rectangle plus the item spacing. An
//! editor shorter than its row (a 22 pt field in a 28 pt `ParamSlider` row, a tree row's rename field 2 pt inside the
//! row) therefore put the cursor above the row's bottom edge, and in a vertical layout the next widget overlapped
//! the row, but only while the field was open.

use super::*;
use crate::ds::icons;

/// A column of one widget and a button under it, with the item spacing a list of rows uses: none.
fn column<S: 'static>(state: S, row: impl FnMut(&mut egui::Ui, &mut S) + 'static) -> egui_kittest::Harness<'static, S> {
    let mut row = row;
    harness_state(Settings::default(), vec2(360.0, 160.0), state, move |ui, state| {
        ui.spacing_mut().item_spacing.y = 0.0;
        row(ui, state);
        ui.add(Button::new("After"));
    })
}

// ---- ParamSlider -------------------------------------------------------------------------------------------------

struct Slider {
    value: f64,
    /// The auto id the next widget of the column would get.
    next: Option<Id>,
}

fn slider_column() -> egui_kittest::Harness<'static, Slider> {
    column(Slider { value: 0.62, next: None }, |ui, s| {
        ParamSlider::new("Width", &mut s.value, 0.0..=1.0).default_value(0.5).show(ui);
        s.next = Some(ui.next_auto_id());
    })
}

fn start_typing(h: &mut egui_kittest::Harness<'_, Slider>) {
    h.get_by_role_and_label(Role::Button, "Edit Width value").click();
    h.run_steps(3);
    h.get_by_role_and_label(Role::TextInput, "Width value");
}

#[test]
fn the_row_below_a_slider_stays_put_and_clear_of_it_while_its_number_is_typed() {
    let mut h = slider_column();
    let resting = h.get_by_role_and_label(Role::Button, "After").rect();
    let track = h.get_by_role_and_label(Role::Slider, "Width").rect();
    start_typing(&mut h);
    let typing = h.get_by_role_and_label(Role::Button, "After").rect();
    assert!(typing.top() >= track.bottom(), "the next row starts under the slider's row: {track:?} then {typing:?}");
    assert!((typing.top() - resting.top()).abs() < 0.01, "and where it was: {resting:?} then {typing:?}");
}

#[test]
fn the_widgets_after_a_slider_keep_their_ids_while_its_number_is_typed() {
    // A press that starts on the next slider while this number is open is lost if that slider is called something
    // else once the field closes.
    let mut h = slider_column();
    let resting = h.state().next;
    start_typing(&mut h);
    assert_eq!(h.state().next, resting);
    h.get_by_role_and_label(Role::TextInput, "Width value").type_text("0.4");
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
    assert_eq!(h.state().next, resting, "and after it closes");
}

// ---- TreeRow -----------------------------------------------------------------------------------------------------

struct Rename {
    name: String,
    renaming: bool,
    next: Option<Id>,
}

fn rename_column() -> egui_kittest::Harness<'static, Rename> {
    column(Rename { name: "Hair".to_owned(), renaming: false, next: None }, |ui, r| {
        let mut row = TreeRow::new("hair", "Hair", icons::CUBE);
        if r.renaming {
            row = row.renaming(&mut r.name);
        }
        row.show(ui);
        r.next = Some(ui.next_auto_id());
    })
}

fn start_renaming(h: &mut egui_kittest::Harness<'_, Rename>) {
    h.state_mut().renaming = true;
    h.run_steps(3);
    h.get_by_role_and_label(Role::TextInput, "Rename Hair");
}

#[test]
fn the_row_below_a_tree_row_stays_put_and_clear_of_it_while_it_is_renamed() {
    let mut h = rename_column();
    let resting = h.get_by_role_and_label(Role::Button, "After").rect();
    let row = h.get_by_role_and_label(Role::TreeItem, "Hair").rect();
    start_renaming(&mut h);
    let renaming = h.get_by_role_and_label(Role::Button, "After").rect();
    assert!(renaming.top() >= row.bottom(), "the next row starts under the row: {row:?} then {renaming:?}");
    assert!((renaming.top() - resting.top()).abs() < 0.01, "and where it was: {resting:?} then {renaming:?}");
}

#[test]
fn the_widgets_after_a_tree_row_keep_their_ids_while_it_is_renamed() {
    let mut h = rename_column();
    let resting = h.state().next;
    start_renaming(&mut h);
    assert_eq!(h.state().next, resting);
    h.state_mut().renaming = false;
    h.run_steps(3);
    assert_eq!(h.state().next, resting, "and after it closes");
}
