//! The math nodes (design §6, category Math): `math.op`, `math.map_range`, `math.ease` and `math.compare` are pure,
//! `math.random_range` is active (it draws on an event, so the pure ones stay pure).
//!
//! A pure node never fails a branch: a domain error (a zero divisor, a power that isn't a finite number, an empty
//! input range) reads as `value = 0`, `ok = false` and an `error` text, and every `value` is a finite number.

use omuzaki_anim_api::Ease;
use omuzaki_live_api::node::{
    Category, EvalCx, Input, Node, NodeClass, NodeCx, NodeError, NodeKind, NodeMeta, ParamErrors, Ports,
};
use omuzaki_live_api::register_node;
use omuzaki_live_api::value::{PortType, Val};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::l10n;

/// A pure node's handle: it never gets an event (design D4).
fn pure_handle(input: &Input<'_>) -> Result<(), NodeError> {
    match input {
        Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
        Input::Start | Input::Stop | Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
        other => Err(NodeError::unhandled(other)),
    }
}

/// The result of a pure computation: a finite value, or a domain error in English.
type Outcome = Result<Val, String>;

/// `value`, `ok` or `error` of an outcome.
fn read(port: &str, outcome: Outcome, zero: Val) -> Result<Val, NodeError> {
    match (port, outcome) {
        ("value", Ok(value)) => Ok(value),
        ("value", Err(_)) => Ok(zero),
        ("ok", outcome) => Ok(Val::Bool(outcome.is_ok())),
        ("error", Ok(_)) => Ok(Val::text("")),
        ("error", Err(why)) => Ok(Val::text(why)),
        (other, _) => Err(NodeError::not_live(other)),
    }
}

/// `x` when finite, else the domain error `what`.
fn finite(x: f64, what: &str) -> Outcome {
    if x.is_finite() { Ok(Val::Number(x)) } else { Err(format!("{what} isn't a finite number")) }
}

/// The three live outputs every pure math node has.
fn results(ports: Ports, value: PortType) -> Ports {
    ports
        .live_out("value", value, "The result; 0 when it can't be computed.")
        .live_out("ok", PortType::Bool, "Whether the result could be computed.")
        .live_out("error", PortType::Text, "Why it couldn't, in English; empty when it could.")
}

// ── math.op ────────────────────────────────────────────────────────────────────────────────────────────────────

/// An arithmetic operation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MathOp {
    /// a + b
    #[default]
    Add,
    /// a − b
    Sub,
    /// a × b
    Mul,
    /// a ÷ b
    Div,
    /// The smaller of a and b.
    Min,
    /// The larger of a and b.
    Max,
    /// The remainder of a ÷ b, with the sign of b (so it suits wrapping counters).
    Mod,
    /// a to the power b.
    Pow,
    /// |a|
    Abs,
    /// a rounded to the nearest whole number, halves away from zero.
    Round,
    /// a rounded down.
    Floor,
    /// a rounded up.
    Ceil,
    /// a kept between `min` and `max`.
    Clamp,
}

impl MathOp {
    /// The number inputs it takes.
    const fn inputs(self) -> &'static [&'static str] {
        match self {
            Self::Add | Self::Sub | Self::Mul | Self::Div | Self::Min | Self::Max | Self::Mod | Self::Pow => {
                &["a", "b"]
            }
            Self::Abs | Self::Round | Self::Floor | Self::Ceil => &["a"],
            Self::Clamp => &["a", "min", "max"],
        }
    }

    fn apply(self, a: f64, b: f64, c: f64) -> Outcome {
        match self {
            Self::Add => finite(a + b, "the sum"),
            Self::Sub => finite(a - b, "the difference"),
            Self::Mul => finite(a * b, "the product"),
            Self::Div if b == 0.0 => Err("division by zero".to_owned()),
            Self::Div => finite(a / b, "the quotient"),
            Self::Min => Ok(Val::Number(a.min(b))),
            Self::Max => Ok(Val::Number(a.max(b))),
            Self::Mod if b == 0.0 => Err("the remainder of a division by zero".to_owned()),
            Self::Mod => finite(a - b * (a / b).floor(), "the remainder"),
            Self::Pow => finite(a.powf(b), "the power"),
            Self::Abs => Ok(Val::Number(a.abs())),
            Self::Round => Ok(Val::Number(a.round())),
            Self::Floor => Ok(Val::Number(a.floor())),
            Self::Ceil => Ok(Val::Number(a.ceil())),
            Self::Clamp if b > c => Err(format!("the minimum {b} is above the maximum {c}")),
            Self::Clamp => Ok(Val::Number(a.clamp(b, c))),
        }
    }
}

