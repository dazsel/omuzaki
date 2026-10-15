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
//! `tx_rollback`, sorted by name. The list is built once for each set of grants (at startup, and for each HTTP access
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
//!
//! **Sessions.** The actor is `Actor::Agent { id: "<client name>#<n>" }` from the client's handshake; grants come from
//! [`McpOptions`] (default `Grants::agent_default()`), or from the access token on the HTTP port, and the tool
//! descriptions and instructions an agent reads are written for its own grants. Every call needs the `read` scope plus the op's own; a preview
//! (`dry_run`) needs only `read`. `tx_begin` opens the session's one transaction and later applied edits join it
//! until `tx_commit` or `tx_rollback`; a second `tx_begin` is refused before it reaches the runner. A connection that
//! ends first rolls the transaction back. Undo and redo are per actor: an
//! agent's `history_undo` reverts only its own changes, never the user's or another agent's.
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
//!
//! **Prompts.** Recipes the client offers its user (`prompts/list`, `prompts/get`). A [`PromptDef`] names the ops its
//! text relies on (`needs`), and a session is offered only the prompts whose ops are all among its tools, so a recipe
//! never recommends a missing tool. The text is a template
//! with `{argument}` and `{tool:face.set_profile}` placeholders (the latter becomes `face_set_profile`); a missing
//! required argument, an unknown one or a non-text one is `invalid_params` naming it, and an unknown or unlisted prompt
//! is `invalid_params` listing those that exist, as rmcp's own prompt router answers. No recipe ships yet: each lands
//! with the feature whose ops it needs. A host adds one with [`McpOptions::with_prompt`].
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
//!    time to send headers (`header_timeout`) and idle sessions (`idle_timeout`). A refusal names the limit and its
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
mod builtin;
mod catalog;
mod changes;
mod describe;
mod error;
mod events;
mod gate;
mod http;
mod limits;
mod names;
mod prompts;
mod remote;
mod render;
mod resources;
mod schema;
mod server;
mod session;
mod signature;
mod suggest;

pub use access::{AccessToken, RECORD_VERSION, TokenId, TokenRecord};
pub use error::ServerError;
pub use http::{HttpOptions, HttpServer};
pub use prompts::{PromptArg, PromptDef};
pub use resources::{MeasureProvider, RenderProvider, ResourceProvider};
pub use server::{McpOptions, McpServer};
pub use session::Session;
