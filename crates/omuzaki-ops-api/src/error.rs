//! Structured op errors: every refusal says what to fix (spec §6 "errors are structured and actionable").
//! Agents act on these directly, so the code, field and hint matter as much as the message.

use std::fmt;

use omuzaki_doc_api::{DocPath, DocVersion, Value};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::approval::{ApprovalId, Need};
use crate::session::{EndReason, PauseReason};
use crate::{Actor, CancelReason, JobId, Scope};

/// Broad class of an op failure; stable, so agents and scripts can branch on it. `#[non_exhaustive]`: a `match` outside
/// this crate needs a wildcard arm.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorCode {
    /// The arguments are wrong (out of range, malformed).
    Invalid,
    /// A referenced thing doesn't exist.
    NotFound,
    /// The actor lacks the scope; it may be queued for approval.
    Denied,
    /// The document changed underneath; re-plan and retry.
    Conflict,
    /// Valid, but not supported here (platform, missing model or device). Retrying won't help: pick another approach.
    Unsupported,
    /// The op needs a service the app did not wire in (a renderer, the character builder). Unlike `Unsupported`, the
    /// op itself is fine: report it to the user, or retry once the app provides the service.
    Unavailable,
    /// A bug in Omuzaki. Report it.
    Internal,
    /// Nothing ran: a person must act first (approve, pair, resume). Do not retry; the detail says what is awaited.
    Waiting,
    /// A session limit (rate or queue) was hit. Retry after the time in the detail.
    Limited,
    /// A job was cancelled (by a person, a stop, a cap or shutdown). Not a failure of the op: the detail says who
    /// and why.
    Cancelled,
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ErrorCode::Invalid => "invalid",
            ErrorCode::NotFound => "not_found",
            ErrorCode::Denied => "denied",
            ErrorCode::Conflict => "conflict",
            ErrorCode::Unsupported => "unsupported",
            ErrorCode::Unavailable => "unavailable",
            ErrorCode::Internal => "internal",
            ErrorCode::Waiting => "waiting",
            ErrorCode::Limited => "limited",
            ErrorCode::Cancelled => "cancelled",
        })
    }
}

/// Why an op was refused or failed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, thiserror::Error)]
#[error("{code}: {message}")]
pub struct OpError {
    /// The class of failure.
    pub code: ErrorCode,
    /// What went wrong, in a sentence.
    pub message: String,
    /// The argument at fault, when there is one (e.g. `blend`).
    pub field: Option<String>,
    /// How to fix it (e.g. "use a value between 0 and 1").
    pub hint: Option<String>,
    /// A machine-readable reason, when the code alone does not say enough (what is awaited, how long to wait). Absent
    /// from the JSON when empty, so older readers and writers are unaffected. Boxed so that `Result<_, OpError>`
    /// stays small (clippy's `result_large_err`); the JSON is the same as an unboxed [`ErrorDetail`]. Read it with
    /// `error.detail.as_deref()`, set it with [`with_detail`](Self::with_detail).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Box<ErrorDetail>>,
}

/// The structured reason behind an [`OpError`], for agents and UIs that branch on it. Set by the executor and by
/// [`OpCtx::require_person`](crate::OpCtx::require_person); an op sets one only through the constructors below.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ErrorDetail {
    /// The actor lacks this scope.
    MissingScope {
        /// The scope to grant.
        scope: Scope,
    },
    /// The op needs a person to act or confirm. Not yet queued: the executor turns it into [`Queued`](Self::Queued)
    /// when it can, and returns it as is when no approver is present.
    NeedsPerson {
        /// Why, shown to the person.
        reason: String,
    },
    /// Only a person can do this, and confirming is not enough. Never queued.
    PersonOnly {
        /// Why, and where the person can do it.
        reason: String,
    },
    /// Nothing ran; the request waits in the approval queue. Do not retry: wait for the resolution.
    Queued {
        /// The approval to wait for.
        approval: ApprovalId,
        /// What it waits for.
        need: Need,
    },
    /// The session is paused.
    Paused {
        /// Who paused it.
        by: Actor,
        /// Why.
        reason: PauseReason,
    },
    /// The session has ended.
    Ended {
        /// Why.
        reason: EndReason,
    },
    /// An agent or plugin made a request without an open session.
    NoSession,
    /// A session limit was hit.
    RetryAfter {
        /// How long to wait, in seconds.
        seconds: f32,
        /// The limit that was hit and its value, for example `edits_per_minute=600`.
        limit: String,
    },
    /// The document changed under the request's `base` version.
    Changed {
        /// The path that changed.
        path: DocPath,
        /// Who changed it last.
        by: Actor,
        /// The document version after that change.
        version: DocVersion,
    },
    /// An agent's edit lies outside its fence (`holder: None`) or inside another session's (`holder: Some`).
    Fenced {
        /// The path of the refused edit.
        path: DocPath,
        /// Whose fence the path lies in; `None` when the agent's own fence does not reach it.
        holder: Option<Actor>,
    },
    /// A job was cancelled.
    Cancelled {
        /// Which job.
        job: JobId,
        /// Why.
        reason: CancelReason,
    },
    /// Where in the source text an op was given the error is (for ops that take source, such as `script.run`).
    Position {
        /// The line, 1-based.
        line: u32,
        /// The column, 1-based.
        column: u32,
    },
    /// A driver job joined a transaction and was cancelled or failed, but reverting its edits conflicted with later
    /// changes. A revert is all or nothing, so it reverted **nothing**: every edit the job made stays in the document.
    Unreverted {
        /// The conflicts that stopped the revert.
        conflicts: Vec<RevertConflict>,
        /// The paths the job edited that did not conflict. They stay too, because nothing was reverted.
        retained: Vec<DocPath>,
    },
}

