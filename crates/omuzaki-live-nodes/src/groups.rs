//! The group nodes (design §6, D12, category Groups): `group.enable`, `group.disable`, `group.toggle` and the pure
//! `group.is_enabled`. Each names a group the show declares and needs `Need::Group`, so the compile checks it. A switch
//! takes effect at the end of the step, so the rest of the step still sees the group as it was.

use omuzaki_live_api::ids::GroupId;
use omuzaki_live_api::need::Need;
use omuzaki_live_api::node::{
    Category, EvalCx, Input, Node, NodeClass, NodeCx, NodeError, NodeKind, NodeMeta, ParamError, ParamErrors, Ports,
};
use omuzaki_live_api::register_node;
use omuzaki_live_api::value::{PortType, Val};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::l10n;

/// Parameters of every group node.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GroupParams {
    /// The group, as the show declares it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<GroupId>,
}

fn group_need(params: &GroupParams) -> Vec<Need> {
    params.group.iter().cloned().map(Need::group).collect()
}

fn check_group(params: &GroupParams) -> Result<(), ParamErrors> {
    match params.group {
        Some(_) => Ok(()),
        None => Err(ParamErrors(vec![ParamError::new("group", l10n::group_missing())])),
    }
}

/// What a switch node does to its group.
#[derive(Clone, Copy, Debug)]
enum Switch {
    Enable,
    Disable,
    Toggle,
}

/// The switch nodes' shared body.
#[derive(Debug)]
struct Switcher {
    group: Option<GroupId>,
    switch: Switch,
}

impl Switcher {
    fn ports(summary: &'static str) -> Ports {
        Ports::new()
            .event_in("in", summary)
            .event_out("done", "The switch is made; it takes effect at the end of this step.")
            .event_out("failed", "The group wasn't switched; the reason is on `error`.")
            .value_out("error", PortType::Text, "Why it wasn't switched, in English.")
    }

    fn handle(&self, input: &Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { port: "in", .. } => {
                let Some(group) = &self.group else { return cx.fail("failed", l10n::group_missing()) };
                let on = match self.switch {
                    Switch::Enable => true,
                    Switch::Disable => false,
                    Switch::Toggle => !cx.group_enabled(group).unwrap_or(false),
                };
                match cx.switch_group(group, on) {
                    Ok(()) => cx.fire("done"),
                    Err(e) => cx.fail("failed", l10n::group_refused(group.as_str(), e.to_string())),
                }
            }
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Start | Input::Stop | Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(other)),
        }
    }
}

macro_rules! switch_node {
    ($name:ident, $id:literal, $summary:literal, $in:literal, $switch:expr) => {
        #[doc = $summary]
        #[derive(Debug)]
        pub struct $name(Switcher);

        impl NodeKind for $name {
            type Params = GroupParams;

            fn meta() -> NodeMeta {
                NodeMeta::new($id, $summary, NodeClass::Active, Category::Groups)
            }

            fn ports(_: &GroupParams) -> Ports {
                Switcher::ports($in)
            }

            fn needs(params: &GroupParams) -> Vec<Need> {
                group_need(params)
            }

            fn check(params: &GroupParams) -> Result<(), ParamErrors> {
                check_group(params)
            }

            fn new(params: GroupParams) -> Self {
                Self(Switcher { group: params.group, switch: $switch })
            }
        }

        impl Node for $name {
            fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
                self.0.handle(&input, cx)
            }
        }
        register_node!($name);
    };
}

switch_node!(Enable, "group.enable", "Turn a group of automations on.", "Turns the group on.", Switch::Enable);
switch_node!(Disable, "group.disable", "Turn a group of automations off.", "Turns the group off.", Switch::Disable);
switch_node!(Toggle, "group.toggle", "Flip a group of automations on or off.", "Flips the group.", Switch::Toggle);

// ── group.is_enabled ───────────────────────────────────────────────────────────────────────────────────────────

/// Whether a group is on.
#[derive(Debug)]
pub struct IsEnabled {
    group: Option<GroupId>,
}

impl NodeKind for IsEnabled {
    type Params = GroupParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("group.is_enabled", "Whether a group of automations is on.", NodeClass::Pure, Category::Groups)
    }

    fn ports(_: &GroupParams) -> Ports {
        Ports::new().live_out("value", PortType::Bool, "Whether the group is on.")
    }

    fn needs(params: &GroupParams) -> Vec<Need> {
        group_need(params)
    }

    fn check(params: &GroupParams) -> Result<(), ParamErrors> {
        check_group(params)
    }

    fn new(params: GroupParams) -> Self {
        Self { group: params.group }
    }
}

impl Node for IsEnabled {
    fn handle(&mut self, input: Input<'_>, _: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Start | Input::Stop | Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }

    fn eval(&self, port: &str, cx: &mut EvalCx<'_>) -> Result<Val, NodeError> {
        match port {
            "value" => Ok(Val::Bool(self.group.as_ref().and_then(|g| cx.group_enabled(g)).unwrap_or(false))),
            other => Err(NodeError::not_live(other)),
        }
    }
}
register_node!(IsEnabled);
