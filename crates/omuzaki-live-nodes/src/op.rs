//! `op.run`: any op allowed for automations, as an action (design D7). One node covers every OBS action, scene
//! switch, expression or sound, so no op needs a node of its own.
//!
//! - A string argument that is exactly `"$in.<name>"` becomes a value input `<name>`, typed from the op's argument
//!   schema at that path (text, number, true/false, list, else json). Placeholders are never spliced into longer
//!   strings; a string starting `$$` writes a literal `$`.
//! - Outputs: `done` with `output` (the op's answer), or `failed` with `error`.
//! - **Checked twice.** The parameters are checked against the op's registration when the show compiles: the op must
//!   exist, be allowed (scope `read`, `perform`, `broadcast`, `render`, `edit:character` or `edit:scene`, never a job,
//!   never one of the effect ops that have their own nodes) and take every argument named. At activation the inputs
//!   are filled in and the whole argument object is decoded through the op registry; a mismatch fires `failed` and
//!   nothing reaches the executor.

use std::collections::{BTreeMap, BTreeSet};

use omuzaki_live_api::ids::PortName;
use omuzaki_live_api::need::Need;
use omuzaki_live_api::node::{
    CallId, Category, Input, Node, NodeClass, NodeCx, NodeError, NodeKind, NodeMeta, ParamError, ParamErrors, Ports,
};
use omuzaki_live_api::register_node;
use omuzaki_live_api::value::{PortType, Val};
use omuzaki_ops_api::{OpError, OpKind, OpRegistration, Scope, find};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::l10n;
use crate::schema::{closed, port_type, properties, resolve};

/// The prefix of a placeholder string.
const PLACEHOLDER: &str = "$in.";

/// The effect ops that have their own nodes, whose paths, hosts and programs are literal and approved (design D7).
/// The backstop for one registered without the right scopes.
const EFFECT_OPS: [&str; 8] = [
    "net.http",
    "net.udp_send",
    "file.read_text",
    "file.write_text",
    "file.append_text",
    "file.stat",
    "exec.run",
    "system.process_running",
];

/// The fixed ports, which a placeholder can't take as its name.
const FIXED_INPUTS: [&str; 1] = ["in"];

/// Parameters of `op.run`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpRunParams {
    /// The op it runs, by name (`obs.scene.set`), as `omuzaki run --list` or the MCP tool list shows it.
    pub op: String,
    /// The op's arguments. A string that is exactly `$in.<name>` becomes a value input `<name>`, filled in from its
    /// wire each time; a string starting `$$` writes a literal `$`.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub args: Map<String, Value>,
}

/// The node that does what a refused scope's ops do, with a literal target the show's approval names (design D7).
fn instead(scope: Scope) -> Option<&'static str> {
    match scope {
        Scope::Net => Some("net.http"),
        Scope::Fs => Some("file.*"),
        Scope::Process => Some("exec.run"),
        _ => None,
    }
}

/// Why an op can't run from a show, or `None` when it can.
fn refusal(registration: &OpRegistration) -> Option<String> {
    let meta = (registration.meta)();
    if EFFECT_OPS.contains(&meta.name) {
        return Some(
            "it reaches files, the network or programs; use its own node, whose target is approved".to_owned(),
        );
    }
    if meta.kind == OpKind::Job {
        return Some("it is a long job".to_owned());
    }
    let allowed = matches!(
        meta.scope,
        Scope::Read | Scope::Perform | Scope::Broadcast | Scope::Render | Scope::EditCharacter | Scope::EditScene
    );
    if allowed {
        return None;
    }
    let scope = meta.scope.name();
    Some(match instead(meta.scope) {
        Some(node) => format!("it needs the {scope} scope, which shows never get; use the {node} node instead"),
        None => format!("it needs the {scope} scope, which shows never get"),
    })
}

/// The schema at a dotted argument path (object keys and list indices), as `placeholders` walks it.
fn schema_at<'a>(schema: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(schema, |schema, key| match properties(schema) {
        Some(props) => props.get(key),
        None => resolve(schema).get("items"),
    })
}

/// The value a placeholder stands for when the arguments are checked before anything is wired: the schema's default,
/// else its first allowed value (an enum's first name), else the default of the input's type.
fn stand_in(schema: Option<&Value>, ty: PortType) -> Value {
    let schema = schema.map(resolve);
    let allowed = schema.and_then(|s| {
        s.get("default").or_else(|| s.get("const")).or_else(|| s.get("enum").and_then(|e| e.get(0))).or_else(|| {
            ["oneOf", "anyOf"].iter().find_map(|key| s.get(*key)?.as_array()?.iter().find_map(|o| o.get("const")))
        })
    });
    if let Some(value) = allowed {
        return value.clone();
    }
    match ty {
        PortType::Text => Value::String(String::new()),
        PortType::Number => Value::from(0),
        PortType::Bool => Value::Bool(false),
        PortType::List => Value::Array(Vec::new()),
        _ => Value::Null,
    }
}

