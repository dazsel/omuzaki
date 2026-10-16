//! What an agent reads never changes with the person's language (design `l10n-ops.md` §6, ADR 0023): `omuzaki mcp`
//! answers `initialize` (the server guide included), `tools/list`, an error result and `prompts/list` with the same bytes
//! under any locale setting, and `omuzaki run ... --json` fails with the same bytes too. The server's text is English.
//!
//! The settings tried are the ones a process can see today: `LANG`, `LC_ALL`/`LANGUAGE` and `OMUZAKI_LANG`. When the
//! `--lang` flag lands (#270) it joins the list.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

use common::new_project;
use serde_json::{Value, json};

const OMUZAKI: &str = env!("CARGO_BIN_EXE_omuzaki");

/// Every way the locale reaches a process, and a few locales to set it to.
fn settings() -> Vec<Vec<(&'static str, &'static str)>> {
    vec![
        vec![("LANG", "en_US.UTF-8")],
        vec![("LANG", "ja_JP.UTF-8")],
        vec![("LC_ALL", "ja_JP.UTF-8"), ("LANGUAGE", "ja")],
        vec![("LANG", "ko_KR.UTF-8"), ("OMUZAKI_LANG", "en-XA")],
    ]
}

/// A command with every locale variable cleared, then `env` set.
fn command(env: &[(&str, &str)]) -> Command {
    let mut command = Command::new(OMUZAKI);
    for name in ["LANG", "LANGUAGE", "LC_ALL", "LC_MESSAGES", "OMUZAKI_LANG"] {
        command.env_remove(name);
    }
    command.envs(env.iter().copied());
    command
}

/// Runs `command` with `stdin` written and closed, and returns (exit code, stdout, stderr).
fn run(mut command: Command, stdin: &str) -> (Option<i32>, String, String) {
    let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("spawn");
    let mut input = child.stdin.take().expect("piped stdin");
    let text = stdin.to_owned();
    // On its own thread: the server may answer before it has read everything.
    let writer = thread::spawn(move || {
        let _ = input.write_all(text.as_bytes());
    });
    let read = |mut stream: Box<dyn Read + Send>| {
        thread::spawn(move || {
            let mut out = String::new();
            let _ = stream.read_to_string(&mut out);
            out
        })
    };
    let stdout = read(Box::new(child.stdout.take().expect("piped stdout")));
    let stderr = read(Box::new(child.stderr.take().expect("piped stderr")));
    let status = child.wait().expect("exits");
    writer.join().expect("writer");
    (status.code(), stdout.join().expect("stdout"), stderr.join().expect("stderr"))
}

/// The conversation: a handshake, the tool list, two failing calls, and the prompt list.
fn conversation() -> String {
    let messages = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "locale-test", "version": "0"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "no_such_tool", "arguments": {}}}),
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {
            "name": "character_create", "arguments": {"misspelt_argument": 1}}}),
        json!({"jsonrpc": "2.0", "id": 5, "method": "prompts/list", "params": {}}),
    ];
    messages.iter().map(|m| format!("{m}\n")).collect()
}

fn mcp_output(project: &Path, env: &[(&str, &str)]) -> String {
    let mut mcp = command(env);
    mcp.arg("--project").arg(project).arg("mcp");
    let (code, stdout, stderr) = run(mcp, &conversation());
    assert_eq!(code, Some(0), "{stderr}");
    stdout
}

#[test]
fn the_mcp_surface_is_byte_identical_under_every_locale_and_in_english() {
    let project = new_project();
    let outputs: Vec<(Vec<(&str, &str)>, String)> =
        settings().into_iter().map(|env| (env.clone(), mcp_output(project.path(), &env))).collect();
    let (first_env, first) = &outputs[0];
    for (env, output) in &outputs {
        assert_eq!(output, first, "{env:?} differs from {first_env:?}");
    }

    // What was compared is the whole surface: the guide, the tools, a refused call and the prompt list.
    let replies: Vec<Value> = first.lines().map(|l| serde_json::from_str(l).expect("JSON-RPC")).collect();
    let by_id = |id: u64| replies.iter().find(|r| r["id"] == json!(id)).unwrap_or_else(|| panic!("no reply {id}"));
    let guide = by_id(1)["result"]["instructions"].as_str().expect("a guide");
    assert!(guide.contains("Everything this server says is in English"), "{guide}");
    assert!(by_id(2)["result"]["tools"].as_array().is_some_and(|tools| !tools.is_empty()));
    for id in [3, 4] {
        let reply = by_id(id);
        assert_eq!(reply["result"]["isError"], json!(true), "{reply}");
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.is_ascii(), "an English error: {text}");
    }
    assert!(by_id(5)["result"]["prompts"].is_array());
}

#[test]
fn a_failing_run_prints_the_same_json_under_every_locale() {
    let project = new_project();
    let outputs: Vec<_> = settings()
        .into_iter()
        .map(|env| {
            let mut run_op = command(&env);
            run_op.arg("--project").arg(project.path()).args(["run", "no.such_op", "--json"]);
            (env, run(run_op, ""))
        })
        .collect();
    let (_, first) = &outputs[0];
    assert_ne!(first.0, Some(0), "the run fails: {first:?}");
    for (env, output) in &outputs {
        assert_eq!(output, first, "{env:?}");
    }
}
