//! Self-registration of ops (spec §39 rule 2). Each op file calls [`register_op!`](crate::register_op); the linker
//! collects the registrations, so no central list exists to conflict on.
//!
//! # Example arguments
//!
//! An op declares example arguments on its args type, with schemars' `extend`, so a registration needs no extra entry
//! and `register_op!(T)` stays as it is. The examples become the schema's `examples` keyword (so they also reach MCP
//! clients and the docs), and [`OpRegistration::examples`] returns them. The registry-wide conformance test
//! (`omuzaki-cli/tests/op_conformance.rs`) runs every example. An example that needs a particular project says so with
//! `x-omuzaki-fixture`, read by [`OpRegistration::fixture`].
//!
//! ```ignore
//! #[derive(Serialize, Deserialize, JsonSchema)]
//! #[schemars(extend("examples" = [{ "text": "hello" }], "x-omuzaki-fixture" = "demo"))]
//! struct SetNote { text: String }
//! ```

use omuzaki_doc_api::Value;
use schemars::Schema;
use serde_path_to_error::Segment;

use crate::{DynOp, JobShape, Op, OpError, OpMeta};

/// Everything the executor and the five surfaces need to know about one op type.
pub struct OpRegistration {
    /// The op's static facts.
    pub meta: fn() -> OpMeta,
    /// The JSON Schema of its arguments: the MCP tool schema, CLI flags, script binding and docs.
    pub schema: fn() -> Schema,
    /// Decodes arguments into a runnable op.
    pub decode: fn(Value) -> Result<Box<dyn DynOp>, OpError>,
    /// See [`Op::coalesces`].
    pub coalesces: fn() -> bool,
    /// See [`Op::job_shape`]: surfaces show a driver job as one (#1110) without decoding the op.
    pub job_shape: fn() -> JobShape,
}

impl OpRegistration {
    /// The registration for op type `T`. A `const fn` so [`register_op!`](crate::register_op) works at link time.
    pub const fn of<T: Op>() -> Self {
        Self {
            meta: T::meta,
            schema: schema_of::<T>,
            decode: decode_as::<T>,
            coalesces: T::coalesces,
            job_shape: T::job_shape,
        }
    }
}

impl OpRegistration {
    /// The example argument sets the op declares (the `examples` of its schema), in declaration order. Empty when it
    /// declares none. Each one is meant to decode and to run against the project named by [`fixture`](Self::fixture).
    pub fn examples(&self) -> Vec<Value> {
        match (self.schema)().get("examples") {
            Some(Value::Array(examples)) => examples.clone(),
            _ => Vec::new(),
        }
    }

    /// The name of the fixture project the examples expect (`x-omuzaki-fixture` in the schema), when the op names
    /// one. `None` means the default fixture project.
    pub fn fixture(&self) -> Option<String> {
        match (self.schema)().get("x-omuzaki-fixture") {
            Some(Value::String(name)) => Some(name.clone()),
            _ => None,
        }
    }
}

/// Op schemas are fully inlined (no `$ref`/`$defs`): they're MCP tool schemas, and agents and MCP clients handle
/// flat schemas far more reliably.
fn schema_of<T: Op>() -> Schema {
    schemars::generate::SchemaSettings::draft2020_12()
        .with(|s| s.inline_subschemas = true)
        .into_generator()
        .into_root_schema_for::<T>()
}

fn decode_as<T: Op>(args: Value) -> Result<Box<dyn DynOp>, OpError> {
    let name = T::meta().name;
    serde_path_to_error::deserialize::<_, T>(args)
        .map(|op| Box::new(op) as Box<dyn DynOp>)
        // `inner()` keeps the message as it was before paths were tracked; `Display` would prefix the path.
        .map_err(|e| OpError::decode_failed(name, field_of(&e), e.inner()))
}

/// The argument at fault, as the path to the bad value joined by `/`: `id`, `params/ease`, `marks/0/size`. Agents fix
/// the argument the error names, so a nested mistake must name the nested key, not the whole `args`. A required key
/// that was not given is named too: serde reports it at its parent, so the key is appended to the parent's path. An
/// empty path means the whole value is wrong (not an object, say), and that stays `args`.
fn field_of(e: &serde_path_to_error::Error<serde_json::Error>) -> String {
    // `Unknown` segments are not keys or indices an agent can use, so they are left out of the path.
    let mut segments: Vec<String> = e
        .path()
        .iter()
        .filter_map(|segment| match segment {
            Segment::Seq { index } => Some(index.to_string()),
            Segment::Map { key } | Segment::Enum { variant: key } => Some(key.clone()),
            Segment::Unknown => None,
        })
        .collect();
    segments.extend(missing_key(e.inner()));
    if segments.is_empty() { "args".to_owned() } else { segments.join("/") }
}

