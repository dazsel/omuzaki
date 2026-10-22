//! The variable nodes (design §6, D10, category Variables): `var.get`, `var.set`, `var.add`, `var.toggle` and the
//! trigger `var.changed`. Each names its variable and its type, so its ports are typed without reading the show, and
//! needs `Need::Variable`, so the compile checks it against the show's declaration. A node fresh from the palette has
//! no variable yet; its check asks for one.

use omuzaki_l10n_api::Text;
use omuzaki_live_api::ids::{EventKind, VarName};
use omuzaki_live_api::need::Need;
use omuzaki_live_api::node::{
    Category, EvalCx, Input, Node, NodeClass, NodeCx, NodeError, NodeKind, NodeMeta, ParamError, ParamErrors, Ports,
};
use omuzaki_live_api::register_node;
use omuzaki_live_api::show::VarType;
use omuzaki_live_api::value::{PortType, Val};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::l10n;

/// The kind `var.changed` listens to.
const VAR_CHANGED: &str = "var.changed";

/// Parameters of every variable node.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VarParams {
    /// The variable, as the show declares it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variable: Option<VarName>,
    /// Its type, as the show declares it: `bool`, `number`, `text` or `list`.
    #[serde(rename = "type")]
    pub ty: VarType,
}

impl Default for VarParams {
    fn default() -> Self {
        Self { variable: None, ty: VarType::Number }
    }
}

fn var_need(params: &VarParams) -> Vec<Need> {
    params.variable.iter().map(|name| Need::variable(name.clone(), params.ty)).collect()
}

/// The variable is chosen, and `params.ty` is `want` when a kind needs one type.
fn check_var(params: &VarParams, want: Option<VarType>) -> Result<(), ParamErrors> {
    let mut errors = Vec::new();
    if params.variable.is_none() {
        errors.push(ParamError::new("variable", l10n::var_missing()));
    }
    if let Some(want) = want
        && params.ty != want
    {
        errors.push(ParamError::new("type", l10n::var_type_needed(want.as_str(), params.ty.as_str())));
    }
    if errors.is_empty() { Ok(()) } else { Err(ParamErrors(errors)) }
}

/// The variable's value; the type's default when it has none or no variable is chosen.
fn read(cx_var: Option<Val>, ty: VarType) -> Val {
    cx_var.unwrap_or_else(|| zero(ty))
}

/// Writes `value` to the chosen variable, or says why it wasn't written.
fn write(cx: &mut NodeCx<'_>, variable: Option<&VarName>, value: Val) -> Result<(), Text> {
    let Some(variable) = variable else { return Err(l10n::var_missing()) };
    cx.set_var(variable, value).map_err(|e| l10n::var_refused(variable.as_str(), e.to_string()))
}

/// The default of a variable type, as its port's default.
fn zero(ty: VarType) -> Val {
    ty.port_type().default_value().unwrap_or(Val::Bool(false))
}

// ── var.get ────────────────────────────────────────────────────────────────────────────────────────────────────

/// Reads a variable.
#[derive(Debug)]
pub struct Get {
    variable: Option<VarName>,
    ty: VarType,
}

impl NodeKind for Get {
    type Params = VarParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("var.get", "Read a variable.", NodeClass::Pure, Category::Variables)
    }

    fn ports(params: &VarParams) -> Ports {
        Ports::new().live_out("value", params.ty.port_type(), "The variable's value.")
    }

    fn needs(params: &VarParams) -> Vec<Need> {
        var_need(params)
    }

    fn check(params: &VarParams) -> Result<(), ParamErrors> {
        check_var(params, None)
    }

    fn new(params: VarParams) -> Self {
        Self { variable: params.variable, ty: params.ty }
    }
}

impl Node for Get {
    fn handle(&mut self, input: Input<'_>, _: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Start | Input::Stop | Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }

    fn eval(&self, port: &str, cx: &mut EvalCx<'_>) -> Result<Val, NodeError> {
        match port {
            "value" => Ok(read(self.variable.as_ref().and_then(|v| cx.var(v)), self.ty)),
            other => Err(NodeError::not_live(other)),
        }
    }
}
register_node!(Get);

// ── var.set ────────────────────────────────────────────────────────────────────────────────────────────────────

/// Writes a variable.
#[derive(Debug)]
pub struct Set {
    variable: Option<VarName>,
}

impl NodeKind for Set {
    type Params = VarParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("var.set", "Set a variable.", NodeClass::Active, Category::Variables)
    }

    fn ports(params: &VarParams) -> Ports {
        Ports::new()
            .event_in("in", "Sets the variable.")
            .value_in("value", params.ty.port_type(), zero(params.ty), "The new value.")
            .event_out("done", "The variable was set.")
            .event_out("failed", "The value wasn't accepted; the reason is on `error`.")
            .value_out("error", PortType::Text, "Why it wasn't set, in English.")
    }

    fn needs(params: &VarParams) -> Vec<Need> {
        var_need(params)
    }

    fn check(params: &VarParams) -> Result<(), ParamErrors> {
        check_var(params, None)
    }

    fn new(params: VarParams) -> Self {
        Self { variable: params.variable }
    }
}

impl Node for Set {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { port: "in", .. } => {
                let value = cx.input("value")?;
                match write(cx, self.variable.as_ref(), value) {
                    Ok(()) => cx.fire("done"),
                    Err(why) => cx.fail("failed", why),
                }
            }
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Start | Input::Stop | Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }
}
register_node!(Set);

