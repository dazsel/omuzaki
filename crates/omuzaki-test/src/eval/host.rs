//! The host an attempt talks to (design 6.1): the same runner and MCP server `omuzaki mcp` builds, reached by an MCP
//! client, so an eval cannot drift from the product.
//!
//! - [`Host::in_process`] opens the attempt's project copy with `omuzaki_cli::session::open_runner`, the constructor
//!   `omuzaki mcp` uses, and serves it with [`omuzaki_mcp::McpServer`] over an in-memory pipe.
//! - [`Host::with_runner`] does the same for any runner (the tests serve a mock one).
//! - [`Host::binary`] runs `omuzaki mcp --project <copy>` as a child process over stdio, with a scrubbed environment, and
//!   kills it by its process id when the attempt ends. `omuzaki mcp` always grants the agent default, so a task that
//!   needs other grants cannot run this way.
//!
//! A host owns a small async runtime and gives the driver a blocking [`Host::call`], so drivers stay plain code.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use omuzaki_cli::cli::{self, Cli};
use omuzaki_doc_api::DocPath;
use omuzaki_mcp::{Connections, McpOptions, McpServer, ServerError};
use omuzaki_ops_api::{Grants, Runner, Scope};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, Tool};
use rmcp::service::{RoleClient, RunningService};
use serde_json::{Map, Value};
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

use super::transcript::ToolNames;

/// The size of the in-memory pipe between the client and the server.
const PIPE_BYTES: usize = 64 * 1024;

/// Why a host could not be opened, or a call could not be made. A tool that answers with an error is not one of these:
/// that is an ordinary [`Reply`] the script or the model reads.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// The async runtime could not start.
    #[error("couldn't start the async runtime: {0}")]
    Runtime(String),
    /// The project could not be opened as a runner.
    #[error("couldn't open the project: {0}")]
    Open(String),
    /// The server or the client could not start or ended abnormally.
    #[error("the MCP connection failed: {0}")]
    Connection(String),
    /// The `omuzaki` binary could not be started.
    #[error("couldn't start {}: {reason}", path.display())]
    Spawn {
        /// The binary.
        path: PathBuf,
        /// The OS error.
        reason: String,
    },
    /// The task needs grants the binary host cannot give (`omuzaki mcp` always grants the agent default).
    #[error("`omuzaki mcp` grants the agent default; this task needs other scopes, so it can only run in process")]
    GrantsNotSupported,
    /// A call took longer than the time left to the attempt.
    #[error("the call to `{0}` did not answer in time")]
    Timeout(String),
    /// A call could not be made or answered.
    #[error("the call to `{tool}` failed: {reason}")]
    Call {
        /// The tool.
        tool: String,
        /// What the client said.
        reason: String,
    },
    /// The document could not be read.
    #[error("couldn't read the document: {0}")]
    Document(String),
}

/// What a tool answered.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    /// Whether the server marked the answer an error.
    pub is_error: bool,
    /// The answer's `structuredContent`: op, mode, applied, output, changes, log.
    pub structured: Option<Value>,
    /// The first text block, which is what a text-only client shows its model.
    pub text: String,
}

/// How the document is read between calls.
enum Reader {
    /// The runner the server runs on.
    Runner(Arc<dyn Runner>),
    /// The project folder, opened fresh each time: the binary host's server is in another process, and saves as it goes.
    Project(PathBuf),
}

/// The server of one attempt and the client the driver calls it through.
pub struct Host {
    runtime: Runtime,
    client: Option<RunningService<RoleClient, ()>>,
    server: Option<JoinHandle<Result<(), ServerError>>>,
    child: Option<tokio::process::Child>,
    reader: Reader,
    tool_names: ToolNames,
    tools_hash: String,
    start_doc: Value,
}

impl Host {
    /// Serves `runner` to a client in this process, with `grants` as the session's scopes.
    ///
    /// # Errors
    /// [`HostError`] when the runtime, the server or the client cannot start, or the document cannot be read.
    pub fn with_runner(runner: Arc<dyn Runner>, grants: Grants) -> Result<Self, HostError> {
        Self::serve(runner, grants, Connections::default())
    }

