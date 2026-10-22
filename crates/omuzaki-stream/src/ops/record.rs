//! `record.start`, `record.stop` and `record.status`: local recording as ops (`docs/design/record.md` D10).
//!
//! Starting and stopping are **live actions**: kind `Edit` with `Undo::None` and scope `perform`, because recording
//! captures the person as performing does, and Ctrl+Z must never delete a file. An agent records only with a
//! `perform` grant. No op takes a path: takes go to the `record.dir` folder, and `name` is one sanitised file-name
//! component. The answers are paths and numbers, never pixels or audio. A dry run reports what would start.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_settings_api::service::{Settings, SettingsExt as _};
use omuzaki_stream_api::error::StreamError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use omuzaki_audio_api::engine::AudioEngine;
use omuzaki_audio_api::graph::MIC_BUS;
use omuzaki_audio_api::ids::BusId;
use omuzaki_audio_api::node::SAMPLE_RATE;
use omuzaki_audio_api::tap::{DropPolicy, TapFormat, TapPoint, TapRate, TapRequest};
use omuzaki_core::time::Seconds;

use crate::record::runtime::{AudioTrack, RecordPlan, TrackStart};
use crate::record::settings::{self, RecordFormat};
use crate::record::take::{check_name, stem, unique_stem};
use crate::service::StreamService;

/// Maps a stream error to the op error a person or an agent can act on.
pub(crate) fn op_error(e: StreamError) -> OpError {
    match e {
        StreamError::Busy => OpError::conflict("a take is already recording").hint("stop it with record.stop first"),
        StreamError::Stopped => OpError::conflict("nothing is recording"),
        StreamError::Unavailable { what, hint } => OpError::unsupported(format!("{what} is unavailable")).hint(hint),
        StreamError::Config(m) => OpError::invalid("args", m),
        other => OpError::internal(other.to_string()),
    }
}

/// Start recording the program picture, with its alpha, and the audio tracks.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, { "name": "intro" }, { "format": "png_sequence", "name": "test take" }]))]
pub struct Start {
    /// `auto` (default: the `record.format` setting), `png_sequence` or `webm_vp9_alpha`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<RecordFormat>,
    /// A name for the take, added to its date and time: letters, digits, spaces, `-`, `_`, `.`; up to 64 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Stop the take and report its files and numbers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Stop {}

/// Report whether a take is recording, and how it is going.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Status {}

/// The format a request asks for, with `auto` resolved to what this machine writes best: `WebM` VP9 with alpha when
/// libvpx loads, else the PNG sequence.
fn resolve_format(asked: RecordFormat) -> RecordFormat {
    match asked {
        RecordFormat::Auto if crate::vp9::available().is_ok() => RecordFormat::WebmVp9Alpha,
        RecordFormat::Auto => RecordFormat::PngSequence,
        other => other,
    }
}

/// How much microphone audio the take's tap holds while the first frame is on its way and the writer catches up.
const MIC_TAP_SECONDS: f64 = 4.0;

/// The running audio engine and its mic bus, when both exist.
fn mic_bus<'a>(ctx: &OpCtx<'a>) -> Option<(&'a dyn AudioEngine, BusId)> {
    let engine = ctx.service::<dyn AudioEngine>().ok()?;
    let bus = BusId::new(MIC_BUS).ok()?;
    engine.graph().bus(&bus).is_some().then_some((engine, bus))
}