/// Parameters of `math.op`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MathOpParams {
    /// The operation: `add`, `sub`, `mul`, `div`, `min`, `max`, `mod`, `pow` on `a` and `b`; `abs`, `round`, `floor`,
    /// `ceil` on `a`; `clamp` on `a` between `min` and `max`.
    #[serde(default)]
    pub op: MathOp,
}

/// One arithmetic operation on its inputs.
#[derive(Debug)]
pub struct MathOpNode {
    op: MathOp,
}

impl MathOpNode {
    fn compute(&self, cx: &mut EvalCx<'_>) -> Result<Outcome, NodeError> {
        let mut x = [0.0; 3];
        for (slot, port) in x.iter_mut().zip(self.op.inputs()) {
            *slot = cx.input_number(port)?;
        }
        Ok(self.op.apply(x[0], x[1], x[2]))
    }
}

impl NodeKind for MathOpNode {
    type Params = MathOpParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("math.op", "Add, subtract, multiply, round and the like.", NodeClass::Pure, Category::Math)
    }

    fn ports(params: &MathOpParams) -> Ports {
        let ports = params.op.inputs().iter().fold(Ports::new(), |ports, port| match *port {
            "a" => ports.value_in("a", PortType::Number, Val::Number(0.0), "The first number."),
            "b" => ports.value_in("b", PortType::Number, Val::Number(0.0), "The second number."),
            "min" => ports.value_in("min", PortType::Number, Val::Number(0.0), "The least the result may be."),
            _ => ports.value_in("max", PortType::Number, Val::Number(1.0), "The most the result may be."),
        });
        results(ports, PortType::Number)
    }

    fn new(params: MathOpParams) -> Self {
        Self { op: params.op }
    }
}

impl Node for MathOpNode {
    fn handle(&mut self, input: Input<'_>, _: &mut NodeCx<'_>) -> Result<(), NodeError> {
        pure_handle(&input)
    }

    fn eval(&self, port: &str, cx: &mut EvalCx<'_>) -> Result<Val, NodeError> {
        read(port, self.compute(cx)?, Val::Number(0.0))
    }
}
register_node!(MathOpNode);

// ── math.map_range ─────────────────────────────────────────────────────────────────────────────────────────────

/// Parameters of `math.map_range`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MapRangeParams {
    /// Keep the result inside the output range even when the value is outside the input range.
    #[serde(default)]
    pub clamp: bool,
}

/// Maps `value` from `in_min..in_max` onto `out_min..out_max`.
#[derive(Debug)]
pub struct MapRange {
    clamp: bool,
}

/// `value` mapped from one range onto another; the endpoints map exactly.
pub(crate) fn map_range(value: f64, from: (f64, f64), to: (f64, f64), clamp: bool) -> Outcome {
    let (in_min, in_max) = from;
    let (out_min, out_max) = to;
    // An exactly empty range is the only one that can't be mapped; any other width, however small, can.
    #[allow(clippy::float_cmp)]
    if in_min == in_max {
        return Err("the input range is empty (its minimum equals its maximum)".to_owned());
    }
    let mut t = (value - in_min) / (in_max - in_min);
    if clamp {
        t = t.clamp(0.0, 1.0);
    }
    // Written so t = 0 gives out_min and t = 1 gives out_max exactly.
    finite(out_min * (1.0 - t) + out_max * t, "the mapped value")
}

impl NodeKind for MapRange {
    type Params = MapRangeParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("math.map_range", "Map a number from one range onto another.", NodeClass::Pure, Category::Math)
    }

    fn ports(_: &MapRangeParams) -> Ports {
        let ports = Ports::new()
            .value_in("value", PortType::Number, Val::Number(0.0), "The number to map.")
            .value_in("in_min", PortType::Number, Val::Number(0.0), "The input range's start.")
            .value_in("in_max", PortType::Number, Val::Number(1.0), "The input range's end.")
            .value_in("out_min", PortType::Number, Val::Number(0.0), "The output range's start.")
            .value_in("out_max", PortType::Number, Val::Number(1.0), "The output range's end.");
        results(ports, PortType::Number)
    }

    fn new(params: MapRangeParams) -> Self {
        Self { clamp: params.clamp }
    }
}

impl Node for MapRange {
    fn handle(&mut self, input: Input<'_>, _: &mut NodeCx<'_>) -> Result<(), NodeError> {
        pure_handle(&input)
    }

    fn eval(&self, port: &str, cx: &mut EvalCx<'_>) -> Result<Val, NodeError> {
        let value = cx.input_number("value")?;
        let from = (cx.input_number("in_min")?, cx.input_number("in_max")?);
        let to = (cx.input_number("out_min")?, cx.input_number("out_max")?);
        read(port, map_range(value, from, to, self.clamp), Val::Number(0.0))
    }
}
register_node!(MapRange);

// ── math.ease ──────────────────────────────────────────────────────────────────────────────────────────────────

