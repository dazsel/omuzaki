//! `audio.devices`, `audio.mic` and `audio.status`: the microphone as ops (spec v3 BR-22).
//!
//! `audio.mic` is the one way to turn the microphone on, choose it, and mute or unmute it: it binds the `mic` role
//! (the system default unless a device is named), adds the mic bus to the graph if the graph has none, and sets the
//! mic mode. It is a **live action** (kind `Edit`, `Undo::None`, scope `perform`) like the output and record ops: it
//! changes what the person sends, not the document, and Ctrl+Z must never unmute anyone. The ops work on any
//! [`AudioEngine`] in the services: the live engine in the app, the mock in tests.

use omuzaki_audio_api::device::{Binding, ChannelMap, DeviceId, Direction};
use omuzaki_audio_api::engine::{AudioEngine, AudioError};
use omuzaki_audio_api::graph::{BusSpec, Channels, GraphSpec, MIC_BUS, MicMode, SourceRef};
use omuzaki_audio_api::ids::{BusId, DeviceRole};
use omuzaki_audio_api::report::MeterPoint;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The role the microphone is bound to.
pub const MIC_ROLE: &str = "mic";

/// List the audio devices on this machine, inputs and outputs, with the default of each marked.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Devices {}

/// Turn the microphone on, choose which one, or mute and unmute it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, { "muted": true }, { "muted": false }]))]
pub struct Mic {
    /// The input device, as `audio.devices` lists it. Default: keep the bound one, or follow the system default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<DeviceId>,
    /// `true` mutes the microphone everywhere (stream, recording, lip sync); `false` unmutes it. Default: unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muted: Option<bool>,
}

/// Report the audio engine: device states, the clock, the mic mode and the microphone's level.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Status {}

fn engine<'a>(ctx: &OpCtx<'a>) -> Result<&'a dyn AudioEngine, OpError> {
    ctx.service::<dyn AudioEngine>()
}

fn audio_error(e: AudioError) -> OpError {
    match e {
        AudioError::Backend(e) => OpError::unsupported(format!("the audio device failed: {e}"))
            .hint("list the devices with audio.devices and pick one that is present"),
        AudioError::Graph(e) => OpError::invalid("graph", e.to_string()),
        other => OpError::internal(other.to_string()),
    }
}

fn ids() -> Result<(DeviceRole, BusId), OpError> {
    let role = DeviceRole::new(MIC_ROLE).map_err(|e| OpError::internal(e.to_string()))?;
    let bus = BusId::new(MIC_BUS).map_err(|e| OpError::internal(e.to_string()))?;
    Ok((role, bus))
}

/// `graph` with a mono mic bus fed by the `mic` role added, if it has no mic bus yet.
fn with_mic_bus(graph: &GraphSpec, role: &DeviceRole, bus: &BusId) -> Option<GraphSpec> {
    if graph.bus(bus).is_some() {
        return None;
    }
    let mut next = graph.clone();
    next.buses.push(BusSpec {
        id: bus.clone(),
        channels: Channels::Mono,
        sources: vec![SourceRef::Device { role: role.clone() }],
        chain: vec![],
        gain_db: 0.0,
        muted: false,
        sends: vec![],
        outputs: vec![],
    });
    Some(next)
}

impl Op for Devices {
    fn meta() -> OpMeta {
        OpMeta {
            name: "audio.devices",
            summary: "List the microphones and other audio devices on this machine, with the defaults marked.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let devices = engine(ctx)?.devices().map_err(audio_error)?;
        let list: Vec<_> = devices
            .iter()
            .map(|d| {
                json!({
                    "id": d.id,
                    "label": d.label,
                    "direction": d.direction,
                    "default": d.is_default,
                    "channels": d.channels,
                })
            })
            .collect();
        Ok(Plan::answer(json!({ "devices": list })))
    }
}

impl Op for Mic {
    fn meta() -> OpMeta {
        OpMeta {
            name: "audio.mic",
            summary: "Turn the microphone on (the default or a chosen device), or mute and unmute it.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let engine = engine(ctx)?;
        let (role, bus) = ids()?;
        if let Some(id) = &self.device {
            let devices = engine.devices().map_err(audio_error)?;
            if !devices.iter().any(|d| &d.id == id && d.direction == Direction::Input) {
                return Err(OpError::not_found(format!("no input device {}:{}", id.host, id.id))
                    .hint("list the devices with audio.devices"));
            }
        }
        let bound = engine.status().devices.iter().any(|(r, _)| r == &role);
        let rebind = self.device.is_some() || !bound;
        let mode = self.muted.map(|m| if m { MicMode::Muted } else { MicMode::Live });
        if ctx.dry_run {
            return Ok(Plan::answer(json!({
                "applied": false,
                "binds": rebind,
                "adds_mic_bus": engine.graph().bus(&bus).is_none(),
                "mode": mode.unwrap_or_else(|| engine.mic_mode()),
            })));
        }
        if rebind {
            let binding = match &self.device {
                Some(id) => {
                    let label = engine
                        .devices()
                        .ok()
                        .and_then(|ds| ds.into_iter().find(|d| &d.id == id).map(|d| d.label))
                        .unwrap_or_default();
                    Binding::Fixed { id: id.clone(), label, channels: ChannelMap::mono(0), speakers: false }
                }
                None => Binding::SystemDefault { channels: ChannelMap::mono(0), speakers: false },
            };
            engine.bind(&role, binding).map_err(audio_error)?;
        }
        if let Some(graph) = with_mic_bus(&engine.graph(), &role, &bus) {
            engine.apply(&graph).map_err(audio_error)?;
        }
        let mode = match mode {
            Some(m) => engine.set_mic_mode(m).map_err(audio_error)?,
            None => engine.mic_mode(),
        };
        let state = engine.status().devices.into_iter().find(|(r, _)| r == &role).map(|(_, s)| s);
        Ok(Plan::answer(json!({ "applied": true, "state": state, "mode": mode })))
    }
}

impl Op for Status {
    fn meta() -> OpMeta {
        OpMeta {
            name: "audio.status",
            summary: "Report the audio devices' states, the mic mode and the microphone's level.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let engine = engine(ctx)?;
        let (role, bus) = ids()?;
        let status = engine.status();
        let level = engine.meters(&MeterPoint::Bus(bus)).or_else(|| engine.meters(&MeterPoint::Input(role)));
        Ok(Plan::answer(json!({
            "clock": status.clock_source,
            "devices": status.devices.iter().map(|(r, s)| json!({ "role": r, "state": s })).collect::<Vec<_>>(),
            "mode": status.mic_mode,
            "mic_level": level.map(|m| json!({ "peak_db": finite(m.peak_db), "rms_db": finite(m.rms_db) })),
        })))
    }
}

/// A level for JSON: silence (−∞ dBFS) as `null`.
fn finite(db: f32) -> Option<f32> {
    db.is_finite().then_some(db)
}

register_op!(Devices);
register_op!(Mic);
register_op!(Status);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mic_bus_is_added_once() {
        let (role, bus) = ids().unwrap_or_else(|e| panic!("{e:?}"));
        let first = with_mic_bus(&GraphSpec::default(), &role, &bus);
        let Some(graph) = first else { panic!("the empty graph gets a mic bus") };
        assert_eq!(graph.buses.len(), 1);
        assert!(with_mic_bus(&graph, &role, &bus).is_none());
    }
}
