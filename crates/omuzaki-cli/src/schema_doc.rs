//! Documents an op's arguments from its JSON Schema: names, types, ranges and descriptions.
//!
//! Nothing here knows any op. `omuzaki help <op>` shows whatever the schema says, so a new op (or a new field on an
//! old one) is documented the moment it is registered. The schemas are the draft 2020-12 documents that
//! `omuzaki-ops-api` generates with every subschema inlined (no `$ref`), so the walk is a plain tree traversal.

use schemars::Schema;
use serde_json::{Map, Value};

use crate::json;

/// One documented argument. Nested object fields appear after their parent with a dotted name (`inner.a`), and
/// fields of array elements with `[]` (`list[].a`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArgDoc {
    /// The field name, dotted for nested fields.
    pub name: String,
    /// The type in words: `number`, `array of string`, `string or null`.
    pub ty: String,
    /// True if the call fails without it (a nested field counts only when every parent is required too).
    pub required: bool,
    /// The field's doc comment, first paragraph.
    pub description: Option<String>,
    /// Limits and defaults, one short phrase each: `range 0.0 to 1.0`, `default 0.5`, `pattern ^[a-z]+$`.
    pub constraints: Vec<String>,
    /// The allowed values, when the field is an enum.
    pub choices: Vec<Choice>,
}

/// One allowed value of an enum argument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    /// The value as written in JSON, e.g. `"Fast"`.
    pub value: String,
    /// The variant's doc comment, first paragraph.
    pub description: Option<String>,
}

/// The op's own description (the doc comment on its argument struct), when it has one.
pub fn description(schema: &Schema) -> Option<String> {
    schema.get("description").and_then(Value::as_str).map(|d| d.trim().to_owned()).filter(|d| !d.is_empty())
}

/// Every argument of the op, required ones first in declaration order, then optional ones alphabetically.
///
/// The order is explicit because `serde_json` maps are sorted or insertion-ordered depending on which crate in the
/// build enables `preserve_order`; the help text must not change with that.
pub fn arguments(schema: &Schema) -> Vec<ArgDoc> {
    let mut out = Vec::new();
    if let Some(root) = schema.as_object() {
        collect(root, "", true, &mut out);
    }
    out
}

fn collect(object: &Map<String, Value>, prefix: &str, parent_required: bool, out: &mut Vec<ArgDoc>) {
    let Some(properties) = object.get("properties").and_then(Value::as_object) else { return };
    let required: Vec<&str> = string_list(object.get("required"));
    let mut names: Vec<&str> = required.iter().copied().filter(|n| properties.contains_key(*n)).collect();
    let mut optional: Vec<&str> = properties.keys().map(String::as_str).filter(|k| !required.contains(k)).collect();
    optional.sort_unstable();
    names.extend(optional);

    for name in names {
        let schema = &properties[name];
        let is_required = parent_required && required.contains(&name);
        let shape = describe(schema);
        out.push(ArgDoc {
            name: format!("{prefix}{name}"),
            ty: shape.label(),
            required: is_required,
            description: shape.description,
            constraints: shape.constraints,
            choices: shape.choices,
        });
        if let Some(inner) = schema.as_object() {
            collect(inner, &format!("{prefix}{name}."), is_required, out);
            if let Some(item) = inner.get("items").and_then(Value::as_object) {
                collect(item, &format!("{prefix}{name}[]."), is_required, out);
            }
        }
    }
}

fn string_list(value: Option<&Value>) -> Vec<&str> {
    value.and_then(Value::as_array).map(|items| items.iter().filter_map(Value::as_str).collect()).unwrap_or_default()
}

/// What one (sub)schema says about a value.
#[derive(Default)]
struct Shape {
    ty: String,
    nullable: bool,
    description: Option<String>,
    constraints: Vec<String>,
    choices: Vec<Choice>,
}

impl Shape {
    fn label(&self) -> String {
        if self.nullable { format!("{} or null", self.ty) } else { self.ty.clone() }
    }
}