/// Every placeholder under `args`, by name, typed from `schema` at its path.
fn placeholders(args: &Value, schema: Option<&Value>, path: &str, found: &mut BTreeMap<String, (PortType, String)>) {
    match args {
        Value::String(s) => {
            if let Some(name) = s.strip_prefix(PLACEHOLDER) {
                let ty = schema.map_or(PortType::Json, port_type);
                found
                    .entry(name.to_owned())
                    .and_modify(|(seen, _)| {
                        if *seen != ty {
                            *seen = PortType::Json;
                        }
                    })
                    .or_insert((ty, path.to_owned()));
            }
        }
        Value::Object(map) => {
            for (key, value) in map {
                let sub = schema.and_then(properties).and_then(|p| p.get(key));
                placeholders(value, sub, &join(path, key), found);
            }
        }
        Value::Array(items) => {
            let sub = schema.map(resolve).and_then(|s| s.get("items"));
            for (i, item) in items.iter().enumerate() {
                placeholders(item, sub, &join(path, &i.to_string()), found);
            }
        }
        _ => {}
    }
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() { key.to_owned() } else { format!("{path}.{key}") }
}

/// The arguments the op's schema doesn't take, by path (only where the schema is closed).
fn unknown_args(args: &Value, schema: &Value, path: &str, out: &mut Vec<String>) {
    let (Value::Object(map), Some(props)) = (args, properties(schema)) else {
        return;
    };
    for (key, value) in map {
        match props.get(key) {
            Some(sub) => unknown_args(value, sub, &join(path, key), out),
            None if closed(schema) => out.push(join(path, key)),
            None => {}
        }
    }
}

/// `args` with each placeholder replaced by its input's value and each `$$` unescaped.
fn substitute(args: &Value, inputs: &BTreeMap<String, Val>) -> Value {
    let inputs: BTreeMap<String, Value> = inputs.iter().map(|(k, v)| (k.clone(), v.to_json())).collect();
    substitute_json(args, &inputs)
}

fn substitute_json(args: &Value, inputs: &BTreeMap<String, Value>) -> Value {
    match args {
        Value::String(s) => match s.strip_prefix(PLACEHOLDER) {
            Some(name) => inputs.get(name).cloned().unwrap_or(Value::Null),
            None if s.starts_with("$$") => Value::String(s[1..].to_owned()),
            None => args.clone(),
        },
        Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), substitute_json(v, inputs))).collect()),
        Value::Array(items) => Value::Array(items.iter().map(|v| substitute_json(v, inputs)).collect()),
        other => other.clone(),
    }
}

/// The placeholders of a set of parameters, as `(port, type, path)`; names that can't be ports are left out (the
/// check reports them).
fn inputs_of(params: &OpRunParams) -> Vec<(PortName, PortType, String)> {
    let schema = find(&params.op).map(|r| (r.schema)());
    let mut found = BTreeMap::new();
    placeholders(&Value::Object(params.args.clone()), schema.as_ref().map(schemars::Schema::as_value), "", &mut found);
    found
        .into_iter()
        .filter(|(name, _)| !FIXED_INPUTS.contains(&name.as_str()))
        .filter_map(|(name, (ty, path))| PortName::new(name).ok().map(|port| (port, ty, path)))
        .collect()
}

/// Runs its op each time `in` fires, then fires `done` or `failed`.
#[derive(Debug)]
pub struct OpRun {
    op: String,
    args: Value,
    inputs: Vec<PortName>,
    pending: BTreeSet<CallId>,
}

impl OpRun {
    fn on_in(&mut self, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        if self.op.is_empty() {
            return cx.fail("failed", l10n::no_op());
        }
        let Some(registration) = find(&self.op) else {
            return cx.fail("failed", l10n::unknown_op(self.op.as_str()));
        };
        let mut values = BTreeMap::new();
        for port in &self.inputs {
            values.insert(port.as_str().to_owned(), cx.input(port.as_str())?);
        }
        let args = substitute(&self.args, &values);
        if let Err(e) = (registration.decode)(args.clone()) {
            let reason = match &e.field {
                Some(field) => format!("{field}: {}", e.message),
                None => e.message.clone(),
            };
            return cx.fail("failed", l10n::invalid_args(self.op.as_str(), reason));
        }
        match cx.run_op(&self.op, args) {
            Ok(call) => {
                self.pending.insert(call);
                Ok(())
            }
            Err(_) => cx.fail("failed", l10n::op_limited(self.op.as_str())),
        }
    }

