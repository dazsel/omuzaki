//! The flow nodes (design §6, category Flow): `flow.wait`, `flow.random`, `flow.sequence`, `flow.switch`,
//! `flow.branch`, `flow.then`, `flow.cooldown` and `flow.gate`. They route events and never reach outside the show.
//! Every random draw comes from the node's own stream (`cx.rng`, design D6), so a show with a pinned seed repeats.

use std::collections::{BTreeMap, VecDeque};

use omuzaki_core::time::Seconds;
use omuzaki_live_api::ids::PortName;
use omuzaki_live_api::node::{
    Category, EvalCx, Input, Node, NodeClass, NodeCx, NodeError, NodeKind, NodeMeta, ParamError, ParamErrors, Ports,
    WakeId,
};
use omuzaki_live_api::register_node;
use omuzaki_live_api::value::{PortType, Val};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::l10n;

/// The most outputs a numbered family (`out_<n>`, `then_<n>`) has.
const MAX_OUTPUTS: usize = 16;
/// The most cases a switch has.
const MAX_CASES: usize = 32;
/// The most keys a per-user cooldown keeps (design D8); the oldest expired first.
pub(crate) const MAX_COOLDOWN_KEYS: usize = 10_000;

/// `<prefix>_<i>` for `i` in `1..=count`, as event outputs.
fn numbered(ports: Ports, prefix: &str, count: usize) -> Ports {
    (1..=count).fold(ports, |ports, i| match PortName::new(format!("{prefix}_{i}")) {
        Ok(name) => ports.dynamic_out(name, PortType::Event, format!("{i}")),
        Err(_) => ports,
    })
}

/// A count as a message's integer.
pub(crate) fn int(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// A count in `2..=MAX_OUTPUTS`, or the error for `path`.
fn check_count(path: &str, count: usize) -> Result<(), ParamErrors> {
    if (2..=MAX_OUTPUTS).contains(&count) {
        Ok(())
    } else {
        Err(ParamErrors::one(path, l10n::count_out_of_range(int(count), 2, int(MAX_OUTPUTS))))
    }
}

/// A uniform draw in `[0, 1)` from the node's stream, with all 53 bits of an `f64`.
#[allow(clippy::cast_precision_loss)] // 53 bits fit an f64's mantissa exactly
fn unit(cx: &mut NodeCx<'_>) -> f64 {
    let draw = (cx.rng().next_u64() >> 11) as f64;
    draw / (1u64 << 53) as f64
}

// ── flow.wait ──────────────────────────────────────────────────────────────────────────────────────────────────

/// Parameters of `flow.wait`: none; the duration is an input so it can vary per event.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaitParams {}

/// Delays each event on `in` by `seconds`; every event waits on its own, and `cancel` drops the waiting ones.
#[derive(Debug)]
pub struct Wait {
    pending: Vec<WakeId>,
}

impl NodeKind for Wait {
    type Params = WaitParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("flow.wait", "Wait, then pass the event on.", NodeClass::Active, Category::Flow)
    }

    fn ports(_: &WaitParams) -> Ports {
        Ports::new()
            .event_in("in", "Each event to delay.")
            .event_in("cancel", "Drops every event still waiting.")
            .value_in("seconds", PortType::Number, Val::Number(1.0), "How long to wait, in seconds.")
            .event_out("out", "The event, after the wait.")
    }

    fn new(_: WaitParams) -> Self {
        Self { pending: Vec::new() }
    }
}

impl Node for Wait {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { port: "in", .. } => {
                let seconds = cx.input_number("seconds")?.max(0.0);
                self.pending.push(cx.wake_after(Seconds(seconds))?);
                Ok(())
            }
            Input::Event { port: "cancel", .. } => {
                self.pending.drain(..).for_each(|id| cx.cancel_wake(id));
                Ok(())
            }
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Wake { id, .. } => {
                self.pending.retain(|p| *p != id);
                cx.fire("out")
            }
            Input::Start | Input::Stop | Input::Trigger { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }
}
register_node!(Wait);

// ── flow.random ────────────────────────────────────────────────────────────────────────────────────────────────

/// Parameters of `flow.random`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RandomParams {
    /// One weight per output (`out_1`, `out_2`, ...): 2 to 16 of them, each at least 0, adding up to more than 0. An
    /// output is picked in proportion to its weight.
    pub weights: Vec<f64>,
}

impl Default for RandomParams {
    fn default() -> Self {
        Self { weights: vec![1.0, 1.0] }
    }
}

/// Passes each event to one of its outputs, picked at random by weight.
#[derive(Debug)]
pub struct Random {
    /// Running totals of the weights, for the pick.
    totals: Vec<f64>,
}

impl NodeKind for Random {
    type Params = RandomParams;

