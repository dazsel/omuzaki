//! Headless `omuzaki mcp` (design `agent-sessions.md` 7.6): with no studio there is nobody to ask, so what the agent
//! may do and spend is set in advance, by `--grant` and `--spend-cap`, or by a credential the person issued with
//! `omuzaki credential issue`, whose secret the agent's process gets from `OMUZAKI_MCP_TOKEN` or `--token-file`.
//! Through the built fixture binary, which adds a demo op that needs `publish` and one that costs 40 cents.

// `CARGO_BIN_EXE_omuzaki-fixture` exists only when the fixture is built (`test-fixtures` feature, see Cargo.toml).
#![cfg(feature = "test-fixtures")]
// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "tests write and read their fixtures on disk; the fence is for ops")]

mod common;

use std::path::Path;
use std::process::{Command, Stdio};

use common::mcp::{LIMIT, Mcp, read_to_string_on_thread, wait_within};
use common::new_project;
use serde_json::{Value, json};

const FIXTURE: &str = env!("CARGO_BIN_EXE_omuzaki-fixture");

/// Runs `omuzaki-fixture --project <project> <args>` to the end with `env` set; returns its code, stdout and stderr.
fn run(project: &Path, args: &[&str], env: &[(&str, &str)]) -> (Option<i32>, String, String) {
    let mut child = Command::new(FIXTURE)
        .env_remove("OMUZAKI_MCP_TOKEN")
        .envs(env.iter().copied())
        .arg("--project")
        .arg(project)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the fixture");
    let readers =
        [child.stdout.take().map(read_to_string_on_thread), child.stderr.take().map(read_to_string_on_thread)];
    let code = wait_within(&mut child, LIMIT, &format!("{args:?}")).code();
    let [stdout, stderr] = readers.map(|r| r.map(|h| h.join().expect("reader")).unwrap_or_default());
    (code, stdout, stderr)
}

/// Issues a credential with `args` and returns its secret, as `--json` prints it.
fn issue(project: &Path, args: &[&str]) -> String {
    let (code, stdout, stderr) = run(project, &[&["--json", "credential", "issue"], args].concat(), &[]);
    assert_eq!(code, Some(0), "{stderr}");
    let issued: Value = serde_json::from_str(&stdout).expect("JSON");
    issued["secret"].as_str().expect("the secret is printed once").to_owned()
}

/// The text of a refused tool call, or the call's structured result when it was not refused.
fn outcome(mcp: &mut Mcp, tool: &str, args: &Value) -> Result<Value, String> {
    let result = mcp.result(tool, args);
    if result["isError"] == json!(true) { Err(Mcp::text(&result)) } else { Ok(result["structuredContent"].clone()) }
}

fn publish(mcp: &mut Mcp) -> Result<Value, String> {
    outcome(mcp, "demo_publish_note", &json!({}))
}

fn paid(mcp: &mut Mcp, text: &str) -> Result<Value, String> {
    outcome(mcp, "demo_paid_note", &json!({ "text": text }))
}

#[test]
fn with_nothing_set_in_advance_publishing_and_spending_are_refused_with_how_to_allow_them() {
    let project = new_project();
    let mut mcp = Mcp::start_with(FIXTURE, project.path(), &[], &["--tools", "all"]);
    mcp.handshake();
    let refused = publish(&mut mcp).expect_err("nobody can approve publish");
    assert!(refused.contains("publish"), "{refused}");
    assert!(refused.contains("credential") || refused.contains("--grant"), "the refusal says how: {refused}");
    let refused = paid(&mut mcp, "paid").expect_err("nobody granted spend");
    assert!(refused.contains("--spend-cap"), "spending is allowed with a cap: {refused}");
    assert_eq!(mcp.close().0, Some(0));
}

#[test]
fn grant_and_spend_cap_say_yes_in_advance_up_to_the_cap() {
    let project = new_project();
    let mut mcp =
        Mcp::start_with(FIXTURE, project.path(), &[], &["--tools", "all", "--grant", "publish", "--spend-cap", "0.50"]);
    mcp.handshake();
    publish(&mut mcp).expect("--grant publish lets it publish");
    paid(&mut mcp, "first").expect("40 cents fit in 50");
    let refused = paid(&mut mcp, "second").expect_err("80 cents don't");
    assert!(refused.contains("budget") && refused.contains("higher `--spend-cap`"), "{refused}");
    let me = mcp.call("session_info", &json!({}));
    assert!(me.to_string().contains("500000"), "the cap is the session's budget: {me}");
    assert_eq!(mcp.close().0, Some(0));
}

#[test]
fn a_credential_issued_in_advance_carries_its_grants_and_budget_and_stops_working_once_revoked() {
    let project = new_project();
    let secret = issue(project.path(), &["--label", "nightly", "--grant", "publish", "--spend-cap", "0.5"]);
    let stored = std::fs::read_to_string(project.path().join(".omuzaki").join("credentials.json")).unwrap();
    assert!(!stored.contains(&secret), "the project keeps a hash, never the secret");
    let (code, listed, _) = run(project.path(), &["credential", "list"], &[]);
    assert_eq!(code, Some(0));
    assert!(listed.contains("nightly") && listed.contains("publish") && listed.contains("$0.50"), "{listed}");
    assert!(!listed.contains(&secret), "the secret is shown once only");

    // From the environment: the credential's grants and budget, nothing typed.
    let env = [("OMUZAKI_MCP_TOKEN", secret.as_str())];
    let mut mcp = Mcp::start_with(FIXTURE, project.path(), &env, &["--tools", "all"]);
    mcp.handshake();
    publish(&mut mcp).expect("the credential grants publish");
    paid(&mut mcp, "first").expect("its budget pays 40 cents");
    assert!(paid(&mut mcp, "second").is_err(), "and no more");
    let (code, stderr) = mcp.close();
    assert_eq!(code, Some(0));
    assert!(
        stderr.contains("nightly") && !stderr.contains(&secret),
        "it names the credential, never the secret: {stderr}"
    );

    // From a file, the same.
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), format!("{secret}\n")).unwrap();
    let file_arg = file.path().to_str().unwrap();
    let mut mcp = Mcp::start_with(FIXTURE, project.path(), &[], &["--tools", "all", "--token-file", file_arg]);
    mcp.handshake();
    publish(&mut mcp).expect("the file's credential grants publish");
    assert_eq!(mcp.close().0, Some(0));

    // A credential decides on its own: typed grants beside it are refused rather than mixed in.
    let (code, _, stderr) = run(project.path(), &["mcp", "--grant", "broadcast"], &env);
    assert_ne!(code, Some(0));
    assert!(stderr.contains("its own grants"), "{stderr}");

    // Revoked, or never issued: refused before the agent gets anywhere.
    let (code, _, stderr) = run(project.path(), &["credential", "revoke", "nightly"], &[]);
    assert_eq!(code, Some(0), "{stderr}");
    for secret in [secret.as_str(), "omuzaki_mcp_not_one_of_ours"] {
        let (code, stdout, stderr) = run(project.path(), &["mcp"], &[("OMUZAKI_MCP_TOKEN", secret)]);
        assert_ne!(code, Some(0), "{stdout}");
        assert!(stderr.contains("isn't one the project issued"), "{stderr}");
        assert!(!stderr.contains(secret), "never echoed: {stderr}");
    }
    let (code, _, stderr) = run(project.path(), &["credential", "revoke", "nightly"], &[]);
    assert_ne!(code, Some(0), "only once");
    assert!(stderr.contains("no credential is labelled"), "{stderr}");
}
