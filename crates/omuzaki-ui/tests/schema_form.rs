//! `ds::schema_form`: each row of the `shell.md` section 7 mapping, drawn and then edited with the keyboard alone, with the
//! typed `FormValue` read back; nested sections, the read-only fallback, the Essentials cap, validation messages
//! from the `Strings` table, an accessible name on every generated control, and no allocation that grows with the form.
//!
//! The allocation test measures the way `tests/no_alloc.rs` does (a counting global allocator that counts only on the
//! measuring thread); it is repeated here because a global allocator is per test binary.

// A counting allocator is the only way to prove "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::cast_precision_loss)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use std::sync::Arc;

use egui::accesskit::Role;
use egui::{Context, Event, Key, Modifiers, Pos2, RawInput, Rect, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use omuzaki_core::colour::Colour;
use omuzaki_doc_api::Value;
use omuzaki_ui::ds::schema_form::{EditPhase, Form, FormValue, MAX_ESSENTIALS, SchemaForm};
use omuzaki_ui::ds::testing::{harness_state, press, unlabelled_interactive};
use omuzaki_ui::ds::widgets::{
    Choice, ColourField, Curve, CurveEditor, Dropdown, FormRow, NumberField, Preset, Segment, Segmented, TextField,
};
use omuzaki_ui::ds::{English, Pseudo, Settings, apply_settings, set_strings};
use schemars::{JsonSchema, Schema};
use serde_json::json;

struct Rig {
    form: Form,
    value: FormValue,
    changes: u32,
    /// Every edit the form reported, with its field's pointer.
    edits: Vec<(String, EditPhase)>,
    /// The same, one entry per frame that reported any, in the order the form gave them.
    frames: Vec<Vec<(String, EditPhase)>>,
}

fn schema(json: Value) -> Schema {
    Schema::try_from(json).unwrap_or_else(|_| panic!("a schema"))
}

fn rig(schema: &Schema) -> Harness<'static, Rig> {
    let form = Form::from_schema(schema);
    let value = form.default_value();
    let rig = Rig { form, value, changes: 0, edits: Vec::new(), frames: Vec::new() };
    harness_state(Settings::default(), vec2(420.0, 1400.0), rig, |ui, r| {
        let out = SchemaForm::new(&r.form, &mut r.value).id_salt("test").show(ui);
        if out.changed {
            r.changes += 1;
        }
        let frame: Vec<_> =
            out.edits.iter().map(|e| (r.form.pointer(e.field).unwrap_or("?").to_owned(), e.phase)).collect();
        r.edits.extend(frame.iter().cloned());
        if !frame.is_empty() {
            r.frames.push(frame);
        }
    })
}

fn json_of(h: &Harness<'_, Rig>) -> Value {
    Value::from(&h.state().value)
}

fn focus(h: &mut Harness<'_, Rig>, role: Role, label: &str) {
    h.get_by_role_and_label(role, label).focus();
    h.run();
}

#[allow(clippy::needless_pass_by_value, reason = "the tests pass a `json!` literal")]
fn one_field(field: Value) -> Schema {
    schema(json!({"type": "object", "properties": {"field": field}}))
}

// ---- the mapping, one test per row, each edited with the keyboard ----

#[test]
fn a_bounded_number_is_a_slider_and_arrows_edit_it() {
    let mut h = rig(&one_field(json!({
        "title": "Width", "type": "number", "minimum": 0.0, "maximum": 1.0,
        "x-ui": {"default": 0.5}
    })));
    assert_eq!(json_of(&h), json!({"field": 0.5}), "the default comes from x-ui.default");
    focus(&mut h, Role::Slider, "Width");
    press(&mut h, Modifiers::NONE, Key::ArrowRight);
    let Value::Number(n) = &json_of(&h)["field"] else { panic!("a number") };
    assert!(n.as_f64().is_some_and(|v| v > 0.5), "{n}");
    assert!(h.state().changes > 0);
}

#[test]
fn an_integer_with_few_values_is_a_segmented_control() {
    let mut h = rig(&one_field(json!({"title": "Eye count", "type": "integer", "minimum": 1, "maximum": 3})));
    assert_eq!(json_of(&h), json!({"field": 1}), "an integer field sends integers");
    focus(&mut h, Role::RadioButton, "1");
    press(&mut h, Modifiers::NONE, Key::ArrowRight);
    assert_eq!(json_of(&h), json!({"field": 2}));
}

#[test]
fn a_wide_integer_is_a_slider_that_keeps_whole_numbers() {
    let mut h = rig(&one_field(json!({"title": "Locks", "type": "integer", "minimum": 0, "maximum": 40})));
    focus(&mut h, Role::Slider, "Locks");
    press(&mut h, Modifiers::NONE, Key::ArrowRight);
    assert_eq!(json_of(&h), json!({"field": 1}));
}

#[test]
fn a_small_enum_is_segmented_and_a_large_one_is_a_dropdown() {
    let mut h = rig(&one_field(json!({"title": "Style", "enum": ["flat", "soft", "hard"]})));
    focus(&mut h, Role::RadioButton, "flat");
    press(&mut h, Modifiers::NONE, Key::ArrowRight);
    assert_eq!(json_of(&h), json!({"field": "soft"}));

    let mut h = rig(&one_field(json!({"title": "Shape", "enum": ["egg", "round", "oval", "square", "long", "heart"]})));
    assert!(
        h.query_by_role_and_label(Role::RadioGroup, "Shape").is_none(),
        "six options do not fit a segmented control"
    );
    focus(&mut h, Role::ComboBox, "Shape");
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
    press(&mut h, Modifiers::NONE, Key::ArrowDown);
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
    assert_eq!(json_of(&h), json!({"field": "round"}));
}

#[test]
fn a_documented_enum_through_a_ref_reads_as_choices_titled_by_its_variants() {
    #[derive(JsonSchema)]
    #[allow(dead_code)]
    enum Mood {
        /// Calm.
        Calm,
        /// Angry.
        Angry,
    }
    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct Args {
        mood: Mood,
    }
    let mut h = rig(&schemars::schema_for!(Args));
    assert_eq!(json_of(&h), json!({"mood": "Calm"}));
    focus(&mut h, Role::RadioButton, "Calm");
    press(&mut h, Modifiers::NONE, Key::ArrowRight);
    assert_eq!(json_of(&h), json!({"mood": "Angry"}));
}

#[test]
fn a_boolean_is_a_toggle() {
    let mut h = rig(&one_field(json!({"title": "Outlines", "type": "boolean"})));
    assert_eq!(json_of(&h), json!({"field": false}));
    focus(&mut h, Role::Switch, "Outlines");
    press(&mut h, Modifiers::NONE, Key::Space);
    assert_eq!(json_of(&h), json!({"field": true}));
}

#[test]
fn a_colour_is_a_colour_field_whatever_way_the_schema_says_so() {
    let hex = json!({"type": "string", "pattern": "^#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?$", "title": "Tint", "default": "#336699"});
    for field in [
        hex,
        json!({"type": "string", "format": "color", "title": "Tint", "default": "#336699"}),
        json!({"type": "string", "x-ui": {"widget": "colour"}, "title": "Tint", "default": "#336699"}),
    ] {
        let mut h = rig(&one_field(field));
        assert_eq!(json_of(&h), json!({"field": "#336699"}));
        focus(&mut h, Role::ColorWell, "Tint");
        press(&mut h, Modifiers::NONE, Key::Enter);
        h.run_steps(3);
        focus(&mut h, Role::Slider, "Hue");
        let before = json_of(&h);
        press(&mut h, Modifiers::SHIFT, Key::ArrowRight);
        press(&mut h, Modifiers::NONE, Key::Enter);
        h.run_steps(3);
        assert_ne!(json_of(&h), before, "a keyboard edit in the popover reaches the form's value");
        assert!(h.state().edits.iter().any(|(p, phase)| p == "/field" && *phase == EditPhase::Commit));
    }
}

