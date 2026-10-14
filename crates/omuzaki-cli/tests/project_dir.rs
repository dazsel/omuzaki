//! The project folder reaches the ops (`--project`): a project-relative file is readable with the default
//! agent grants, a path outside the project is denied naming `fs`, and a folder that cannot be a project is refused.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]

mod common;

use common::new_project;
use omuzaki_cli::cli::try_parse_from;
use omuzaki_cli::session::open_runner;
use omuzaki_ops_api::{
    Actor, ErrorCode, FsMode, Grants, Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Request, Runner, Scope, Undo,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Asks the op context for read access to a path and reports the path it resolved.
#[derive(Serialize, Deserialize, JsonSchema)]
struct ProbeRead {
    /// The path to read, relative to the project or absolute.
    path: String,
}

impl Op for ProbeRead {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.probe_read",
            summary: "Check read access to a path.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let resolved = ctx.fs_access(&self.path, FsMode::Read)?;
        Ok(Plan { output: Some(json!(resolved.to_string())), ..Plan::default() })
    }
}

omuzaki_ops_api::register_op!(ProbeRead);

fn runner(project: &str) -> Result<std::sync::Arc<dyn Runner>, OpError> {
    let cli = try_parse_from(["omuzaki", "--project", project, "ops"]).expect("valid command line");
    open_runner(&cli)
}

fn probe(runner: &dyn Runner, path: &str) -> Result<serde_json::Value, OpError> {
    let request = Request {
        op: "test.probe_read".to_owned(),
        args: json!({ "path": path }),
        mode: Mode::DryRun,
        actor: Actor::Agent { id: "probe".to_owned(), model: None },
        grants: Grants::agent_default(),
        cause: None,
        tx: None,
        base: None,
    };
    runner.run(request).map(|outcome| outcome.plan.output.expect("the probe answers"))
}

#[test]
fn a_project_relative_file_is_readable_with_the_default_agent_grants() {
    let project = new_project();
    let runner = runner(project.path().to_str().unwrap()).unwrap();
    let answer = probe(runner.as_ref(), "project.toml").unwrap();
    assert!(answer.as_str().unwrap().ends_with("project.toml"), "{answer}");
}

#[test]
fn a_path_outside_the_project_is_denied_naming_fs() {
    let project = new_project();
    let outside = tempfile::tempdir().unwrap();
    let file = outside.path().join("secret.txt");
    std::fs::write(&file, "x").unwrap();
    let runner = runner(project.path().to_str().unwrap()).unwrap();
    let error = probe(runner.as_ref(), file.to_str().unwrap()).unwrap_err();
    assert_eq!(error.code, ErrorCode::Denied, "{error:?}");
    assert!(error.message.contains("fs"), "{error:?}");
}

#[test]
fn a_filesystem_root_is_not_a_project() {
    let error = runner("/").err().expect("refused");
    assert_eq!(error.code, ErrorCode::Invalid);
    assert!(error.message.contains("filesystem root"), "{error:?}");
}
