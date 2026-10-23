//! A running `omuzaki mcp` for the end-to-end tests, spoken to over its standard input and output.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Longest any answer or child process may take. Generous: a loaded CI machine is slow, a hang is infinite.
pub const LIMIT: Duration = Duration::from_secs(60);

/// Reads `stream` to its end on a thread of its own.
pub fn read_to_string_on_thread(mut stream: impl Read + Send + 'static) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut text = String::new();
        let _ = stream.read_to_string(&mut text);
        text
    })
}

/// Waits for `child` to exit; kills it and fails the test if it takes longer than `limit`.
pub fn wait_within(child: &mut Child, limit: Duration, what: &str) -> std::process::ExitStatus {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait().expect("poll the child") {
            return status;
        }
        assert!(Instant::now() < deadline, "{what} didn't finish within {limit:?}");
        thread::sleep(Duration::from_millis(10));
    }
}

/// A running `omuzaki mcp`, spoken to in newline-delimited JSON-RPC. Killed when dropped, so no test leaves a server
/// behind.
pub struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stderr: Option<thread::JoinHandle<String>>,
    next_id: u64,
}

impl Mcp {
    /// `omuzaki mcp` listing every op as a tool, which these tests look for by name.
    pub fn start(bin: &str, project: &Path) -> Self {
        Self::start_with_env(bin, project, &[])
    }

    pub fn start_with_env(bin: &str, project: &Path, env: &[(&str, &str)]) -> Self {
        Self::start_with(bin, project, env, &["--tools", "all"])
    }

    pub fn start_with(bin: &str, project: &Path, env: &[(&str, &str)], args: &[&str]) -> Self {
        let mut child = Command::new(bin)
            .envs(env.iter().copied())
            .arg("--project")
            .arg(project)
            .arg("mcp")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn omuzaki mcp");
        let stdin = child.stdin.take();
        let stderr = child.stderr.take().map(read_to_string_on_thread);
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self { child, stdin, lines, stderr, next_id: 1 }
    }

    pub fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().expect("stdin is open");
        writeln!(stdin, "{message}").expect("write to the server");
        stdin.flush().expect("flush");
    }

    /// The next line of standard output, which must be a JSON-RPC message: the server prints nothing else there.
    pub fn next_message(&mut self) -> Value {
        let line = self.lines.recv_timeout(LIMIT).expect("the server answers in time");
        let message: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("stdout line {line:?}: {e}"));
        assert_eq!(message["jsonrpc"], "2.0", "not a JSON-RPC message: {line}");
        message
    }

    /// Sends a request and returns its response, skipping any notifications that arrive first.
    pub fn request(&mut self, method: &str, params: &Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let message = self.next_message();
            if message["id"] == json!(id) {
                assert!(message.get("error").is_none(), "{method} failed: {message}");
                return message["result"].clone();
            }
        }
    }

    pub fn handshake(&mut self) -> Value {
        let client = json!({"name": "e2e", "version": "0"});
        let info = self
            .request("initialize", &json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": client}));
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        info
    }

    pub fn tool_names(&mut self) -> Vec<String> {
        let listed = self.request("tools/list", &json!({}));
        let mut names: Vec<String> =
            listed["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_owned()).collect();
        names.sort();
        names
    }

    /// Calls a tool; the call must succeed. Returns its structured result.
    pub fn call(&mut self, tool: &str, arguments: &Value) -> Value {
        let result = self.request("tools/call", &json!({"name": tool, "arguments": arguments}));
        assert_ne!(result["isError"], json!(true), "{tool} failed: {result}");
        result["structuredContent"].clone()
    }

    /// Calls a tool and returns its whole result, a refusal included.
    pub fn result(&mut self, tool: &str, arguments: &Value) -> Value {
        self.request("tools/call", &json!({"name": tool, "arguments": arguments}))
    }

    /// The text blocks of a tool's result, joined.
    pub fn text(result: &Value) -> String {
        let blocks = result["content"].as_array().cloned().unwrap_or_default();
        blocks.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n")
    }

    /// Sends a tool call without waiting for its answer; [`answer`](Self::answer) reads it. Returns its request id.
    pub fn call_later(&mut self, tool: &str, arguments: &Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let params = json!({"name": tool, "arguments": arguments});
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": params}));
        id
    }

    /// The result of the request `id` sent earlier, skipping anything else that arrives first.
    pub fn answer(&mut self, id: u64) -> Value {
        loop {
            let message = self.next_message();
            if message["id"] == json!(id) {
                assert!(message.get("error").is_none(), "request {id} failed: {message}");
                return message["result"].clone();
            }
        }
    }

    /// Closes the server's stdin, as a client does when it is done, and returns the exit code and what the server
    /// wrote to stderr. Everything it still prints on stdout must be protocol.
    pub fn close(mut self) -> (Option<i32>, String) {
        drop(self.stdin.take());
        let code = wait_within(&mut self.child, LIMIT, "omuzaki mcp after its stdin closed").code();
        // The reader thread ends with the process, which disconnects the channel.
        while let Ok(line) = self.lines.recv_timeout(LIMIT) {
            let message: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("stdout line {line:?}: {e}"));
            assert_eq!(message["jsonrpc"], "2.0", "not a JSON-RPC message: {line}");
        }
        let stderr = self.stderr.take().map(|handle| handle.join().expect("stderr reader")).unwrap_or_default();
        (code, stderr)
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
