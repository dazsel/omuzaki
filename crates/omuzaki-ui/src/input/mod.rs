//! The OS input surface of the desktop studio (ADR 0060, `docs/design/input-motion.md` §4 and §5): the capture
//! indicator in the status bar ([`indicator`], task UI-1) and the Input panel ([`panel`], task UI-2 and #752's capture
//! section): capture with what needs a permission, the bindings with their keys, and the protected programs.
//!
//! The hub's handles ([`InputSurface`]) are handed to the shell as a view and never put in the runner's services, so
//! no op, MCP tool or agent can install an indicator, learn or read a chord, or read the protected list. Everything
//! else goes through the `input.*` ops as the person.

pub mod indicator;
pub mod panel;
pub mod registration;
pub mod text;

use std::sync::Arc;

use omuzaki_input_api::capture::IndicatorHandle;
use omuzaki_input_api::feed::{ChordStore, InputHub, ProtectList};
use omuzaki_ops_api::{Actor, Grants, Mode, Request, Runner};
use serde_json::Value;

/// What the shell holds of the OS input hub: its installed indicator, its chord store and its protected list.
#[derive(Clone)]
pub struct InputSurface {
    indicator: IndicatorHandle,
    chords: Arc<dyn ChordStore>,
    protect: Arc<dyn ProtectList>,
}

impl InputSurface {
    /// Installs the shell's indicator on `hub` and takes its chord store and protected list.
    pub fn install(hub: &dyn InputHub) -> Self {
        Self { indicator: hub.indicator_host().install(), chords: hub.chord_store(), protect: hub.protect_list() }
    }

    /// The installed indicator.
    pub fn indicator(&self) -> &IndicatorHandle {
        &self.indicator
    }

    /// The chord store: the binding editor's alone.
    pub fn chords(&self) -> &Arc<dyn ChordStore> {
        &self.chords
    }

    /// The protected programs: the person's own list.
    pub fn protect(&self) -> &Arc<dyn ProtectList> {
        &self.protect
    }
}

impl std::fmt::Debug for InputSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputSurface").field("indicator", &self.indicator).finish_non_exhaustive()
    }
}

/// Runs `op` with `args` as `actor`, with the person's grants; its output, or the refusal's words.
fn run(runner: &dyn Runner, actor: &Actor, op: &str, args: Value) -> Result<Value, String> {
    let request = Request {
        op: op.to_owned(),
        args,
        mode: Mode::Apply,
        actor: actor.clone(),
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    runner.run(request).map(|outcome| outcome.plan.output.unwrap_or(Value::Null)).map_err(|error| error.to_string())
}
