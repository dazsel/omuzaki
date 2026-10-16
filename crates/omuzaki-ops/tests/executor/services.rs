//! Injected services: an op that needs one runs with it and fails with `Unavailable` without it, executors with
//! different service sets don't leak into each other, and a dry-run holds back effects through a service.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    ErrorCode, Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Request, Runner, Scope, Services, ServicesBuilder, Undo,
    register_op,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{open_agents, request, rig};

/// The shape of an API-crate service (the real renderer trait will live in `omuzaki-render-api`).
trait RenderService: Send + Sync {
    /// Names the renderer, so tests can tell two apart.
    fn name(&self) -> &'static str;
    /// An effect outside the document, such as writing a render to the store.
    fn publish(&self);
}

/// A mock renderer that counts its effects.
struct MockRenderer {
    name: &'static str,
    published: AtomicU32,
}

impl MockRenderer {
    fn new(name: &'static str) -> Arc<Self> {
        Arc::new(Self { name, published: AtomicU32::new(0) })
    }

    fn published(&self) -> u32 {
        self.published.load(Ordering::SeqCst)
    }
}

impl RenderService for MockRenderer {
    fn name(&self) -> &'static str {
        self.name
    }

    fn publish(&self) {
        self.published.fetch_add(1, Ordering::SeqCst);
    }
}

/// Asks the renderer who it is, and publishes unless this is a dry-run. Needs a service.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Render {}

impl Op for Render {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.render",
            summary: "Render through the injected renderer.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let renderer = ctx.service::<dyn RenderService>()?;
        if !ctx.dry_run {
            renderer.publish();
        }
        Ok(Plan::answer(json!({"renderer": renderer.name(), "dry_run": ctx.dry_run})))
    }
}
register_op!(Render);

fn executor(services: Services) -> Executor {
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let exec = Executor::new(Box::new(MemDoc::from_value(json!({}))), clock).unwrap().with_services(services);
    open_agents(&exec);
    exec
}

fn renderer_services(renderer: &Arc<MockRenderer>) -> Services {
    ServicesBuilder::new().insert::<dyn RenderService>(renderer.clone()).build()
}

fn render(exec: &Executor, mode: Mode) -> Result<serde_json::Value, OpError> {
    let outcome = exec.run(Request { mode, ..request("test.render", json!({})) })?;
    Ok(outcome.plan.output.unwrap())
}

#[test]
fn an_op_that_needs_a_service_runs_with_it() {
    let renderer = MockRenderer::new("cpu");
    let exec = executor(renderer_services(&renderer));
    assert_eq!(render(&exec, Mode::Apply).unwrap(), json!({"renderer": "cpu", "dry_run": false}));
    assert_eq!(renderer.published(), 1);
}

#[test]
fn without_the_service_the_op_fails_with_unavailable_and_nothing_changes() {
    let rig = rig(json!({}));
    let e = rig.run("test.render", json!({})).unwrap_err();
    assert_eq!(e.code, ErrorCode::Unavailable, "{e:?}");
    assert!(e.message.contains("RenderService"), "the message names the trait: {e:?}");
    assert!(e.hint.is_some());
    assert_eq!(rig.log().len(), 0, "a failed op is not logged");

    // The same for an executor that was given other services, just not this one.
    let other = executor(ServicesBuilder::new().build());
    assert_eq!(render(&other, Mode::DryRun).unwrap_err().code, ErrorCode::Unavailable);
}

#[test]
fn a_dry_run_sees_the_same_services_and_the_op_holds_back_its_effects() {
    let renderer = MockRenderer::new("cpu");
    let exec = executor(renderer_services(&renderer));
    assert_eq!(render(&exec, Mode::DryRun).unwrap(), json!({"renderer": "cpu", "dry_run": true}));
    assert_eq!(renderer.published(), 0, "a dry-run changes nothing through the service");
    render(&exec, Mode::Apply).unwrap();
    assert_eq!(renderer.published(), 1);
}

#[test]
fn executors_with_different_service_sets_do_not_leak_into_each_other() {
    let (gpu, cpu) = (MockRenderer::new("gpu"), MockRenderer::new("cpu"));
    let (first, second, bare) =
        (executor(renderer_services(&gpu)), executor(renderer_services(&cpu)), executor(Services::none()));
    for _ in 0..2 {
        assert_eq!(render(&first, Mode::Apply).unwrap()["renderer"], "gpu");
        assert_eq!(render(&second, Mode::Apply).unwrap()["renderer"], "cpu");
        assert_eq!(render(&bare, Mode::Apply).unwrap_err().code, ErrorCode::Unavailable);
    }
    assert_eq!((gpu.published(), cpu.published()), (2, 2));
}

#[test]
fn listing_ops_and_ops_without_a_service_work_whatever_the_service_set() {
    let rig = rig(json!({}));
    let ops = rig.exec.ops();
    let render = ops.iter().find(|o| o.name == "test.render").expect("listed without any service");
    assert_eq!(render.summary, "Render through the injected renderer.");
    rig.set("a", json!(1));
    assert_eq!(rig.at("a"), Some(json!(1)));

    let exec = executor(renderer_services(&MockRenderer::new("cpu")));
    exec.run(request("test.set", json!({"path": "a", "value": 1}))).unwrap();
    assert_eq!(exec.get(&super::path("a"), &omuzaki_ops_api::Grants::person()).unwrap(), Some(json!(1)));
}
