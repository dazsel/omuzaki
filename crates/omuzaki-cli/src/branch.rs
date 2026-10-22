//! `omuzaki branch …` (`docs/design/branches.md` 5 and 7): the project's branches from the shell, over the app core's
//! [`Branches`] and the `branch.*` ops. Every subcommand prints a line for people and the whole result with `--json`.

use clap::Subcommand;
use omuzaki_app::AppCore;
use omuzaki_history_api::branch::{BranchError, BranchId, BranchInfo, BranchName, Branches, NewBranch, PointId};
use omuzaki_history_api::state::StateRef;
use omuzaki_ops_api::{Cause, CauseKind, ErrorCode, Mode, OpError, Request, TxId};
use serde_json::{Value, json};

use crate::exit;
use crate::output::Reply;
use crate::session::Session;

/// A `omuzaki branch` subcommand.
#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum BranchCommand {
    /// The branches, the folder's first, with what each changed since it left the folder branch (`branch.list`).
    List {
        /// Include the automatic branches restores keep, and the bin.
        #[arg(long)]
        all: bool,
    },
    /// How a branch stands against the folder branch: what changed on each side (`branch.status`).
    Status {
        /// The branch; the folder's when left out.
        name: Option<String>,
    },
    /// Make a branch from the folder branch now, or from `--from`: another branch, a transaction number, or a state
    /// as JSON (`{"point": "…"}`).
    Create {
        /// The new branch's name, e.g. `aria/oval-face`.
        name: String,
        /// Where it starts.
        #[arg(long, value_name = "STATE")]
        from: Option<String>,
        /// A title shown beside the name.
        #[arg(long)]
        title: Option<String>,
        /// The paths it is about (the compare and merge views start there).
        #[arg(long, value_name = "PATH")]
        focus: Vec<String>,
    },
    /// Put a branch in the project folder: the files become that branch's, and the next open is on it.
    Switch {
        /// The branch.
        name: String,
    },
    /// Record a branch's state now as a point, optionally marked ready for review.
    Checkpoint {
        /// The branch; the folder's when left out.
        #[arg(long)]
        branch: Option<String>,
        /// What the point is.
        #[arg(long, default_value = "Checkpoint")]
        label: String,
        /// Mark it ready for review.
        #[arg(long)]
        ready: bool,
        /// A note for the reviewer.
        #[arg(long, requires = "ready")]
        note: Option<String>,
    },
    /// Rename a branch; its history doesn't change.
    Rename {
        /// The branch.
        name: String,
        /// Its new name.
        new: String,
    },
    /// Put a branch in the bin for 30 days.
    Delete {
        /// The branch.
        name: String,
    },
    /// Bring a branch back from the bin.
    Undelete {
        /// The branch.
        name: String,
    },
    /// Merge a branch into the folder branch, unit by unit, as one undoable step (`branch.merge`). Unresolved
    /// conflicts exit with code 13 and list the units.
    Merge {
        /// The branch to merge from.
        from: String,
        /// The point to merge; the branch's head when left out.
        #[arg(long, value_name = "POINT")]
        at: Option<String>,
        /// Show the merge, changing nothing.
        #[arg(long)]
        dry_run: bool,
        /// A choice for a unit: `<unit>=mine`, `<unit>=theirs` or `<unit>=both`.
        #[arg(long, value_name = "UNIT=SIDE")]
        resolve: Vec<String>,
        /// A unit to leave out (the merge becomes a pick).
        #[arg(long, value_name = "UNIT")]
        exclude: Vec<String>,
    },
    /// What differs between two branches, grouped by unit (`history.diff`).
    Diff {
        /// The earlier branch.
        a: String,
        /// The later branch.
        b: String,
    },
}

fn branch_error(error: BranchError) -> OpError {
    let message = error.to_string();
    let hint = error.hint();
    let op = match error {
        BranchError::NotFound(_) | BranchError::PointNotFound(_) => OpError::not_found(message),
        BranchError::Denied(_) => OpError::denied(message),
        BranchError::InvalidName(_) => OpError::invalid("name", message),
        BranchError::Invalid(_) => OpError::invalid("branch", message),
        BranchError::Io(_) => OpError::internal(message),
        _ => OpError::conflict(message),
    };
    match hint {
        Some(hint) => op.hint(hint),
        None => op,
    }
}

fn name(text: &str) -> Result<BranchName, OpError> {
    BranchName::new(text).map_err(|e| branch_error(BranchError::from(e)))
}

fn by_name(core: &AppCore, text: &str) -> Result<BranchInfo, OpError> {
    core.branches().by_name(&name(text)?).map_err(branch_error)
}

