//! A bare JSON-RPC client that keeps what it sent and what it got, so a test can show an MCP transcript: each request
//! and each reply or notification, in order, exactly as they cross the wire. The transcripts under
//! `tests/transcripts/` are written by these tests and compared by them, so they are evidence that cannot go stale.
//!
//! Regenerate them with `UPDATE_TRANSCRIPTS=1 cargo test -p omuzaki-mcp --test mcp_transcripts`.

#![allow(clippy::expect_used, clippy::unwrap_used, dead_code)]

use std::fmt::Write as _;
use std::time::Duration;

use omuzaki_mcp::McpServer;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

/// How long a test waits for a message that should arrive.
const WAIT: Duration = Duration::from_secs(10);

/// Instructions longer than this are cut in a transcript: they are the same text in every one.
const INSTRUCTIONS_SHOWN: usize = 160;

enum Direction {
    ToServer,
    ToClient,
}

pub struct Wire {
    reader: BufReader<ReadHalf<DuplexStream>>,
    writer: WriteHalf<DuplexStream>,
    next_id: u64,
    seen: Vec<(Direction, Value)>,
    /// Notifications that arrived while a reply was awaited, in order.
    pub pushed: Vec<Value>,
}

impl Wire {
    /// Connects to `mcp` with the legacy `initialize` handshake as a client called `name` declaring `capabilities`.
    pub async fn open(mcp: &McpServer, name: &str, capabilities: Value) -> Self {
        let (server_io, client_io) = tokio::io::duplex(1 << 20);
        let serving = mcp.clone();
        tokio::spawn(async move {
            let _ = serving.serve(server_io).await;
        });
        Self::over(client_io, name, capabilities).await
    }

    /// Does the handshake as a client called `name` over `client_io`, whose other end a server is already serving.
    pub async fn over(client_io: DuplexStream, name: &str, capabilities: Value) -> Self {
        let (reader, writer) = tokio::io::split(client_io);
        let mut wire =
            Self { reader: BufReader::new(reader), writer, next_id: 1, seen: Vec::new(), pushed: Vec::new() };
        wire.request(
            "initialize",
            json!({"protocolVersion": "2025-11-25", "capabilities": capabilities, "clientInfo": {"name": name, "version": "1.0"}}),
        )
        .await;
        wire.notify("notifications/initialized", json!({})).await;
        wire
    }

    async fn send(&mut self, message: Value) {
        self.seen.push((Direction::ToServer, message.clone()));
        let mut line = message.to_string();
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await.expect("the server reads");
    }

    async fn receive(&mut self) -> Value {
        let mut line = String::new();
        let read = tokio::time::timeout(WAIT, self.reader.read_line(&mut line)).await.expect("a message in time");
        assert!(read.expect("readable") > 0, "the server closed the connection");
        serde_json::from_str(&line).expect("JSON-RPC")
    }

    /// Sends a request and returns its reply (`result` or `error` member's object, whole message). Notifications that
    /// arrive first are kept in [`Wire::pushed`].
    pub async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).await;
        loop {
            let message = self.receive().await;
            if message.get("id") == Some(&json!(id))
                && (message.get("result").is_some() || message.get("error").is_some())
            {
                // A push that raced the reply is shown when the test asks for it (`next_push`), never before the reply,
                // so a transcript does not depend on which of the two the scheduler delivered first.
                self.seen.push((Direction::ToClient, message.clone()));
                return message;
            }
            self.pushed.push(message);
        }
    }

    /// The `result` of a request, which must have succeeded.
    pub async fn result(&mut self, method: &str, params: Value) -> Value {
        let reply = self.request(method, params).await;
        assert!(reply.get("error").is_none(), "{method} failed: {reply}");
        reply["result"].clone()
    }

    /// A `tools/call`, returning the tool result.
    pub async fn call(&mut self, tool: &str, arguments: Value) -> Value {
        self.result("tools/call", json!({"name": tool, "arguments": arguments})).await
    }

    /// A `tools/call` that asks for progress under `token`.
    pub async fn call_with_progress(&mut self, tool: &str, arguments: Value, token: &str) -> Value {
        self.result("tools/call", json!({"name": tool, "arguments": arguments, "_meta": {"progressToken": token}}))
            .await
    }

    pub async fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params})).await;
    }

    /// The next notification the server pushes (one already received first).
    pub async fn next_push(&mut self) -> Value {
        let message = if self.pushed.is_empty() { self.receive().await } else { self.pushed.remove(0) };
        self.seen.push((Direction::ToClient, message.clone()));
        message
    }

    /// The transcript as Markdown: one fenced JSON block per message, `-->` to the server and `<--` from it.
    pub fn transcript(&self, title: &str, about: &str) -> String {
        let mut out =
            format!("# {title}\n\n{about}\n\nEach block is one JSON-RPC message as it crossed an in-memory stream.\n");
        for (direction, message) in &self.seen {
            let arrow = match direction {
                Direction::ToServer => "-->",
                Direction::ToClient => "<--",
            };
            let shown = normalised(message.clone());
            let block = serde_json::to_string_pretty(&shown).unwrap();
            let _ = write!(out, "\n`{arrow}`\n```json\n{block}\n```\n");
        }
        out
    }
}

/// `message` with what changes from run to run or repeats in every transcript replaced.
fn normalised(mut message: Value) -> Value {
    fn walk(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (key, inner) in map.iter_mut() {
                    match (key.as_str(), &*inner) {
                        ("createdAt" | "lastUpdatedAt", Value::String(_)) => *inner = json!("<time>"),
                        ("instructions", Value::String(text)) if text.chars().count() > INSTRUCTIONS_SHOWN => {
                            let cut: String = text.chars().take(INSTRUCTIONS_SHOWN).collect();
                            *inner = json!(format!("{cut} [...the rest of the server guide...]"));
                        }
                        _ => walk(inner),
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(walk),
            _ => {}
        }
    }
    walk(&mut message);
    message
}

/// Compares `text` with the committed transcript `name`, or writes it when `UPDATE_TRANSCRIPTS` is set.
#[allow(clippy::disallowed_methods, reason = "a test reads and writes its committed transcript; the fence is for ops")]
pub fn check_transcript(name: &str, text: &str) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/transcripts").join(format!("{name}.md"));
    if std::env::var_os("UPDATE_TRANSCRIPTS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    if committed.replace("\r\n", "\n") != text {
        // Kept for a diff against the committed one.
        let _ = std::fs::write(std::env::temp_dir().join(format!("{name}.actual.md")), text);
    }
    assert!(
        committed.replace("\r\n", "\n") == text,
        "the transcript {name} changed; rerun with UPDATE_TRANSCRIPTS=1 and review the diff ({})",
        path.display()
    );
}