    fn meta() -> NodeMeta {
        NodeMeta::new(
            "flow.random",
            "Pass the event to one output, picked at random.",
            NodeClass::Active,
            Category::Flow,
        )
    }

    fn ports(params: &RandomParams) -> Ports {
        numbered(Ports::new().event_in("in", "Each event to send on."), "out", params.weights.len())
    }

    fn check(params: &RandomParams) -> Result<(), ParamErrors> {
        check_count("weights", params.weights.len())?;
        let mut errors = Vec::new();
        for (i, w) in params.weights.iter().enumerate() {
            if !w.is_finite() || *w < 0.0 {
                errors.push(ParamError::new(format!("weights.{i}"), l10n::bad_weight()));
            }
        }
        if errors.is_empty() && params.weights.iter().sum::<f64>() <= 0.0 {
            errors.push(ParamError::new("weights", l10n::weights_sum_zero()));
        }
        if errors.is_empty() { Ok(()) } else { Err(ParamErrors(errors)) }
    }

    fn new(params: RandomParams) -> Self {
        let totals = params
            .weights
            .iter()
            .scan(0.0, |sum, w| {
                *sum += w;
                Some(*sum)
            })
            .collect();
        Self { totals }
    }
}

impl Node for Random {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { port: "in", .. } => {
                let total = self.totals.last().copied().unwrap_or(0.0);
                let draw = unit(cx) * total;
                // The first output whose running total passes the draw; a zero weight never does.
                let pick = self.totals.iter().position(|t| draw < *t).unwrap_or(self.totals.len().saturating_sub(1));
                cx.fire(&format!("out_{}", pick + 1))
            }
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Start | Input::Stop | Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }
}
register_node!(Random);

// ── flow.sequence ──────────────────────────────────────────────────────────────────────────────────────────────

fn default_true() -> bool {
    true
}

/// Parameters of `flow.sequence`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SequenceParams {
    /// How many outputs (`out_1` ... `out_<count>`): 2 to 16.
    pub count: usize,
    /// After the last output, start again at the first; otherwise fire nothing until `reset`.
    #[serde(default = "default_true")]
    pub wrap: bool,
}

impl Default for SequenceParams {
    fn default() -> Self {
        Self { count: 2, wrap: true }
    }
}

/// Passes each event to the next output in turn.
#[derive(Debug)]
pub struct Sequence {
    count: usize,
    wrap: bool,
    next: usize,
}

impl NodeKind for Sequence {
    type Params = SequenceParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("flow.sequence", "Pass each event to the next output in turn.", NodeClass::Active, Category::Flow)
    }

    fn ports(params: &SequenceParams) -> Ports {
        let ports =
            Ports::new().event_in("in", "Each event to send on.").event_in("reset", "Starts again at the first.");
        numbered(ports, "out", params.count)
    }

    fn check(params: &SequenceParams) -> Result<(), ParamErrors> {
        check_count("count", params.count)
    }

    fn new(params: SequenceParams) -> Self {
        Self { count: params.count, wrap: params.wrap, next: 0 }
    }
}

impl Node for Sequence {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { port: "in", .. } => {
                if self.next >= self.count {
                    if !self.wrap {
                        return Ok(());
                    }
                    self.next = 0;
                }
                self.next += 1;
                cx.fire(&format!("out_{}", self.next))
            }
            Input::Event { port: "reset", .. } => {
                self.next = 0;
                Ok(())
            }
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Start | Input::Stop | Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }
}
register_node!(Sequence);

// ── flow.switch ────────────────────────────────────────────────────────────────────────────────────────────────

/// Parameters of `flow.switch`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SwitchParams {
    /// The values to match, in order: 1 to 32. The event goes to `case_<i>` for the first one equal to `value`.
    pub cases: Vec<String>,
}

impl Default for SwitchParams {
    fn default() -> Self {
        Self { cases: vec![String::new()] }
    }
}

/// Passes each event to the first case equal to `value`, else to `default`.
#[derive(Debug)]
pub struct Switch {
    cases: Vec<String>,
}

impl NodeKind for Switch {
    type Params = SwitchParams;

    fn meta() -> NodeMeta {
        NodeMeta::new(
            "flow.switch",
            "Pass the event to the case that matches a value.",
            NodeClass::Active,
            Category::Flow,
        )
    }

    fn ports(params: &SwitchParams) -> Ports {
        let ports = Ports::new().event_in("in", "Each event to send on.").value_in(
            "value",
            PortType::Text,
            Val::text(""),
            "The value to match against the cases.",
        );
        let ports = params.cases.iter().enumerate().fold(ports, |ports, (i, case)| {
            match PortName::new(format!("case_{}", i + 1)) {
                Ok(name) => ports.dynamic_out(name, PortType::Event, case.clone()),
                Err(_) => ports,
            }
        });
        ports.event_out("default", "No case matched.")
    }

