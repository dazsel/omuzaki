//! The audio level condition (design §6, D7, D11, category Conditions): `cond.audio_level` polls the `audio.meters`
//! query for one bus or device input while the show runs, and reports when the level rises above a threshold for a
//! while and falls back below it, with hysteresis. It reads numbers through the op; it never opens a device or a tap.

use omuzaki_core::time::Seconds;
use omuzaki_live_api::need::Need;
use omuzaki_live_api::node::{
    CallId, Category, EvalCx, Input, Node, NodeClass, NodeCx, NodeError, NodeKind, NodeMeta, ParamError, ParamErrors,
    Ports, WakeId,
};
use omuzaki_live_api::register_node;
use omuzaki_live_api::value::{PortType, Val};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::l10n;

/// The op the node polls.
const METERS: &str = "audio.meters";

/// The shortest poll period, in milliseconds.
const MIN_POLL_MS: f64 = 50.0;

/// Slack for comparing sums of poll periods against `for_ms`.
const EPSILON: f64 = 1e-9;

/// Which level to watch: a bus of the mixer, or a device input by its role.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub enum Source {
    /// A bus, after its fader: `{ "bus": "mic" }`.
    Bus {
        /// The bus id.
        bus: String,
    },
    /// A device input, before any processing: `{ "input": "mic" }`.
    Input {
        /// The device role.
        input: String,
    },
}

impl Default for Source {
    fn default() -> Self {
        Source::Bus { bus: "mic".to_owned() }
    }
}

impl Source {
    fn args(&self) -> Value {
        match self {
            Source::Bus { bus } => json!({ "bus": bus }),
            Source::Input { input } => json!({ "input": input }),
        }
    }
}

const fn above_default() -> f64 {
    -30.0
}
const fn for_default() -> f64 {
    300.0
}
const fn hysteresis_default() -> f64 {
    3.0
}
const fn poll_default() -> f64 {
    100.0
}

/// Parameters of `cond.audio_level`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LevelParams {
    /// The bus or device input to watch.
    #[serde(default)]
    pub source: Source,
    /// The level, in dBFS (RMS over 300 ms), that counts as loud.
    #[serde(default = "above_default")]
    pub above_db: f64,
    /// How long it has to stay above, in milliseconds, before `rose`.
    #[serde(default = "for_default")]
    pub for_ms: f64,
    /// How far below `above_db` it has to drop, in dB, before `fell`.
    #[serde(default = "hysteresis_default")]
    pub hysteresis_db: f64,
    /// How often to read the level, in milliseconds: at least 50.
    #[serde(default = "poll_default")]
    pub poll_ms: f64,
}

impl Default for LevelParams {
    fn default() -> Self {
        Self {
            source: Source::default(),
            above_db: above_default(),
            for_ms: for_default(),
            hysteresis_db: hysteresis_default(),
            poll_ms: poll_default(),
        }
    }
}

/// Whether a bus or input is loud, with `rose` and `fell` as it changes.
#[derive(Debug)]
pub struct AudioLevel {
    args: Value,
    above_db: f64,
    for_s: f64,
    hysteresis_db: f64,
    poll_ms: f64,
    /// When the show started, and polls since: poll n is at start + n × `poll_ms`, without drift.
    started: Seconds,
    polls: f64,
    /// While the show runs: the next poll.
    wake: Option<WakeId>,
    /// The poll in flight; a new one waits for it.
    call: Option<CallId>,
    /// Whether it is loud.
    loud: bool,
    /// Since when the level has been above the threshold, while not yet loud.
    above_since: Option<Seconds>,
    /// Polls that failed since the start.
    failed: f64,
}

impl NodeKind for AudioLevel {
    type Params = LevelParams;

    fn meta() -> NodeMeta {
        NodeMeta::new(
            "cond.audio_level",
            "When a microphone or an audio bus gets loud, and when it goes quiet.",
            NodeClass::Active,
            Category::Conditions,
        )
    }

    fn ports(_: &LevelParams) -> Ports {
        Ports::new()
            .event_out("rose", "The level stayed above the threshold for the time set.")
            .event_out("fell", "The level dropped below the threshold minus the hysteresis.")
            .live_out("value", PortType::Bool, "Whether it is loud now.")
            .live_out("failed_polls", PortType::Number, "Level reads that failed since the show started.")
    }