/// One path where a driver job's revert conflicted with a later change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RevertConflict {
    /// The path.
    pub path: DocPath,
    /// The value the job left there.
    pub job_value: Value,
    /// The value there now; `None` if it was removed.
    pub current: Option<Value>,
    /// Who made the later change.
    pub actor: Actor,
}

impl OpError {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), field: None, hint: None, detail: None }
    }

    /// Bad argument `field`.
    pub fn invalid(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self { field: Some(field.into()), ..Self::new(ErrorCode::Invalid, message) }
    }

    /// Something referenced doesn't exist.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    /// The actor lacks a scope.
    pub fn denied(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Denied, message)
    }

    /// The document changed underneath.
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Conflict, message)
    }

    /// Not supported here.
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unsupported, message)
    }

    /// The op needs the service named `service` (the trait's type name) and the app did not register it.
    pub fn unavailable(service: &'static str) -> Self {
        Self::new(ErrorCode::Unavailable, format!("the service `{service}` is not available"))
            .hint("the app did not wire this service in: report it to the user, or retry once it is available")
    }

    /// The op needs a person to act or confirm (`Denied`, detail [`ErrorDetail::NeedsPerson`]). The executor queues
    /// it for approval when a person can be asked.
    pub fn needs_person(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self::new(ErrorCode::Denied, format!("a person must confirm this: {reason}"))
            .with_detail(ErrorDetail::NeedsPerson { reason })
    }

    /// Only a person can do this and nobody can confirm it for them (`Denied`, detail [`ErrorDetail::PersonOnly`]).
    pub fn person_only(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self::new(ErrorCode::Denied, format!("only a person can do this: {reason}"))
            .hint("only a person can do this, in the Omuzaki window")
            .with_detail(ErrorDetail::PersonOnly { reason })
    }

    /// An agent's edit at `path` is outside its fence (`holder: None`) or inside `holder`'s (`Denied`, detail
    /// [`ErrorDetail::Fenced`]). The message names the holder.
    pub fn fenced(path: DocPath, holder: Option<Actor>) -> Self {
        let (message, hint) = match &holder {
            Some(actor) => (
                format!("`{path}` is fenced by {}", actor_name(actor)),
                "work on another part, or ask a person to change the fence",
            ),
            None => (
                format!("`{path}` is outside this session's fence"),
                "edit only inside the fence, or ask a person to widen it",
            ),
        };
        Self::new(ErrorCode::Denied, message).hint(hint).with_detail(ErrorDetail::Fenced { path, holder })
    }

    /// The job `job` was cancelled (code [`Cancelled`](ErrorCode::Cancelled)). The executor attaches the
    /// [`ErrorDetail::Cancelled`] naming who and why; an op only gets this from [`JobCtx::checkpoint`](crate::JobCtx).
    pub fn cancelled(job: JobId) -> Self {
        Self::new(ErrorCode::Cancelled, format!("job {} was cancelled", job.0))
    }

    /// A bug.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    /// Adds a fix-it hint.
    #[must_use]
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Attaches the machine-readable reason.
    #[must_use]
    pub fn with_detail(mut self, detail: ErrorDetail) -> Self {
        self.detail = Some(Box::new(detail));
        self
    }

    /// The error for arguments that don't decode into the schema of op `op`. `field` is the argument at fault, or
    /// `args` when the whole value is wrong. Its hint is the one [`is_decode_error`](Self::is_decode_error) recognises.
    pub fn decode_failed(op: &str, field: impl Into<String>, reason: impl fmt::Display) -> Self {
        Self::invalid(field, format!("arguments for {op} don't match its schema: {reason}"))
            .hint(format!("{DECODE_HINT_START}{op} (MCP tools/list, or `omuzaki help {op}`)"))
    }

    /// Whether this is an argument-decode failure from [`decode_failed`](Self::decode_failed). Its hint names the CLI,
    /// so a surface that talks to agents over MCP shows the expected call next to it instead.
    pub fn is_decode_error(&self) -> bool {
        self.code == ErrorCode::Invalid && self.hint.as_deref().is_some_and(|hint| hint.starts_with(DECODE_HINT_START))
    }
}

/// Opens the hint of every argument-decode error. [`OpError::is_decode_error`] matches on it; the wording is free to
/// change as long as both sides use this constant.
const DECODE_HINT_START: &str = "ask for the schema of ";

/// Who `actor` is, for a message.
fn actor_name(actor: &Actor) -> String {
    match actor {
        Actor::Person { name } => name.clone(),
        Actor::Agent { id, .. } => id.clone(),
        Actor::Plugin { id } => format!("plugin {id}"),
        Actor::System => "Omuzaki".to_owned(),
    }
}