// ── var.add ────────────────────────────────────────────────────────────────────────────────────────────────────

/// Adds to a number variable.
#[derive(Debug)]
pub struct Add {
    variable: Option<VarName>,
}

impl NodeKind for Add {
    type Params = VarParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("var.add", "Add to a number variable (a counter).", NodeClass::Active, Category::Variables)
    }

    fn ports(_: &VarParams) -> Ports {
        Ports::new()
            .event_in("in", "Adds to the variable.")
            .value_in("by", PortType::Number, Val::Number(1.0), "How much to add; negative takes away.")
            .event_out("done", "The variable changed; its new value is on `value`.")
            .event_out("failed", "The variable wasn't changed; the reason is on `error`.")
            .value_out("value", PortType::Number, "The new value.")
            .value_out("error", PortType::Text, "Why it wasn't changed, in English.")
    }

    fn needs(params: &VarParams) -> Vec<Need> {
        var_need(params)
    }

    fn check(params: &VarParams) -> Result<(), ParamErrors> {
        check_var(params, Some(VarType::Number))
    }

    fn new(params: VarParams) -> Self {
        Self { variable: params.variable }
    }
}

impl Node for Add {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { port: "in", .. } => {
                let by = cx.input_number("by")?;
                let now = match self.variable.as_ref().and_then(|v| cx.var(v)) {
                    Some(Val::Number(x)) => x,
                    _ => 0.0,
                };
                let next = now + by;
                if !next.is_finite() {
                    return cx.fail("failed", l10n::sum_not_finite());
                }
                match write(cx, self.variable.as_ref(), Val::Number(next)) {
                    Ok(()) => {
                        cx.set("value", Val::Number(next))?;
                        cx.fire("done")
                    }
                    Err(why) => cx.fail("failed", why),
                }
            }
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Start | Input::Stop | Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }
}
register_node!(Add);

// ── var.toggle ─────────────────────────────────────────────────────────────────────────────────────────────────

/// Flips a true/false variable.
#[derive(Debug)]
pub struct Toggle {
    variable: Option<VarName>,
}

impl NodeKind for Toggle {
    type Params = VarParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("var.toggle", "Flip a true/false variable.", NodeClass::Active, Category::Variables)
    }

    fn ports(_: &VarParams) -> Ports {
        Ports::new()
            .event_in("in", "Flips the variable.")
            .event_out("done", "The variable flipped; its new value is on `value`.")
            .event_out("failed", "The variable wasn't changed; the reason is on `error`.")
            .value_out("value", PortType::Bool, "The new value.")
            .value_out("error", PortType::Text, "Why it wasn't changed, in English.")
    }

    fn needs(params: &VarParams) -> Vec<Need> {
        var_need(params)
    }

    fn check(params: &VarParams) -> Result<(), ParamErrors> {
        check_var(params, Some(VarType::Bool))
    }

    fn new(params: VarParams) -> Self {
        Self { variable: params.variable }
    }
}

impl Node for Toggle {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { port: "in", .. } => {
                let next = !matches!(self.variable.as_ref().and_then(|v| cx.var(v)), Some(Val::Bool(true)));
                match write(cx, self.variable.as_ref(), Val::Bool(next)) {
                    Ok(()) => {
                        cx.set("value", Val::Bool(next))?;
                        cx.fire("done")
                    }
                    Err(why) => cx.fail("failed", why),
                }
            }
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Start | Input::Stop | Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }
}
register_node!(Toggle);

// ── var.changed ────────────────────────────────────────────────────────────────────────────────────────────────

/// Fires when its variable changes, with the old and new values.
#[derive(Debug)]
pub struct Changed {
    variable: Option<VarName>,
    ty: VarType,
}

impl NodeKind for Changed {
    type Params = VarParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("var.changed", "When a variable changes.", NodeClass::Trigger, Category::Variables)
    }

    fn ports(params: &VarParams) -> Ports {
        Ports::new()
            .value_out("old", params.ty.port_type(), "The value before.")
            .value_out("new", params.ty.port_type(), "The value now.")
            .event_out("fired", "The variable changed.")
    }

    fn listens(_: &VarParams) -> Vec<EventKind> {
        vec![EventKind::from_static(VAR_CHANGED)]
    }

    fn needs(params: &VarParams) -> Vec<Need> {
        var_need(params)
    }

    fn check(params: &VarParams) -> Result<(), ParamErrors> {
        check_var(params, None)
    }

    fn new(params: VarParams) -> Self {
        Self { variable: params.variable, ty: params.ty }
    }
}

impl Changed {
    fn value(&self, json: Option<&Value>) -> Val {
        json.and_then(|v| Val::from_json(v, self.ty.port_type()).ok()).unwrap_or_else(|| zero(self.ty))
    }
}

impl Node for Changed {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Trigger { event, .. } => {
                let name = event.payload.get("name").and_then(Value::as_str);
                if name.is_none() || name != self.variable.as_ref().map(VarName::as_str) {
                    return Ok(());
                }
                let (old, new) = (self.value(event.payload.get("old")), self.value(event.payload.get("new")));
                cx.set("old", old)?;
                cx.set("new", new)?;
                cx.fire("fired")
            }
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Start | Input::Stop | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }
}
register_node!(Changed);
