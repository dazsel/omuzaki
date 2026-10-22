//! Test node kinds shared by the engine's guard and group tests, and the helpers that build shows from them.

// Each test binary uses some of these.
#![allow(dead_code)]

use omuzaki_core::time::Seconds;
use omuzaki_live::testing::ShowHarness;
use omuzaki_live_api::event::{LiveEvent, Source};
use omuzaki_live_api::graph::GraphDef;
use omuzaki_live_api::ids::{EventKind, GraphId, GroupId, ShowId, VarName};
use omuzaki_live_api::node::{Category, Input, Node, NodeClass, NodeCx, NodeError, NodeKind, NodeMeta, Ports};
use omuzaki_live_api::register_node;
use omuzaki_live_api::show::{GroupDef, ShowDef, VarScope, VarType, VariableDef};
use omuzaki_live_api::value::Val;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Parameters naming an event kind.
#[derive(Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KindParams {
    /// The kind.
    #[serde(default)]
    pub kind: String,
}

/// A trigger on `kind` that fires `fired`.
pub struct On;

impl NodeKind for On {
    type Params = KindParams;
    fn meta() -> NodeMeta {
        NodeMeta::new("test.on", "A trigger.", NodeClass::Trigger, Category::Triggers)
    }
    fn ports(_: &KindParams) -> Ports {
        Ports::new().event_out("fired", "It fired.")
    }
    fn listens(params: &KindParams) -> Vec<EventKind> {
        EventKind::new(params.kind.as_str()).into_iter().collect()
    }
    fn new(_: KindParams) -> Self {
        Self
    }
}

impl Node for On {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Trigger { .. } => cx.fire("fired"),
            _ => Ok(()),
        }
    }
}
register_node!(On);

/// Parameters with nothing in them.
#[derive(Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoParams {}

fn bump(cx: &mut NodeCx<'_>, name: &str) -> Result<(), NodeError> {
    let Ok(name) = VarName::new(name) else { return Ok(()) };
    if let Some(Val::Number(n)) = cx.var(&name) {
        cx.set_var(&name, Val::Number(n + 1.0))?;
    }
    Ok(())
}

/// Adds one to the variable `n` (when the show declares it) on each pulse and fires `out`; adds one to `starts` on
/// each `Start`.
pub struct Count;

impl NodeKind for Count {
    type Params = NoParams;
    fn meta() -> NodeMeta {
        NodeMeta::new("test.count", "Counts.", NodeClass::Active, Category::Flow)
    }
    fn ports(_: &NoParams) -> Ports {
        Ports::new().event_in("in", "In.").event_out("out", "Out.")
    }
    fn new(_: NoParams) -> Self {
        Self
    }
}

impl Node for Count {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Start => bump(cx, "starts"),
            Input::Event { .. } => {
                bump(cx, "n")?;
                cx.fire("out")
            }
            _ => Ok(()),
        }
    }
}
register_node!(Count);

/// Parameters naming an op.
#[derive(Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpParams {
    /// The op.
    #[serde(default)]
    pub op: String,
}

/// Requests its op; fires `done` when the reply comes back.
pub struct Call {
    op: String,
}

impl NodeKind for Call {
    type Params = OpParams;
    fn meta() -> NodeMeta {
        NodeMeta::new("test.call", "Calls an op.", NodeClass::Active, Category::Actions)
    }
    fn ports(_: &OpParams) -> Ports {
        Ports::new().event_in("in", "Run.").event_out("done", "Answered.")
    }
    fn new(params: OpParams) -> Self {
        Self { op: params.op }
    }
}

impl Node for Call {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { .. } => cx.run_op(&self.op, json!({})).map(|_| ()),
            Input::Reply { .. } => cx.fire("done"),
            _ => Ok(()),
        }
    }
}
register_node!(Call);

/// Emits a `kind` event on each pulse.
pub struct Emit {
    kind: String,
}

impl NodeKind for Emit {
    type Params = KindParams;
    fn meta() -> NodeMeta {
        NodeMeta::new("test.emit", "Emits.", NodeClass::Active, Category::Actions)
    }
    fn ports(_: &KindParams) -> Ports {
        Ports::new().event_in("in", "In.")
    }
    fn new(params: KindParams) -> Self {
        Self { kind: params.kind }
    }
}

