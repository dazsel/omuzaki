//! File access through the executor: the grants an op sees are the request's own, the project folder is the one the
//! executor was given, and a dry-run and the real run get the same answer.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    ErrorCode, FsMode, Grants, Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, ProjectDir, ProjectDirError, Request,
    Runner, Scope, Undo, register_op,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tempfile::TempDir;

use super::{agent, person, request};

/// Asks for read access to `path` and answers with where it really is.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Open {
    path: String,
}

impl Op for Open {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.open",
            summary: "Resolve a path for reading.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let path = ctx.fs_access(&self.path, FsMode::Read)?;
        Ok(Plan::answer(json!(path.to_string())))
    }
}
register_op!(Open);

/// Asks for write access to `path`, as an edit that changes nothing in the document.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Save {
    path: String,
}

impl Op for Save {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.save",
            summary: "Resolve a path for writing.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let path = ctx.fs_access(&self.path, FsMode::Write)?;
        Ok(Plan { output: Some(json!(path.to_string())), ..Plan::default() })
    }
}
register_op!(Save);

/// A project folder with one file, next to a folder that is not part of it.
struct Site {
    _dir: TempDir,
    project: PathBuf,
    outside: PathBuf,
}

fn site() -> Site {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    // Canonical, so the paths the tests expect are the paths `fs_access` returns.
    let (project, outside) = (base.join("project"), base.join("outside"));
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(project.join("model.vrm"), "m").unwrap();
    fs::write(outside.join("secret.txt"), "s").unwrap();
    Site { _dir: dir, project, outside }
}

fn executor(project: Option<&Path>) -> Executor {
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let exec = Executor::new(Box::new(MemDoc::from_value(json!({}))), clock).unwrap();
    match project {
        Some(dir) => exec.with_project_dir(ProjectDir::open(dir).unwrap()),
        None => exec,
    }
}

fn text(path: &Path) -> String {
    path.to_str().unwrap().to_owned()
}

fn open(exec: &Executor, who: Request, path: &str) -> Result<String, OpError> {
    let outcome = exec.run(Request { op: "test.open".into(), args: json!({"path": path}), ..who })?;
    Ok(outcome.plan.output.unwrap().as_str().unwrap().to_owned())
}

#[test]
fn ops_see_the_grants_of_the_request_that_runs_them() {
    let site = site();
    let exec = executor(Some(&site.project));
    let secret = text(&site.outside.join("secret.txt"));

    // The same executor, three callers: each is held to its own grants.
    let as_agent = Request { actor: agent(), grants: Grants::agent_default(), ..request("test.open", json!({})) };
    let person_with_root = Request {
        actor: person(),
        grants: Grants::person().with_fs_root(text(&site.outside)),
        ..request("test.open", json!({}))
    };
    let person_without_root = request("test.open", json!({}));

    assert_eq!(open(&exec, as_agent.clone(), "model.vrm").unwrap(), text(&site.project.join("model.vrm")));
    assert_eq!(open(&exec, as_agent.clone(), &secret).unwrap_err().code, ErrorCode::Denied);
    assert_eq!(open(&exec, person_with_root, &secret).unwrap(), secret);
    assert_eq!(open(&exec, person_without_root, &secret).unwrap_err().code, ErrorCode::Denied);
    // The grants of an earlier request do not linger.
    assert_eq!(open(&exec, as_agent, &secret).unwrap_err().code, ErrorCode::Denied);
}

#[test]
fn a_denial_names_the_path_and_changes_nothing() {
    let site = site();
    let exec = executor(Some(&site.project));
    let secret = text(&site.outside.join("secret.txt"));
    let e = open(
        &exec,
        Request { actor: agent(), grants: Grants::agent_default(), ..request("test.open", json!({})) },
        &secret,
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);
    assert!(e.message.contains(&secret), "{e:?}");
    assert!(e.hint.unwrap().contains("`fs`"));
    assert_eq!(exec.log(0, usize::MAX, &Grants::person()).unwrap().len(), 0, "a refused op is not logged");
}

#[test]
fn a_dry_run_and_the_real_run_get_the_same_answer() {
    let site = site();
    let exec = executor(Some(&site.project));
    let inside = text(&site.project.join("out.glb"));
    let outside = text(&site.outside.join("out.glb"));
    let granted = Grants::agent_default().with_fs_root(text(&site.outside));
    let plain = Grants::agent_default();

    for (grants, path, allowed) in
        [(&granted, &outside, true), (&granted, &inside, true), (&plain, &outside, false), (&plain, &inside, false)]
    {
        let run = |mode| {
            let req = Request {
                op: "test.save".into(),
                args: json!({"path": path}),
                mode,
                actor: agent(),
                grants: grants.clone(),
                ..request("", json!(null))
            };
            exec.run(req).map(|outcome| outcome.plan.output)
        };
        let (dry, applied) = (run(Mode::DryRun), run(Mode::Apply));
        assert_eq!(dry, applied, "{path}");
        assert_eq!(dry.is_ok(), allowed, "{path}: {dry:?}");
    }
}

#[test]
fn without_a_project_folder_nothing_is_implicitly_allowed() {
    let site = site();
    let exec = executor(None);
    let as_agent = Request { actor: agent(), grants: Grants::agent_default(), ..request("test.open", json!({})) };

    let relative = open(&exec, as_agent.clone(), "model.vrm").unwrap_err();
    assert_eq!(relative.code, ErrorCode::Invalid, "a relative path has nothing to be relative to");
    let inside = text(&site.project.join("model.vrm"));
    assert_eq!(open(&exec, as_agent, &inside).unwrap_err().code, ErrorCode::Denied, "read alone covers no folder");

    // Granted roots still work, so an app that has no project (a headless render, say) can grant what it needs.
    let granted = Request {
        grants: Grants::agent_default().with_fs_root(text(&site.project)),
        ..request("test.open", json!({}))
    };
    assert_eq!(open(&exec, granted, &inside).unwrap(), inside);
}

#[test]
fn each_executor_has_its_own_project_folder() {
    let (first, second) = (site(), site());
    let (a, b) = (executor(Some(&first.project)), executor(Some(&second.project)));
    let as_agent = Request { actor: agent(), grants: Grants::agent_default(), ..request("test.open", json!({})) };
    assert_eq!(open(&a, as_agent.clone(), "model.vrm").unwrap(), text(&first.project.join("model.vrm")));
    assert_eq!(open(&b, as_agent.clone(), "model.vrm").unwrap(), text(&second.project.join("model.vrm")));
    assert_eq!(open(&a, as_agent, &text(&second.project.join("model.vrm"))).unwrap_err().code, ErrorCode::Denied);
}

#[test]
fn a_relative_project_folder_is_pinned_to_where_it_pointed_when_given() {
    // `cargo test` runs from the crate's folder, so "." is a real folder with a real file in it.
    let exec = executor(Some(Path::new(".")));
    let as_agent = Request { actor: agent(), grants: Grants::agent_default(), ..request("test.open", json!({})) };
    let found = open(&exec, as_agent, "Cargo.toml").unwrap();
    let expected = fs::canonicalize("Cargo.toml").unwrap();
    assert_eq!(found, text(&expected));
}

#[test]
fn a_filesystem_root_cannot_be_the_project_folder() {
    // The executor only takes a validated `ProjectDir`, so `read` can never mean "everything".
    for root in [Path::new("/"), Path::new("/."), Path::new("/tmp/..")] {
        if cfg!(unix) {
            assert!(matches!(ProjectDir::open(root), Err(ProjectDirError::FilesystemRoot(_))), "{root:?}");
        }
    }
}
