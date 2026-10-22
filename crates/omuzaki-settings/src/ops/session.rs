//! `session.recent`, `session.get` and `session.forget`: the session state (D9) as every surface reads it, and the one
//! change a person makes to it by hand (File > Open Recent > Remove). `session.forget` is undone by
//! `session.restore_recent`, which puts the entry back where it was.

use std::path::{Path, PathBuf};

use omuzaki_ops_api::{Actor, Effect, Op, OpCall, OpCtx, OpError, OpMeta, Plan, register_op};
use omuzaki_settings_api::session::{RecentProject, SessionState};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{edit, query, service};

/// The projects opened most recently, newest first, and the one open when the app last ran.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Recent {}

/// Everything the session remembers: recent projects, the last project, window places, the last release notes seen,
/// and a project that was being opened when the app stopped.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Get {}

/// Remove a project from the recent list (and from "last project" if it is that one). The folder is not touched. A
/// person's call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "path": "/shows/spring" }]))]
pub struct Forget {
    /// The project folder, as `session.recent` lists it.
    pub path: PathBuf,
}

/// Put a removed project back in the recent list at `index`: the undo of `session.forget`. A person's call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "path": "/shows/spring", "name": "Spring show", "opened": 1_793_000_000, "index": 0 }]))]
pub struct RestoreRecent {
    /// The project folder.
    pub path: PathBuf,
    /// Its display name.
    pub name: String,
    /// When it was opened, in Unix seconds.
    pub opened: i64,
    /// Its place in the list, 0 being the newest.
    pub index: usize,
    /// Whether it was also the last project.
    #[serde(default)]
    pub last: bool,
}

fn recent_json(entry: &RecentProject) -> Value {
    json!({ "path": entry.path, "name": entry.name, "opened": entry.opened })
}

fn person_only(ctx: &OpCtx<'_>, what: &str) -> Result<(), OpError> {
    if matches!(ctx.actor, Actor::Person { .. }) {
        Ok(())
    } else {
        Err(OpError::person_only(format!("only a person may {what}: the recent list is theirs")))
    }
}

fn state_json(state: &SessionState) -> Value {
    json!({
        "last_project": state.last_project,
        "recent": state.recent.iter().map(recent_json).collect::<Vec<_>>(),
        "windows": state.windows,
        "last_seen_version": state.last_seen_version,
        "opening": state.opening,
    })
}

impl Op for Recent {
    fn meta() -> OpMeta {
        query("session.recent", "List the projects opened most recently, newest first, and the last one open.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let state = service(ctx)?.session().state();
        Ok(Plan::answer(json!({
            "recent": state.recent.iter().map(recent_json).collect::<Vec<_>>(),
            "last_project": state.last_project,
        })))
    }
}

impl Op for Get {
    fn meta() -> OpMeta {
        query("session.get", "Read what the session remembers: recent projects, window places, the last version seen.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(state_json(&service(ctx)?.session().state())))
    }
}

fn restore_call(entry: &RecentProject, index: usize, last: bool) -> OpCall {
    OpCall {
        op: "session.restore_recent".to_owned(),
        args: json!({ "path": entry.path, "name": entry.name, "opened": entry.opened, "index": index, "last": last }),
    }
}

fn forget_call(path: &Path) -> OpCall {
    OpCall { op: "session.forget".to_owned(), args: json!({ "path": path }) }
}

impl Op for Forget {
    fn meta() -> OpMeta {
        edit("session.forget", "Remove a project from the recent list; the folder itself is left alone.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        person_only(ctx, "remove a recent project")?;
        let session = service(ctx)?.session();
        let state = session.state();
        let Some(index) = state.recent.iter().position(|r| r.path == self.path) else {
            return Ok(Plan::answer(json!({ "path": self.path, "forgotten": false })));
        };
        let entry = state.recent[index].clone();
        let last = state.last_project.as_deref() == Some(self.path.as_path());
        if !ctx.dry_run {
            session.forget(&self.path);
        }
        let effect = Effect {
            summary: format!("removed {} from the recent projects", entry.name),
            undo: restore_call(&entry, index, last),
            redo: forget_call(&self.path),
        };
        Ok(Plan {
            effect: Some(effect),
            ..Plan::answer(json!({ "path": self.path, "forgotten": true, "applied": !ctx.dry_run }))
        })
    }
}

impl Op for RestoreRecent {
    fn meta() -> OpMeta {
        edit("session.restore_recent", "Put a removed project back in the recent list where it was.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        person_only(ctx, "change the recent projects")?;
        let session = service(ctx)?.session();
        let entry = RecentProject::new(&self.path, &self.name, self.opened);
        if !ctx.dry_run {
            session.restore_recent(entry.clone(), self.index, self.last);
        }
        let effect = Effect {
            summary: format!("put {} back in the recent projects", self.name),
            undo: forget_call(&self.path),
            redo: restore_call(&entry, self.index, self.last),
        };
        Ok(Plan { effect: Some(effect), ..Plan::answer(json!({ "path": self.path, "applied": !ctx.dry_run })) })
    }
}

register_op!(Recent);
register_op!(Get);
register_op!(Forget);
register_op!(RestoreRecent);
