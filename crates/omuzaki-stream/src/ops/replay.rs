//! `record.replay_start`, `record.replay_stop`, `record.save_replay` and `record.replay_status`: the replay buffer as ops
//! (`docs/design/record.md` D5, D10).
//!
//! Starting, stopping and saving are **live actions** (`Edit`, `Undo::None`, scope `perform`): the buffer is a
//! continuous capture of the person, and Ctrl+Z must never delete a clip. A hotkey, the Record panel's switch or an
//! automation's `op.run` node calls them. No op takes a path: clips go to the `replay.dir` folder. The answers are
//! paths and numbers, never pixels or audio.

use omuzaki_core::time::Seconds;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_settings_api::service::{Settings, SettingsExt as _};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::record::{mic_track, op_error};
use crate::record::settings as record_settings;
use crate::replay::ring::{MAX_SPAN, MIN_SPAN};
use crate::replay::runtime::{ReplayError, ReplayPlan};
use crate::replay::settings::{self, ReplaySettings};
use crate::service::StreamService;

/// The folder under the clips folder a disk-backed buffer keeps its segments in.
const CACHE_SUBDIR: &str = ".cache";

fn replay_error(e: ReplayError) -> OpError {
    match e {
        ReplayError::Disabled => {
            OpError::conflict("the replay buffer is off").hint("start it with record.replay_start")
        }
        ReplayError::Empty => {
            OpError::conflict("the replay buffer holds nothing yet").hint("it fills while the program runs")
        }
        ReplayError::Busy => OpError::conflict("a save is already running").hint("try again when it completes"),
        ReplayError::Stream(e) => op_error(e),
    }
}

fn to_json<T: Serialize>(value: &T) -> Result<serde_json::Value, OpError> {
    serde_json::to_value(value).map_err(|e| OpError::internal(format!("serialising the answer failed: {e}")))
}

fn check_seconds(seconds: Option<f64>) -> Result<Option<Seconds>, OpError> {
    match seconds {
        Some(s) if !(MIN_SPAN.0..=MAX_SPAN.0).contains(&s) => {
            Err(OpError::invalid("seconds", "a replay is 5 to 300 seconds long"))
        }
        other => Ok(other.map(Seconds)),
    }
}

/// Start keeping the last seconds of the program ready to save.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, { "seconds": 30 }]))]
pub struct ReplayStart {
    /// How many seconds a save holds, 5 to 300 (default: the `replay.length_s` setting).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f64>,
}

/// Stop the replay buffer and forget what it held.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct ReplayStop {}

/// Save the last seconds of the program as a clip.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, { "seconds": 20 }]))]
pub struct SaveReplay {
    /// How many seconds to save, 5 to 300 (default: the buffer's length). The clip starts on the keyframe at or before
    /// that point, so it may be up to a second longer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f64>,
}

/// Report whether the replay buffer runs, what it holds and how the last save went.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct ReplayStatusOp {}

const fn live_action(name: &'static str, summary: &'static str) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Edit, scope: Scope::Perform, undo: Undo::None }
}

impl Op for ReplayStart {
    fn meta() -> OpMeta {
        live_action(
            "record.replay_start",
            "Start keeping the last seconds of the program, with transparency and the microphone, ready to save as a clip.",
        )
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = ctx.service::<StreamService>()?;
        let asked = check_seconds(self.seconds)?;
        let settings = ctx.service::<dyn Settings>().ok();
        let length = asked.unwrap_or(Seconds(
            settings.and_then(|s| s.get::<settings::LengthS>().ok()).unwrap_or(settings::DEFAULT_LENGTH.0),
        ));
        let max_mb = settings.and_then(|s| s.get::<settings::MaxMb>().ok()).unwrap_or(settings::DEFAULT_MB);
        let backing = settings.and_then(|s| s.get::<settings::BackingSetting>().ok()).unwrap_or_default();
        let replay_dir = settings.and_then(|s| s.get::<settings::Dir>().ok()).unwrap_or_default();
        let record_dir = settings.and_then(|s| s.get::<record_settings::Dir>().ok()).unwrap_or_default();
        let dir = settings::resolve_dir(&replay_dir, &record_dir);
        let limits = ReplaySettings::new(length, max_mb << 20, backing);
        if ctx.dry_run {
            let available = crate::vp9::available();
            return Ok(Plan::answer(json!({
                "started": false,
                "running": service.replay_status().running,
                "length_s": length.0,
                "cap_bytes": limits.max_bytes,
                "backing": backing,
                "dir": dir,
                "available": available.is_ok(),
                "hint": available.err().map(|e| e.to_string()),
            })));
        }
        crate::vp9::available().map_err(op_error)?;
        let tracks = mic_track(ctx)?.into_iter().collect();
        let plan = ReplayPlan::vp9(limits, dir.clone(), dir.join(CACHE_SUBDIR), tracks);
        let status = service.replay_start(plan).map_err(|e| match e {
            omuzaki_stream_api::error::StreamError::Busy => OpError::conflict("the replay buffer already runs"),
            other => op_error(other),
        })?;
        let mut answer = to_json(&status)?;
        answer["started"] = json!(true);
        answer["dir"] = json!(dir);
        Ok(Plan::answer(answer))
    }
}

impl Op for ReplayStop {
    fn meta() -> OpMeta {
        live_action("record.replay_stop", "Stop the replay buffer; saves already asked for complete first.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = ctx.service::<StreamService>()?;
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "stopped": false, "running": service.replay_status().running })));
        }
        let status = service.replay_stop().map_err(|_| replay_error(ReplayError::Disabled))?;
        let mut answer = to_json(&status)?;
        answer["stopped"] = json!(true);
        Ok(Plan::answer(answer))
    }
}

impl Op for SaveReplay {
    fn meta() -> OpMeta {
        live_action(
            "record.save_replay",
            "Save the last seconds of the program as a clip with transparency, and its audio, to the replays folder.",
        )
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = ctx.service::<StreamService>()?;
        let seconds = check_seconds(self.seconds)?;
        if ctx.dry_run {
            let status = service.replay_status();
            return Ok(Plan::answer(json!({
                "saved": false,
                "running": status.running,
                "buffered_s": status.buffered_s,
                "requested_s": seconds.map_or(status.length_s, |s| s.0),
            })));
        }
        let report = service.replay_save(seconds).map_err(replay_error)?;
        Ok(Plan::answer(to_json(&report)?))
    }
}

impl Op for ReplayStatusOp {
    fn meta() -> OpMeta {
        OpMeta {
            name: "record.replay_status",
            summary: "Say whether the replay buffer runs, how many seconds it holds, and how the last save went.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(to_json(&ctx.service::<StreamService>()?.replay_status())?))
    }
}

register_op!(ReplayStart);
register_op!(ReplayStop);
register_op!(SaveReplay);
register_op!(ReplayStatusOp);
