//! `SchemaForm`: a JSON Schema turned into ds controls, once, for the command palette and the inspector
//! (`shell.md` sections 7 and 8, principle P8: the UI cannot drift from MCP because it is generated from the same schema).
//!
//! ```text
//! let form = Form::from_schema(&op_info.schema);      // once per schema
//! let mut value = form.default_value();               // or form.value_from_json(&arguments)
//! // every frame:
//! let out = SchemaForm::new(&form, &mut value).id_salt("palette-args").show(ui);
//! if run_pressed && form.invalid_count(&value) == 0 { request.args = Value::from(&value); }
//! ```
//!
//! # The mapping
//!
//! | Schema | Control |
//! |---|---|
//! | `number`/`integer` with `minimum` and `maximum` | `ParamSlider` (`x-ui.step`, `x-ui.decimals`, `x-ui.unit`, a reset away from the default) |
//! | `integer` with five or fewer values; `enum`, or `oneOf` of `const`s, with two to five options | `Segmented` |
//! | an enum with one option or more than five | `Dropdown` |
//! | `boolean` | `Toggle` |
//! | `string` with `format` or `x-ui.widget` `colour`, or the hex pattern `Colour` publishes | `ColourField` |
//! | `number[]` with `x-ui.widget = "curve"`: x, y pairs over the unit square | `CurveEditor` |
//! | `string` with a `pattern` | `TextField` that shows its pattern as the error while it does not match |
//! | `string` | `TextField` |
//! | `number`/`integer` missing a bound | `NumberField` |
//! | `object` with `properties` | a `Section` |
//! | anything else (`null`-able types, unions, arrays, a `pattern` this matcher cannot evaluate) | a read-only row naming the type |
//!
//! `$ref` and `$defs` are followed. A field's label is its `title`, else its property name.
//!
//! # `x-ui.*`
//!
//! `level` (`"essentials"`, the default, or `"advanced"`), `group` (fields of the same group share a section),
//! `unit`, `step`, `decimals`, `default` (else the schema's `default`) and `widget`. At most [`MAX_ESSENTIALS`] fields
//! are Essentials: the rest are listed under Advanced and counted in [`Form::demoted`]. A form with no Advanced field
//! has no section headers. Nested objects are one section; their fields are listed in schema order.
//!
//! # What it promises
//!
//! - **Fails closed.** A shape with no control is a read-only row that names the type, and its value passes through
//!   [`FormValue::Raw`] unchanged. A value that does not fit its field is kept the same way.
//! - **Words come from the [`Strings`](crate::ds::Strings) table**; labels, titles and option names come from the
//!   schema, which is data.
//! - **Allocation does not grow with the form** (`tests/no_alloc.rs`), except for a read-only row (one string), a
//!   field that fails its pattern (one string), a dropdown of more than 32 options (one `Vec`), and the Advanced badge.

mod model;
mod pattern;
mod show;
mod value;

pub use model::{Form, Level, MAX_ESSENTIALS, MAX_SEGMENTS};
pub use show::{FormOutput, SchemaForm};
pub use value::FormValue;
