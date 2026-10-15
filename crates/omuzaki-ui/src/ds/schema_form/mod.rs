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
//! | `number`/`integer` with `minimum` and `maximum` | `ParamSlider` |
//! | `integer` with five or fewer values; `enum`, or `oneOf` of `const`s, with two to five options | `Segmented` |
//! | an enum with one option or more than five | `Dropdown` |
//! | `boolean` | `Toggle` |
//! | `string` with a `pattern`, `minLength` or `maxLength` | `TextField` that shows what it needs while the text does not satisfy it |
//! | `string` | `TextField` |
//! | `number`/`integer` with one bound, or none | `NumberField` that enforces the bound it has (exclusive ones as exclusive) |
//! | `object` with `properties` | a `Section` |
//! | anything else (`null`-able types, unions, arrays, a `pattern` this matcher cannot evaluate, a string with `format` or `contentEncoding`, `multipleOf`) | a read-only row naming the type |
//!
//! `$ref` and `$defs` are followed. A field's label is its `title`, else its property name.
//!
//! # `x-ui.*`
//!
//! `level` (`"essentials"`, the default, or `"advanced"`), `group` (fields of the same group share a section) and
//! `default` (else the schema's `default`, the field's first value). At most [`MAX_ESSENTIALS`] fields are Essentials: the rest
//! are listed under Advanced and counted in [`Form::demoted`]. A form with no Advanced field has no section headers. Nested objects are one section; their fields are listed in schema order.
//!
//! `unit`, `step`, `decimals`, the reset away from the default, `ColourField` and `CurveEditor` arrive in the second part
//! of #1920.
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
//!   `u64`, so every value the schema allows round-trips exactly; a bound is enforced as written, exclusive or not.

mod model;
mod pattern;
mod show;
mod value;

pub use model::{Form, Level, MAX_ESSENTIALS, MAX_SEGMENTS};
pub use show::{FormOutput, SchemaForm};
pub use value::FormValue;