fn describe(schema: &Value) -> Shape {
    let Some(object) = schema.as_object() else {
        // `true` accepts anything (a `serde_json::Value` field); `false` accepts nothing.
        return Shape {
            ty: if schema == &Value::Bool(false) { "never" } else { "any" }.to_owned(),
            ..Shape::default()
        };
    };
    let mut shape = match object.get("oneOf").or_else(|| object.get("anyOf")).and_then(Value::as_array) {
        Some(variants) => describe_union(variants),
        None => describe_plain(object),
    };
    // A description on the field itself wins over one inherited from a variant.
    if let Some(own) = first_paragraph(object.get("description")) {
        shape.description = Some(own);
    }
    shape.constraints.extend(common_constraints(object));
    shape
}

fn describe_plain(object: &Map<String, Value>) -> Shape {
    let mut shape = Shape::default();
    let types = type_names(object.get("type"));
    shape.nullable = types.contains(&"null");
    let concrete: Vec<&str> = types.into_iter().filter(|t| *t != "null").collect();
    shape.ty = match concrete.as_slice() {
        [] if shape.nullable => "null".to_owned(),
        [] => "any".to_owned(),
        many => many.iter().map(|t| type_label(t, object)).collect::<Vec<_>>().join(" or "),
    };
    if let Some(values) = object.get("enum").and_then(Value::as_array) {
        shape.choices =
            values.iter().map(|v| Choice { value: json::render_value(v, false), description: None }).collect();
    } else if let Some(value) = object.get("const") {
        shape.choices = vec![Choice { value: json::render_value(value, false), description: None }];
    }
    shape.constraints = type_constraints(object);
    shape
}

fn describe_union(variants: &[Value]) -> Shape {
    let (nulls, rest): (Vec<&Value>, Vec<&Value>) = variants.iter().partition(|v| is_null_schema(v));
    let nullable = !nulls.is_empty();
    let mut shape = match rest.as_slice() {
        [] => Shape { ty: "null".to_owned(), ..Shape::default() },
        [only] => describe(only),
        many if many.iter().all(|v| v.get("const").is_some()) => enum_of_consts(many),
        many => {
            let mut labels: Vec<String> = many.iter().map(|v| describe(v).label()).collect();
            labels.dedup();
            Shape {
                ty: labels.join(" or "),
                constraints: vec![format!("{} variants; see --json for the full schema", many.len())],
                ..Shape::default()
            }
        }
    };
    shape.nullable |= nullable;
    shape
}

/// A unit-only Rust enum: `oneOf` of `{const, description}` entries.
fn enum_of_consts(variants: &[&Value]) -> Shape {
    let choices = variants
        .iter()
        .filter_map(|v| {
            Some(Choice {
                value: json::render_value(v.get("const")?, false),
                description: first_paragraph(v.get("description")),
            })
        })
        .collect();
    let ty = match variants.iter().filter_map(|v| v.get("type").and_then(Value::as_str)).collect::<Vec<_>>().as_slice()
    {
        [first, rest @ ..] if rest.iter().all(|t| t == first) => (*first).to_owned(),
        _ => "any".to_owned(),
    };
    Shape { ty, choices, ..Shape::default() }
}

fn is_null_schema(schema: &Value) -> bool {
    schema.get("type").and_then(Value::as_str) == Some("null")
}

fn type_names(value: Option<&Value>) -> Vec<&str> {
    match value {
        Some(Value::String(one)) => vec![one.as_str()],
        other => string_list(other),
    }
}

