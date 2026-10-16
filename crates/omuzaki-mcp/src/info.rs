//! `session_info` and `session_list`: what an agent may read about its own session and about the others (design
//! `agent-sessions.md` §3.1, §10.2, ADR 0020). Text for a model, and the same facts as `structuredContent`.
//!
//! Names that other clients chose (their client name, model, role, fence paths) pass through the same one-line
//! sanitiser as the heads-up, so a hostile name can't smuggle instructions or hide behind invisible characters.

use omuzaki_ops_api::{Actor, Need, PauseReason, PendingApproval, SessionInfo, SessionState, Transport};
use rmcp::model::CallToolResult;
use serde_json::{Value, json};

use crate::agent::Credential;
use crate::approvals::need_text;
use crate::changes::{actor_label, plain};
use crate::render;

const NAME_CHARS: usize = 60;
const PATH_CHARS: usize = 100;

fn transport_name(transport: &Transport) -> String {
    match transport {
        Transport::Stdio => "stdio".to_owned(),
        Transport::Http { token } => format!("http (token {})", plain(token, NAME_CHARS)),
        Transport::InApp => "in-app".to_owned(),
        Transport::Plugin => "plugin".to_owned(),
        Transport::Automation => "automation".to_owned(),
        Transport::Replay => "replay".to_owned(),
    }
}

fn transport_kind(transport: &Transport) -> &'static str {
    match transport {
        Transport::Stdio => "stdio",
        Transport::Http { .. } => "http",
        Transport::InApp => "in_app",
        Transport::Plugin => "plugin",
        Transport::Automation => "automation",
        Transport::Replay => "replay",
    }
}

fn state_text(state: &SessionState) -> String {
    match state {
        SessionState::Active => "active".to_owned(),
        SessionState::Paused { by, reason } => {
            format!("paused by {} ({})", actor_label(by), pause_text(*reason))
        }
        SessionState::Ended { .. } => "ended".to_owned(),
    }
}

fn pause_text(reason: PauseReason) -> &'static str {
    match reason {
        PauseReason::Person => "a person paused it",
        PauseReason::Budget => "its budget ran out",
        PauseReason::Undo => "a person is undoing its work",
        PauseReason::Step => "it stops after each step",
        PauseReason::Pairing => "waiting to be paired in Omuzaki's window",
    }
}

/// The state with the name of whoever paused it passed through the same one-line sanitiser as every other name.
fn state_json(state: &SessionState) -> Value {
    match state {
        SessionState::Active => json!({"state": "active"}),
        SessionState::Paused { by, reason } => {
            json!({"state": "paused", "by": actor_label(by), "reason": render::value_of(reason)})
        }
        SessionState::Ended { reason, at } => {
            json!({"state": "ended", "reason": render::value_of(&crate::approvals::withdrawn_text(reason)), "at": render::value_of(at)})
        }
    }
}

/// What a person is asked, with the text a client chose (a pairing client's name, a reason) sanitised.
pub(crate) fn need_json(need: &Need) -> Value {
    match need {
        Need::Scope { scope } => json!({"need": "scope", "scope": render::value_of(scope)}),
        Need::Person { reason } => json!({"need": "person", "reason": plain(reason, 200)}),
        Need::Budget { needed, left } => {
            json!({"need": "budget", "needed": render::value_of(needed), "left": render::value_of(left)})
        }
        Need::Pairing { client } => json!({"need": "pairing", "client": plain(client, NAME_CHARS)}),
        Need::Review { policy } => json!({"need": "review", "policy": plain(policy, NAME_CHARS)}),
    }
}

fn fence_paths(info: &SessionInfo) -> Vec<String> {
    info.fence.iter().map(|path| plain(&path.to_string(), PATH_CHARS)).collect()
}

fn fence_text(info: &SessionInfo) -> String {
    let paths = fence_paths(info);
    if paths.is_empty() {
        "none (the whole project, except parts another session holds)".to_owned()
    } else {
        paths.join(", ")
    }
}

fn pending_json(pending: &[&PendingApproval]) -> Vec<Value> {
    pending
        .iter()
        .map(|approval| {
            json!({
                "id": approval.id.0,
                "need": need_json(&approval.need),
                "asks": need_text(&approval.need),
                "op": approval.request.as_ref().map(|r| r.op.clone()),
            })
        })
        .collect()
}

