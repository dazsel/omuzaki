//! The `voice_commands.*` ops.
//!
//! | Op | Kind | Scope | Who |
//! |---|---|---|---|
//! | `voice_commands.enable` | edit | `edit:settings` | a person only (an agent can't open a microphone, ADR 0018) |
//! | `voice_commands.disable` | edit | `edit:settings` | anyone |
//! | `voice_commands.status` | query | `read` | anyone |
//! | `voice_commands.set_phrases` | edit | `edit:settings` | a person, or an agent the person confirms |
//!
//! They act on the app's [`VoiceCommands`] service and keep the person's choice in their settings (`dyn Settings`,
//! when the app has one): `voice_commands.enabled` and `voice_commands.phrases`. None of them touches the document,
//! so none is undone; the opposite op is the undo.

use omuzaki_ops_api::{Actor, Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_settings_api::service::{Layer, Settings};
use omuzaki_settings_api::setting::Setting;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::matcher::PhraseList;
use super::{Enabled, Phrases, VoiceCommands};

/// Turn voice commands on: your microphone's speech is matched against your phrase list and each command becomes a
/// `voice.command` event for the show's automations. Only you can do this, in the Omuzaki window.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
#[non_exhaustive]
pub struct VoiceCommandsEnable {}

/// Turn voice commands off.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
#[non_exhaustive]
pub struct VoiceCommandsDisable {}

/// Show whether voice commands are on, the last command heard, and the command ids the phrase list has.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
#[non_exhaustive]
pub struct VoiceCommandsStatus {}

/// Replace the phrase list: what you say for each command id, per language. Kept in your profile, never in a show.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "list": { "command": [
    { "id": "wave", "say": ["wave", "say hi"], "lang": "en" },
    { "id": "wave", "say": ["手を振って"], "lang": "ja" },
] } }]))]
#[non_exhaustive]
pub struct VoiceCommandsSetPhrases {
    /// The new list.
    pub list: PhraseList,
}

impl VoiceCommandsSetPhrases {
    /// Set `list`.
    pub fn new(list: PhraseList) -> Self {
        Self { list }
    }
}

register_op!(VoiceCommandsEnable);
register_op!(VoiceCommandsDisable);
register_op!(VoiceCommandsStatus);
register_op!(VoiceCommandsSetPhrases);

fn mode(ctx: &OpCtx<'_>) -> Mode {
    if ctx.dry_run { Mode::DryRun } else { Mode::Apply }
}

/// Keeps `value` under `S` in the person's settings, when the app has a settings service.
fn remember<S: Setting>(ctx: &OpCtx<'_>, value: &S::Value) -> Result<(), OpError> {
    let Ok(settings) = ctx.service::<dyn Settings>() else { return Ok(()) };
    let value = serde_json::to_value(value).map_err(|e| OpError::internal(e.to_string()))?;
    settings
        .set_raw(S::META.key, value, Layer::Auto, mode(ctx), ctx.actor)
        .map(|_| ())
        .map_err(|e| OpError::invalid("list", e.to_string()))
}

fn switch(ctx: &OpCtx<'_>, on: bool) -> Result<Plan, OpError> {
    let commands = ctx.service::<VoiceCommands>()?;
    let was = commands.enabled();
    remember::<Enabled>(ctx, &on)?;
    if !ctx.dry_run {
        commands.set_enabled(on);
    }
    Ok(Plan::answer(json!({ "enabled": on, "was": was })))
}

impl Op for VoiceCommandsEnable {
    fn meta() -> OpMeta {
        OpMeta {
            name: "voice_commands.enable",
            summary: "Turn voice commands on (a person only).",
            kind: OpKind::Edit,
            scope: Scope::EditSettings,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if !matches!(ctx.actor, Actor::Person { .. }) {
            return Err(OpError::person_only("voice commands listen to the person's microphone"));
        }
        switch(ctx, true)
    }
}

impl Op for VoiceCommandsDisable {
    fn meta() -> OpMeta {
        OpMeta {
            name: "voice_commands.disable",
            summary: "Turn voice commands off.",
            kind: OpKind::Edit,
            scope: Scope::EditSettings,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        switch(ctx, false)
    }
}

impl Op for VoiceCommandsStatus {
    fn meta() -> OpMeta {
        OpMeta {
            name: "voice_commands.status",
            summary: "Show whether voice commands are on and the last command heard.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let commands = ctx.service::<VoiceCommands>()?;
        let phrases = commands.phrases();
        let mut ids: Vec<&str> = phrases.command.iter().map(|c| c.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        let mut langs: Vec<&str> = phrases.command.iter().map(|c| c.lang.as_str()).collect();
        langs.sort_unstable();
        langs.dedup();
        Ok(Plan::answer(json!({ "status": commands.status(), "commands": ids, "languages": langs })))
    }
}

impl Op for VoiceCommandsSetPhrases {
    fn meta() -> OpMeta {
        OpMeta {
            name: "voice_commands.set_phrases",
            summary: "Replace what you say for each voice command.",
            kind: OpKind::Edit,
            scope: Scope::EditSettings,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        self.list.check().map_err(|e| OpError::invalid("list", e.to_string()))?;
        ctx.require_person("changing what your voice commands listen for")?;
        let commands = ctx.service::<VoiceCommands>()?;
        remember::<Phrases>(ctx, &self.list)?;
        if !ctx.dry_run {
            commands.set_phrases(self.list.clone());
        }
        Ok(Plan::answer(json!({ "commands": self.list.command.len() })))
    }
}
