//! `omuzaki mcp`: the op registry served to an AI agent over MCP on this process's standard input and output.
//!
//! The protocol is [`omuzaki_mcp`]'s; this module owns the process side: build the server over the runner, run it on an
//! async runtime until the client goes away, and turn its failures into the CLI's errors. Standard output carries the
//! protocol, so nothing else may write to it: this crate prints only what [`output::finish`](crate::output::finish)
//! is handed, and `finish` is handed an empty string here. Logging (`tracing`, which the server and its MCP library
//! use) goes to standard error.

use std::io;
use std::sync::Arc;

use omuzaki_app::AppCore;
use omuzaki_mcp::{McpOptions, McpServer, ServerError};
use omuzaki_ops_api::{OpError, Runner};
use tracing_subscriber::EnvFilter;

use crate::cli::McpArgs;
use crate::session::Session;

/// Serves `runner` to one MCP client on stdin/stdout, with the default agent grants (read, edit, render and review) plus
/// `--grant`, listing the tools `--tools` says. Returns when
/// the client closes the connection. `connections` is the handle the runner's services hold (see
/// [`open_runner_with`](crate::session::open_runner_with)), which is what makes `agent.connection` listed and
/// answerable. Warnings and errors are logged to stderr; set `RUST_LOG` (for example `RUST_LOG=debug`) for more.
///
/// # Errors
/// `Internal` if the server can't start (an op that can't be a tool), the async runtime can't be created, or the
/// connection fails: the client sent something that isn't MCP, or went away before the handshake finished.
pub fn serve_stdio(runner: Arc<dyn Runner>, session: &Session, args: &McpArgs) -> Result<(), OpError> {
    log_to_stderr();
    let mut options = McpOptions::with_grants(args.grants())
        .with_tools(args.tools.mode())
        .with_connections(session.connections.clone());
    if let Some(sessions) = &session.sessions {
        // The agent works for the person running the command.
        options = options.with_sessions(Arc::clone(sessions), session.actor.clone());
    }
    if let Some(project) = &session.project {
        // The project folder, and no folder the person did not share with agents (ADR 0013): the command line shares none.
        options = options.with_roots(project.clone(), []);
    }
    let mut server = McpServer::new(runner, options).map_err(|e| server_error(&e))?;
    if let Some(jobs) = &session.jobs {
        // Long ops become tasks for clients that ask for them.
        server = server.with_jobs(Arc::clone(jobs));
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| OpError::internal(format!("couldn't start the async runtime: {e}")))?;
    let result = runtime.block_on(server.serve_stdio());
    // The runtime reads stdin on a blocking thread. When the session ends for another reason than stdin closing, that
    // thread is still waiting for input, and dropping the runtime would wait for it too: leave it to the exit.
    runtime.shutdown_background();
    result.map_err(|e| server_error(&e))
}

/// Serves the app core's MCP server to one MCP client on stdin/stdout: the same server, sessions, journal and project
/// root as the core's other surfaces, with the core's headless rules (nothing waits for an approval). Returns when the
/// client closes the connection; the caller closes the core.
///
/// # Errors
/// As [`serve_stdio`].
pub fn serve_core(core: &AppCore) -> Result<(), OpError> {
    log_to_stderr();
    core.runtime().block_on(core.mcp().serve_stdio()).map_err(|e| server_error(&e))
}

/// Sends `tracing` events to stderr, at `warn` unless `RUST_LOG` asks for another level.
pub(crate) fn log_to_stderr() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    // `try_init` fails only if a logger is already installed, and then that one keeps the job.
    let _ = tracing_subscriber::fmt().with_env_filter(filter).with_writer(io::stderr).with_ansi(false).try_init();
}

fn server_error(error: &ServerError) -> OpError {
    let hint = match error {
        ServerError::UnusableOp { .. } | ServerError::DuplicateTool { .. } => {
            "an op's name or schema can't become an MCP tool: fix the op that is named"
        }
        ServerError::Handshake(_) | ServerError::Session(_) => {
            "run `omuzaki mcp` from an MCP client (it speaks JSON-RPC on stdin and stdout), not by hand"
        }
        _ => "this is a bug in Omuzaki: report it",
    };
    OpError::internal(error.to_string()).hint(hint)
}
