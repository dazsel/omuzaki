//! `settings.profile.*` (D7, D8): list, save, rename, delete and switch profiles over the `Settings` service.
//!
//! An agent's save, rename, delete or switch waits for a person (D4: profiles change what the app does on every
//! project). Each edit is undone by the inverse call it records: a switch by switching back, a rename by the old
//! name, a new profile by deleting it. Deleting a profile and saving over one are not undone (the file is gone or
//! replaced); their answer says `undoable: false`, and an agent's call already needed a person.

use omuzaki_ops_api::{Actor, Effect, Mode, Op, OpCall, OpCtx, OpError, OpMeta, Plan, register_op};
use omuzaki_settings_api::error::{SettingError, SettingErrorKind};
use omuzaki_settings_api::profile::{ProfileId, ProfileInfo, ProfileSource};
use omuzaki_settings_api::service::Settings;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{edit, query, service};

/// The profiles, with how many settings each overrides and which one is active.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct List {}

/// Save the settings as a profile: by default every setting a profile can hold whose value now differs from the base
/// (so the profile reproduces today's setup), or a copy of another profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "name": "Rehearsal" }, { "name": "Record", "from": { "profile": "stream-0001" } }]))]
pub struct Save {
    /// The display name, 1 to 48 characters.
    pub name: String,
    /// Where the values come from: `"current"` (the default) or `{"profile": "<id>"}` to copy one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<SaveFrom>,
    /// Replace a profile that already has this name.
    #[serde(default)]
    pub overwrite: bool,
}

/// Where a saved profile's values come from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SaveFrom {
    /// The values in effect now.
    Current,
    /// A copy of this profile.
    Profile(ProfileId),
}

/// Give a profile a new display name; its id and file stay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "profile": "stream-0001", "name": "Streaming" }]))]
pub struct Rename {
    /// The profile's id, as `settings.profile.list` gives it.
    pub profile: ProfileId,
    /// The new display name.
    pub name: String,
}

/// Delete a profile. Deleting the active one switches to no profile first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "profile": "stream-0001" }]))]
pub struct Delete {
    /// The profile's id.
    pub profile: ProfileId,
}

/// Switch to a profile, or to none with `null`: every setting it holds takes effect at once, in one step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "profile": "stream-0001" }, { "profile": null }]))]
pub struct Switch {
    /// The profile's id, or `null` for no profile.
    pub profile: Option<ProfileId>,
}

fn mode(ctx: &OpCtx<'_>) -> Mode {
    if ctx.dry_run { Mode::DryRun } else { Mode::Apply }
}

/// An agent's profile change waits for a person: a profile changes how the app behaves on every project.
fn may_change(ctx: &OpCtx<'_>, what: &str) -> Result<(), OpError> {
    if matches!(ctx.actor, Actor::Person { .. }) { Ok(()) } else { ctx.require_person(what.to_owned()) }
}

fn profile_error(e: &SettingError) -> OpError {
    match &e.kind {
        SettingErrorKind::UnknownProfile { .. } => {
            OpError::not_found(e.to_string()).hint("settings.profile.list gives every profile's id")
        }
        SettingErrorKind::ProfileExists { .. } => {
            OpError::conflict(e.to_string()).hint("pick another name, or pass \"overwrite\": true to replace it")
        }
        SettingErrorKind::BadProfileName { .. } => OpError::invalid("name", e.to_string()),
        _ => OpError::internal(e.to_string()),
    }
}

fn info_json(info: &ProfileInfo) -> Value {
    json!({ "id": info.id, "name": info.name, "keys": info.keys, "active": info.active })
}

fn active(settings: &dyn Settings) -> Option<ProfileId> {
    settings.profiles().into_iter().find(|p| p.active).map(|p| p.id)
}

fn switch_call(to: Option<&ProfileId>) -> OpCall {
    OpCall { op: "settings.profile.switch".to_owned(), args: json!({ "profile": to }) }
}