fn type_label(ty: &str, object: &Map<String, Value>) -> String {
    match ty {
        "array" => match (object.get("prefixItems").and_then(Value::as_array), object.get("items")) {
            (Some(items), _) => {
                format!("[{}]", items.iter().map(|i| describe(i).label()).collect::<Vec<_>>().join(", "))
            }
            (None, Some(items)) => format!("array of {}", describe(items).label()),
            (None, None) => "array".to_owned(),
        },
        "object" => match object.get("additionalProperties") {
            Some(values) if values.is_object() => format!("map of {}", describe(values).label()),
            _ => "object".to_owned(),
        },
        // Numeric formats (`float`, `uint32`) only repeat the Rust type; named string formats (`uuid`) are real.
        "string" => match object.get("format").and_then(Value::as_str) {
            Some(format) => format!("string ({format})"),
            None => "string".to_owned(),
        },
        other => other.to_owned(),
    }
}

/// Limits that belong to the value's own type.
fn type_constraints(object: &Map<String, Value>) -> Vec<String> {
    let mut out = Vec::new();
    out.extend(numeric_range(object));
    out.extend(bounds("length", object.get("minLength"), object.get("maxLength")));
    if let Some(pattern) = object.get("pattern").and_then(Value::as_str) {
        out.push(format!("pattern {pattern}"));
    }
    // A tuple's length is already in its type label.
    if !object.contains_key("prefixItems") {
        out.extend(bounds("items", object.get("minItems"), object.get("maxItems")));
    }
    if object.get("uniqueItems") == Some(&Value::Bool(true)) {
        out.push("items must be unique".to_owned());
    }
    out
}

/// Notes that apply to any value, whatever its type.
fn common_constraints(object: &Map<String, Value>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(default) = object.get("default") {
        out.push(format!("default {}", json::render_value(default, false)));
    }
    if object.get("deprecated") == Some(&Value::Bool(true)) {
        out.push("deprecated".to_owned());
    }
    out
}

/// `range 0.0 to 1.0`, or with open ends / exclusive limits `range >= 0`, `range > 0 and <= 1`.
fn numeric_range(object: &Map<String, Value>) -> Option<String> {
    let low = object.get("minimum").map(|v| (">=", v)).or_else(|| object.get("exclusiveMinimum").map(|v| (">", v)));
    let high = object.get("maximum").map(|v| ("<=", v)).or_else(|| object.get("exclusiveMaximum").map(|v| ("<", v)));
    match (low, high) {
        (Some((">=", a)), Some(("<=", b))) => Some(format!("range {a} to {b}")),
        (Some((lo_op, a)), Some((hi_op, b))) => Some(format!("range {lo_op} {a} and {hi_op} {b}")),
        (Some((op, v)), None) | (None, Some((op, v))) => Some(format!("range {op} {v}")),
        (None, None) => None,
    }
}

/// `length 1 to 10`, `length >= 1`, `items <= 4`.
fn bounds(what: &str, min: Option<&Value>, max: Option<&Value>) -> Option<String> {
    match (min, max) {
        (Some(a), Some(b)) => Some(format!("{what} {a} to {b}")),
        (Some(a), None) => Some(format!("{what} >= {a}")),
        (None, Some(b)) => Some(format!("{what} <= {b}")),
        (None, None) => None,
    }
}

/// A doc comment's first paragraph on one line: multi-paragraph docs would bury the argument table.
fn first_paragraph(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    let paragraph = text.trim().split("\n\n").next()?;
    let line = paragraph.split_whitespace().collect::<Vec<_>>().join(" ");
    (!line.is_empty()).then_some(line)
}

#[cfg(test)]
mod tests {
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    use super::*;

    /// Generated the way `omuzaki-ops-api` generates op schemas.
    fn schema_of<T: JsonSchema>() -> Schema {
        schemars::generate::SchemaSettings::draft2020_12()
            .with(|s| s.inline_subschemas = true)
            .into_generator()
            .into_root_schema_for::<T>()
    }

    fn from_json(value: Value) -> Schema {
        Schema::try_from(value).expect("a schema")
    }

