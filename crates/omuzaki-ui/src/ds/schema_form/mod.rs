//! `SchemaForm`: a JSON Schema turned into ds controls, once, for the command palette and the inspector
//! (`shell.md` sections 7 and 8, principle P8: the UI cannot drift from MCP because it is generated from the same schema).
//!
//! ```text
//! let form = Form::from_schema(&op_info.schema);      // once per schema
//! let mut value = form.default_value();               // or form.value_from_json(&arguments)
//! // every frame:
//! let out = SchemaForm::new(&form, &mut value).id_salt("palette-args").show(ui);
//! if run_pressed && form.invalid_count(&value) == 0 { request.args = Value::from(&value); }
//! // or, one field at a time (the inspector), one gesture as one transaction:
//! for edit in out.edits { send(form.pointer(edit.field), form.field_value(&value, edit.field), edit.phase); }
//! ```
//!
//! # The mapping
//!
//! | Schema | Control |
//! |---|---|
//! | `number`/`integer` with two different bounds (an integer's within ±2^53) | `ParamSlider` |
//! | `integer` with five or fewer values; `enum`, or `oneOf` of `const`s, with two to five options | `Segmented` |
//! | an enum with one option or more than five; an integer with one value | `Dropdown` |
//! | `boolean` | `Toggle` |
//! | `string` with `x-ui.widget` or `format` `"colour"` (or `"color"`), or the pattern `Colour` publishes | `ColourField` (alpha when the pattern allows it) |
//! | `number[]` with `x-ui.widget = "curve"`: `[x0, y0, x1, y1, …]` over the unit square | `CurveEditor` |
//! | `string` with a `pattern`, `minLength` or `maxLength` | `TextField` that shows what it needs while the text does not satisfy it |
//! | `string` | `TextField` |
//! | `number` with one bound, none, or two equal ones; `integer` whose values are all exact as floats | `NumberField` that enforces the bounds it has (exclusive ones as exclusive) |
//! | `integer` that can hold values beyond ±2^53 (`int64`, `uint64`, unbounded) | `TextField` parsed as an integer, exactly, refusing what is not a whole number within the bounds |
//! | `object` with `properties` | a `Section` |
//! | anything else (`null`-able types, unions, arrays, a `pattern` this matcher cannot evaluate, a string with another `format` or a `contentEncoding`, `multipleOf`) | a read-only row naming the type |
//!
//! `$ref` and `$defs` are followed. A field's label is its `title`, else its property name.
//!
//! # `x-ui.*`
//!
//! - `level` (`"essentials"`, the default, or `"advanced"`) and `group` (fields of the same group share a section). At
//!   most [`MAX_ESSENTIALS`] fields are Essentials: the rest are listed under Advanced and counted in
//!   [`Form::demoted`]. A form with no Advanced field has no section headers. Nested objects are one section; their
//!   fields are listed in schema order. A nested object marked `"advanced"` starts folded.
//! - `default` (else the schema's `default`, else the field's natural first value). A field with a declared default
//!   shows the modified dot and a reset while its value differs from it; the reset's space is kept while it is hidden.
//! - `unit`, `step` and `decimals` for number fields (an integer steps by a whole number and shows no decimals).
//! - `widget`: `"colour"` and `"curve"`, as in the table.
//!
//! # Gestures
//!
//! [`FormOutput::edits`] names each field edited this frame and where its gesture is ([`EditPhase`]). A host makes one
//! gesture one transaction (`shell.md` section 7). Every control follows the same life cycle, and every input that can end a
//! gesture is in this table:
//!
//! | Control | Starts and goes on (`Preview`) | Ends (`Commit`) | Esc (`Cancel`) | Leaves the field (Tab, click away) |
//! |---|---|---|---|---|
//! | slider | press and drag, or a held key | release; a key press is one step | puts the value back first | n/a |
//! | colour field | the picker moving | the picker closed | puts the value back first | n/a |
//! | curve editor | a point dragged | release | puts the curve back first | n/a |
//! | text field | every changed keystroke | Enter or leaving the field | puts back the text the field had when it took focus | commits the text as it is |
//! | exact integer (text) | a keystroke that makes an allowed number | Enter or leaving, with the last allowed number even if the text no longer parses | puts back the number the field had when it took focus | commits the last allowed number, and the text goes back to it |
//! | number field, choice, segmented, toggle | none: one step | the change | none | none |
//! | reset (any row) | none | the default | none | none |
//!
//! The rules behind the table, which the tests in `tests/schema_form.rs` check:
//!
//! 1. A `Preview` is always followed by exactly one `Commit` or `Cancel` of the same field: no way out of a control
//!    (blur, Enter, release, Esc) leaves the host's transaction open.
//! 2. A `Cancel` is reported after the value is back to what it was when the gesture began, and the blur that Esc
//!    causes reports nothing more.
//! 3. A value the field would refuse (text that is not an allowed number, a tangent the array has no place for) is
//!    never previewed or committed; the form holds the last value that can be saved, so what is shown is what is sent.
//! 4. Every field that reports in a frame gets its own entry in [`FormOutput::edits`]; none replaces or merges into
//!    another. A gesture's end that another input caused comes before that input's edit (a text field's blur is
//!    reported when the pointer is released), then the rest in the order the fields are drawn. So a host that handles
//!    them in order closes the old gesture before it applies the next edit.
//!
//! What can share a frame, and what [`FormOutput::edits`] then holds (`tests/schema_form.rs`,
//! `two_fields_in_one_frame_each_reach_their_own_edit_in_order`). "A" is a text or exact integer field with focus,
//! the one gesture that has no pointer of its own and so can be open while another control is used:
//!
//! | Input | Reports | Order in `edits` |
//! |---|---|---|
//! | release on a toggle, choice, segment, or press a reset, while A is focused | A: Commit; the other: Commit, in the same frame | A, then the other |
//! | press and drag a slider while A is focused | the other: Preview in the frames it moves; at the release A: Commit and the other: Commit | Preview of the other first; then A, then the other. A host takes another field's Preview as the end of A's gesture |
//! | Tab or click into another text or integer field | A: Commit, once (egui reports a lost focus in more than one frame; only an open gesture ends); the other reports nothing until it changes | A |
//! | Esc | A: Cancel; the blur it causes reports nothing | A |
//! | a drag or picker going on, and no other input | Preview | the one field |
//!
//! # What it promises
//!
//! - **Fails closed.** A shape with no control is a read-only row that names the type, and its value passes through
//!   [`FormValue::Raw`] unchanged. A value that does not fit its field is kept the same way.
//! - **Words come from the [`Strings`](crate::ds::Strings) table**; labels, titles and option names come from the
//!   schema, which is data.
//! - **Allocation does not grow with the form** (`tests/schema_form.rs`), except for a read-only row (one string), a
//!   field that fails its pattern or length (one string), a dropdown of more than 32 options (one `Vec`), and the
//!   Advanced badge. A `pattern` is matched when the text changes, not every frame, in time linear in the text.
//! - **Integers stay integers.** An `integer` field holds an `i128` ([`FormValue::Integer`]) and sends an `i64` or
//!   `u64`, so every value the schema allows round-trips exactly; a bound is enforced as written, exclusive or not. A
//!   typed integer goes through a float only where every value the field allows is exact as one; elsewhere it is
//!   parsed as an integer. An integer in a `number` field that a float cannot hold stays [`FormValue::Raw`].

mod model;
mod pattern;
mod show;
mod text;
mod value;

pub use model::{Form, Level, MAX_ESSENTIALS, MAX_SEGMENTS};
pub use show::{EditPhase, FieldEdit, FieldId, FormOutput, SchemaForm};
pub use value::FormValue;