fn folder(core: &AppCore) -> Result<BranchInfo, OpError> {
    let id = core.branches().folder();
    let listed = core.branches().list().into_iter().find(|b| b.id == id);
    Ok(listed.unwrap_or_else(|| {
        BranchInfo::new(
            id,
            BranchName::main(),
            PointId(omuzaki_store_api::ContentHash::of(b"")),
            0,
            Default::default(),
            core.person().clone(),
        )
    }))
}

/// `--from`: a branch's name, a transaction number, or a state reference as JSON.
fn start(core: &AppCore, from: Option<&str>) -> Result<(BranchId, StateRef), OpError> {
    let here = core.branches().folder();
    let Some(text) = from.map(str::trim) else { return Ok((here, StateRef::Now)) };
    if let Ok(tx) = text.parse::<u64>() {
        return Ok((here, StateRef::tx(TxId(tx))));
    }
    if text.starts_with('{') || text == "\"now\"" || text == "now" {
        let state: StateRef = serde_json::from_str(if text == "now" { "\"now\"" } else { text })
            .map_err(|e| OpError::invalid("from", format!("not a state: {e}")))?;
        return Ok((here, state));
    }
    Ok((by_name(core, text)?.id, StateRef::Now))
}

fn request(session: &Session, op: &str, args: Value, mode: Mode) -> Request {
    Request {
        op: op.to_owned(),
        args,
        mode,
        actor: session.actor.clone(),
        grants: session.grants.clone(),
        cause: Some(Cause { kind: CauseKind::Direct, text: None }),
        tx: None,
        base: None,
    }
}

fn answer(core: &AppCore, session: &Session, op: &str, args: Value, mode: Mode) -> Result<Value, OpError> {
    Ok(core.runner().run(request(session, op, args, mode))?.plan.output.unwrap_or(Value::Null))
}

fn reply(json: bool, value: &Value, text: String) -> Result<Reply, OpError> {
    if json { crate::json::pretty(value).map(Reply::ok) } else { Ok(Reply::ok(text)) }
}

fn row(info: &BranchInfo) -> Value {
    json!({"id": info.id, "name": info.name, "head": info.head.to_string(), "seq": info.seq, "lane": info.lane})
}

/// The merge's units as lines: `  status  key  (title)`.
fn unit_lines(output: &Value) -> String {
    let units = output["units"].as_array().cloned().unwrap_or_default();
    units
        .iter()
        .map(|u| {
            let note = u["note"].as_str().map(|n| format!(" ({n})")).unwrap_or_default();
            format!(
                "  {:<9}  {}  {}{note}\n",
                u["status"].as_str().unwrap_or(""),
                u["key"].as_str().unwrap_or(""),
                u["title"].as_str().unwrap_or("")
            )
        })
        .collect()
}