impl Op for List {
    fn meta() -> OpMeta {
        query("settings.profile.list", "List the settings profiles, how many settings each holds, and the active one.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let profiles: Vec<Value> = service(ctx)?.profiles().iter().map(info_json).collect();
        Ok(Plan::answer(json!({ "profiles": profiles })))
    }
}

impl Op for Save {
    fn meta() -> OpMeta {
        edit("settings.profile.save", "Save the settings in effect (or a copy of a profile) as a named profile.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        may_change(ctx, "save a settings profile")?;
        let settings = service(ctx)?;
        let replaced = settings.profiles().into_iter().any(|p| p.name == self.name.trim());
        let from = match &self.from {
            Some(SaveFrom::Profile(id)) => ProfileSource::Profile(id.clone()),
            _ => ProfileSource::Current,
        };
        let saved = settings
            .save_profile(&self.name, from, self.overwrite, mode(ctx), ctx.actor)
            .map_err(|e| profile_error(&e))?;
        let output = json!({ "profile": info_json(&saved), "replaced": replaced, "undoable": !replaced, "applied": !ctx.dry_run });
        // A new profile is undone by deleting it; one saved over cannot be put back.
        let effect = (!replaced).then(|| Effect {
            summary: format!("saved the settings profile {}", saved.name),
            undo: OpCall { op: "settings.profile.delete".to_owned(), args: json!({ "profile": saved.id }) },
            redo: OpCall {
                op: "settings.profile.save".to_owned(),
                args: serde_json::to_value(self).unwrap_or(Value::Null),
            },
        });
        Ok(Plan { effect, ..Plan::answer(output) })
    }
}

impl Op for Rename {
    fn meta() -> OpMeta {
        edit("settings.profile.rename", "Give a settings profile a new name; what it holds is unchanged.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        may_change(ctx, "rename a settings profile")?;
        let settings = service(ctx)?;
        let old = settings.profiles().into_iter().find(|p| p.id == self.profile).map(|p| p.name);
        let renamed =
            settings.rename_profile(&self.profile, &self.name, mode(ctx), ctx.actor).map_err(|e| profile_error(&e))?;
        let effect = old.filter(|old| *old != renamed.name).map(|old| Effect {
            summary: format!("renamed the settings profile {old} to {}", renamed.name),
            undo: OpCall {
                op: "settings.profile.rename".to_owned(),
                args: json!({ "profile": self.profile, "name": old }),
            },
            redo: OpCall {
                op: "settings.profile.rename".to_owned(),
                args: json!({ "profile": self.profile, "name": renamed.name }),
            },
        });
        Ok(Plan { effect, ..Plan::answer(json!({ "profile": info_json(&renamed), "applied": !ctx.dry_run })) })
    }
}

impl Op for Delete {
    fn meta() -> OpMeta {
        edit("settings.profile.delete", "Delete a settings profile; deleting the active one switches to none first.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        may_change(ctx, "delete a settings profile")?;
        let settings = service(ctx)?;
        let report = settings.delete_profile(&self.profile, mode(ctx), ctx.actor).map_err(|e| profile_error(&e))?;
        Ok(Plan::answer(json!({
            "profile": self.profile,
            "changed": report.changed,
            "pending_restart": report.pending_restart,
            "undoable": false,
            "applied": !ctx.dry_run,
        })))
    }
}

impl Op for Switch {
    fn meta() -> OpMeta {
        edit("settings.profile.switch", "Switch to a settings profile, or to none: its settings take effect at once.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        may_change(ctx, "switch the settings profile")?;
        let settings = service(ctx)?;
        let from = active(settings);
        let to = self.profile.as_ref();
        let report = settings.switch_profile(to, mode(ctx), ctx.actor).map_err(|e| profile_error(&e))?;
        let effect = (from.as_ref() != to).then(|| Effect {
            summary: match to {
                Some(id) => format!("switched to the settings profile {id}"),
                None => "switched to no settings profile".to_owned(),
            },
            undo: switch_call(from.as_ref()),
            redo: switch_call(to),
        });
        Ok(Plan {
            effect,
            ..Plan::answer(json!({
                "from": from,
                "to": to,
                "changed": report.changed,
                "pending_restart": report.pending_restart,
                "applied": !ctx.dry_run,
            }))
        })
    }
}

register_op!(List);
register_op!(Save);
register_op!(Rename);
register_op!(Delete);
register_op!(Switch);