#[test]
fn a_colour_field_without_alpha_keeps_a_colour_with_alpha_as_it_is() {
    let opaque = Form::from_schema(&one_field(json!({"type": "string", "pattern": "^#[0-9a-fA-F]{6}$"})));
    assert_eq!(Value::from(&opaque.default_value()), json!({"field": "#000000"}));
    let kept = opaque.value_from_json(&json!({"field": "#33669980"}));
    assert!(matches!(&kept, FormValue::Object(f) if matches!(f[0].1, FormValue::Raw(_))), "{kept:?}");
    let read = opaque.value_from_json(&json!({"field": "#336699"}));
    assert_eq!(Value::from(&read), json!({"field": "#336699"}));
    let other =
        Form::from_schema(&one_field(json!({"type": "string", "pattern": "^#[0-9a-f]{3}$", "format": "color"})));
    assert!(
        matches!(other.default_value(), FormValue::Object(f) if f[0].1 == FormValue::Absent),
        "a colour hint over a pattern the colour field can't promise is not a colour, and its format is read-only"
    );
}

#[test]
fn a_curve_array_is_a_curve_editor_and_arrows_move_a_point() {
    let mut h = rig(&one_field(json!({
        "title": "Response", "type": "array", "items": {"type": "number"}, "x-ui": {"widget": "curve"}
    })));
    assert_eq!(json_of(&h), json!({"field": [0, 0, 1, 1]}), "the straight line, as x and y pairs");
    focus(&mut h, Role::Slider, "Response, point 1 of 2");
    press(&mut h, Modifiers::NONE, Key::ArrowUp);
    assert_eq!(json_of(&h), json!({"field": [0, 0.01, 1, 1]}));
    assert_eq!(h.state().edits.last(), Some(&("/field".to_owned(), EditPhase::Commit)));
}

#[test]
fn a_curve_value_the_editor_would_change_is_kept_as_it_is() {
    let form = Form::from_schema(&one_field(
        json!({"type": "array", "items": {"type": "number"}, "x-ui": {"widget": "curve"}}),
    ));
    for given in [
        json!([0.2, 0, 1, 1]),                   // the editor would move the first point onto x = 0
        json!([0, 0, 0.5, 2, 1, 1]),             // and clamp y into the unit range
        json!([0, 0, 1]),                        // not pairs
        json!([0, 0, 0.123_456_789, 0.5, 1, 1]), // more precision than a curve point holds
    ] {
        let value = form.value_from_json(&json!({"field": given}));
        assert!(matches!(&value, FormValue::Object(f) if matches!(f[0].1, FormValue::Raw(_))), "{given}");
        assert_eq!(Value::from(&value), json!({"field": given}), "passes through unchanged");
    }
    let fine = form.value_from_json(&json!({"field": [0, 0.25, 0.5, 0.3, 1, 1]}));
    assert_eq!(Value::from(&fine), json!({"field": [0, 0.25, 0.5, 0.3, 1, 1]}));
}

#[test]
fn x_ui_unit_step_and_decimals_reach_the_slider() {
    let mut h = rig(&one_field(json!({
        "title": "Width", "type": "number", "minimum": 0, "maximum": 10,
        "x-ui": {"unit": "mm", "step": 0.25, "decimals": 3, "default": 5}
    })));
    let strings = omuzaki_ui::ds::testing::accesskit_strings(&h);
    assert!(strings.iter().any(|s| s.contains("5.000") && s.contains("mm")), "{strings:?}");
    focus(&mut h, Role::Slider, "Width");
    press(&mut h, Modifiers::NONE, Key::ArrowRight);
    assert_eq!(json_of(&h), json!({"field": 5.25}), "one arrow press is one x-ui.step");

    let mut whole = rig(&one_field(json!({
        "title": "Count", "type": "integer", "minimum": 0, "maximum": 100, "x-ui": {"step": 2.6, "decimals": 2}
    })));
    focus(&mut whole, Role::Slider, "Count");
    press(&mut whole, Modifiers::NONE, Key::ArrowRight);
    assert_eq!(json_of(&whole), json!({"field": 3}), "an integer's step is rounded to a whole number");
    let strings = omuzaki_ui::ds::testing::accesskit_strings(&whole);
    assert!(!strings.iter().any(|s| s.contains("3.00")), "an integer shows no decimals: {strings:?}");

    let mut field =
        rig(&one_field(json!({"title": "Gap", "type": "number", "minimum": 0, "x-ui": {"unit": "mm", "step": 0.5}})));
    focus(&mut field, Role::SpinButton, "Gap");
    press(&mut field, Modifiers::NONE, Key::ArrowUp);
    assert_eq!(json_of(&field), json!({"field": 0.5}), "a number field steps by x-ui.step too");
}

#[test]
fn a_reset_shows_only_away_from_the_default() {
    let mut h = rig(&one_field(json!({"title": "Outlines", "type": "boolean", "default": true})));
    assert_eq!(json_of(&h), json!({"field": true}));
    assert!(
        h.query_by_role_and_label(Role::Button, "Reset Outlines").is_none(),
        "at the default there is nothing to reset"
    );
    focus(&mut h, Role::Switch, "Outlines");
    press(&mut h, Modifiers::NONE, Key::Space);
    assert_eq!(json_of(&h), json!({"field": false}));
    h.get_by_role_and_label(Role::Button, "Reset Outlines").focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
    assert_eq!(json_of(&h), json!({"field": true}));
    assert!(h.query_by_role_and_label(Role::Button, "Reset Outlines").is_none());
    assert_eq!(h.state().edits.last(), Some(&("/field".to_owned(), EditPhase::Commit)), "a reset is one step");

    let mut h = rig(&one_field(json!({"title": "Style", "enum": ["flat", "soft"], "default": "soft"})));
    focus(&mut h, Role::RadioButton, "soft");
    press(&mut h, Modifiers::NONE, Key::ArrowLeft);
    assert_eq!(json_of(&h), json!({"field": "flat"}));
    assert!(h.query_by_role_and_label(Role::Button, "Reset Style").is_some(), "choice rows get the same reset");

    let mut h = rig(&one_field(json!({"title": "Name", "type": "string"})));
    focus(&mut h, Role::TextInput, "Name");
    h.event(Event::Text("x".to_owned()));
    h.run_steps(3);
    assert!(h.query_by_role_and_label(Role::Button, "Reset Name").is_none(), "no declared default, no reset");
}

#[test]
fn typing_previews_and_leaving_the_field_commits() {
    let mut h = rig(&one_field(json!({"title": "Name", "type": "string"})));
    focus(&mut h, Role::TextInput, "Name");
    h.event(Event::Text("ab".to_owned()));
    h.run_steps(3);
    let edits = &h.state().edits;
    assert!(
        matches!(edits.as_slice(), [_, ..]) && edits.iter().all(|(_, phase)| *phase == EditPhase::Preview),
        "{edits:?}"
    );
    press(&mut h, Modifiers::NONE, Key::Tab);
    assert_eq!(h.state().edits.last(), Some(&("/field".to_owned(), EditPhase::Commit)));
}

// ---- two fields reporting in one frame ----

/// Presses the primary button at the centre of `node` in one frame and releases it in the next, as a mouse does.
/// With `drag`, the pointer moves along the control before the release, as it does on a slider.
fn click(h: &mut Harness<'_, Rig>, role: Role, label: &str, drag: bool) {
    let pos = h.get_by_role_and_label(role, label).rect().center();
    h.hover_at(pos);
    h.run_steps(3);
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
        h.step();
        if pressed && drag {
            h.event(Event::PointerMoved(pos + vec2(30.0, 0.0)));
            h.step();
        }
    }
    h.run_steps(3);
}

