//! How a stateful widget names itself to egui.
//!
//! egui keeps a widget's state (keyboard focus, a drag in progress, an open popover, a typing buffer) under its `Id`.
//! By default that is an *auto id*, counted from the widgets added to the same `Ui` before it, so the state belongs
//! to a **place in the layout**: reorder the rows of an inspector and the open popover or the half-typed number stays
//! where it was, now on the wrong row. A widget given an `.id_salt(..)` is named by the salt instead, so its state
//! follows it.
//!
//! The helpers here are shared by every widget that takes a salt, so the rule is written once:
//!
//! - The salt is **namespaced by the kind of widget**, so a toggle and a slider that both use the salt `"width"` in
//!   one `Ui` cannot name the same id. Two widgets of the *same* kind and salt in one `Ui` are a mistake; egui marks
//!   them in debug builds. A `Ui` made by `ui.scope`, `ui.horizontal` and the like has the id of its parent, so rows
//!   built each in one of those share their salts: give every row its own with `ui.push_id(row_key, ..)`, or a salt
//!   that is unique to the row.
//! - The layout is always claimed with an ordinary allocation, salted or not. The salt changes what the widget is
//!   *called*, never where it goes or how many auto ids it uses, so the widgets after it keep their ids.

use egui::{Align, Id, Layout, Rect, Response, Sense, Ui, UiBuilder, Vec2};

/// The id of a `kind` of widget with `salt`, inside the `Ui` it is added to.
///
/// `kind` names the widget the way an identifier does (`ds.toggle`), not as a word for a person: it is a key, and the
/// source lint for hard-coded text reads a word in a component as a message that should be translated.
pub(super) fn salted(ui: &Ui, kind: &'static str, salt: Id) -> Id {
    ui.make_persistent_id((kind, salt))
}

/// Allocates `size` for a widget of `kind` and returns the rectangle and the response that carries its id.
///
/// Without a salt this is `Ui::allocate_exact_size`: the id is the auto id. With one, the allocation only claims the
/// space (hover only, so it is no second Tab stop) and the widget senses through `ui.interact` under the salted id.
pub(super) fn allocate(
    ui: &mut Ui,
    size: Vec2,
    sense: Sense,
    kind: &'static str,
    salt: Option<Id>,
) -> (Rect, Response) {
    let Some(salt) = salt else { return ui.allocate_exact_size(size, sense) };
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let mut response = ui.interact(rect, salted(ui, kind, salt), sense);
    response.set_intrinsic_size(size);
    (rect, response)
}

/// A child `Ui` for an editor drawn inside `rect`, a part of the widget's own row that was already allocated.
///
/// `Ui::scope_builder` would also move the parent's cursor to the bottom of `rect` plus the item spacing. When the
/// editor is shorter than its row that is a point above the row's bottom edge, and in a vertical layout the next
/// widget would overlap this one. A plain child leaves the cursor alone. It lays out left to right and centred, so
/// the editor sits at mid-height whatever layout the parent has.
///
/// The child takes one auto id from the parent; frames with no editor must take it too ([`skip_editor_id`]).
pub(super) fn editor_ui(ui: &mut Ui, rect: Rect) -> Ui {
    ui.new_child(UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)))
}