    fn check(params: &SwitchParams) -> Result<(), ParamErrors> {
        if (1..=MAX_CASES).contains(&params.cases.len()) {
            Ok(())
        } else {
            Err(ParamErrors::one("cases", l10n::count_out_of_range(int(params.cases.len()), 1, int(MAX_CASES))))
        }
    }

    fn new(params: SwitchParams) -> Self {
        Self { cases: params.cases }
    }
}

impl Node for Switch {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { port: "in", .. } => {
                // Numbers arrive as text by the coercion rules (design D3), so 3 matches the case "3".
                let value = cx.input_text("value")?;
                match self.cases.iter().position(|case| case.as_str() == &*value) {
                    Some(i) => cx.fire(&format!("case_{}", i + 1)),
                    None => cx.fire("default"),
                }
            }
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Start | Input::Stop | Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }
}
register_node!(Switch);

// ── flow.branch ────────────────────────────────────────────────────────────────────────────────────────────────

/// Parameters of `flow.branch`: none.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BranchParams {}

/// Passes each event to `true` or `false`, by its condition.
#[derive(Debug)]
pub struct Branch;

impl NodeKind for Branch {
    type Params = BranchParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("flow.branch", "Pass the event one way or the other.", NodeClass::Active, Category::Flow)
    }

    fn ports(_: &BranchParams) -> Ports {
        Ports::new()
            .event_in("in", "Each event to send on.")
            .value_in("condition", PortType::Bool, Val::Bool(false), "Which way it goes.")
            .event_out("true", "The condition holds.")
            .event_out("false", "The condition doesn't hold.")
    }

    fn new(_: BranchParams) -> Self {
        Self
    }
}

impl Node for Branch {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { port: "in", .. } => {
                let port = if cx.input_bool("condition")? { "true" } else { "false" };
                cx.fire(port)
            }
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Start | Input::Stop | Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }
}
register_node!(Branch);

// ── flow.then ──────────────────────────────────────────────────────────────────────────────────────────────────

/// Parameters of `flow.then`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ThenParams {
    /// How many outputs (`then_1` ... `then_<count>`): 2 to 16.
    pub count: usize,
}

impl Default for ThenParams {
    fn default() -> Self {
        Self { count: 2 }
    }
}

/// Passes each event to every output in order; each output's whole chain runs before the next starts (design D5).
#[derive(Debug)]
pub struct Then {
    count: usize,
}

impl NodeKind for Then {
    type Params = ThenParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("flow.then", "Do several things, one after the other.", NodeClass::Active, Category::Flow)
    }

    fn ports(params: &ThenParams) -> Ports {
        numbered(Ports::new().event_in("in", "Each event to send on."), "then", params.count)
    }

    fn check(params: &ThenParams) -> Result<(), ParamErrors> {
        check_count("count", params.count)
    }

    fn new(params: ThenParams) -> Self {
        Self { count: params.count }
    }
}

impl Node for Then {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { port: "in", .. } => {
                for i in 1..=self.count {
                    cx.fire(&format!("then_{i}"))?;
                }
                Ok(())
            }
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Start | Input::Stop | Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }
}
register_node!(Then);

// ── flow.cooldown ──────────────────────────────────────────────────────────────────────────────────────────────

/// Whose cooldown an event counts against.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CooldownPer {
    /// One cooldown for everyone.
    #[default]
    None,
    /// One per user (the root event's `user.id`); events without a user share one.
    User,
}

/// Parameters of `flow.cooldown`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CooldownParams {
    /// One cooldown for everyone (`none`) or one per user (`user`).
    #[serde(default)]
    pub per: CooldownPer,
}

/// When each key last passed, oldest first, bounded to [`MAX_COOLDOWN_KEYS`].
#[derive(Debug, Default)]
pub(crate) struct Cooldowns {
    last: BTreeMap<String, Seconds>,
    order: VecDeque<(String, Seconds)>,
}

impl Cooldowns {
    /// Seconds left for `key` under a cooldown of `seconds` at `now`, or `None` when it may pass.
    pub(crate) fn left(&self, key: &str, seconds: f64, now: Seconds) -> Option<f64> {
        let last = self.last.get(key)?;
        let left = last.0 + seconds - now.0;
        (left > 0.0).then_some(left)
    }

    /// Records that `key` passed at `now`, forgetting the oldest key past the bound.
    pub(crate) fn pass(&mut self, key: &str, now: Seconds) {
        self.last.insert(key.to_owned(), now);
        self.order.push_back((key.to_owned(), now));
        while self.order.len() > MAX_COOLDOWN_KEYS * 2 || self.last.len() > MAX_COOLDOWN_KEYS {
            let Some((old, at)) = self.order.pop_front() else { break };
            // Only an entry still current is forgotten; a stale one (the key passed again since) just leaves the list.
            if self.last.get(&old) == Some(&at) && self.last.len() > MAX_COOLDOWN_KEYS {
                self.last.remove(&old);
            }
        }
    }