/// `tail` of the edits since typing began, as (pointer, phase) pairs.
fn after_typing(h: &Harness<'_, Rig>, typed: usize) -> Vec<(String, EditPhase)> {
    h.state().edits[typed..].to_vec()
}

#[test]
fn two_fields_in_one_frame_each_reach_their_own_edit_in_order() {
    use EditPhase::{Commit, Preview};
    // The other control sits above the text field it takes focus from. Each case: its schema, how it is clicked, and
    // the phase it reports; the text field's blur is its Commit and must come first, whatever the draw order.
    let others = [
        ("toggle", json!({"title": "Outlines", "type": "boolean"}), Role::Switch, "Outlines", Commit),
        ("segmented", json!({"title": "Style", "enum": ["flat", "soft", "hard"]}), Role::RadioButton, "soft", Commit),
        (
            "reset",
            json!({"title": "Tilt", "type": "number", "minimum": 0.0, "maximum": 1.0, "x-ui": {"default": 0.5}}),
            Role::Button,
            "Reset Tilt",
            Commit,
        ),
        (
            "slider",
            json!({"title": "Width", "type": "number", "minimum": 0.0, "maximum": 1.0, "x-ui": {"default": 0.5}}),
            Role::Slider,
            "Width",
            Preview,
        ),
    ];
    for (focused, text_schema, label) in [
        ("text", json!({"title": "Name", "type": "string"}), "Name"),
        ("exact integer", json!({"title": "Name", "type": "integer", "format": "int64"}), "Name"),
    ] {
        for (kind, other, role, other_label, phase) in &others {
            let mut h = rig(&schema(json!({"type": "object", "properties": {"other": other, "field": text_schema}})));
            if *kind == "reset" {
                let away = h.state().form.value_from_json(&json!({"other": 0.2}));
                h.state_mut().value = away;
                h.run();
            }
            focus(&mut h, Role::TextInput, label);
            h.event(Event::Text("7".to_owned()));
            h.run_steps(3);
            let typed = h.state().edits.len();
            let frames_before = h.state().frames.len();
            assert!(typed > 0, "{focused}: typing previewed");
            click(&mut h, *role, other_label, *phase == Preview);
            let got = after_typing(&h, typed);
            let text_ends = ("/field".to_owned(), Commit);
            let other_ends = ("/other".to_owned(), Commit);
            // The text field's own Commit, once, and the other control's own edit: nothing replaced, nothing doubled.
            assert_eq!(got.iter().filter(|e| **e == text_ends).count(), 1, "{focused} then {kind}: {got:?}");
            assert!(got.contains(&("/other".to_owned(), *phase)), "{focused} then {kind}: its edit is kept: {got:?}");
            // egui reports the blur when the pointer is released, in the frame that ends the other control's edit.
            // The blur, as the older gesture's end, comes first in that frame.
            let at = |e: &(String, EditPhase)| got.iter().rposition(|g| g == e);
            assert!(at(&text_ends) < at(&other_ends), "{focused} then {kind}: the blur comes first: {got:?}");
            let frames = &h.state().frames[frames_before..];
            let together = frames.iter().find(|f| f.contains(&other_ends));
            assert_eq!(
                together,
                Some(&vec![text_ends.clone(), other_ends.clone()]),
                "{focused} then {kind}: {frames:?}"
            );
        }
    }
}

#[test]
fn tabbing_from_one_text_field_to_another_ends_the_first_gesture_exactly_once() {
    let mut h = rig(&schema(json!({"type": "object", "properties": {
        "a": {"title": "First", "type": "string"},
        "b": {"title": "Second", "type": "integer", "format": "int64"}
    }})));
    focus(&mut h, Role::TextInput, "First");
    h.event(Event::Text("x".to_owned()));
    h.run_steps(3);
    press(&mut h, Modifiers::NONE, Key::Tab);
    h.run_steps(3);
    h.run_steps(3);
    assert_eq!(h.state().edits.last(), Some(&("/a".to_owned(), EditPhase::Commit)));
    focus(&mut h, Role::TextInput, "Second");
    h.event(Event::Text("5".to_owned()));
    h.run_steps(3);
    press(&mut h, Modifiers::NONE, Key::Tab);
    h.run_steps(3);
    let count = |pointer: &str, phase| h.state().edits.iter().filter(|e| **e == (pointer.to_owned(), phase)).count();
    let commits = |pointer: &str| count(pointer, EditPhase::Commit);
    assert_eq!((commits("/a"), commits("/b")), (1, 1), "each gesture ends once: {:?}", h.state().edits);
    let previews = |pointer: &str| count(pointer, EditPhase::Preview);
    assert!(previews("/a") > 0 && previews("/b") > 0, "{:?}", h.state().edits);
    assert_eq!(json_of(&h), json!({"a": "x", "b": 5}));
}

// ---- #2030's verification findings ----

/// Replaces the text of the focused field with `text` and leaves it.
fn retype(h: &mut Harness<'_, Rig>, label: &str, text: &str) {
    focus(h, Role::TextInput, label);
    press(h, Modifiers::COMMAND, Key::A);
    h.event(Event::Text(text.to_owned()));
    h.run_steps(3);
}

#[test]
fn a_typed_integer_beyond_two_to_the_53_is_parsed_exactly_or_refused() {
    let mut h = rig(&one_field(json!({"title": "Seed", "type": "integer", "format": "int64"})));
    assert!(h.query_by_role_and_label(Role::TextInput, "Seed").is_some(), "an i64 is typed, not slid");
    retype(&mut h, "Seed", "9007199254740993");
    assert_eq!(json_of(&h), json!({"field": 9_007_199_254_740_993_i64}), "2^53 + 1, not rounded to 2^53");
    retype(&mut h, "Seed", "9223372036854775807");
    assert_eq!(json_of(&h), json!({"field": i64::MAX}));

    for (typed, says) in [("12.5", "whole number"), ("1e20", "whole number"), ("9223372036854775808", "at most")] {
        retype(&mut h, "Seed", typed);
        assert_eq!(json_of(&h), json!({"field": i64::MAX}), "{typed} leaves the value alone");
        assert!(h.query_by_label_contains(says).is_some(), "{typed} is refused with a message saying {says}");
    }
    press(&mut h, Modifiers::NONE, Key::Tab);
    assert!(h.query_by_label_contains("whole number").is_none(), "leaving the field puts the value's text back");

    let mut big = rig(&one_field(json!({"title": "Id", "type": "integer", "minimum": 0, "format": "uint64"})));
    retype(&mut big, "Id", "18446744073709551615");
    assert_eq!(json_of(&big), json!({"field": u64::MAX}));
    retype(&mut big, "Id", "-1");
    assert!(big.query_by_label_contains("at least 0").is_some());
    assert_eq!(json_of(&big), json!({"field": u64::MAX}));
}

// ---- the end of a gesture ----

/// The phases the form reported for `/field`, in order.
fn phases(h: &Harness<'_, Rig>) -> Vec<EditPhase> {
    h.state().edits.iter().filter(|(p, _)| p == "/field").map(|(_, phase)| *phase).collect()
}

#[test]
fn leaving_an_integer_field_on_text_that_is_not_a_number_still_ends_the_gesture() {
    let mut h = rig(&one_field(json!({"title": "Seed", "type": "integer", "format": "int64"})));
    focus(&mut h, Role::TextInput, "Seed");
    press(&mut h, Modifiers::COMMAND, Key::A);
    h.event(Event::Text("5".to_owned()));
    h.run_steps(3);
    assert_eq!(phases(&h).last(), Some(&EditPhase::Preview), "a valid number previews");
    // More text that makes it invalid, then Tab out: the gesture the preview opened must end.
    h.event(Event::Text("x".to_owned()));
    h.run_steps(3);
    press(&mut h, Modifiers::NONE, Key::Tab);
    h.run_steps(3);
    assert_eq!(phases(&h).last(), Some(&EditPhase::Commit), "blur ends the gesture: {:?}", phases(&h));
    assert_eq!(json_of(&h), json!({"field": 5}), "with the last valid value");
}

