//! From an op's JSON Schema to an MCP tool input schema, and the argument check that makes mistakes self-correcting:
//! unknown, missing and wrongly typed arguments are caught here, with the argument named, before the op sees them.

use omuzaki_ops_api::{OpError, OpKind};
use rmcp::model::JsonObject;
use serde_json::{Value, json};

use crate::error::ServerError;
use crate::signature::shape;
use crate::suggest::suggest;

/// The reserved argument every edit and job tool accepts in addition to the op's own.
pub(crate) const DRY_RUN: &str = "dry_run";

const DRY_RUN_DOC: &str = "Set true to preview: validates the arguments and returns the exact changes (and cost \
estimate) without applying anything. Omit or set false to apply.";

/// An op's arguments as the MCP surface sees them.
pub(crate) struct ToolSchema {
    /// The tool's `inputSchema`: the op schema, plus `dry_run` unless the op is a read-only query.
    pub input: JsonObject,
    /// True for an op whose arguments are a unit struct (`null` in serde), which has no arguments to pass.
    pub unit: bool,
}

/// Builds the tool input schema for `op` from its registered schema. Queries don't advertise `dry_run`: there is
/// nothing to preview (the server still accepts and ignores it, as agents add it by habit).
///
/// # Errors
/// [`ServerError::UnusableOp`] when the schema isn't an object schema or already uses the reserved `dry_run` name.
pub(crate) fn tool_schema(op: &str, kind: OpKind, schema: &schemars::Schema) -> Result<ToolSchema, ServerError> {
    let unusable = |reason: &str| ServerError::UnusableOp { op: op.to_owned(), reason: reason.to_owned() };
    let Ok(Value::Object(mut input)) = serde_json::to_value(schema) else {
        return Err(unusable("its argument schema is not a JSON Schema object"));
    };
    let unit = match input.get("type").and_then(Value::as_str) {
        Some("object") => false,
        Some("null") => true,
        _ => return Err(unusable("its arguments must be a JSON object (use a struct, not a bare value or enum)")),
    };
    if unit {
        input = JsonObject::from_iter([
            ("type".to_owned(), json!("object")),
            ("properties".to_owned(), json!({})),
            ("additionalProperties".to_owned(), json!(false)),
        ]);
    }
    // The root `title` is the op's Rust type name, `$schema` the dialect every tool shares and `x-omuzaki-*` the
    // registry's own marks (the conformance fixture): noise for an agent, repeated in every tool.
    input.remove("title");
    input.remove("$schema");
    input.retain(|key, _| !key.starts_with("x-omuzaki"));
    let Value::Object(properties) = input.entry("properties").or_insert_with(|| json!({})) else {
        return Err(unusable("its `properties` is not an object"));
    };
    if properties.contains_key(DRY_RUN) {
        return Err(unusable("it declares an argument named `dry_run`, which the MCP surface reserves for previews"));
    }
    if kind != OpKind::Query {
        properties.insert(DRY_RUN.to_owned(), json!({"type": "boolean", "default": false, "description": DRY_RUN_DOC}));
    }
    Ok(ToolSchema { input, unit })
}

/// Checks the arguments against the top level of the schema: nothing unknown, nothing required missing, and each
/// value of the declared type. Deeper structure is left to the op.
///
/// serde ignores unknown fields by default, so without the first check a misspelt optional argument (`blnd`,
/// `dry-run`) is silently dropped and the op runs with its default: an agent that meant to preview would apply.
/// Serde's own type errors don't say which argument was wrong; this does. Schemas that allow extra properties (a
/// flattened map) are left alone.
///
/// # Errors
/// An `invalid` [`OpError`] naming the first problem and how to fix it.
pub(crate) fn check_args(args: &JsonObject, input: &JsonObject) -> Result<(), OpError> {
    let Some(Value::Object(properties)) = input.get("properties") else { return Ok(()) };
    let extras_allowed = input.get("additionalProperties").is_some_and(|v| v != &json!(false));
    if !extras_allowed && let Some(unknown) = args.keys().find(|key| !properties.contains_key(key.as_str())) {
        return Err(unknown_argument(unknown, properties.keys().map(String::as_str)));
    }
    let required = input.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str);
    for key in required {
        if !args.contains_key(key) {
            let expected = properties.get(key).map_or_else(|| "a value".to_owned(), shape);
            return Err(OpError::invalid(key, format!("missing required argument `{key}`"))
                .hint(format!("pass `{key}`: {expected}")));
        }
    }
    for (key, value) in args {
        if let Some(schema) = properties.get(key) {
            check_type(key, value, schema)?;
        }
    }
    Ok(())
}