    fn needs(_: &LevelParams) -> Vec<Need> {
        vec![Need::op(METERS)]
    }

    fn check(params: &LevelParams) -> Result<(), ParamErrors> {
        let mut errors = Vec::new();
        let (Source::Bus { bus: name } | Source::Input { input: name }) = &params.source;
        if name.is_empty() {
            errors.push(ParamError::new("source", l10n::audio_source_missing()));
        }
        for (key, value) in [("above_db", params.above_db), ("hysteresis_db", params.hysteresis_db)] {
            if !value.is_finite() || (key == "hysteresis_db" && value < 0.0) {
                errors.push(ParamError::new(key, l10n::bad_level()));
            }
        }
        if !params.for_ms.is_finite() || params.for_ms < 0.0 {
            errors.push(ParamError::new("for_ms", l10n::bad_seconds()));
        }
        if !params.poll_ms.is_finite() || params.poll_ms < MIN_POLL_MS {
            errors.push(ParamError::new("poll_ms", l10n::poll_too_fast(MIN_POLL_MS)));
        }
        if errors.is_empty() { Ok(()) } else { Err(ParamErrors(errors)) }
    }

    fn new(params: LevelParams) -> Self {
        Self {
            args: params.source.args(),
            above_db: params.above_db,
            for_s: params.for_ms.max(0.0) / 1_000.0,
            hysteresis_db: params.hysteresis_db.max(0.0),
            poll_ms: params.poll_ms.max(MIN_POLL_MS),
            started: Seconds(0.0),
            polls: 0.0,
            wake: None,
            call: None,
            loud: false,
            above_since: None,
            failed: 0.0,
        }
    }
}

impl AudioLevel {
    /// Asks for the wake of the next poll, counted from the start in milliseconds so sums of periods never drift.
    fn next_poll(&mut self, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        self.polls += 1.0;
        self.wake = Some(cx.wake_at(Seconds(self.started.0 + self.polls * self.poll_ms / 1_000.0))?);
        Ok(())
    }

    /// The RMS level in a reply of `audio.meters`; silence (`null`) is −∞.
    fn level(output: Option<&Value>) -> Option<f64> {
        let reading = output?.get("meters")?.get(0)?;
        let db = reading.get("rms_db")?;
        if db.is_null() { Some(f64::NEG_INFINITY) } else { db.as_f64() }
    }

    fn on_level(&mut self, db: f64, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        let now = cx.now();
        if db >= self.above_db {
            if self.loud {
                return Ok(());
            }
            let since = *self.above_since.get_or_insert(now);
            if now.0 - since.0 + EPSILON >= self.for_s {
                self.loud = true;
                self.above_since = None;
                return cx.fire("rose");
            }
            return Ok(());
        }
        self.above_since = None;
        if self.loud && db < self.above_db - self.hysteresis_db {
            self.loud = false;
            return cx.fire("fell");
        }
        Ok(())
    }
}

impl Node for AudioLevel {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Start => {
                (self.loud, self.above_since, self.call, self.failed) = (false, None, None, 0.0);
                (self.started, self.polls) = (cx.now(), 0.0);
                self.next_poll(cx)
            }
            Input::Stop => {
                if let Some(id) = self.wake.take() {
                    cx.cancel_wake(id);
                }
                self.call = None;
                Ok(())
            }
            Input::Wake { id, .. } if Some(id) == self.wake => {
                self.next_poll(cx)?;
                if self.call.is_none() {
                    self.call = Some(cx.run_op(METERS, self.args.clone())?);
                }
                Ok(())
            }
            Input::Reply { call, outcome, .. } if Some(call) == self.call => {
                self.call = None;
                if let Some(db) = outcome.as_ref().ok().and_then(|output| Self::level(output.as_ref())) {
                    self.on_level(db, cx)
                } else {
                    // A failed poll keeps the last state; the next one retries.
                    self.failed += 1.0;
                    Ok(())
                }
            }
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }

    fn eval(&self, port: &str, _: &mut EvalCx<'_>) -> Result<Val, NodeError> {
        match port {
            "value" => Ok(Val::Bool(self.loud)),
            "failed_polls" => Ok(Val::Number(self.failed)),
            other => Err(NodeError::not_live(other)),
        }
    }
}
register_node!(AudioLevel);