/// Runs `command` on the project `core` holds.
///
/// # Errors
/// What the branch or op refuses with: an unknown branch, an open transaction blocking a switch, a merge that needs
/// a choice (exit 13).
#[allow(clippy::too_many_lines, reason = "one arm per subcommand, each short")]
pub fn run(core: &AppCore, command: &BranchCommand, session: &Session, json: bool) -> Result<Reply, OpError> {
    let by = &session.actor;
    let branches = core.branches();
    match command {
        BranchCommand::List { all } => {
            let args = json!({"include_auto": all, "include_bin": all});
            let listed = answer(core, session, "branch.list", args, Mode::Apply)?;
            let rows = listed["branches"].as_array().cloned().unwrap_or_default();
            let mut text: String = rows
                .iter()
                .map(|r| {
                    let here = if matches!(r["lane"]["open"]["folder"].as_bool(), Some(true)) { "*" } else { " " };
                    let changed = r["changed"]
                        .as_array()
                        .map(|c| c.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "));
                    let changed =
                        changed.filter(|c| !c.is_empty()).map(|c| format!("  changed: {c}")).unwrap_or_default();
                    format!("{here} {}{changed}\n", r["name"].as_str().unwrap_or(""))
                })
                .collect();
            if rows.is_empty() {
                text = "* main (nothing branched yet)\n".to_owned();
            }
            reply(json, &listed, text)
        }
        BranchCommand::Status { name: wanted } => {
            let args = match wanted {
                Some(n) => json!({"branch": n}),
                None => json!({}),
            };
            let status = answer(core, session, "branch.status", args, Mode::Apply)?;
            let text = format!("{}\n", status["summary"].as_str().unwrap_or(""));
            reply(json, &status, text)
        }
        BranchCommand::Create { name: new, from, title, focus } => {
            let (source, at) = start(core, from.as_deref())?;
            let mut spec = NewBranch::new(name(new)?, source).at(at).with_focus(focus.clone());
            if let Some(title) = title {
                spec = spec.with_title(title.clone());
            }
            let made = branches.create(spec, by).map_err(branch_error)?;
            reply(json, &row(&made), format!("created {} at version {}\n", made.name, made.seq))
        }
        BranchCommand::Switch { name: to } => {
            let target = by_name(core, to)?;
            let switched = branches.switch(&target.id, by).map_err(branch_error)?;
            reply(json, &json!({"from": switched.from, "to": switched.to}), format!("switched to {}\n", target.name))
        }
        BranchCommand::Checkpoint { branch, label, ready, note } => {
            let info = match branch {
                Some(b) => by_name(core, b)?,
                None => folder(core)?,
            };
            let ready = ready.then(|| note.clone().unwrap_or_default());
            let point = branches.checkpoint(&info.id, label, ready.clone(), by).map_err(branch_error)?;
            let what = if ready.is_some() { "ready for review" } else { "checkpoint" };
            reply(
                json,
                &json!({"branch": info.name, "point": point.to_string()}),
                format!("{what} {point} on {}\n", info.name),
            )
        }
        BranchCommand::Rename { name: old, new } => {
            let info = by_name(core, old)?;
            let renamed = branches.rename(&info.id, name(new)?, info.title.clone(), by).map_err(branch_error)?;
            reply(json, &row(&renamed), format!("renamed {} to {}\n", info.name, renamed.name))
        }
        BranchCommand::Delete { name: gone } => {
            let info = by_name(core, gone)?;
            branches.delete(&info.id, by).map_err(branch_error)?;
            reply(json, &row(&info), format!("deleted {} (in the bin for 30 days)\n", info.name))
        }
        BranchCommand::Undelete { name: back } => {
            let wanted = name(back)?;
            let binned = branches.list().into_iter().find(|b| b.deleted.is_some() && b.name == wanted);
            let info = binned.ok_or_else(|| OpError::not_found(format!("no branch {wanted} in the bin")))?;
            let restored = branches.undelete(&info.id, by).map_err(branch_error)?;
            reply(json, &row(&restored), format!("brought back {}\n", restored.name))
        }
        BranchCommand::Merge { from, at, dry_run, resolve, exclude } => {
            let mut choices = serde_json::Map::new();
            for pair in resolve {
                let (unit, side) = pair.split_once('=').ok_or_else(|| {
                    OpError::invalid("resolve", format!("`{pair}` is not UNIT=SIDE"))
                        .hint("e.g. --resolve characters/kit/parts/hair=theirs")
                })?;
                choices.insert(unit.trim().to_owned(), json!(side.trim()));
            }
            let mut args = json!({"from": from, "resolve": choices, "exclude": exclude});
            if let Some(at) = at {
                args["at"] = json!(at);
            }
            let mode = if *dry_run { Mode::DryRun } else { Mode::Apply };
            match answer(core, session, "branch.merge", args.clone(), mode) {
                Ok(output) => {
                    let head = if *dry_run { "would merge" } else { "merged" };
                    let text =
                        format!("{head} {from}: {}\n{}", output["summary"].as_str().unwrap_or(""), unit_lines(&output));
                    reply(json, &output, text)
                }
                Err(refused) if refused.code == ErrorCode::Conflict && refused.message.contains("need a choice") => {
                    // The units that need a choice, from the preview, with the exit code scripts branch on.
                    let preview = answer(core, session, "branch.merge", args, Mode::DryRun)?;
                    let text = format!("{}\n{}", refused.message, unit_lines(&preview));
                    let text = if json { crate::json::pretty(&preview)? } else { text };
                    Ok(Reply { text, code: exit::MERGE_CHOICES })
                }
                Err(refused) => Err(refused),
            }
        }
        BranchCommand::Diff { a, b } => {
            let here = folder(core)?;
            let state = |text: &str| -> Result<Value, OpError> {
                let info = by_name(core, text)?;
                Ok(if info.id == here.id { json!("now") } else { json!({"branch": info.name}) })
            };
            let args = json!({"from": state(a)?, "to": state(b)?});
            let output = answer(core, session, "history.diff", args, Mode::Apply)?;
            let units = output["units"].as_array().cloned().unwrap_or_default();
            let mut text = format!("{} changes between {a} and {b}\n", output["changes"]);
            for unit in units {
                text.push_str(&format!("  {}\n", unit["title"].as_str().unwrap_or("")));
            }
            reply(json, &output, text)
        }
    }
}
