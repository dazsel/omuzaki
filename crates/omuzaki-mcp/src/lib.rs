//! The MCP server (spec v3 §11): every registered op is a tool, so Claude Code, Codex and any other MCP client can
//! drive Omuzaki. It serves an `Arc<dyn Runner>` and knows nothing about the executor behind it.
//!
//! ```ignore
//! let server = omuzaki_mcp::McpServer::new(runner, omuzaki_mcp::McpOptions::default())?;
//! server.serve_stdio().await?; // what `omuzaki mcp` runs
//! ```
//!
//! # What an agent sees
//!
//! **Tools.** One per `Runner::ops()` entry, plus `history_undo`, `history_redo`, `history_log`, `tx_begin`, `tx_commit` and
//! `tx_rollback`, and, when the host wired a session registry ([`McpOptions::with_sessions`]), `session_info`,
//! `session_list` and `approval_wait`, sorted by name. The list is built once for each set of grants (at startup, and for each HTTP access
//! token), and construction fails rather than serve a list that differs from `ops()`.
//!
//! - `name`: the op name with `.` replaced by `_` (`face.set_profile` is the tool `face_set_profile`), because
//!   Anthropic's tool-name rule is `^[a-zA-Z0-9_-]{1,64}$` and many clients follow it. Startup fails if two ops
//!   become the same name or one is not 1 to 64 characters of letters, digits, `_` and `-`. The dotted op name is the
//!   tool's `title`, the start of its description, and still accepted as a name in calls; the runner always gets the
//!   dotted name. Everything the server says (results, errors, `Expected:` lines, instructions) uses the tool name.
//! - `description`: `face.set_profile: <the op's summary>`, then its kind (read-only / undoable edit / job), whether this session holds its
//!   scope (and `read`), how to preview, and what to check afterwards. Built from registry facts and from which tools
//!   exist, so it never recommends a missing tool.
//! - `inputSchema`: the op's schema (minus its Rust type name as `title`), plus an optional boolean `dry_run` on edits
//!   and jobs. Queries have nothing to preview, so they don't list it (a stray `dry_run` on a query is ignored).
//!   Arguments are checked against the schema's top level before the op runs: unknown (with a "did you mean"; serde
//!   would otherwise drop a misspelt `dry_run` and apply), missing, or of the wrong type, each naming the argument.
//! - `annotations`: `readOnlyHint` for queries; `destructiveHint` false unless a non-query can't be undone;
//!   `openWorldHint` for scopes that reach outside the machine; `idempotentHint` for queries.
//!
//! **Results.** Always a tool result, never a protocol error, so the agent sees the message.
//!
//! - Success: text and `structuredContent` `{ok: true, op, mode?: "dry_run" | "apply", applied?, output?, changes?,
//!   cost?, log?: {seq, tx, version}}` (`mode` and `applied` are absent for queries). The text is what text-only
//!   clients show a model, so it is complete: a headline, the changes, cost and next steps, then the output as
//!   pretty JSON, **never cut** (a result over 100 KB says so in the first line and is still included whole). Change
//!   lists over 100 lines, or with a value over 200 characters, are shortened in the first block and given in full
//!   in a second text block.
//! - Failure (`isError: true`): `{ok: false, op, code, message, field?, hint, signature?}` where `code` is the
//!   [`ErrorCode`](omuzaki_ops_api::ErrorCode) (`invalid`, `not_found`, `denied`, `conflict`, `unsupported`,
//!   `unavailable`, `internal`). `hint` is the op's own, or a default for the code. `invalid` errors also carry the
//!   expected call `signature` (nested shapes spelt out, e.g. `eye?: {size: number}`), and the text ends with
//!   `Expected: ...`.
//!   Unknown tool names get suggestions.
//! - Unsaved change (optional, only with a journal): an applied change that the journal hasn't saved within five
//!   seconds still succeeds, and its text opens with `Warning: applied, not yet saved: <status>`, before any heads-up.
//!   `structuredContent.warning` carries the same text. See [`McpOptions::with_journal`].
//!
//! **Sessions.** With a registry ([`McpOptions::with_sessions`]) every connection is a
//! registered session (design `agent-sessions.md`): it is opened at the handshake with the client's name, the
//! transport (stdio, or the HTTP token's label), the person it works for (the host's, or the one who issued the token)
//! and the server's grants as its ceiling, and the actor is `Actor::Agent { id: "<client>#<n>", .. }` **as the registry
//! built it**; this crate invents no name. A client whose handshake fails (a protocol version it can't speak) is never
//! named or connected. The session ends through the registry: `Disconnected` when the client leaves, `IdleTimeout` when
//! an HTTP session goes quiet, `Revoked` when its token is revoked or its grants change; the open transaction is
//! abandoned with it. When the registry ends a session first (a person revoked it, the project closed) the connection
//! closes, and when it revoked the credential too, the token stops working and its other sessions end. Without a
//! registry the server names the agent itself and rolls the transaction back on drop.
//!
//! **Headless.** `omuzaki mcp` has no window to approve in, so the app opens its executor with no approver (design
//! §7.6): a request that would queue is refused with `denied`, what it needs as the detail and a hint that says how to
//! grant it in advance, and `approval_wait` has nothing to wait for. With the studio's window the same request queues
//! and the person answers it there.
//!
//! Grants come from [`McpOptions`] (default `Grants::agent_default()`), or from the access token on the HTTP port, plus
//! the file roots the host lists with [`McpOptions::with_roots`] (the project folder and the folders the person shared
//! with agents, nothing else, ADR 0013); the tool descriptions and instructions an agent reads are written for its own
//! grants. Every call needs the `read` scope plus the op's own; a preview (`dry_run`) needs only `read`. `tx_begin`
//! opens the session's one transaction and later applied edits join it until `tx_commit` or `tx_rollback`; a second
//! `tx_begin` is refused before it reaches the runner. Undo and redo are per actor: an agent's `history_undo` reverts only
//! its own changes, never the user's or another agent's.
//!
//! - **The stale-write guard.** Every request carries `base`, the newest document version the session's heads-up
//!   cursor has told the agent about, so a person's (or another agent's) edit to the same value between the agent's
//!   read and its write is a `conflict` (`structuredContent.detail.kind` is `changed`, naming who and where) and the
//!   retry, caught up by the heads-up, goes through.
//! - **Waiting.** A call refused because a person must act comes back `waiting`, never retried by this crate beyond the
//!   rules below: *paused* says "the person paused this session; nothing ran"; *queued* says "approval #N asks the
//!   person to ...; nothing ran; don't retry; call `approval_wait` or carry on". A call refused for a **pause** is held
//!   up to [`McpOptions::with_pause_hold`] (20 s) for the person to resume the session, then runs once more. A call
//!   refused by a **rate limit** that frees up within 2 s in all is slowed, not failed; a longer wait is `limited`
//!   with the time to wait.
//! - **`approval_wait { id, timeout_seconds? }`** waits (30 s by default, 50 at most) for the person to decide one of
//!   *this session's* approvals, including the pairing of a new client, and returns the outcome once: an approved
//!   request renders like the op's own success, a failure like its error, a rejection carries the person's comment, a
//!   withdrawal says why. Time running out says the approval is still pending. Outcomes the agent didn't wait for are
//!   listed in the heads-up of its next result, once. Nothing in this crate can approve anything.
//! - **`session_info`** (no arguments) is the session's own record: actor, person, transport, credential (label and
//!   whether it is paired), grants (scopes and file roots), fence, budget and spend, limits, state, open transaction
//!   and pending approvals. **`session_list`** lists every live session with this one marked `you`. Both need `read`,
//!   and both answer while the session is paused, including while it waits to be paired.
//! - **Fences.** An edit outside the session's fence, or inside another's, is refused: "Refused: <path> is held by
//!   <holder> (their fence: <paths>). Nothing changed." (or "outside your fence"), with
//!   `structuredContent.detail = {kind: "fenced", path, holder?}`; names pass through the same one-line sanitiser as the
//!   heads-up. The server guide tells a session its own fence, or that parts held by others are refused.
//!
//! **Tasks.** With [`McpServer::with_jobs`], a job op called by a client that declared the tasks capability starts
//! through `Jobs::start` and answers with a task handle (`job-<n>`); `tasks/get` reads the job, `tasks/cancel` cancels it
//! (only the session that started it may), progress and task-status notifications follow the job's events, and a
//! finished task's result is exactly what the blocking call of the same op returns. Other clients keep the blocking call.
//!
//! **In-app.** [`McpServer::connect_in_app`] opens a session for the built-in assistant (or a sub-agent) at once and
//! serves it on an in-memory stream: the same tools, results and rules as any session, no port, and an
//! [`InAppLink`] that says when the session ended.
//!
//! **What others changed.** Most clients never show a model the resource notifications below, so an agent that edits
//! while the user or another agent also edits could act on stale state without knowing. Every session therefore
//! remembers the newest `omuzaki://log` event number (`n`) it has been told about, and any tool result (success or
//! failure) opens with a `Heads-up:` paragraph when other actors have changed the project since:
//!
//! ```text
//! Heads-up: since your last call, others changed the project (3 changes; the log is now at n=44).
//! - user Sam applied face_set_blend x2 (n=38-41): characters/char-a/face/blend
//! - agent hair-bot#2 applied hair_set_style (n=43): characters/char-a/hair/style
//! - agent hair-bot#2 undid "oval hair" (n=44): characters/char-a/hair/style
//! Re-read what you rely on before editing it. For old and new values, read omuzaki://log (or call history_log) and look at events with n above 37.
//! ```
//!
//! The same facts are in `structuredContent.since_your_last_call` (`total`, `after_n`, `up_to_n`, `changes`: who,
//! `action`, `what`, `times`, `first_n`, `last_n`, `paths`; `not_listed`, `missed`, `more`). Edits are grouped by who
//! did what, so a burst is one line with a count; at most 10 lines are listed (the most recent) and older ones are
//! counted; paths are capped at three a line; values are left to the log. An idle project adds nothing, and an
//! actor's own edits, undos and rollbacks are never repeated back (an undo, redo or rollback is attributed to the owner
//! of the transaction's ops). A session starts caught up with the log, and reading `omuzaki://log` catches it up too.
//! The digest needs `read`, as the log does. Names, labels and paths from other clients are cut to one short line of
//! letters, digits and ASCII punctuation (anything else, such as an invisible or bidi character, shows as `?`). An
//! event reaches the log a moment after the op that caused it, so a result may leave out a change made microseconds
//! earlier; the next result reports it, because positions are event numbers. See `changes` for the details.
//!
//! **Events.** Runner events (op applied, transaction committed/rolled back, undo, redo) feed the resource
//! `omuzaki://log`. Events carry document values and `Runner::get`/`subscribe` don't check grants themselves, so
//! this server does: reading the log, subscribing to it and being notified all need the `read` scope. A client that
//! subscribes gets `notifications/resources/updated` when the log changes and reads it to catch up. Resource updates
//! are used because they exist in every protocol version (via `resources/subscribe` up to 2025-11-25 and
//! `subscriptions/listen` from 2026-07-28), while logging is deprecated and progress only exists inside a request.
//! See `events` for the details.
//!
//! **Resources.** `omuzaki://log` (above) plus whatever the registered [`ResourceProvider`]s serve. The built-in provider
//! gives `omuzaki://project`, a summary of the document (each top-level section's ids or value, not a dump), and
//! `omuzaki://character/<id>`, the stored character verbatim (listed, and templated for `resources/templates/list`). Reads
//! need `read`; an unknown URI is `resource_not_found` naming the URIs that exist, as is an id with no character. A host
//! adds a family with [`McpOptions::with_provider`], never by editing a list here. [`MeasureProvider`] serves
//! `omuzaki://measure/liveliness/..`: a read runs the liveliness measurement and returns its answer, nothing is stored.
//! [`RenderProvider`] serves `omuzaki://render/<id>.png`, a PNG the asset store holds, read as an `image/png` blob.
//! [`MetricsProvider`] serves `omuzaki://metrics/live`, the studio's live numbers from a [`MetricsSource`] the app supplies,
//! and [`LibraryProvider`] serves `omuzaki://library/<item>[@<version>]`, one library item with its publisher, manifest
//! and versions (the publisher's text is marked as data unless the publisher is first-party or verified).
//!
//! **Prompts.** Recipes the client offers its user (`prompts/list`, `prompts/get`). A [`PromptDef`] names the ops its
//! text relies on (`needs`), and a session is offered only the prompts whose ops are all among its tools, so a recipe
//! never recommends a missing tool. The text is a template
//! with `{argument}` and `{tool:face.set_profile}` placeholders (the latter becomes `face_set_profile`); a missing
//! required argument, an unknown one or a non-text one is `invalid_params` naming it, and an unknown or unlisted prompt
//! is `invalid_params` listing those that exist, as rmcp's own prompt router answers. The built-in recipes (create a
//! character from a sheet, match a reference look, make an emote pack, make a Quest variant) are listed when their ops
//! exist; the rest of the spec's seven land with the features whose ops they need. A host adds one with
//! [`McpOptions::with_prompt`].
//!
//! # Transports and logging
//!
//! **stdio** ([`McpServer::serve_stdio`]): `omuzaki mcp`, for an agent that starts the process. Any other rmcp
//! transport works through [`McpServer::serve`]. On stdio, stdout is the protocol: log to stderr.
//!
//! **Streamable HTTP** ([`McpServer::serve_http`]): a local port for other apps.
//!
//! ```ignore
//! let port = server.serve_http(omuzaki_mcp::HttpOptions::default()).await?; // 127.0.0.1:7437/mcp
//! // The user approves a client; the studio issues it a token carrying exactly the approved grants:
//! let token = port.issue("Codex", Grants::agent_default())?;
//! // The client sends `Authorization: Bearer <token.secret()>` to `port.url()`.
//! port.revoke(token.id()).await; // ends its sessions too
//! let connected = port.tokens().await; // the live tokens, for the "connected apps" panel: never their secrets
//! ```
//!
//! What stands between a request and the runner, in order:
//!
//! 1. **Loopback only.** The port binds `127.0.0.1` (or `::1`) and refuses to bind anything else: remote access needs
//!    OAuth, which is a separate task. [`HttpOptions`] has no way around this.
//! 2. **A token.** The port starts with no valid credentials, and every request needs `Authorization: Bearer <token>`.
//!    Tokens are 256 random bits from the OS, compared in constant time, never logged, and never accepted in a URL.
//!    Each token has its own grants, fixed when it is issued ([`HttpServer::issue`]): a remote or less trusted client
//!    can't inherit what a local one holds, and `broadcast`, `publish` and `spend` appear only if the host lists them.
//! 3. **`Host` and `Origin`** (the MCP spec's DNS-rebinding guidance). Only loopback `Host` names are served, so a
//!    page that rebinds its own domain to `127.0.0.1` is refused. Any request carrying an `Origin` header (that is,
//!    every cross-origin request from a web page) is refused unless the host listed that origin with
//!    [`HttpOptions::allow_origin`]. Native clients send no `Origin`.
//! 4. **A session of the token's own.** An rmcp service exists per token, so a session id means nothing to another
//!    token, and revoking a token ends its sessions and rolls back their open transactions. Idle sessions are closed
//!    after [`HttpOptions::idle_timeout`], with the same rollback.
//! 5. **Bounded resources.** A valid token is not a licence to exhaust the port, so everything a client can make it
//!    hold on to has a limit with a documented default in [`HttpOptions`]: sessions per token
//!    (`max_sessions_per_token`, refused with 429 at the cap, never by closing a session in use), request body size
//!    (`max_body_bytes`, 413) and time to deliver a whole body (`body_timeout`, 408), connections (`max_connections`),
//!    time to send headers (`header_timeout`), time a response may stay blocked on a client that stopped reading it
//!    (`write_timeout`) and idle sessions (`idle_timeout`). A refusal names the limit and its
//!    value, in the response and in the log.
//!
//! **Lifecycle.** The HTTP port speaks the `initialize` session lifecycle (protocol 2025-11-25 and earlier, which is
//! what Claude Code and Codex use). Protocol 2026-07-28 removed sessions: each request stands alone, and rmcp gives
//! each one a fresh handler, so a transaction or an agent identity could not outlive a request. The port therefore
//! answers `server/discover` as a server without a stateless mode does (so a client that probes first falls back to
//! `initialize`), and a request that arrives stateless anyway can query and edit but gets an `unsupported` error
//! from the stateful tools (`tx_*`, `history_*`) that says what to do.
#![deny(missing_docs)]

mod access;
mod agent;
mod approvals;
mod builtin;
mod catalog;
mod changes;
mod connection;
mod describe;
mod error;
mod events;
mod floats;
mod gate;
mod http;
mod in_app;
mod info;
mod jobs;
mod limits;
mod names;
mod prompts;
mod recipes;
mod remote;
mod render;
mod resources;
mod schema;
mod server;
mod session;
mod signature;
mod suggest;

pub use access::{AccessToken, RECORD_VERSION, TokenId, TokenInfo, TokenRecord};
pub use connection::Connections;
pub use error::ServerError;
pub use http::{HttpOptions, HttpServer, RevokedHook};
pub use in_app::{InAppEnd, InAppLink, InAppSpec};
pub use prompts::{PromptArg, PromptDef};
pub use resources::{
    LibraryProvider, MeasureProvider, MetricsProvider, MetricsSource, RenderProvider, ResourceProvider,
};
pub use server::{McpOptions, McpServer};
pub use session::Session;

// This crate registers ops (`agent.connection`), so the app that calls it links them in (ADR 0015).
omuzaki_core::define_link! {
    omuzaki_library_api::link();
}