/// The microphone track: the mic bus after its fader (so mute is silence, marked as such), at 48 kHz mono, lined up
/// with the first video frame on the engine clock. `None` when no audio engine runs or its graph has no mic bus.
///
/// The engine clock and the frames' capture times share the pipeline clock's zero (`StreamService::with_epoch`).
fn mic_track(ctx: &OpCtx<'_>) -> Result<Option<AudioTrack>, OpError> {
    let Some((engine, bus)) = mic_bus(ctx) else { return Ok(None) };
    let request = TapRequest {
        point: TapPoint::BusPostFader(bus),
        format: TapFormat { rate: TapRate::Graph48k, mono: true },
        capacity: Seconds(MIC_TAP_SECONDS),
        policy: DropPolicy::DropNewest,
        consumer: "record".into(),
        leaves_device: false,
    };
    let (reader, info) =
        engine.tap_ring(&request).map_err(|e| OpError::internal(format!("the mic tap failed: {e}")))?;
    let clock = engine.clock();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Milliseconds of declared latency.
    let offset = (info.capture_offset.0.max(0.0) * f64::from(SAMPLE_RATE)).round() as u64;
    #[allow(clippy::cast_precision_loss)] // Nanoseconds since the epoch: exact enough in f64 for days.
    let align =
        move |capture: omuzaki_stream_api::time::Ticks| clock.sample_at(Seconds(capture.0 as f64 / 1e9)) + offset;
    Ok(Some(AudioTrack {
        name: "mic".into(),
        source: Box::new(reader),
        rate: info.rate,
        channels: 1,
        start: TrackStart::FirstFrame(Box::new(align)),
    }))
}

fn to_json<T: Serialize>(value: &T) -> Result<serde_json::Value, OpError> {
    serde_json::to_value(value).map_err(|e| OpError::internal(format!("serialising the answer failed: {e}")))
}

impl Op for Start {
    fn meta() -> OpMeta {
        OpMeta {
            name: "record.start",
            summary: "Start recording the program picture with transparency, and its audio tracks, to the recordings folder.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = ctx.service::<StreamService>()?;
        let name =
            self.name.as_deref().map(check_name).transpose().map_err(|e| OpError::invalid("name", e.to_string()))?;
        let settings = ctx.service::<dyn Settings>().ok();
        let asked = match self.format {
            Some(format) => format,
            None => settings.and_then(|s| s.get::<settings::Format>().ok()).unwrap_or_default(),
        };
        let format = resolve_format(asked);
        let dir_setting = settings.and_then(|s| s.get::<settings::Dir>().ok()).unwrap_or_default();
        let dir = settings::resolve_dir(&dir_setting);
        let stem = unique_stem(&dir, &stem(std::time::SystemTime::now(), name.as_deref()));
        if ctx.dry_run {
            let program = service.program();
            // A dry run reports whether the format can be written here, rather than failing on this machine.
            let available = match format {
                RecordFormat::WebmVp9Alpha => crate::vp9::available(),
                _ => Ok(()),
            };
            return Ok(Plan::answer(json!({
                "started": false,
                "stem": stem,
                "format": format,
                "available": available.is_ok(),
                "hint": available.err().map(|e| e.to_string()),
                "dir": dir,
                "tracks": if mic_bus(ctx).is_some() { vec!["mic"] } else { Vec::new() },
                "size": [program.size.x, program.size.y],
                "fps": [program.fps.0, program.fps.1],
            })));
        }
        let tracks = mic_track(ctx)?.into_iter().collect();
        let started = service.record_start(RecordPlan { dir, stem, format, tracks }).map_err(op_error)?;
        let mut answer = to_json(&started)?;
        answer["started"] = json!(true);
        Ok(Plan::answer(answer))
    }
}

impl Op for Stop {
    fn meta() -> OpMeta {
        OpMeta {
            name: "record.stop",
            summary: "Stop recording and report the take's files, length and dropped frames.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = ctx.service::<StreamService>()?;
        if ctx.dry_run {
            let status = service.record_status();
            return Ok(Plan::answer(json!({ "stopped": false, "recording": status.recording, "stem": status.stem })));
        }
        let report = service.record_stop().map_err(op_error)?;
        Ok(Plan::answer(to_json(&report)?))
    }
}

impl Op for Status {
    fn meta() -> OpMeta {
        OpMeta {
            name: "record.status",
            summary: "Say whether a take is recording, for how long, and whether it keeps up.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(to_json(&ctx.service::<StreamService>()?.record_status())?))
    }
}

register_op!(Start);
register_op!(Stop);
register_op!(Status);