/// The key that serde names in its "missing field" message. serde's wording is stable across versions; if it ever
/// changes, the error falls back to naming the parent (or `args`), which is still a valid answer.
fn missing_key(err: &serde_json::Error) -> Option<String> {
    let message = err.to_string();
    let (key, _) = message.strip_prefix("missing field `")?.split_once('`')?;
    Some(key.to_owned())
}

inventory::collect!(OpRegistration);

/// Every registered op, in link order. Surfaces sort by name before showing them.
pub fn registrations() -> impl Iterator<Item = &'static OpRegistration> {
    inventory::iter::<OpRegistration>.into_iter()
}

/// The registration for the op called `name`. Linear: the executor builds an index once at startup.
pub fn find(name: &str) -> Option<&'static OpRegistration> {
    registrations().find(|r| (r.meta)().name == name)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use omuzaki_core::id::Slug;
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    use super::*;
    use crate::{ErrorCode, OpCtx, OpKind, Plan, Scope, Undo};

    /// A stand-in op with the shapes that matter for paths: a slug, a map and a list of nested objects.
    #[derive(Serialize, Deserialize, JsonSchema)]
    struct PartAdd {
        id: Slug,
        #[serde(default)]
        params: BTreeMap<String, f32>,
        #[serde(default)]
        marks: Vec<Mark>,
    }

    #[derive(Serialize, Deserialize, JsonSchema)]
    struct Mark {
        size: f32,
    }

    impl Op for PartAdd {
        fn meta() -> OpMeta {
            OpMeta {
                name: "part.add",
                summary: "Adds a part.",
                kind: OpKind::Edit,
                scope: Scope::EditCharacter,
                undo: Undo::Auto,
            }
        }

        fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
            Ok(Plan::default())
        }
    }

    /// The error from decoding `args` as `part.add`. The test op is never registered, so it stays off every surface.
    fn decode_error(args: Value) -> OpError {
        match (OpRegistration::of::<PartAdd>().decode)(args) {
            Ok(_) => panic!("these arguments should not decode"),
            Err(err) => err,
        }
    }

    #[test]
    fn a_bad_top_level_field_names_that_field() {
        let err = decode_error(json!({ "id": "Hair B" }));
        assert_eq!(err.code, ErrorCode::Invalid);
        assert_eq!(err.field.as_deref(), Some("id"));
        assert!(err.message.starts_with("arguments for part.add don't match its schema: "));
        assert!(err.hint.is_some());
    }

    #[test]
    fn a_nested_bad_value_names_its_path() {
        let err = decode_error(json!({ "id": "hair-b", "params": { "ease": "fast" } }));
        assert_eq!(err.field.as_deref(), Some("params/ease"));

        let err = decode_error(json!({ "id": "hair-b", "marks": [{ "size": 1.0 }, { "size": "big" }] }));
        assert_eq!(err.field.as_deref(), Some("marks/1/size"));
    }

    #[test]
    fn a_non_object_names_the_whole_args() {
        let err = decode_error(json!("hair-b"));
        assert_eq!(err.field.as_deref(), Some("args"));
    }

    #[test]
    fn a_missing_required_argument_names_itself() {
        let err = decode_error(json!({}));
        assert_eq!(err.field.as_deref(), Some("id"));
        assert!(err.message.contains("missing field `id`"), "{}", err.message);

        let err = decode_error(json!({ "id": "hair-b", "marks": [{}] }));
        assert_eq!(err.field.as_deref(), Some("marks/0/size"));
    }

    #[derive(Serialize, Deserialize, JsonSchema)]
    #[schemars(extend("examples" = [{ "id": "hair-b" }, { "id": "hair-c" }], "x-omuzaki-fixture" = "demo"))]
    struct WithExamples {
        id: Slug,
    }

    impl Op for WithExamples {
        fn meta() -> OpMeta {
            OpMeta { name: "part.example", ..<PartAdd as Op>::meta() }
        }

        fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
            Ok(Plan::default())
        }
    }

    #[test]
    fn examples_and_fixture_come_from_the_args_type() {
        let declared = OpRegistration::of::<WithExamples>();
        assert_eq!(declared.examples(), vec![json!({ "id": "hair-b" }), json!({ "id": "hair-c" })]);
        assert_eq!(declared.fixture().as_deref(), Some("demo"));
        assert!(declared.examples().into_iter().all(|args| (declared.decode)(args).is_ok()));

        let bare = OpRegistration::of::<PartAdd>();
        assert_eq!(bare.examples(), Vec::<Value>::new());
        assert_eq!(bare.fixture(), None);
    }

    #[test]
    fn only_decode_failures_are_decode_errors() {
        assert!(decode_error(json!({})).is_decode_error());
        assert!(!OpError::invalid("id", "already taken").hint("pick another name").is_decode_error());
    }
}