    fn on_reply(
        &mut self,
        call: CallId,
        outcome: Result<Option<Value>, OpError>,
        cx: &mut NodeCx<'_>,
    ) -> Result<(), NodeError> {
        if !self.pending.remove(&call) {
            return Ok(());
        }
        match outcome {
            Ok(output) => {
                cx.set("output", Val::from(output.unwrap_or(Value::Null)))?;
                cx.fire("done")
            }
            // The code first (`limited: ...`), so a graph can tell a refusal from a failure by reading the text.
            Err(e) => cx.fail("failed", l10n::op_failed(e.code.to_string(), e.message)),
        }
    }
}

impl NodeKind for OpRun {
    type Params = OpRunParams;

    fn meta() -> NodeMeta {
        NodeMeta::new(
            "op.run",
            "Run an op: switch a scene, play an expression, control OBS.",
            NodeClass::Active,
            Category::Actions,
        )
    }

    fn ports(params: &OpRunParams) -> Ports {
        let ports = Ports::new().event_in("in", "Runs the op.");
        inputs_of(params)
            .into_iter()
            .fold(ports, |ports, (port, ty, path)| ports.dynamic_in(port, ty, format!("args.{path}")))
            .event_out("done", "The op ran; its answer is on `output`.")
            .event_out("failed", "The op wasn't run or refused; the reason is on `error`.")
            .value_out("output", PortType::Json, "The op's answer.")
            .value_out("error", PortType::Text, "Why it failed, in English.")
    }

    fn needs(params: &OpRunParams) -> Vec<Need> {
        match find(&params.op) {
            Some(registration) => {
                let scope = (registration.meta)().scope;
                // Reading and performing are what every show may do; anything else is approved per show (D11).
                let mut needs = Vec::new();
                if !matches!(scope, Scope::Read | Scope::Perform) {
                    needs.push(Need::Scope(scope));
                }
                needs.push(Need::op(params.op.clone()));
                needs
            }
            None => Vec::new(),
        }
    }

    fn check(params: &OpRunParams) -> Result<(), ParamErrors> {
        if params.op.is_empty() {
            // Not chosen yet: the node compiles and fails each time it runs, so a half-built graph still loads.
            return Ok(());
        }
        let registration =
            find(&params.op).ok_or_else(|| ParamErrors::one("op", l10n::unknown_op(params.op.as_str())))?;
        if let Some(reason) = refusal(registration) {
            return Err(ParamErrors::one("op", l10n::op_not_allowed(params.op.as_str(), reason)));
        }
        let args = Value::Object(params.args.clone());
        let schema = (registration.schema)();
        let mut errors = Vec::new();
        let mut unknown = Vec::new();
        unknown_args(&args, schema.as_value(), "", &mut unknown);
        for path in unknown {
            errors.push(ParamError::new(format!("args.{path}"), l10n::unknown_arg(params.op.as_str(), path.as_str())));
        }
        let mut found = BTreeMap::new();
        placeholders(&args, Some(schema.as_value()), "", &mut found);
        let mut stand_ins = BTreeMap::new();
        for (name, (ty, path)) in &found {
            if FIXED_INPUTS.contains(&name.as_str()) || PortName::new(name.as_str()).is_err() {
                errors
                    .push(ParamError::new(format!("args.{path}"), l10n::bad_placeholder(path.as_str(), name.as_str())));
            }
            stand_ins.insert(name.clone(), stand_in(schema_at(schema.as_value(), path), *ty));
        }
        if errors.is_empty() {
            // The whole object must decode with each input at its type's default, so a missing required argument or a
            // constant of the wrong type is found now, not on the first event.
            if let Err(e) = (registration.decode)(substitute_json(&args, &stand_ins)) {
                let path = e.field.as_deref().map_or_else(|| "args".to_owned(), |f| format!("args.{f}"));
                errors.push(ParamError::new(path, l10n::invalid_args(params.op.as_str(), e.message)));
            }
        }
        if errors.is_empty() { Ok(()) } else { Err(ParamErrors(errors)) }
    }

    fn new(params: OpRunParams) -> Self {
        let inputs = inputs_of(&params).into_iter().map(|(port, _, _)| port).collect();
        Self { op: params.op, args: Value::Object(params.args), inputs, pending: BTreeSet::new() }
    }
}

impl Node for OpRun {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { port: "in", .. } => self.on_in(cx),
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Reply { call, outcome, .. } => self.on_reply(call, outcome, cx),
            Input::Start | Input::Stop | Input::Trigger { .. } | Input::Wake { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }
}
register_node!(OpRun);