/// An easing curve.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Curve {
    /// Constant speed.
    Linear,
    /// Slow in, slow out.
    Smooth,
    /// Fast start, slow settle.
    #[default]
    OutCubic,
    /// Cubic in and out.
    InOutCubic,
    /// Overshoots a little, then settles.
    OutBack,
    /// Springs past the end and wobbles to rest.
    OutElastic,
    /// Bounces to rest.
    OutBounce,
}

impl Curve {
    /// The eased fraction for `t`, clamped to `0..=1`; every curve gives 0 at 0 and 1 at 1.
    pub fn apply(self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        let anim = |ease: Ease| {
            // The animation crate's curves, reused; its endpoints are exact in f32 too.
            #[allow(clippy::cast_possible_truncation)]
            let eased = ease.apply(t as f32);
            f64::from(eased)
        };
        match self {
            Self::Linear => t,
            Self::Smooth => anim(Ease::Smooth),
            Self::OutCubic => anim(Ease::OutCubic),
            Self::InOutCubic => anim(Ease::InOutCubic),
            Self::OutBack => {
                const C1: f64 = 1.701_58;
                const C3: f64 = C1 + 1.0;
                1.0 + C3 * (t - 1.0).powi(3) + C1 * (t - 1.0).powi(2)
            }
            Self::OutElastic => {
                // The ends are exact by definition; the formula would miss them by a rounding error.
                #[allow(clippy::float_cmp)]
                if t == 0.0 || t == 1.0 {
                    return t;
                }
                let c4 = std::f64::consts::TAU / 3.0;
                2f64.powf(-10.0 * t) * ((t * 10.0 - 0.75) * c4).sin() + 1.0
            }
            Self::OutBounce => out_bounce(t),
        }
    }
}

fn out_bounce(t: f64) -> f64 {
    const N1: f64 = 7.5625;
    const D1: f64 = 2.75;
    if t < 1.0 / D1 {
        N1 * t * t
    } else if t < 2.0 / D1 {
        let t = t - 1.5 / D1;
        N1 * t * t + 0.75
    } else if t < 2.5 / D1 {
        let t = t - 2.25 / D1;
        N1 * t * t + 0.9375
    } else {
        let t = t - 2.625 / D1;
        N1 * t * t + 0.984_375
    }
}

/// Parameters of `math.ease`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EaseParams {
    /// The curve.
    #[serde(default)]
    pub curve: Curve,
}

/// Eases `t` (0 to 1) along a curve.
#[derive(Debug)]
pub struct EaseNode {
    curve: Curve,
}

impl NodeKind for EaseNode {
    type Params = EaseParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("math.ease", "Shape a 0-to-1 fraction along an easing curve.", NodeClass::Pure, Category::Math)
    }

    fn ports(_: &EaseParams) -> Ports {
        let ports = Ports::new().value_in("t", PortType::Number, Val::Number(0.0), "The fraction, from 0 to 1.");
        results(ports, PortType::Number)
    }

    fn new(params: EaseParams) -> Self {
        Self { curve: params.curve }
    }
}

impl Node for EaseNode {
    fn handle(&mut self, input: Input<'_>, _: &mut NodeCx<'_>) -> Result<(), NodeError> {
        pure_handle(&input)
    }

    fn eval(&self, port: &str, cx: &mut EvalCx<'_>) -> Result<Val, NodeError> {
        let t = cx.input_number("t")?;
        read(port, finite(self.curve.apply(t), "the eased value"), Val::Number(0.0))
    }
}
register_node!(EaseNode);

// ── math.compare ───────────────────────────────────────────────────────────────────────────────────────────────

/// A comparison.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum CompareOp {
    /// Equal.
    #[default]
    #[serde(rename = "==")]
    Eq,
    /// Not equal.
    #[serde(rename = "!=")]
    Ne,
    /// Less than.
    #[serde(rename = "<")]
    Lt,
    /// At most.
    #[serde(rename = "<=")]
    Le,
    /// More than.
    #[serde(rename = ">")]
    Gt,
    /// At least.
    #[serde(rename = ">=")]
    Ge,
}

/// What a comparison compares.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompareOn {
    /// Numbers.
    #[default]
    Number,
    /// Text, exactly.
    Text,
}

fn default_epsilon() -> f64 {
    1e-9
}

/// Parameters of `math.compare`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompareParams {
    /// The comparison: `==`, `!=`, `<`, `<=`, `>`, `>=` for numbers; `==` and `!=` for text.
    #[serde(default)]
    pub op: CompareOp,
    /// Whether `a` and `b` are numbers or text.
    #[serde(default)]
    pub on: CompareOn,
    /// How close two numbers must be to count as equal.
    #[serde(default = "default_epsilon")]
    pub epsilon: f64,
}