#[test]
fn escape_in_a_text_or_integer_field_puts_the_value_back_and_cancels() {
    // A text field: Escape used to clear it and preview the deletion.
    let text = one_field(json!({"title": "Name", "type": "string"}));
    let mut h = rig(&text);
    let value = h.state().form.value_from_json(&json!({"field": "abc"}));
    h.state_mut().value = value;
    h.run();
    focus(&mut h, Role::TextInput, "Name");
    press(&mut h, Modifiers::COMMAND, Key::A);
    h.event(Event::Text("xyz".to_owned()));
    h.run_steps(3);
    assert_eq!(json_of(&h), json!({"field": "xyz"}));
    press(&mut h, Modifiers::NONE, Key::Escape);
    h.run_steps(3);
    assert_eq!(json_of(&h), json!({"field": "abc"}), "the original text, not an empty one");
    assert_eq!(phases(&h).last(), Some(&EditPhase::Cancel));

    // An exact integer field.
    let int = one_field(json!({"title": "Seed", "type": "integer", "format": "int64"}));
    let mut h = rig(&int);
    let value = h.state().form.value_from_json(&json!({"field": 7}));
    h.state_mut().value = value;
    h.run();
    focus(&mut h, Role::TextInput, "Seed");
    press(&mut h, Modifiers::COMMAND, Key::A);
    h.event(Event::Text("42".to_owned()));
    h.run_steps(3);
    assert_eq!(json_of(&h), json!({"field": 42}));
    press(&mut h, Modifiers::NONE, Key::Escape);
    h.run_steps(3);
    assert_eq!(json_of(&h), json!({"field": 7}), "Escape puts the value back");
    assert_eq!(phases(&h).last(), Some(&EditPhase::Cancel));
}

#[test]
fn a_curve_field_disables_its_tangent_so_tangents_cannot_change_through_the_form() {
    let mut h = rig(&one_field(json!({
        "title": "Response", "type": "array", "items": {"type": "number"}, "x-ui": {"widget": "curve"}
    })));
    focus(&mut h, Role::Slider, "Response, point 1 of 2");
    h.run_steps(2);
    let tangent = h.get_by_role_and_label(Role::SpinButton, "Tangent");
    assert!(tangent.accesskit_node().is_disabled(), "the array carries points only, so there is no tangent to edit");
    assert_eq!(
        tangent.accesskit_node().description().as_deref(),
        Some("Tangents are not saved in this field"),
        "and it says why"
    );
    // Clicking it and typing changes nothing: the curve the form holds still has automatic tangents throughout.
    tangent.click();
    h.run_steps(3);
    assert!(h.query_by_role_and_label(Role::TextInput, "Tangent value").is_none(), "a disabled field does not edit");
    press(&mut h, Modifiers::CTRL, Key::Backspace);
    h.run_steps(2);
    let FormValue::Object(fields) = &h.state().value else { panic!("an object") };
    let FormValue::Curve(curve) = &fields[0].1 else { panic!("a curve") };
    assert!(curve.points().iter().all(|p| p.tangent.is_none()), "{:?}", curve.points());
    assert_eq!(json_of(&h), json!({"field": [0, 0, 1, 1]}), "what is shown is what is saved");
    assert!(h.state().edits.is_empty(), "no edit was reported: {:?}", h.state().edits);
}

#[test]
fn an_integer_a_float_cannot_hold_stays_raw_in_a_number_field() {
    let form = Form::from_schema(&one_field(json!({"type": "number"})));
    for given in [json!(9_007_199_254_740_993_u64), json!(u64::MAX), json!(i64::MIN + 1)] {
        let value = form.value_from_json(&json!({"field": given}));
        assert!(matches!(&value, FormValue::Object(f) if matches!(f[0].1, FormValue::Raw(_))), "{given}");
        assert_eq!(Value::from(&value), json!({"field": given}), "{given} passes through unchanged");
    }
    let exact = form.value_from_json(&json!({"field": 9_007_199_254_740_992_u64}));
    assert!(matches!(&exact, FormValue::Object(f) if matches!(f[0].1, FormValue::Number(_))), "2^53 is exact");
}

#[test]
fn equal_inclusive_bounds_allow_their_one_value_and_only_an_empty_range_is_read_only() {
    for (field, one) in [
        (json!({"type": "number", "minimum": 2.5, "maximum": 2.5}), json!(2.5)),
        (json!({"type": "number", "minimum": 2.5, "maximum": 2.5, "default": 9}), json!(2.5)),
        (json!({"type": "integer", "minimum": 4, "maximum": 4}), json!(4)),
    ] {
        let form = Form::from_schema(&one_field(field.clone()));
        assert_eq!(Value::from(&form.default_value()), json!({"field": one}), "{field}: the one value is the default");
        let value = form.value_from_json(&json!({"field": one}));
        assert!(!matches!(&value, FormValue::Object(f) if matches!(f[0].1, FormValue::Raw(_))), "{field}");
    }
    let h = rig(&one_field(json!({"title": "Pinned", "type": "number", "minimum": 1, "maximum": 1})));
    assert!(h.query_by_role_and_label(Role::ComboBox, "Pinned").is_some(), "a choice of its one value, not read-only");
    let form = Form::from_schema(&one_field(json!({"type": "number", "minimum": 1, "maximum": 1})));
    let whole = form.value_from_json(&json!({"field": 1.0}));
    assert!(!matches!(&whole, FormValue::Object(f) if matches!(f[0].1, FormValue::Raw(_))), "1.0 is the value 1");
    for empty in [
        json!({"type": "number", "minimum": 2, "maximum": 1}),
        json!({"type": "number", "exclusiveMinimum": 1, "maximum": 1}),
        json!({"type": "number", "minimum": 1, "exclusiveMaximum": 1}),
        json!({"type": "integer", "minimum": 2, "maximum": 1}),
        json!({"type": "integer", "exclusiveMinimum": 1, "maximum": 1}),
    ] {
        let form = Form::from_schema(&one_field(empty.clone()));
        assert!(matches!(form.default_value(), FormValue::Object(f) if f[0].1 == FormValue::Absent), "{empty}");
    }
}

#[test]
fn a_string_with_a_pattern_is_a_validated_field_that_says_so_through_the_strings_table() {
    let schema = one_field(
        json!({"title": "Slug", "type": "string", "pattern": "^[a-z0-9-]+$", "x-ui": {"default": "hair-front"}}),
    );
    let mut h = rig(&schema);
    assert_eq!(h.state().form.invalid_count(&h.state().value), 0);
    assert!(h.query_by_label_contains("match the pattern").is_none());

    focus(&mut h, Role::TextInput, "Slug");
    h.event(Event::Text("Two Words".to_owned()));
    h.run_steps(3);
    assert_eq!(json_of(&h), json!({"field": "Two Words"}), "the text stays as typed so it can be fixed");
    assert_eq!(h.state().form.invalid_count(&h.state().value), 1);
    assert!(h.query_by_label_contains("match the pattern ^[a-z0-9-]+$").is_some(), "the message names the pattern");

    set_strings(&h.ctx, Arc::new(Pseudo(English)));
    h.run_steps(3);
    assert!(
        h.query_by_label_contains("match the pattern").is_none(),
        "no English is hard-coded: the message is translated"
    );
}

#[test]
fn a_pattern_that_cannot_be_evaluated_is_read_only_rather_than_unchecked() {
    let h = rig(&one_field(json!({"title": "Id", "type": "string", "pattern": "^(?=a)b$"})));
    assert!(h.query_by_role_and_label(Role::TextInput, "Id").is_none());
    assert!(h.query_by_label_contains("Not editable here (string with an unsupported pattern)").is_some());
}