fn unknown_argument<'a>(unknown: &str, known: impl Iterator<Item = &'a str> + Clone) -> OpError {
    let names: Vec<&str> = known.clone().collect();
    let close = suggest(unknown, known);
    let hint = if names.is_empty() {
        "this tool takes no arguments".to_owned()
    } else if close.is_empty() {
        format!("valid arguments: {}", names.join(", "))
    } else {
        format!("did you mean `{}`? Valid arguments: {}", close.join("`, `"), names.join(", "))
    };
    OpError::invalid(unknown, format!("unknown argument `{unknown}`")).hint(hint)
}

/// Checks one value against a declared `type` or `enum`. Schemas that declare neither (unions, `$ref`-free
/// compositions) pass: the op decodes those.
fn check_type(key: &str, value: &Value, schema: &Value) -> Result<(), OpError> {
    let allowed: Vec<&str> = match schema.get("type") {
        Some(Value::String(kind)) => vec![kind.as_str()],
        Some(Value::Array(kinds)) => kinds.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    let type_ok = allowed.is_empty() || allowed.iter().any(|kind| is_type(value, kind));
    let enum_ok = schema.get("enum").and_then(Value::as_array).is_none_or(|options| options.contains(value));
    if type_ok && enum_ok {
        return Ok(());
    }
    let expected = shape(schema);
    Err(OpError::invalid(key, format!("`{key}` must be {expected}, got {}", describe(value)))
        .hint(format!("pass `{key}` as {expected}")))
}

/// Whether `value` is a `kind` the way `serde_json` reads it: integers must not have a fraction.
fn is_type(value: &Value, kind: &str) -> bool {
    match kind {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true,
    }
}

/// A short rendering of a wrong value for an error message: `string "high"`, `number 3`, `array`.
fn describe(value: &Value) -> String {
    const SHOWN: usize = 40;
    let kind = match value {
        Value::Null => return "null".to_owned(),
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => return "an array".to_owned(),
        Value::Object(_) => return "an object".to_owned(),
    };
    let text = value.to_string();
    let cut: String = text.chars().take(SHOWN).collect();
    format!("{kind} {cut}{}", if text.chars().count() > SHOWN { "..." } else { "" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(value: Value) -> schemars::Schema {
        serde_json::from_value(value).unwrap()
    }

    fn object_schema() -> schemars::Schema {
        schema(json!({
            "type": "object",
            "properties": {
                "blend": {"type": "number", "minimum": 0.0, "maximum": 1.0},
                "character": {"type": "string"},
                "mode": {"enum": ["soft", "hard"]},
                "note": {"type": ["string", "null"]}
            },
            "required": ["character", "blend"]
        }))
    }

    fn built(kind: OpKind) -> JsonObject {
        tool_schema("face.set_profile", kind, &object_schema()).unwrap().input
    }

    fn args(value: &Value) -> JsonObject {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn edits_get_dry_run_and_keep_the_op_schema() {
        let input = built(OpKind::Edit);
        assert_eq!(input["properties"]["dry_run"]["type"], "boolean");
        assert_eq!(input["properties"]["blend"]["maximum"], 1.0);
        assert_eq!(input["required"], json!(["character", "blend"]));
        assert!(input.get("title").is_none());
    }

    #[test]
    fn queries_do_not_advertise_dry_run() {
        assert!(built(OpKind::Query)["properties"].get("dry_run").is_none());
    }

    #[test]
    fn unit_ops_become_an_empty_object() {
        let unit = schema(json!({"type": "null"}));
        let edit = tool_schema("project.save", OpKind::Edit, &unit).unwrap();
        assert!(edit.unit);
        assert_eq!(edit.input["type"], "object");
        assert_eq!(edit.input["properties"].as_object().unwrap().keys().collect::<Vec<_>>(), ["dry_run"]);
        let query = tool_schema("project.ping", OpKind::Query, &unit).unwrap();
        assert!(query.input["properties"].as_object().unwrap().is_empty());
    }

    #[test]
    fn rejects_schemas_that_cannot_be_tool_inputs() {
        for bad in [json!({"type": "string"}), json!({"oneOf": []}), json!(true)] {
            assert!(matches!(tool_schema("x.y", OpKind::Edit, &schema(bad)), Err(ServerError::UnusableOp { .. })));
        }
        let reserved = schema(json!({"type": "object", "properties": {"dry_run": {"type": "string"}}}));
        for kind in [OpKind::Edit, OpKind::Query] {
            let result = tool_schema("x.y", kind, &reserved);
            assert!(matches!(result, Err(ServerError::UnusableOp { reason, .. }) if reason.contains("dry_run")));
        }
    }

    #[test]
    fn unknown_arguments_are_named_with_a_suggestion() {
        let input = built(OpKind::Edit);
        let err = check_args(&args(&json!({"character": "d", "blend": 0.5, "blnd": 0.5})), &input).unwrap_err();
        assert_eq!(err.field.as_deref(), Some("blnd"));
        assert!(err.hint.unwrap().contains("did you mean `blend`"));
        assert!(check_args(&args(&json!({"character": "d", "blend": 0.5, "dry_run": true})), &input).is_ok());
    }

    #[test]
    fn missing_required_arguments_say_what_to_pass() {
        let err = check_args(&args(&json!({"character": "d"})), &built(OpKind::Edit)).unwrap_err();
        assert_eq!(err.field.as_deref(), Some("blend"));
        assert_eq!(err.hint.as_deref(), Some("pass `blend`: number[0..1]"));
    }

    #[test]
    fn wrong_types_and_enum_values_name_the_argument() {
        let input = built(OpKind::Edit);
        let err = check_args(&args(&json!({"character": "d", "blend": "high"})), &input).unwrap_err();
        assert_eq!(err.field.as_deref(), Some("blend"));
        assert_eq!(err.message, "`blend` must be number[0..1], got string \"high\"");
        let err = check_args(&args(&json!({"character": "d", "blend": 1, "mode": "soft-ish"})), &input).unwrap_err();
        assert!(
            err.message.contains("must be \"soft\"|\"hard\"") && err.message.contains("got string"),
            "{}",
            err.message
        );
        // `null` is fine for an optional `string|null`; a number is not.
        assert!(check_args(&args(&json!({"character": "d", "blend": 1, "note": null})), &input).is_ok());
        assert!(check_args(&args(&json!({"character": "d", "blend": 1, "note": 3})), &input).is_err());
    }

    #[test]
    fn integers_must_not_have_a_fraction() {
        let count = json!({"type": "object", "properties": {"n": {"type": "integer"}}, "required": ["n"]});
        let input = tool_schema("x.y", OpKind::Edit, &schema(count)).unwrap().input;
        assert!(check_args(&args(&json!({"n": 3})), &input).is_ok());
        assert!(check_args(&args(&json!({"n": 3.5})), &input).is_err());
    }

    #[test]
    fn open_schemas_accept_extra_arguments() {
        let open = schema(json!({"type": "object", "properties": {}, "additionalProperties": {"type": "number"}}));
        let input = tool_schema("x.y", OpKind::Edit, &open).unwrap().input;
        assert!(check_args(&args(&json!({"anything": 1})), &input).is_ok());
    }
}