    /// Opens the project copy in `project` with `omuzaki-cli`'s own constructor and serves it in this process.
    ///
    /// # Errors
    /// [`HostError::Open`] when the project cannot be opened; otherwise as [`Host::with_runner`].
    pub fn in_process(project: &Path, grants: Grants) -> Result<Self, HostError> {
        let connections = Connections::default();
        let runner = open_runner(project, &connections)?;
        Self::serve(runner, grants, connections)
    }

    fn serve(runner: Arc<dyn Runner>, grants: Grants, connections: Connections) -> Result<Self, HostError> {
        let runtime = runtime()?;
        let options = McpOptions::with_grants(grants).with_connections(connections);
        let server = McpServer::new(runner.clone(), options).map_err(|e| HostError::Connection(e.to_string()))?;
        let tool_names = ToolNames::of_server(&server);
        let tools_hash = hash_tools(&server.tools())?;
        let (server_io, client_io) = tokio::io::duplex(PIPE_BYTES);
        let serving = server.clone();
        let task = runtime.spawn(async move { serving.serve(server_io).await });
        let client = bounded(&runtime, STARTUP, "the connection", ().serve(client_io))?;
        let reader = Reader::Runner(runner);
        let start_doc = read_document(&reader)?;
        Ok(Self {
            runtime,
            client: Some(client),
            server: Some(task),
            child: None,
            reader,
            tool_names,
            tools_hash,
            start_doc,
        })
    }

    /// Runs `omuzaki mcp --project <project>` from the binary at `omuzaki` and connects to it over stdio.
    ///
    /// The child's environment is [`scrubbed`]: no key, token or `OMUZAKI_EVAL_*` variable reaches it. It is killed by its
    /// process id when the host finishes or is dropped.
    ///
    /// # Errors
    /// [`HostError::GrantsNotSupported`] unless `grants` are the agent default; [`HostError::Spawn`] when the binary does
    /// not start; otherwise as [`Host::with_runner`].
    pub fn binary(omuzaki: &Path, project: &Path, grants: &Grants) -> Result<Self, HostError> {
        if *grants != Grants::agent_default() {
            return Err(HostError::GrantsNotSupported);
        }
        let runtime = runtime()?;
        let spawn_error = |reason: String| HostError::Spawn { path: omuzaki.to_owned(), reason };
        let mut child = {
            let _enter = runtime.enter();
            tokio::process::Command::new(omuzaki)
                .arg("--project")
                .arg(project)
                .arg("mcp")
                .env_clear()
                .envs(scrubbed(std::env::vars_os()))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .map_err(|e| spawn_error(e.to_string()))?
        };
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(spawn_error("the child's pipes are not open".to_owned()));
        };
        // A binary that never answers must not hang the run: past the limit the early return drops the child, which kills it.
        let client = bounded(&runtime, STARTUP, "the connection", ().serve((stdout, stdin)))?;
        let tools = bounded(&runtime, STARTUP, "the tool list", client.list_all_tools())?;
        let reader = Reader::Project(project.to_owned());
        let start_doc = read_document(&reader)?;
        Ok(Self {
            runtime,
            client: Some(client),
            server: None,
            child: Some(child),
            reader,
            tool_names: ToolNames::of_tools(&tools),
            tools_hash: hash_tools(&tools)?,
            start_doc,
        })
    }

    /// The tool-name to op-name map of this server's catalog.
    #[must_use]
    pub fn tool_names(&self) -> &ToolNames {
        &self.tool_names
    }

    /// A hash of the tool list the server offers (names, descriptions and schemas), as `b3:<hex>`: a changed tool surface
    /// is a different measurement, and the history keeps the hash beside every attempt.
    #[must_use]
    pub fn tools_hash(&self) -> &str {
        &self.tools_hash
    }

    /// The document as the fixture built it, before any call.
    #[must_use]
    pub fn start_document(&self) -> &Value {
        &self.start_doc
    }

    /// The document now.
    ///
    /// # Errors
    /// [`HostError::Document`] when it cannot be read.
    pub fn document(&self) -> Result<Value, HostError> {
        read_document(&self.reader)
    }

    /// The process id of the child, for the binary host.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().and_then(tokio::process::Child::id)
    }

    /// Calls the tool `tool` with `args`, waiting at most `within`.
    ///
    /// # Errors
    /// [`HostError::Timeout`] when it takes longer; [`HostError::Call`] when the call cannot be made. A tool that
    /// answers with an error is a [`Reply`] with `is_error`.
    pub fn call(&self, tool: &str, args: Map<String, Value>, within: Duration) -> Result<Reply, HostError> {
        let fail = |reason: String| HostError::Call { tool: tool.to_owned(), reason };
        let client = self.client.as_ref().ok_or_else(|| fail("the host has finished".to_owned()))?;
        let request = CallToolRequestParams::new(tool.to_owned()).with_arguments(args);
        let answer = self
            .runtime
            .block_on(async { tokio::time::timeout(within, client.call_tool(request)).await })
            .map_err(|_| HostError::Timeout(tool.to_owned()))?
            .map_err(|e| fail(e.to_string()))?;
        Ok(reply_of(&answer))
    }

    /// Ends the session, stops the server and kills the child. Returns what went wrong, if anything; dropping the host
    /// does the same without reporting.
    ///
    /// # Errors
    /// [`HostError::Connection`] when the server ended abnormally.
    pub fn finish(mut self) -> Result<(), HostError> {
        self.shutdown()
    }

    fn shutdown(&mut self) -> Result<(), HostError> {
        let mut outcome = Ok(());
        if let Some(mut client) = self.client.take() {
            // A closed pipe is the expected end of a session; what matters is that the server then stops. A peer that
            // does not answer is given up on after a while.
            let _ = bounded(&self.runtime, SHUTDOWN, "closing the session", client.close());
        }
        if let Some(mut task) = self.server.take() {
            match self.runtime.block_on(async { tokio::time::timeout(SHUTDOWN, &mut task).await }) {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(e))) => outcome = Err(HostError::Connection(e.to_string())),
                Ok(Err(e)) => outcome = Err(HostError::Connection(e.to_string())),
                Err(_) => {
                    task.abort();
                    outcome = Err(HostError::Connection("the server did not stop in time".to_owned()));
                }
            }
        }
        if let Some(mut child) = self.child.take() {
            // Killed, then reaped: no child outlives the attempt (machine rules).
            let _ = self.runtime.block_on(async {
                child.start_kill()?;
                let _ = tokio::time::timeout(SHUTDOWN, child.wait()).await;
                Ok::<(), std::io::Error>(())
            });
        }
        outcome
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // Nothing to report to from a destructor; `finish` is the way to see an error.
        let _ = self.shutdown();
    }
}