#[test]
fn a_pattern_with_groups_and_alternation_is_checked() {
    let colour = "^(#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?|@[a-z0-9][a-z0-9_-]{0,63})$";
    let mut h = rig(&one_field(json!({"title": "Colour", "type": "string", "pattern": colour})));
    assert_eq!(h.state().form.invalid_count(&h.state().value), 1, "empty is neither a colour nor a reference");
    focus(&mut h, Role::TextInput, "Colour");
    h.event(Event::Text("@skin".to_owned()));
    h.run();
    assert_eq!(h.state().form.invalid_count(&h.state().value), 0);
    assert_eq!(json_of(&h), json!({"field": "@skin"}));
}

#[test]
fn a_plain_string_and_an_unbounded_number_get_a_text_field_and_a_number_field() {
    let mut h = rig(&schema(json!({"type": "object", "properties": {
        "name": {"type": "string"},
        "count": {"type": "number", "minimum": 0}
    }})));
    assert!(h.query_by_role_and_label(Role::TextInput, "name").is_some());
    focus(&mut h, Role::SpinButton, "count");
    press(&mut h, Modifiers::NONE, Key::ArrowUp);
    assert_eq!(json_of(&h), json!({"name": "", "count": 1}));
}

#[test]
fn a_nested_object_is_a_section_of_its_own_fields() {
    let mut h = rig(&schema(json!({"type": "object", "properties": {
        "eyes": {"title": "Eyes", "type": "object", "properties": {
            "size": {"title": "Size", "type": "number", "minimum": 0, "maximum": 1, "default": 0.4},
            "glint": {"title": "Glint", "type": "boolean"}
        }}
    }})));
    assert!(h.query_by_role_and_label(Role::DisclosureTriangle, "Eyes").is_some());
    focus(&mut h, Role::Slider, "Size");
    press(&mut h, Modifiers::NONE, Key::ArrowLeft);
    let Value::Number(size) = &json_of(&h)["eyes"]["size"] else { panic!("a number") };
    assert!(size.as_f64().is_some_and(|v| v < 0.4));
    assert_eq!(json_of(&h)["eyes"]["glint"], json!(false));
}

#[test]
fn a_nested_object_marked_advanced_starts_folded_and_opens_on_a_click() {
    let mut h = rig(&schema(json!({"type": "object", "properties": {
        "ears": {"title": "Ears", "type": "object", "properties": {
            "size": {"title": "Size", "type": "number", "minimum": 0, "maximum": 2, "default": 1},
            "animal": {"title": "Animal", "type": "object", "x-ui": {"level": "advanced"}, "properties": {
                "round": {"title": "Round", "type": "number", "minimum": 0, "maximum": 1, "default": 0.5}
            }}
        }}
    }})));
    assert!(h.query_by_role_and_label(Role::Slider, "Size").is_some(), "the section's own fields show");
    assert!(h.query_by_role_and_label(Role::Slider, "Round").is_none(), "the advanced section starts folded");
    h.get_by_role_and_label(Role::DisclosureTriangle, "Animal").click();
    h.run();
    assert!(h.query_by_role_and_label(Role::Slider, "Round").is_some(), "a click opens it");
    assert_eq!(json_of(&h)["ears"]["animal"]["round"], json!(0.5), "a folded section keeps its value");
}

// ---- failing closed ----

#[test]
fn a_shape_with_no_control_of_its_own_is_edited_as_json_and_its_value_survives() {
    let schema = schema(json!({"type": "object", "properties": {
        "keep": {"title": "Keep", "type": "boolean"},
        "tags": {"title": "Tags", "type": "array", "items": {"type": "string"}},
        "either": {"title": "Either", "oneOf": [{"type": "string"}, {"type": "integer"}]},
        "loop": {"title": "Loop", "$ref": "#/$defs/Loop"}
    }, "$defs": {"Loop": {"$ref": "#/$defs/Loop"}}}));
    let form = Form::from_schema(&schema);
    assert_eq!(form.read_only(), vec![("/loop".to_owned(), "unresolved reference".to_owned())]);
    let given = json!({"keep": true, "tags": ["a", "b"], "either": 3, "loop": 7});
    let value = form.value_from_json(&given);
    let mut h = harness_state(Settings::default(), vec2(420.0, 600.0), (form, value), |ui, s| {
        SchemaForm::new(&s.0, &mut s.1).show(ui);
    });
    assert!(h.query_by_role_and_label(Role::TextInput, "Tags").is_some(), "a list is typed as JSON");
    assert!(h.query_by_role_and_label(Role::TextInput, "Either").is_some(), "so is a union");
    assert!(h.query_by_label_contains("Not editable here (unresolved reference)").is_some());
    h.get_by_role_and_label(Role::Switch, "Keep").focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::Space);
    let after = Value::from(&h.state().1);
    assert_eq!(
        after,
        json!({"keep": false, "tags": ["a", "b"], "either": 3, "loop": 7}),
        "nothing is dropped or rewritten"
    );
}

#[test]
fn a_json_field_holds_the_form_back_until_its_text_parses() {
    let mut h = rig(&one_field(json!({"title": "Either", "oneOf": [{"type": "string"}, {"type": "integer"}]})));
    assert_eq!(h.state().form.invalid_count(&h.state().value), 1, "empty is not JSON");
    focus(&mut h, Role::TextInput, "Either");
    h.event(Event::Text("\"c".to_owned()));
    h.run();
    assert_eq!(h.state().form.invalid_count(&h.state().value), 1, "an unclosed string is not JSON");
    h.event(Event::Text("\"".to_owned()));
    h.run();
    assert_eq!(h.state().form.invalid_count(&h.state().value), 0);
    assert_eq!(json_of(&h), json!({"field": "c"}));
}

#[test]
fn a_nullable_field_starts_unset_is_left_out_and_set_on_request() {
    let schema = schema(json!({"type": "object", "properties": {
        "name": {"title": "Name", "type": ["string", "null"]},
        "size": {"title": "Size", "anyOf": [{"type": "number", "minimum": 0, "maximum": 1}, {"type": "null"}]}
    }}));
    let form = Form::from_schema(&schema);
    let value = form.value_from_json(&json!({"name": null}));
    assert_eq!(Value::from(&value), json!({}), "an unset option is left out");
    let mut h = harness_state(Settings::default(), vec2(420.0, 400.0), (form, value), |ui, s| {
        SchemaForm::new(&s.0, &mut s.1).show(ui);
    });
    assert!(h.query_by_role_and_label(Role::TextInput, "Name").is_none(), "unset: no field yet");
    h.get_by_role_and_label(Role::Button, "Set Name").click();
    h.run_steps(2);
    assert!(h.query_by_role_and_label(Role::TextInput, "Name").is_some());
    assert_eq!(Value::from(&h.state().1), json!({"name": ""}));
    h.get_by_role_and_label(Role::Button, "Set Size").click();
    h.run_steps(2);
    assert!(h.query_by_role_and_label(Role::Slider, "Size").is_some());
    let after = Value::from(&h.state().1);
    assert_eq!(after["name"], json!(""));
    assert_eq!(after["size"].as_f64(), Some(0.0), "set to its lower bound");
}

#[test]
fn a_value_that_does_not_fit_its_field_is_kept_not_clamped() {
    let form = Form::from_schema(&one_field(json!({"title": "Width", "type": "number", "minimum": 0, "maximum": 1})));
    let value = form.value_from_json(&json!({"field": 7.5}));
    assert_eq!(value, FormValue::Object(vec![("field".to_owned(), FormValue::Raw(json!(7.5)))]));
    assert_eq!(Value::from(&value), json!({"field": 7.5}));
}

// ---- integers, bounds, lengths and defaults ----

