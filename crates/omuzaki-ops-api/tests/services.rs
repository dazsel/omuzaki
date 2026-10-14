//! Injected services as an op sees them: `OpCtx::service` finds what the app wired in, fails cleanly with
//! `Unavailable` when it did not, and the dry-run flag reaches the op.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{
    Actor, ErrorCode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Services, ServicesBuilder, Undo, find,
    registrations,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The shape of an API-crate service (`omuzaki-render-api` will define the real one).
trait RenderService: Send + Sync {
    fn render(&self, what: &str) -> String;
    /// Has an effect outside the document, like a render written to the store.
    fn publish(&self);
}

#[derive(Default)]
struct MockRenderer {
    published: AtomicU32,
}

impl RenderService for MockRenderer {
    fn render(&self, what: &str) -> String {
        format!("rendered {what}")
    }

    fn publish(&self) {
        self.published.fetch_add(1, Ordering::SeqCst);
    }
}

/// An op that needs the renderer, and only publishes when it is not a dry-run.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Preview {
    what: String,
}

impl Op for Preview {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.preview",
            summary: "Render something through the injected renderer.",
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
        Ok(Plan::answer(json!(renderer.render(&self.what))))
    }
}
omuzaki_ops_api::register_op!(Preview);

/// Plans `test.preview` in a context that has `services` (or, for `None`, one made by `OpCtx::new` alone).
fn plan_preview(services: Option<&Services>, dry_run: bool) -> Result<Plan, OpError> {
    let op = (find("test.preview").expect("registered").decode)(json!({"what": "face"}))?;
    let (doc, clock) = (MemDoc::new(), FakeClock::new(Seconds(0.0)));
    let actor = Actor::Agent { id: "test".into(), model: None };
    let ctx = OpCtx::new(&doc, &actor, &clock).with_dry_run(dry_run);
    match services {
        Some(services) => op.plan(&ctx.with_services(services)),
        None => op.plan(&ctx),
    }
}

#[test]
fn an_op_that_needs_a_service_runs_with_it() {
    let renderer = Arc::new(MockRenderer::default());
    let services = ServicesBuilder::new().insert::<dyn RenderService>(renderer.clone()).build();
    let plan = plan_preview(Some(&services), false).unwrap();
    assert_eq!(plan.output, Some(json!("rendered face")));
    assert_eq!(renderer.published.load(Ordering::SeqCst), 1);
}

#[test]
fn without_the_service_it_fails_with_unavailable_naming_the_trait() {
    for services in [None, Some(Services::none()), Some(ServicesBuilder::new().build())] {
        let e = plan_preview(services.as_ref(), false).unwrap_err();
        assert_eq!(e.code, ErrorCode::Unavailable, "{e:?}");
        assert!(e.message.contains("RenderService"), "the message names the trait: {e:?}");
        assert!(e.hint.is_some() && e.field.is_none(), "{e:?}");
    }
}

#[test]
fn a_dry_run_sees_the_same_services_and_the_flag_lets_the_op_hold_back_effects() {
    let renderer = Arc::new(MockRenderer::default());
    let services = ServicesBuilder::new().insert::<dyn RenderService>(renderer.clone()).build();
    let plan = plan_preview(Some(&services), true).unwrap();
    assert_eq!(plan.output, Some(json!("rendered face")), "the dry-run planned the same answer");
    assert_eq!(renderer.published.load(Ordering::SeqCst), 0, "and changed nothing through the service");
}

#[test]
fn a_new_context_is_not_a_dry_run_and_has_no_services() {
    let (doc, clock) = (MemDoc::new(), FakeClock::new(Seconds(0.0)));
    let actor = Actor::Agent { id: "test".into(), model: None };
    let ctx = OpCtx::new(&doc, &actor, &clock);
    assert!(!ctx.dry_run);
    assert_eq!(ctx.service::<dyn RenderService>().err().map(|e| e.code), Some(ErrorCode::Unavailable));
    assert!(ctx.with_dry_run(true).dry_run);
}

#[test]
fn contexts_with_different_service_sets_do_not_see_each_other() {
    let one = ServicesBuilder::new().insert::<dyn RenderService>(Arc::new(MockRenderer::default())).build();
    let none = Services::none();
    let (a, b) = (plan_preview(Some(&one), false), plan_preview(Some(&none), false));
    assert!(a.is_ok());
    assert_eq!(b.unwrap_err().code, ErrorCode::Unavailable);
}

#[test]
fn listing_and_describing_ops_needs_no_services() {
    assert!(registrations().any(|r| (r.meta)().name == "test.preview"));
    let schema = serde_json::to_value((find("test.preview").unwrap().schema)()).unwrap();
    assert!(schema["properties"]["what"].is_object(), "schema: {schema}");
}

#[test]
fn unavailable_is_its_own_error_code_on_the_wire() {
    let e = OpError::unavailable("dyn omuzaki_render_api::RenderService");
    assert_eq!(serde_json::to_value(e.code).unwrap(), json!("unavailable"));
    assert_eq!(e.to_string(), "unavailable: the service `dyn omuzaki_render_api::RenderService` is not available");
    assert_ne!(e.code, ErrorCode::Unsupported, "an agent must be able to tell the two apart");
}