/// `session_info`: this session's own record.
pub(crate) fn session_info(
    info: &SessionInfo,
    pending: &[&PendingApproval],
    credential: Option<&Credential>,
) -> CallToolResult {
    let scopes: Vec<&str> = info.grants.scopes().map(omuzaki_ops_api::Scope::name).collect();
    let credential_json = credential.map(|c| json!({"label": plain(c.label(), NAME_CHARS), "paired": c.paired()}));
    let data = json!({
        "ok": true,
        "session": {
            "id": info.id.0,
            "actor": actor_label(&info.actor),
            "on_behalf_of": actor_label(&info.on_behalf_of),
            "client": plain(&info.client, NAME_CHARS),
            "model": info.model.as_deref().map(|m| plain(m, NAME_CHARS)),
            "role": info.role.as_deref().map(|r| plain(r, NAME_CHARS)),
            "transport": transport_kind(&info.transport),
            "credential": credential_json,
            "grants": {"scopes": scopes, "file_roots": info.grants.fs_roots()},
            "fence": fence_paths(info),
            "budget": render::value_of(&info.budget),
            "spent": render::value_of(&info.spent),
            "limits": render::value_of(&info.limits),
            "state": state_json(&info.state),
            "open_transaction": info.open_tx.as_ref().map(|tx| json!({"id": tx.tx.0, "label": plain(&tx.label, NAME_CHARS)})),
            "pending_approvals": pending_json(pending),
            "steps": info.steps,
        }
    });
    let mut lines = vec![format!(
        "Session {}: you are {} (client {}{}), working for {}, connected over {}.",
        info.id.0,
        actor_label(&info.actor),
        plain(&info.client, NAME_CHARS),
        info.model.as_deref().map(|m| format!(", model {}", plain(m, NAME_CHARS))).unwrap_or_default(),
        actor_label(&info.on_behalf_of),
        transport_name(&info.transport),
    )];
    lines.push(format!("State: {}.", state_text(&info.state)));
    if let SessionState::Paused { reason: PauseReason::Pairing, .. } = info.state {
        lines.push(
            "This session is waiting to be paired: ask the person to approve the client in Omuzaki's window. Nothing \
             you call will run until they do; session_info and approval_wait still answer."
                .to_owned(),
        );
    }
    if let Some(credential) = credential {
        let state = if credential.paired() { "paired" } else { "not paired yet" };
        lines.push(format!("Credential: access token {}, {state}.", plain(credential.label(), NAME_CHARS)));
    }
    let roots = info.grants.fs_roots();
    let roots = if roots.is_empty() { "none".to_owned() } else { roots.join(", ") };
    lines.push(format!("Scopes: {}. File roots: {roots}.", scopes.join(", ")));
    lines.push(format!("Fence: {}.", fence_text(info)));
    lines.push(format!(
        "Budget: {}; spent {} micro-dollars, {} tokens, {} seconds. Rate limits: {} edits/min (burst {}), {} runs/min (burst {}), {} pending approvals at most.",
        if info.budget == omuzaki_ops_api::Budget::default() { "unlimited".to_owned() } else { format!("{:?}", info.budget) },
        info.spent.usd_micros,
        info.spent.tokens,
        info.spent.seconds,
        info.limits.edits_per_minute,
        info.limits.edit_burst,
        info.limits.runs_per_minute,
        info.limits.run_burst,
        info.limits.max_pending,
    ));
    if let Some(tx) = &info.open_tx {
        lines.push(format!("Open transaction: {} ({:?}).", tx.tx.0, plain(&tx.label, NAME_CHARS)));
    }
    if pending.is_empty() {
        lines.push("Pending approvals: none.".to_owned());
    } else {
        lines.push("Pending approvals (call approval_wait with an id to wait for the person):".to_owned());
        for approval in pending {
            let op =
                approval.request.as_ref().map_or_else(String::new, |r| format!(" for {}", plain(&r.op, NAME_CHARS)));
            lines.push(format!("- #{}{op}: asks the person to {}.", approval.id.0, need_text(&approval.need)));
        }
    }
    render::confirmation(lines.join("\n"), data)
}

/// `session_list`: every live session, with this one marked.
pub(crate) fn session_list(sessions: &[SessionInfo], me: &Actor) -> CallToolResult {
    let live: Vec<&SessionInfo> = sessions.iter().filter(|s| !matches!(s.state, SessionState::Ended { .. })).collect();
    let rows: Vec<Value> = live
        .iter()
        .map(|info| {
            json!({
                "id": info.id.0,
                "actor": actor_label(&info.actor),
                "client": plain(&info.client, NAME_CHARS),
                "model": info.model.as_deref().map(|m| plain(m, NAME_CHARS)),
                "transport": transport_kind(&info.transport),
                "state": state_json(&info.state),
                "fence": fence_paths(info),
                "steps": info.steps,
                "you": &info.actor == me,
            })
        })
        .collect();
    let mut lines = vec![format!("{} live session{}:", live.len(), if live.len() == 1 { "" } else { "s" })];
    for info in &live {
        let you = if &info.actor == me { " (you)" } else { "" };
        lines.push(format!(
            "- #{} {}{you}: client {}, {}, {}, fence {}, {} steps.",
            info.id.0,
            actor_label(&info.actor),
            plain(&info.client, NAME_CHARS),
            transport_name(&info.transport),
            state_text(&info.state),
            fence_text(info),
            info.steps,
        ));
    }
    render::confirmation(lines.join("\n"), json!({"ok": true, "sessions": rows}))
}

/// The pending approvals of session `id`.
pub(crate) fn pending_of(all: &[PendingApproval], session: omuzaki_ops_api::SessionId) -> Vec<&PendingApproval> {
    all.iter().filter(|a| a.session == session).collect()
}