fn round_trip(field: Value, given: &Value) -> (FormValue, Value) {
    let form = Form::from_schema(&one_field(field));
    let value = form.value_from_json(&json!({"field": given}));
    let out = Value::from(&value);
    (value, out["field"].clone())
}

#[test]
fn integers_keep_their_json_type_across_the_whole_i64_and_u64_range() {
    for given in [
        json!(0),
        json!(2_000_000_000),
        json!(4_294_967_295_u64),
        json!(9_007_199_254_740_993_i64),
        json!(i64::MAX),
        json!(i64::MIN),
        json!(u64::MAX),
        json!(-9_007_199_254_740_993_i64),
    ] {
        let (value, out) = round_trip(json!({"type": "integer"}), &given);
        assert!(matches!(&value, FormValue::Object(f) if matches!(f[0].1, FormValue::Integer(_))), "{given}");
        assert_eq!(out, given, "an integer comes back as the same integer");
        assert!(out.is_i64() || out.is_u64(), "{out} is an integer, not a float");
    }
    let (_, out) = round_trip(json!({"type": "integer", "minimum": 0, "maximum": u64::MAX}), &json!(u64::MAX));
    assert_eq!(out, json!(u64::MAX));
}

#[test]
fn a_one_sided_bound_is_a_number_field_that_keeps_the_bound() {
    // An integer with no upper bound can pass 2^53, so it is typed as text and parsed exactly; the arrows still step.
    let schema = one_field(json!({"title": "Seed", "type": "integer", "minimum": 5}));
    let mut h = rig(&schema);
    assert_eq!(json_of(&h), json!({"field": 5}), "the default respects the bound");
    assert!(h.query_by_role_and_label(Role::Slider, "Seed").is_none(), "one bound is not a slider");
    assert!(h.query_by_role_and_label(Role::TextInput, "Seed").is_some());
    let form = &h.state().form;
    let kept = form.value_from_json(&json!({"field": 4}));
    assert!(
        matches!(&kept, FormValue::Object(f) if matches!(f[0].1, FormValue::Raw(_))),
        "below the minimum is kept, not accepted"
    );
    let big = form.value_from_json(&json!({"field": 5_000_000_000_u64}));
    assert_eq!(Value::from(&big), json!({"field": 5_000_000_000_u64}), "no hidden upper limit");
    focus(&mut h, Role::TextInput, "Seed");
    press(&mut h, Modifiers::NONE, Key::ArrowDown);
    assert_eq!(json_of(&h), json!({"field": 5}), "arrow down stops at the minimum");
    press(&mut h, Modifiers::NONE, Key::ArrowUp);
    assert_eq!(json_of(&h), json!({"field": 6}), "arrow up steps by one");

    let float_bounded = rig(&one_field(json!({"title": "Free", "type": "number", "minimum": 5})));
    assert!(
        float_bounded.query_by_role_and_label(Role::SpinButton, "Free").is_some(),
        "a number keeps its spin button"
    );

    let only_max = Form::from_schema(&one_field(json!({"type": "number", "maximum": -2.5})));
    assert_eq!(Value::from(&only_max.default_value()), json!({"field": -2.5}));
    assert!(
        matches!(&only_max.value_from_json(&json!({"field": -2.0})), FormValue::Object(f) if matches!(f[0].1, FormValue::Raw(_)))
    );
}

#[test]
fn exclusive_bounds_are_exclusive() {
    let whole = Form::from_schema(&one_field(json!({"type": "integer", "exclusiveMinimum": 0, "maximum": 100})));
    assert_eq!(Value::from(&whole.default_value()), json!({"field": 1}));
    for (given, kept) in [(0, true), (1, false), (100, false), (101, true)] {
        let value = whole.value_from_json(&json!({"field": given}));
        assert_eq!(matches!(&value, FormValue::Object(f) if matches!(f[0].1, FormValue::Raw(_))), kept, "{given}");
    }
    let real = Form::from_schema(&one_field(json!({"type": "number", "exclusiveMinimum": 0, "exclusiveMaximum": 1})));
    let FormValue::Object(fields) = real.default_value() else { panic!("an object") };
    assert!(matches!(fields[0].1, FormValue::Number(n) if n > 0.0 && n < 1.0), "{:?}", fields[0].1);
    for (given, kept) in [(0.0, true), (1.0, true), (0.5, false)] {
        let value = real.value_from_json(&json!({"field": given}));
        assert_eq!(matches!(&value, FormValue::Object(f) if matches!(f[0].1, FormValue::Raw(_))), kept, "{given}");
    }
    let above = Form::from_schema(&one_field(json!({"type": "number", "exclusiveMinimum": 0})));
    let FormValue::Object(fields) = above.default_value() else { panic!("an object") };
    assert!(matches!(fields[0].1, FormValue::Number(n) if n > 0.0), "zero is excluded");
}

#[test]
fn string_lengths_are_enforced_and_unenforceable_assertions_fail_closed() {
    let mut h = rig(&one_field(json!({"title": "Name", "type": "string", "minLength": 3, "maxLength": 5})));
    assert_eq!(h.state().form.invalid_count(&h.state().value), 1, "empty is shorter than three");
    focus(&mut h, Role::TextInput, "Name");
    h.event(Event::Text("abc".to_owned()));
    h.run();
    assert_eq!(json_of(&h), json!({"field": "abc"}));
    assert_eq!(h.state().form.invalid_count(&h.state().value), 0);
    h.event(Event::Text("defg".to_owned()));
    h.run();
    assert_eq!(h.state().form.invalid_count(&h.state().value), 1, "seven characters is longer than five");
    assert!(h.query_by_label_contains("Use 3 to 5 characters").is_some());

    // Characters, not bytes.
    let form = Form::from_schema(&one_field(json!({"type": "string", "maxLength": 2})));
    assert_eq!(form.invalid_count(&form.value_from_json(&json!({"field": "éé"}))), 0);

    for (assertion, named) in [
        (json!({"format": "uuid"}), "Not editable here (string with format)"),
        (json!({"contentEncoding": "base64"}), "Not editable here (string with contentEncoding)"),
        (json!({"minLength": -1}), "Not editable here (string with an unsupported length)"),
    ] {
        let mut field = json!({"title": "Id", "type": "string"});
        field
            .as_object_mut()
            .unwrap_or_else(|| panic!("an object"))
            .extend(assertion.as_object().cloned().unwrap_or_default());
        let form = Form::from_schema(&one_field(field));
        let value = form.value_from_json(&json!({"field": "x"}));
        let mut h = harness_state(Settings::default(), vec2(420.0, 300.0), (form, value), |ui, s| {
            SchemaForm::new(&s.0, &mut s.1).show(ui);
        });
        h.run();
        assert!(h.query_by_label_contains(named).is_some(), "{named}");
        assert_eq!(Value::from(&h.state().1), json!({"field": "x"}));
    }
}

#[test]
fn a_declared_object_default_is_applied() {
    let schema = schema(json!({"type": "object", "properties": {
        "eye": {
            "type": "object",
            "default": {"size": 0.8},
            "properties": {
                "size": {"type": "number", "minimum": 0, "maximum": 1},
                "lids": {"type": "integer", "minimum": 0, "maximum": 40, "default": 4}
            }
        },
        "brow": {
            "type": "object",
            "x-ui": {"default": {"lift": 3}},
            "default": {"lift": 1},
            "properties": {"lift": {"type": "integer", "minimum": 0, "maximum": 40}}
        }
    }}));
    let form = Form::from_schema(&schema);
    assert_eq!(
        Value::from(&form.default_value()),
        json!({"eye": {"size": 0.8, "lids": 4}, "brow": {"lift": 3}}),
        "x-ui.default wins; a key the default leaves out takes the field's own default"
    );
}

// ---- levels, groups, the Essentials cap ----