/// The hash of a `tools/list` answer: the tools sorted by name, as JSON.
fn hash_tools(tools: &[Tool]) -> Result<String, HostError> {
    let mut sorted: Vec<&Tool> = tools.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    // A hash of nothing would group the attempt under a tool surface that does not exist: a failure is harness trouble.
    let bytes = serde_json::to_vec(&sorted)
        .map_err(|e| HostError::Connection(format!("the tool list cannot be written as JSON: {e}")))?;
    Ok(format!("b3:{}", blake3::hash(&bytes).to_hex()))
}

/// How long a session may take to start (the connection and the tool list), and to end.
const STARTUP: Duration = Duration::from_secs(30);
const SHUTDOWN: Duration = Duration::from_secs(10);

/// Runs `future` on `runtime`, giving up after `limit`: a server that never answers is a harness error, not a hang.
fn bounded<T, E: std::fmt::Display>(
    runtime: &Runtime,
    limit: Duration,
    what: &str,
    future: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, HostError> {
    match runtime.block_on(async { tokio::time::timeout(limit, future).await }) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(HostError::Connection(e.to_string())),
        Err(_) => Err(HostError::Connection(format!("{what} did not complete within {}s", limit.as_secs()))),
    }
}

fn runtime() -> Result<Runtime, HostError> {
    tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| HostError::Runtime(e.to_string()))
}

fn cli_for(project: &Path) -> Result<Cli, HostError> {
    let args: [OsString; 4] = ["omuzaki".into(), "--project".into(), project.into(), "mcp".into()];
    cli::try_parse_from(args).map_err(|e| HostError::Open(e.to_string()))
}