impl Default for CompareParams {
    fn default() -> Self {
        Self { op: CompareOp::Eq, on: CompareOn::Number, epsilon: default_epsilon() }
    }
}

/// Compares `a` with `b`.
#[derive(Debug)]
pub struct Compare {
    params: CompareParams,
}

impl NodeKind for Compare {
    type Params = CompareParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("math.compare", "Compare two numbers or two texts.", NodeClass::Pure, Category::Math)
    }

    fn ports(params: &CompareParams) -> Ports {
        let ty = match params.on {
            CompareOn::Number => PortType::Number,
            CompareOn::Text => PortType::Text,
        };
        let zero = match params.on {
            CompareOn::Number => Val::Number(0.0),
            CompareOn::Text => Val::text(""),
        };
        let ports = Ports::new().value_in("a", ty, zero.clone(), "The first value.").value_in(
            "b",
            ty,
            zero,
            "The second value.",
        );
        results(ports, PortType::Bool)
    }

    fn check(params: &CompareParams) -> Result<(), ParamErrors> {
        if params.on == CompareOn::Text && !matches!(params.op, CompareOp::Eq | CompareOp::Ne) {
            return Err(ParamErrors::one("op", l10n::text_compare_op()));
        }
        if !params.epsilon.is_finite() || params.epsilon < 0.0 {
            return Err(ParamErrors::one("epsilon", l10n::bad_epsilon()));
        }
        Ok(())
    }

    fn new(params: CompareParams) -> Self {
        Self { params }
    }
}

impl Node for Compare {
    fn handle(&mut self, input: Input<'_>, _: &mut NodeCx<'_>) -> Result<(), NodeError> {
        pure_handle(&input)
    }

    fn eval(&self, port: &str, cx: &mut EvalCx<'_>) -> Result<Val, NodeError> {
        let result = match self.params.on {
            CompareOn::Text => {
                let equal = cx.input_text("a")? == cx.input_text("b")?;
                if self.params.op == CompareOp::Ne { !equal } else { equal }
            }
            CompareOn::Number => {
                let (a, b) = (cx.input_number("a")?, cx.input_number("b")?);
                let eq = (a - b).abs() <= self.params.epsilon;
                match self.params.op {
                    CompareOp::Eq => eq,
                    CompareOp::Ne => !eq,
                    CompareOp::Lt => a < b && !eq,
                    CompareOp::Le => a < b || eq,
                    CompareOp::Gt => a > b && !eq,
                    CompareOp::Ge => a > b || eq,
                }
            }
        };
        read(port, Ok(Val::Bool(result)), Val::Bool(false))
    }
}
register_node!(Compare);

// ── math.random_range ──────────────────────────────────────────────────────────────────────────────────────────

/// Parameters of `math.random_range`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RandomRangeParams {
    /// Whole numbers from `min` to `max`, both included; otherwise any number from `min` up to (not including) `max`.
    #[serde(default)]
    pub integer: bool,
}

/// Draws a random number on each event.
#[derive(Debug)]
pub struct RandomRange {
    integer: bool,
}

impl NodeKind for RandomRange {
    type Params = RandomRangeParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("math.random_range", "Pick a random number in a range.", NodeClass::Active, Category::Math)
    }

    fn ports(_: &RandomRangeParams) -> Ports {
        Ports::new()
            .event_in("in", "Draws a number.")
            .value_in("min", PortType::Number, Val::Number(0.0), "The least it may be.")
            .value_in("max", PortType::Number, Val::Number(1.0), "The most it may be.")
            .event_out("out", "A number was drawn; it is on `value`.")
            .event_out("failed", "The range is backwards; the reason is on `error`.")
            .value_out("value", PortType::Number, "The number drawn.")
            .value_out("error", PortType::Text, "Why nothing was drawn, in English.")
    }

    fn new(params: RandomRangeParams) -> Self {
        Self { integer: params.integer }
    }
}

impl Node for RandomRange {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { port: "in", .. } => {
                let (min, max) = (cx.input_number("min")?, cx.input_number("max")?);
                if min > max {
                    return cx.fail("failed", l10n::range_backwards(min, max));
                }
                #[allow(clippy::cast_precision_loss)] // 53 bits fit an f64's mantissa exactly
                let unit = (cx.rng().next_u64() >> 11) as f64 / (1u64 << 53) as f64;
                let value = if self.integer {
                    let (lo, hi) = (min.ceil(), max.floor());
                    if lo > hi {
                        return cx.fail("failed", l10n::range_backwards(min, max));
                    }
                    (lo + (unit * (hi - lo + 1.0)).floor()).min(hi)
                } else {
                    min + unit * (max - min)
                };
                cx.set("value", Val::Number(value))?;
                cx.fire("out")
            }
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Start | Input::Stop | Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }
}
register_node!(RandomRange);