fn many(count: usize, level: Option<&str>) -> Schema {
    let props: serde_json::Map<String, Value> = (0..count)
        .map(|i| {
            let mut field = json!({"title": format!("Field {i}"), "type": "number", "minimum": 0, "maximum": 1});
            if let Some(level) = level {
                field["x-ui"] = json!({"level": level});
            }
            (format!("f{i:02}"), field)
        })
        .collect();
    schema(json!({"type": "object", "properties": props}))
}

#[test]
fn at_most_seven_fields_are_essentials_and_the_rest_wait_behind_a_count() {
    let mut h = rig(&many(10, None));
    assert_eq!(h.state().form.demoted(), 10 - MAX_ESSENTIALS);
    assert!(h.query_by_role_and_label(Role::DisclosureTriangle, "Essentials").is_some());
    assert!(h.query_by_role_and_label(Role::DisclosureTriangle, "Advanced").is_some());
    assert!(
        h.query_by_label_contains("3 more").is_none(),
        "the badge is painted text, not a node: the section is collapsed"
    );
    assert_eq!(h.get_all_by_role(Role::Slider).count(), MAX_ESSENTIALS, "Advanced is collapsed");
    h.get_by_role_and_label(Role::DisclosureTriangle, "Advanced").click();
    h.run_steps(5);
    assert_eq!(h.get_all_by_role(Role::Slider).count(), 10);
}

#[test]
fn a_form_within_the_cap_has_no_section_headers() {
    let h = rig(&many(3, None));
    assert_eq!(h.state().form.demoted(), 0);
    assert!(h.query_by_role_and_label(Role::DisclosureTriangle, "Essentials").is_none());
    assert_eq!(h.get_all_by_role(Role::Slider).count(), 3);
}

#[test]
fn x_ui_level_and_group_place_fields() {
    let h = rig(&schema(json!({"type": "object", "properties": {
        "a": {"title": "A", "type": "boolean"},
        "b": {"title": "B", "type": "boolean", "x-ui": {"group": "Look"}},
        "c": {"title": "C", "type": "boolean", "x-ui": {"group": "Look"}},
        "d": {"title": "D", "type": "boolean", "x-ui": {"level": "advanced"}}
    }})));
    assert!(h.query_by_role_and_label(Role::DisclosureTriangle, "Look").is_some());
    assert!(h.query_by_role_and_label(Role::Switch, "B").is_some());
    assert!(h.query_by_role_and_label(Role::Switch, "D").is_none(), "advanced fields start collapsed");
    assert_eq!(h.state().form.demoted(), 0);
}

// ---- accessible names ----

#[test]
fn every_generated_control_has_an_accessible_name_and_its_own_tab_stop() {
    let h = rig(&schema(json!({"type": "object", "properties": {
        "width": {"title": "Width", "type": "number", "minimum": 0, "maximum": 1},
        "count": {"title": "Count", "type": "integer", "minimum": 1, "maximum": 3},
        "shape": {"title": "Shape", "enum": ["a", "b", "c", "d", "e", "f"]},
        "on": {"title": "On", "type": "boolean"},
        "slug": {"title": "Slug", "type": "string", "pattern": "^[a-z]+$"},
        "free": {"title": "Free", "type": "number"},
        "nested": {"title": "Nested", "type": "object", "properties": {"inner": {"title": "Inner", "type": "boolean"}}},
        "odd": {"title": "Odd", "type": "array", "items": {"type": "string"}}
    }})));
    let mut h = h;
    h.get_by_role_and_label(Role::DisclosureTriangle, "Advanced").click();
    h.run_steps(5);
    assert!(unlabelled_interactive(&h).is_empty(), "{:?}", unlabelled_interactive(&h));
    for (role, label) in [
        (Role::Slider, "Width"),
        (Role::RadioGroup, "Count"),
        (Role::RadioButton, "2"),
        (Role::ComboBox, "Shape"),
        (Role::Switch, "On"),
        (Role::TextInput, "Slug"),
        (Role::SpinButton, "Free"),
        (Role::Switch, "Inner"),
    ] {
        assert!(h.query_by_role_and_label(role, label).is_some(), "{role:?} {label}");
    }
}

#[test]
fn defaults_and_json_round_trip() {
    let schema = schema(json!({"type": "object", "properties": {
        "width": {"type": "number", "minimum": 0, "maximum": 1, "default": 0.25},
        "style": {"enum": ["flat", "soft"]},
        "on": {"type": "boolean", "default": true},
        "name": {"type": "string", "default": "x"}
    }}));
    let form = Form::from_schema(&schema);
    let value = form.default_value();
    let json = Value::from(&value);
    assert_eq!(json, json!({"width": 0.25, "style": "flat", "on": true, "name": "x"}));
    assert_eq!(form.value_from_json(&json), value, "reading the JSON back gives the same typed value");
}

// ---- no allocation that scales with the form ----

struct Counting;