/// Takes the auto id an editor's child `Ui` would have taken, for a frame in which the widget shows no editor.
///
/// Otherwise every widget after this one in the same `Ui` would get a different auto id while the editor is open
/// than while it is not. Its state would be dropped when the editor closes, and a press on it that started while the
/// editor was open would be lost.
pub(super) fn skip_editor_id(ui: &mut Ui) {
    ui.skip_ahead_auto_ids(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `add` in the first `Ui` of one headless frame.
    fn in_ui(add: impl FnOnce(&mut Ui)) {
        let ctx = egui::Context::default();
        let mut add = Some(add);
        ctx.run_ui(egui::RawInput::default(), |ui| {
            if let Some(add) = add.take() {
                add(ui);
            }
        })
        .drop_without_applying_deltas();
    }

    /// Two children that start with the same auto id counter, so what each takes can be compared exactly.
    fn twins(ui: &mut Ui) -> (Ui, Ui) {
        let rect = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 100.0));
        let twin = |ui: &mut Ui| ui.new_child(UiBuilder::new().id(Id::new("twin")).max_rect(rect));
        (twin(ui), twin(ui))
    }

    #[test]
    fn a_salt_names_the_same_kind_the_same_and_another_kind_differently() {
        in_ui(|ui| {
            let salt = Id::new("width");
            assert_eq!(salted(ui, "slider", salt), salted(ui, "slider", salt));
            assert_ne!(salted(ui, "slider", salt), salted(ui, "toggle", salt), "kinds do not share salts");
            assert_ne!(salted(ui, "slider", salt), salted(ui, "slider", Id::new("height")));
        });
    }

    #[test]
    fn the_same_salt_in_two_parents_names_two_widgets_if_each_parent_has_its_own_id() {
        in_ui(|ui| {
            let salt = Id::new("width");
            let first = ui.push_id("row 1", |ui| salted(ui, "slider", salt)).inner;
            let second = ui.push_id("row 2", |ui| salted(ui, "slider", salt)).inner;
            assert_ne!(first, second, "a row in its own pushed id has its own salts");
            // egui gives anonymous child Uis (`ui.scope`, `ui.horizontal`) the id of their parent plus a constant, so
            // sibling rows built in them share one: the reason the docs ask for `push_id` or a salt unique to the row.
            let first = ui.scope(|ui| salted(ui, "slider", salt)).inner;
            let second = ui.scope(|ui| salted(ui, "slider", salt)).inner;
            assert_eq!(first, second, "anonymous siblings are one parent as far as a salt is concerned");
        });
    }

    #[test]
    fn a_salted_allocation_takes_the_same_space_and_auto_ids_as_an_unsalted_one() {
        in_ui(|ui| {
            let (mut plain, mut with_salt) = twins(ui);
            let size = egui::vec2(80.0, 20.0);
            let (plain_rect, plain_response) = allocate(&mut plain, size, Sense::click(), "kind", None);
            let (salt_rect, salt_response) = allocate(&mut with_salt, size, Sense::click(), "kind", Some(Id::new("a")));
            assert_eq!(plain_rect, salt_rect, "the salt does not move the widget");
            assert_eq!(
                plain.next_auto_id(),
                with_salt.next_auto_id(),
                "or change what the widgets after it are called"
            );
            assert_eq!(salt_response.id, salted(&with_salt, "kind", Id::new("a")));
            assert_ne!(salt_response.id, plain_response.id);
            assert!(salt_response.sense.is_focusable() && plain_response.sense.is_focusable());
        });
    }

    #[test]
    fn a_frame_with_no_editor_takes_the_auto_ids_a_frame_with_one_takes() {
        in_ui(|ui| {
            let (mut open, mut shut) = twins(ui);
            let rect = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(40.0, 20.0));
            let start = open.next_auto_id();
            drop(editor_ui(&mut open, rect));
            skip_editor_id(&mut shut);
            assert_ne!(start, open.next_auto_id(), "the editor's child takes an auto id");
            assert_eq!(open.next_auto_id(), shut.next_auto_id());
        });
    }

    #[test]
    fn an_editor_does_not_move_the_parents_cursor() {
        in_ui(|ui| {
            let row = ui.allocate_exact_size(egui::vec2(100.0, 28.0), Sense::hover()).0;
            let cursor = ui.cursor();
            // Shorter than the row and not touching its bottom edge: what `scope_builder` gets wrong.
            let field = Rect::from_min_size(row.min + egui::vec2(0.0, 3.0), egui::vec2(60.0, 22.0));
            let mut editor = editor_ui(ui, field);
            editor.label("12");
            drop(editor);
            assert_eq!(ui.cursor(), cursor);
        });
    }
}
