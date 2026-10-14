//! Errors from building and running the server. Op failures are not here: they reach the agent as MCP tool errors
//! (see [`crate`] docs), because a protocol error would hide the hint the agent needs to fix its call.

use std::net::SocketAddr;

use thiserror::Error;

/// Why the MCP server could not be built or run.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ServerError {
    /// An op can't be exposed as a tool. The server refuses to start rather than serve a tool list that differs from
    /// `Runner::ops()`, so fix the op.
    #[error("op `{op}` can't be an MCP tool: {reason}")]
    UnusableOp {
        /// The op's name.
        op: String,
        /// What is wrong with it.
        reason: String,
    },
    /// Two ops (or an op and a built-in such as `history.undo`) would become the same tool. Tool names are op names
    /// with `.` replaced by `_` (clients reject dots), so `a.b_c` and `a_b.c` clash.
    #[error("`{first}` and `{second}` would both be the tool `{name}` (dots become underscores in tool names)")]
    DuplicateTool {
        /// The tool name they share.
        name: String,
        /// One op.
        first: String,
        /// The other.
        second: String,
    },
    /// The worker thread that forwards op events could not start.
    #[error("couldn't start the op event feed: {0}")]
    EventFeed(#[source] std::io::Error),
    /// The HTTP port was asked to listen on an address that isn't loopback. Remote access needs OAuth, which isn't built
    /// yet; until then the port is for apps on this machine.
    #[error("the MCP HTTP port may only listen on a loopback address (127.0.0.1 or ::1), not {0}")]
    NotLoopback(SocketAddr),
    /// The HTTP endpoint path is not a path.
    #[error("the MCP HTTP path must start with `/` and have no `?` or `#`, got {0:?}")]
    BadPath(String),
    /// An HTTP limit was set to a value the port can't work with: zero, which would refuse every client, or a timeout
    /// longer than [`HttpOptions::MAX_TIMEOUT`](crate::HttpOptions::MAX_TIMEOUT), which no deadline can be built from.
    #[error("the MCP HTTP option `{name}` {rule}, got {value}")]
    BadLimit {
        /// The `HttpOptions` field.
        name: &'static str,
        /// What it was set to.
        value: String,
        /// What it must satisfy, e.g. "must not be zero".
        rule: String,
    },
    /// The HTTP port could not be opened.
    #[error("couldn't listen on {0}: {1}")]
    Bind(SocketAddr, #[source] std::io::Error),
    /// The operating system could not provide random bytes for an access token.
    #[error("couldn't generate an access token: {0}")]
    Entropy(String),
    /// A stored access token record can't be admitted: a newer format than this build reads, a malformed hash, or a
    /// token that is already admitted.
    #[error("can't admit the stored access token `{label}`: {reason}")]
    BadTokenRecord {
        /// The record's label.
        label: String,
        /// What is wrong with it.
        reason: String,
    },
    /// A stored access token record is a tombstone: the token was revoked and stays revoked.
    #[error("the access token `{0}` was revoked and can't be admitted again")]
    TokenRevoked(String),
    /// The transport failed before the MCP handshake completed.
    #[error("MCP handshake failed: {0}")]
    Handshake(String),
    /// The connection ended abnormally after the handshake.
    #[error("MCP session failed: {0}")]
    Session(String),
}