    fn arg<'a>(args: &'a [ArgDoc], name: &str) -> &'a ArgDoc {
        args.iter().find(|a| a.name == name).unwrap_or_else(|| panic!("no argument {name} in {args:#?}"))
    }

    fn names(args: &[ArgDoc]) -> Vec<&str> {
        args.iter().map(|a| a.name.as_str()).collect()
    }

    #[derive(Serialize, Deserialize, JsonSchema)]
    struct Ordering {
        zebra: Option<u8>,
        second: f32,
        first: u32,
        apple: Option<u8>,
    }

    #[test]
    fn required_arguments_come_first_in_declaration_order_then_optional_ones_alphabetically() {
        let args = arguments(&schema_of::<Ordering>());
        assert_eq!(names(&args), ["second", "first", "apple", "zebra"]);
        assert_eq!(args.iter().map(|a| a.required).collect::<Vec<_>>(), [true, true, false, false]);
    }

    #[derive(Serialize, Deserialize, JsonSchema)]
    struct Scalars {
        /// Mixes the two faces.
        #[schemars(range(min = 0.0, max = 1.0))]
        blend: f32,
        count: u32,
        small: Option<u8>,
        #[schemars(length(min = 1, max = 10), regex(pattern = "^[a-z]+$"))]
        name: String,
        #[serde(default)]
        flag: bool,
        names: Vec<String>,
        #[schemars(length(min = 1, max = 4))]
        bounded: Vec<u8>,
        by_name: std::collections::BTreeMap<String, f32>,
        pair: (f32, f32),
        anything: serde_json::Value,
    }

    #[test]
    fn types_ranges_lengths_patterns_and_defaults_are_read_from_the_schema() {
        let args = arguments(&schema_of::<Scalars>());
        let blend = arg(&args, "blend");
        assert_eq!((blend.ty.as_str(), blend.required), ("number", true));
        assert_eq!(blend.constraints, ["range 0.0 to 1.0"]);
        assert_eq!(blend.description.as_deref(), Some("Mixes the two faces."));

        assert_eq!(arg(&args, "count").constraints, ["range >= 0"]);
        let small = arg(&args, "small");
        assert_eq!((small.ty.as_str(), small.required), ("integer or null", false));
        assert_eq!(small.constraints, ["range 0 to 255"]);
        assert_eq!(arg(&args, "name").constraints, ["length 1 to 10", "pattern ^[a-z]+$"]);
        assert_eq!(arg(&args, "flag").constraints, ["default false"]);
        assert_eq!(arg(&args, "names").ty, "array of string");
        assert_eq!(arg(&args, "bounded").constraints, ["items 1 to 4"]);
        assert_eq!(arg(&args, "by_name").ty, "map of number");
        assert_eq!(arg(&args, "pair").ty, "[number, number]");
        assert!(arg(&args, "pair").constraints.is_empty(), "a tuple's length is in its type");
        assert_eq!(arg(&args, "anything").ty, "any");
    }

    /// Enum doc, not shown: the field's own doc wins.
    #[derive(Serialize, Deserialize, JsonSchema)]
    enum Quality {
        /// Fast, noisy.
        Draft,
        /// Slow, clean.
        Final,
    }

    #[derive(Serialize, Deserialize, JsonSchema)]
    #[serde(tag = "kind")]
    enum Shape {
        Ball { radius: f32 },
        Cube { side: f32 },
    }

    #[derive(Serialize, Deserialize, JsonSchema)]
    struct Choices {
        /// How good.
        quality: Quality,
        shape: Shape,
        maybe_quality: Option<Quality>,
    }

    #[test]
    fn enums_list_their_values_with_descriptions_and_unions_point_at_the_schema() {
        let args = arguments(&schema_of::<Choices>());
        let quality = arg(&args, "quality");
        assert_eq!(quality.ty, "string");
        assert_eq!(quality.description.as_deref(), Some("How good."));
        let values: Vec<(&str, Option<&str>)> =
            quality.choices.iter().map(|c| (c.value.as_str(), c.description.as_deref())).collect();
        assert_eq!(values, [("\"Draft\"", Some("Fast, noisy.")), ("\"Final\"", Some("Slow, clean."))]);

        let maybe = arg(&args, "maybe_quality");
        assert_eq!((maybe.ty.as_str(), maybe.choices.len()), ("string or null", 2));

        let shape = arg(&args, "shape");
        assert_eq!(shape.ty, "object");
        assert_eq!(shape.constraints, ["2 variants; see --json for the full schema"]);
    }

    #[derive(Serialize, Deserialize, JsonSchema)]
    struct Leaf {
        /// Radius in metres.
        radius: f32,
        label: Option<String>,
    }

    #[derive(Serialize, Deserialize, JsonSchema)]
    struct Tree {
        leaf: Leaf,
        maybe_leaf: Option<Leaf>,
        leaves: Vec<Leaf>,
    }

    #[test]
    fn nested_fields_follow_their_parent_with_dotted_names_and_inherit_optionality() {
        let args = arguments(&schema_of::<Tree>());
        assert_eq!(
            names(&args),
            [
                "leaf",
                "leaf.radius",
                "leaf.label",
                "leaves",
                "leaves[].radius",
                "leaves[].label",
                "maybe_leaf",
                "maybe_leaf.radius",
                "maybe_leaf.label"
            ]
        );
        assert!(arg(&args, "leaf.radius").required && !arg(&args, "leaf.label").required);
        assert!(arg(&args, "leaves[].radius").required);
        assert!(!arg(&args, "maybe_leaf.radius").required, "required only when maybe_leaf is given");
        assert_eq!(arg(&args, "leaves").ty, "array of object");
        assert_eq!(arg(&args, "maybe_leaf").ty, "object or null");
        assert_eq!(arg(&args, "leaf.radius").description.as_deref(), Some("Radius in metres."));
    }

    #[test]
    fn hand_written_schemas_cover_exclusive_limits_formats_enums_and_deprecation() {
        let schema = from_json(json!({
            "type": "object",
            "properties": {
                "x": {"type": "number", "exclusiveMinimum": 0, "maximum": 1},
                "y": {"type": "number", "minimum": 2},
                "z": {"type": "number", "exclusiveMaximum": 9},
                "id": {"type": "string", "format": "uuid"},
                "mode": {"enum": ["a", "b"]},
                "old": {"type": "string", "deprecated": true},
                "ids": {"type": "array", "items": {"type": "integer"}, "uniqueItems": true, "minItems": 1}
            },
            "required": ["x"]
        }));
        let args = arguments(&schema);
        assert_eq!(arg(&args, "x").constraints, ["range > 0 and <= 1"]);
        assert_eq!(arg(&args, "y").constraints, ["range >= 2"]);
        assert_eq!(arg(&args, "z").constraints, ["range < 9"]);
        assert_eq!(arg(&args, "id").ty, "string (uuid)");
        assert_eq!(arg(&args, "mode").choices.iter().map(|c| c.value.as_str()).collect::<Vec<_>>(), ["\"a\"", "\"b\""]);
        assert_eq!(arg(&args, "old").constraints, ["deprecated"]);
        assert_eq!(arg(&args, "ids").constraints, ["items >= 1", "items must be unique"]);
    }

    #[test]
    fn a_schema_without_properties_has_no_arguments() {
        for schema in [json!(true), json!({"type": "object"}), json!({"type": "null"})] {
            assert_eq!(arguments(&from_json(schema)), Vec::<ArgDoc>::new());
        }
    }

    #[test]
    fn descriptions_keep_the_first_paragraph_on_one_line() {
        let schema = from_json(json!({
            "description": "  Does a thing.\n\nMore.  ",
            "type": "object",
            "properties": {"a": {"type": "string", "description": "First line\ncontinues here.\n\nSecond paragraph."}}
        }));
        assert_eq!(description(&schema).as_deref(), Some("Does a thing.\n\nMore."));
        assert_eq!(arguments(&schema)[0].description.as_deref(), Some("First line continues here."));
        assert_eq!(description(&from_json(json!({"description": " "}))), None);
    }
}