impl Node for Emit {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { .. } => {
                let kind = EventKind::new(self.kind.as_str()).map_err(|_| NodeError::unknown_port("kind"))?;
                cx.emit(kind, json!({}))
            }
            _ => Ok(()),
        }
    }
}
register_node!(Emit);

/// Parameters of `test.switch`.
#[derive(Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SwitchParams {
    /// The group.
    #[serde(default)]
    pub group: String,
    /// On or off.
    #[serde(default)]
    pub on: bool,
}

/// Switches its group and fires `out`, so the chain goes on in the same step.
pub struct Switch {
    group: String,
    on: bool,
}

impl NodeKind for Switch {
    type Params = SwitchParams;
    fn meta() -> NodeMeta {
        NodeMeta::new("test.switch", "Switches a group.", NodeClass::Active, Category::Flow)
    }
    fn ports(_: &SwitchParams) -> Ports {
        Ports::new().event_in("in", "In.").event_out("out", "Out.")
    }
    fn new(params: SwitchParams) -> Self {
        Self { group: params.group, on: params.on }
    }
}

impl Node for Switch {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { .. } => {
                let group = GroupId::new(self.group.as_str()).map_err(|_| NodeError::unknown_port("group"))?;
                cx.switch_group(&group, self.on)?;
                cx.fire("out")
            }
            _ => Ok(()),
        }
    }
}
register_node!(Switch);

/// Parameters of `test.wait`.
#[derive(Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaitParams {
    /// Seconds.
    #[serde(default)]
    pub seconds: f64,
}

/// Waits `seconds` in the token that arrived, then fires `out`.
pub struct Wait {
    seconds: f64,
}

impl NodeKind for Wait {
    type Params = WaitParams;
    fn meta() -> NodeMeta {
        NodeMeta::new("test.wait", "Waits.", NodeClass::Active, Category::Flow)
    }
    fn ports(_: &WaitParams) -> Ports {
        Ports::new().event_in("in", "In.").event_out("out", "Out.")
    }
    fn new(params: WaitParams) -> Self {
        Self { seconds: params.seconds }
    }
}

impl Node for Wait {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { .. } => cx.wake_after(Seconds(self.seconds)).map(|_| ()),
            Input::Wake { .. } => cx.fire("out"),
            _ => Ok(()),
        }
    }
}
register_node!(Wait);

// ── Helpers ────────────────────────────────────────────────────────────────────────────────────────────────────

/// A show with the counters `n` and `starts`, the groups `groups` (each on at the start) and `graphs`, each
/// `(id, group, nodes)`.
pub fn show(groups: &[&str], graphs: &[(&str, Option<&str>, Value)]) -> ShowDef {
    let counter = || VariableDef::new(VarType::Number, VarScope::Session).with_default(json!(0));
    let mut def = ShowDef::new("Test")
        .with_seed(7)
        .with_variable(VarName::new("n").unwrap(), counter())
        .with_variable(VarName::new("starts").unwrap(), counter());
    for group in groups {
        def = def.with_group(GroupId::new(*group).unwrap(), GroupDef::new());
    }
    for (id, group, nodes) in graphs {
        let mut graph = GraphDef::from_node(&json!({ "nodes": nodes })).unwrap();
        if let Some(group) = group {
            graph = graph.with_group(GroupId::new(*group).unwrap());
        }
        def = def.with_graph(GraphId::new(*id).unwrap(), graph);
    }
    def
}

/// `def` in a harness, as the show `main`.
pub fn harness(def: ShowDef) -> ShowHarness {
    ShowHarness::new(ShowId::new("main").unwrap(), def)
}

/// A `custom.<name>` event at `at`.
pub fn custom(name: &str, at: f64) -> LiveEvent {
    LiveEvent::new(EventKind::new(format!("custom.{name}")).unwrap(), Source::Test, Seconds(at), json!({}))
}

/// The number in variable `name`.
pub fn var(h: &mut ShowHarness, name: &str) -> f64 {
    let values = h.values();
    let row = values.variables.iter().find(|v| v.name.as_str() == name).unwrap();
    row.value.as_f64().unwrap()
}

/// The ops requested so far, in order.
pub fn ops(h: &ShowHarness) -> Vec<String> {
    h.requests().iter().map(|r| r.op.clone()).collect()
}