/// Opens the runner `omuzaki mcp --project <dir>` would serve, with the ops linked into this binary: the fixture builder
/// and the checker open projects through it as well, so every runner in an eval is built by the product's constructor.
///
/// # Errors
/// [`HostError::Open`] when the folder is not a project this build can open.
pub fn open_project_runner(project: &Path) -> Result<Arc<dyn Runner>, HostError> {
    open_runner(project, &Connections::default())
}

fn open_runner(project: &Path, connections: &Connections) -> Result<Arc<dyn Runner>, HostError> {
    // The ops are linked into the binary that calls this; `link()` keeps the linker from dropping them (ADR 0015).
    omuzaki_cli::link();
    omuzaki_cli::session::open_runner_with(&cli_for(project)?, connections).map_err(|e| HostError::Open(e.to_string()))
}

fn read_document(reader: &Reader) -> Result<Value, HostError> {
    let document_error = |e: &dyn std::fmt::Display| HostError::Document(e.to_string());
    let runner = match reader {
        Reader::Runner(runner) => runner.clone(),
        Reader::Project(dir) => open_project_runner(dir)?,
    };
    let doc = runner.get(&DocPath::root(), &Grants::of([Scope::Read])).map_err(|e| document_error(&e))?;
    Ok(doc.unwrap_or(Value::Null))
}

fn reply_of(answer: &CallToolResult) -> Reply {
    Reply {
        is_error: answer.is_error.unwrap_or(false),
        structured: answer.structured_content.clone(),
        text: answer.content.first().and_then(|c| c.as_text()).map(|t| t.text.clone()).unwrap_or_default(),
    }
}

/// The environment a child may inherit: **only** what a program needs to run (the search path, the home and temporary
/// folders, locale and time zone, Windows' system variables, logging) and the `OMUZAKI_*` settings of the product, never
/// the eval's own (`OMUZAKI_EVAL_*`). Anything else, whatever it is called, stays out: a list of what to hold back would
/// miss the next credential variable. A child that calls a hosted model, or reads the eval's spend caps, is not an eval
/// of the tools.
pub fn scrubbed(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Vec<(OsString, OsString)> {
    const ALLOWED: [&str; 14] = [
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "TMPDIR",
        "TEMP",
        "TMP",
        "TZ",
        "LANG",
        "SYSTEMROOT",
        "USERPROFILE",
        "LOCALAPPDATA",
        "RUST_LOG",
        "RUST_BACKTRACE",
    ];
    vars.into_iter()
        .filter(|(name, _)| {
            let name = name.to_string_lossy().to_ascii_uppercase();
            let product = name.starts_with("OMUZAKI_") && !name.starts_with("OMUZAKI_EVAL_");
            product || name.starts_with("LC_") || ALLOWED.contains(&name.as_str())
        })
        .collect()
}

/// How long a call may take when the attempt has `left` of its wall clock to spend (never zero: a zero timeout would
/// fail every call at once).
#[must_use]
pub fn within(started: Instant, wall: Duration) -> Duration {
    wall.saturating_sub(started.elapsed()).max(Duration::from_millis(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect()
    }

    #[test]
    fn only_what_a_program_needs_and_the_products_settings_reach_a_child() {
        let kept = scrubbed(os(&[
            ("PATH", "/bin"),
            ("HOME", "/home/x"),
            ("LC_ALL", "C"),
            ("ANTHROPIC_API_KEY", "k"),
            ("omuzaki_eval_api_key", "k"),
            ("OMUZAKI_EVAL_OUT", "/x"),
            ("OMUZAKI_ASSETS", "/assets"),
            ("GITHUB_TOKEN", "t"),
            ("DB_PASSWORD", "p"),
            ("MY_SECRET", "s"),
            ("AWS_SECRET_ACCESS_KEY", "s"),
            ("AWS_ACCESS_KEY_ID", "s"),
            ("GOOGLE_APPLICATION_CREDENTIALS", "/c.json"),
            ("SSH_AUTH_SOCK", "/s"),
        ]));
        let names: Vec<String> = kept.iter().map(|(k, _)| k.to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["PATH", "HOME", "LC_ALL", "OMUZAKI_ASSETS"]);
    }

    #[test]
    fn the_time_left_is_never_zero() {
        let started = Instant::now();
        assert!(within(started, Duration::ZERO) >= Duration::from_millis(1));
        assert!(within(started, Duration::from_secs(60)) > Duration::from_secs(59));
    }
}