    /// How many keys it holds.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.last.len()
    }
}

/// Passes an event at most once per `seconds` (for everyone or per user); the rest go to `cooling` with `left`.
#[derive(Debug)]
pub struct Cooldown {
    per: CooldownPer,
    keys: Cooldowns,
}

impl NodeKind for Cooldown {
    type Params = CooldownParams;

    fn meta() -> NodeMeta {
        NodeMeta::new(
            "flow.cooldown",
            "Let an event through at most once in a while.",
            NodeClass::Active,
            Category::Flow,
        )
    }

    fn ports(_: &CooldownParams) -> Ports {
        Ports::new()
            .event_in("in", "Each event to check.")
            .value_in("seconds", PortType::Number, Val::Number(10.0), "How long the cooldown lasts, in seconds.")
            .event_out("out", "The event, outside the cooldown.")
            .event_out("cooling", "The event came during the cooldown; it doesn't pass.")
            .value_out("left", PortType::Number, "Seconds left in the cooldown.")
    }

    fn new(params: CooldownParams) -> Self {
        Self { per: params.per, keys: Cooldowns::default() }
    }
}

impl Node for Cooldown {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { port: "in", .. } => {
                let seconds = cx.input_number("seconds")?.max(0.0);
                let key = match self.per {
                    CooldownPer::None => String::new(),
                    CooldownPer::User => {
                        cx.event().payload.pointer("/user/id").and_then(|v| v.as_str()).unwrap_or_default().to_owned()
                    }
                };
                let now = cx.now();
                if let Some(left) = self.keys.left(&key, seconds, now) {
                    cx.set("left", Val::Number(left))?;
                    cx.fire("cooling")
                } else {
                    self.keys.pass(&key, now);
                    cx.fire("out")
                }
            }
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Start | Input::Stop | Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }
}
register_node!(Cooldown);

// ── flow.gate ──────────────────────────────────────────────────────────────────────────────────────────────────

/// Parameters of `flow.gate`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GateParams {
    /// Whether the gate is open when the show starts.
    #[serde(default)]
    pub start_open: bool,
}

/// Passes events only while open; `open`, `close` and `toggle` change it.
#[derive(Debug)]
pub struct Gate {
    start_open: bool,
    open: bool,
}

impl NodeKind for Gate {
    type Params = GateParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("flow.gate", "Let events through only while open.", NodeClass::Active, Category::Flow)
    }

    fn ports(_: &GateParams) -> Ports {
        Ports::new()
            .event_in("in", "Each event to let through.")
            .event_in("open", "Opens the gate.")
            .event_in("close", "Closes the gate.")
            .event_in("toggle", "Opens the gate if closed, closes it if open.")
            .event_out("out", "The event, while the gate is open.")
            .live_out("is_open", PortType::Bool, "Whether the gate is open.")
    }

    fn new(params: GateParams) -> Self {
        Self { start_open: params.start_open, open: params.start_open }
    }
}

impl Node for Gate {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { port: "in", .. } => {
                if self.open {
                    cx.fire("out")?;
                }
                Ok(())
            }
            Input::Event { port: "open", .. } => {
                self.open = true;
                Ok(())
            }
            Input::Event { port: "close", .. } => {
                self.open = false;
                Ok(())
            }
            Input::Event { port: "toggle", .. } => {
                self.open = !self.open;
                Ok(())
            }
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Start => {
                self.open = self.start_open;
                Ok(())
            }
            Input::Stop | Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }

    fn eval(&self, port: &str, _: &mut EvalCx<'_>) -> Result<Val, NodeError> {
        match port {
            "is_open" => Ok(Val::Bool(self.open)),
            other => Err(NodeError::not_live(other)),
        }
    }
}
register_node!(Gate);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cooldown_keys_stay_bounded_and_forget_the_oldest() {
        let mut keys = Cooldowns::default();
        for i in 0..MAX_COOLDOWN_KEYS + 50 {
            #[allow(clippy::cast_precision_loss)]
            keys.pass(&format!("user{i}"), Seconds(i as f64));
        }
        assert_eq!(keys.len(), MAX_COOLDOWN_KEYS);
        assert!(keys.left("user0", 1e9, Seconds(1e6)).is_none(), "the oldest was forgotten");
        assert!(keys.left(&format!("user{}", MAX_COOLDOWN_KEYS + 49), 1e9, Seconds(1e6)).is_some());
    }
}