thread_local! {
    // `const` init with no destructor, so reading them never allocates and they are safe inside the allocator.
    static MEASURING: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            let _ = COUNT.try_with(|c| c.set(c.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// The kinds of field a form has, fourteen of them.
fn section_fields() -> Value {
    json!({
        "a": {"type": "number", "minimum": 0, "maximum": 1, "default": 0.5},
        "b": {"type": "number", "minimum": 0, "maximum": 1, "default": 0.5},
        "c": {"type": "integer", "minimum": 1, "maximum": 3, "default": 1},
        "d": {"enum": ["p", "q", "r"], "default": "p"},
        "e": {"enum": ["p", "q", "r", "s", "t", "u"], "default": "p"},
        "f": {"type": "boolean", "default": false},
        "g": {"type": "boolean", "default": true},
        "j": {"type": "string", "default": "text"},
        "k": {"type": "string", "pattern": "^[a-z]+$", "default": "slug"},
        "l": {"type": "number", "default": 3},
        "m": {"type": "integer", "minimum": 0, "maximum": 40, "default": 4},
        "n": {"type": "string", "x-ui": {"widget": "colour"}, "default": "#336699"},
        "o": {"type": "array", "items": {"type": "number"}, "x-ui": {"widget": "curve"}, "default": [0, 0, 1, 1]},
        "p": {"type": "integer", "format": "int64", "default": 7}
    })
}

/// A form of four nested objects (every section open), each holding `per_section` fields cycling through the kinds in
/// `keys`. The section count stays fixed: a `Section` has a cost of its own, and what is measured is the fields.
fn four_sections(per_section: usize, keys: &[&str]) -> Form {
    let Value::Object(all) = section_fields() else { panic!("an object") };
    let fields: serde_json::Map<String, Value> = (0..per_section)
        .filter_map(|i| {
            let key = keys[i % keys.len()];
            all.get(key).map(|template| (format!("{key}{i}"), template.clone()))
        })
        .collect();
    let props: serde_json::Map<String, Value> =
        (0..4).map(|i| (format!("s{i}"), json!({"type": "object", "properties": fields}))).collect();
    Form::from_schema(&schema(json!({"type": "object", "properties": props})))
}

/// Moves every field away from its default, so the rows are drawn with values other than their starting ones.
fn perturb(value: &mut FormValue) {
    match value {
        FormValue::Object(fields) => fields.iter_mut().for_each(|(_, v)| perturb(v)),
        FormValue::Number(n) => *n += 1.0,
        FormValue::Integer(n) => *n += 1,
        FormValue::Bool(on) => *on = !*on,
        FormValue::Text(text) | FormValue::Json(text) => text.push('x'),
        FormValue::Choice { index, value } => {
            *index = 1;
            *value = json!("q");
        }
        FormValue::Colour(colour) => *colour = Colour::WHITE,
        FormValue::Curve(curve) => curve.apply_preset(Preset::EaseIn),
        FormValue::Raw(_) | FormValue::Absent => {}
    }
}

/// Allocations per frame in steady state for whatever `scene` draws.
fn measure(mut scene: impl FnMut(&mut egui::Ui)) -> f64 {
    let ctx = Context::default();
    apply_settings(&ctx, Settings::default());
    let mut time = 0.0;
    let mut run = |time: f64| {
        let input = RawInput {
            time: Some(time),
            predicted_dt: 1.0 / 60.0,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(480.0, 40_000.0))),
            ..RawInput::default()
        };
        ctx.run_ui(input, &mut scene).drop_without_applying_deltas();
    };
    // Warm up: fonts activate, glyphs rasterise, the text cache and egui's maps reach their working size.
    for _ in 0..40 {
        time += 1.0 / 60.0;
        run(time);
    }
    let before = COUNT.with(Cell::get);
    MEASURING.with(|m| m.set(true));
    for _ in 0..30 {
        time += 1.0 / 60.0;
        run(time);
    }
    MEASURING.with(|m| m.set(false));
    (COUNT.with(Cell::get) - before) as f64 / 30.0
}

/// Allocations per frame for `form`, at its defaults or moved off them.
fn per_frame(form: &Form, modified: bool) -> f64 {
    let mut value = form.default_value();
    if modified {
        perturb(&mut value);
    }
    measure(|ui| {
        SchemaForm::new(form, &mut value).show(ui);
    })
}

/// Allocations per frame for `count` of the widget the form uses for the field `key`, drawn by hand in a `FormRow`.
fn by_hand(key: &str, count: usize) -> f64 {
    let (mut texts, mut picks) = (vec![String::from("slug"); count], vec![0usize; count]);
    let mut numbers = vec![3.0; count];
    let mut colours = vec![Colour::hex(0x33_66_99); count];
    let mut curves = vec![Curve::linear(); count];
    let (three, six) = ([("p", 0usize), ("q", 1), ("r", 2)], ["p", "q", "r", "s", "t", "u"]);
    measure(|ui| {
        for i in 0..count {
            FormRow::new("Field").show(ui, |ui, name| match key {
                "segmented" => {
                    let segments = three.map(|(label, at)| Segment::new(at, label));
                    ui.add(Segmented::new(name, &mut picks[i], &segments).id_salt(i));
                }
                "dropdown" => {
                    let choices: Vec<_> = six.iter().enumerate().map(|(at, label)| Choice::new(at, label)).collect();
                    ui.add(Dropdown::new(name, &mut picks[i], &choices).id_salt(i).stretch());
                }
                "text" => {
                    ui.add(TextField::new(name, &mut texts[i]).id_salt(i));
                }
                "colour" => {
                    ColourField::new(name, &mut colours[i]).id_salt(i).show(ui);
                }
                "curve" => {
                    CurveEditor::new(name, &mut curves[i]).id_salt(i).show(ui);
                }
                _ => {
                    NumberField::new(name, &mut numbers[i], -1.0e9..=1.0e9).id_salt(i).show(ui);
                }
            });
        }
    })
}

/// Inspector-typical fields whose widgets allocate nothing by themselves: sliders (whole and fractional), toggles.
const FLAT: [&str; 5] = ["a", "b", "m", "f", "g"];

#[test]
fn a_52_field_form_allocates_no_more_per_frame_than_a_24_field_one() {
    for modified in [false, true] {
        let keys = &FLAT[..];
        let (small, big) =
            (per_frame(&four_sections(6, keys), modified), per_frame(&four_sections(13, keys), modified));
        eprintln!(
            "schema form, modified={modified}: {small:.1} allocations/frame at 24 fields, {big:.1} at 52 (marginal {:+.3} per field)",
            (big - small) / 28.0
        );
        assert!(
            big - small <= 2.0,
            "the form allocates per field per frame: {small} at 24 fields, {big} at 52 (modified: {modified})"
        );
    }
}

/// `count` fields of the kind `key`, side by side in one flat form (no more than the Essentials cap, so all are drawn).
fn row_of(key: &str, count: usize) -> Form {
    let Value::Object(all) = section_fields() else { panic!("an object") };
    let template = all.get(key).cloned().unwrap_or(Value::Null);
    let props: serde_json::Map<String, Value> = (0..count).map(|i| (format!("f{i}"), template.clone())).collect();
    Form::from_schema(&schema(json!({"type": "object", "properties": props})))
}

/// What the form allocates per field beyond what the widget does by itself, which the form can't change: the `TextField`
/// allocates a dozen times a frame inside egui's `TextEdit`.
#[test]
fn the_form_adds_no_allocation_to_the_widgets_it_composes() {
    let marginal = |small: f64, big: f64| (big - small) / 4.0;
    for (key, widget) in [
        ("a", ""),
        ("b", ""),
        ("m", ""),
        ("c", "segmented"),
        ("e", "dropdown"),
        ("f", ""),
        ("j", "text"),
        ("k", "text"),
        ("l", "number"),
        ("n", "colour"),
        ("o", "curve"),
        ("p", "text"),
    ] {
        let at_default = marginal(per_frame(&row_of(key, 2), false), per_frame(&row_of(key, 6), false));
        let own = if widget.is_empty() { 0.0 } else { marginal(by_hand(widget, 2), by_hand(widget, 6)) };
        eprintln!("field {key}: form {at_default:+.2}; the widget alone {own:+.2}");
        assert!(
            at_default <= own + 0.01,
            "field {key} allocates {at_default} per frame; its widget alone allocates {own}"
        );
    }
}

// ---- usability: keyboard presses to make one edit ----

/// Key presses and typed characters spent so far.
struct Keys(usize);

impl Keys {
    fn press(&mut self, h: &mut Harness<'_, Rig>, key: Key) {
        self.0 += 1;
        press(h, Modifiers::NONE, key);
        h.run_steps(3);
    }

    fn type_text(&mut self, h: &mut Harness<'_, Rig>, text: &str) {
        self.0 += text.chars().count();
        h.event(Event::Text(text.to_owned()));
        h.run_steps(3);
    }
}

/// The fixture ops of the PR's usability table. Each edit starts with the form open and its field focused, which is
/// where the palette puts the keyboard (the palette's own Ctrl+K, query and Tab are the same for every op), and counts
/// every key press and typed character until the value has changed. The table in the PR body quotes these numbers.
#[test]
fn keyboard_steps_to_make_each_fixture_edit() {
    // `face.set_profile`: the face shape, one of six, from Egg to Oval.
    let mut h = rig(&schema(json!({"type": "object", "properties": {
        "shape": {"title": "Face shape", "enum": ["Egg", "Round", "Oval", "Square", "Long", "Heart"]}
    }})));
    focus(&mut h, Role::ComboBox, "Face shape");
    let mut keys = Keys(0);
    keys.press(&mut h, Key::Enter);
    keys.press(&mut h, Key::ArrowDown);
    keys.press(&mut h, Key::ArrowDown);
    keys.press(&mut h, Key::Enter);
    assert_eq!(json_of(&h), json!({"shape": "Oval"}));
    let (pick_enum, dropdown_steps) = ("face.set_profile: shape Egg to Oval", keys.0);

    // `eyes.set_size`: a 0 to 1 slider, from 0.42 to 0.55, typed.
    let mut h =
        rig(&one_field(json!({"title": "Eye size", "type": "number", "minimum": 0, "maximum": 1, "default": 0.42})));
    focus(&mut h, Role::Slider, "Eye size");
    let mut keys = Keys(0);
    keys.press(&mut h, Key::Enter);
    keys.type_text(&mut h, "0.55");
    keys.press(&mut h, Key::Enter);
    assert_eq!(json_of(&h), json!({"field": 0.55}));
    let (type_number, slider_steps) = ("eyes.set_size: size 0.42 to 0.55", keys.0);

    eprintln!("{pick_enum}: {dropdown_steps} presses");
    eprintln!("{type_number}: {slider_steps} presses");
    assert_eq!([dropdown_steps, slider_steps], [4, 6]);
}
