//! The logic nodes (design §6, category Logic): `logic.and`, `logic.or` and `logic.not`. They are pure: no event ports,
//! a live `value` computed from their inputs whenever something reads it.

use omuzaki_live_api::node::{Category, EvalCx, Input, Node, NodeClass, NodeCx, NodeError, NodeKind, NodeMeta, Ports};
use omuzaki_live_api::register_node;
use omuzaki_live_api::value::{PortType, Val};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Parameters of the logic nodes: none.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogicParams {}

/// A pure node never handles an input (design D4); a stray one is refused as unknown.
fn pure_handle(input: &Input<'_>) -> Result<(), NodeError> {
    match input {
        Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
        Input::Start | Input::Stop | Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
        other => Err(NodeError::unhandled(other)),
    }
}

/// True when both `a` and `b` are.
#[derive(Debug)]
pub struct And;

impl NodeKind for And {
    type Params = LogicParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("logic.and", "True when both inputs are true.", NodeClass::Pure, Category::Logic)
    }

    fn ports(_: &LogicParams) -> Ports {
        Ports::new()
            .value_in("a", PortType::Bool, Val::Bool(false), "The first condition.")
            .value_in("b", PortType::Bool, Val::Bool(false), "The second condition.")
            .live_out("value", PortType::Bool, "Both are true.")
    }

    fn new(_: LogicParams) -> Self {
        Self
    }
}

impl Node for And {
    fn handle(&mut self, input: Input<'_>, _: &mut NodeCx<'_>) -> Result<(), NodeError> {
        pure_handle(&input)
    }

    fn eval(&self, port: &str, cx: &mut EvalCx<'_>) -> Result<Val, NodeError> {
        match port {
            "value" => Ok(Val::Bool(cx.input_bool("a")? && cx.input_bool("b")?)),
            other => Err(NodeError::not_live(other)),
        }
    }
}
register_node!(And);

/// True when `a` or `b` is.
#[derive(Debug)]
pub struct Or;

impl NodeKind for Or {
    type Params = LogicParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("logic.or", "True when either input is true.", NodeClass::Pure, Category::Logic)
    }

    fn ports(_: &LogicParams) -> Ports {
        Ports::new()
            .value_in("a", PortType::Bool, Val::Bool(false), "The first condition.")
            .value_in("b", PortType::Bool, Val::Bool(false), "The second condition.")
            .live_out("value", PortType::Bool, "Either is true.")
    }

    fn new(_: LogicParams) -> Self {
        Self
    }
}

impl Node for Or {
    fn handle(&mut self, input: Input<'_>, _: &mut NodeCx<'_>) -> Result<(), NodeError> {
        pure_handle(&input)
    }

    fn eval(&self, port: &str, cx: &mut EvalCx<'_>) -> Result<Val, NodeError> {
        match port {
            "value" => Ok(Val::Bool(cx.input_bool("a")? || cx.input_bool("b")?)),
            other => Err(NodeError::not_live(other)),
        }
    }
}
register_node!(Or);

/// The opposite of `a`.
#[derive(Debug)]
pub struct Not;

impl NodeKind for Not {
    type Params = LogicParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("logic.not", "True when the input is false.", NodeClass::Pure, Category::Logic)
    }

    fn ports(_: &LogicParams) -> Ports {
        Ports::new().value_in("a", PortType::Bool, Val::Bool(false), "The condition.").live_out(
            "value",
            PortType::Bool,
            "The condition is false.",
        )
    }

    fn new(_: LogicParams) -> Self {
        Self
    }
}

impl Node for Not {
    fn handle(&mut self, input: Input<'_>, _: &mut NodeCx<'_>) -> Result<(), NodeError> {
        pure_handle(&input)
    }

    fn eval(&self, port: &str, cx: &mut EvalCx<'_>) -> Result<Val, NodeError> {
        match port {
            "value" => Ok(Val::Bool(!cx.input_bool("a")?)),
            other => Err(NodeError::not_live(other)),
        }
    }
}
register_node!(Not);
