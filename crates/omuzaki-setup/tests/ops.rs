//! The `setup.*` ops over the settings mock (#988): status for a person and an agent, an agent's skip and finish wait
//! for a person and its restart is refused, a person's calls change the progress and are undone by `setup.restore`, dry
//! runs change nothing, and the session store the flow uses reads and writes the same progress.

use std::sync::Arc;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, ErrorCode, ErrorDetail, OpCall, OpCtx, OpError, Plan, Services, ServicesBuilder, find};
use omuzaki_settings_api::service::Settings;
use omuzaki_settings_api::session::SetupProgress;
use omuzaki_settings_mock::MemorySettings;
use omuzaki_setup::{Progress, SessionSetupStore, SetupStore, Step};
use serde_json::{Value, json};

struct Rig {
    store: Arc<MemorySettings>,
    services: Services,
}

fn rig() -> Rig {
    let store = Arc::new(MemorySettings::default());
    let services = ServicesBuilder::new().insert::<dyn Settings>(Arc::clone(&store) as Arc<dyn Settings>).build();
    Rig { store, services }
}

fn person() -> Actor {
    Actor::Person { name: "kit".into() }
}

fn agent() -> Actor {
    Actor::Agent { id: "designer".into(), model: None }
}

fn plan(services: &Services, actor: &Actor, name: &str, args: Value, dry_run: bool) -> Result<Plan, OpError> {
    let op = (find(name).unwrap_or_else(|| panic!("{name} is registered")).decode)(args)?;
    let (doc, clock) = (MemDoc::new(), FakeClock::new(Seconds(0.0)));
    let ctx = OpCtx::new(&doc, actor, &clock).with_services(services).with_dry_run(dry_run);
    op.plan(&ctx)
}

fn run(rig: &Rig, actor: &Actor, name: &str, args: Value) -> Result<Value, OpError> {
    plan(&rig.services, actor, name, args, false).map(|p| p.output.unwrap_or(Value::Null))
}

fn detail(error: &OpError) -> Option<&ErrorDetail> {
    error.detail.as_deref()
}

fn setup_status(rig: &Rig) -> Value {
    run(rig, &agent(), "setup.status", json!({})).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn setup_status_is_readable_by_both_and_reports_a_first_run_with_the_budgets() {
    let rig = rig();
    let status = setup_status(&rig);
    assert_eq!(status, run(&rig, &person(), "setup.status", json!({})).unwrap_or_else(|e| panic!("{e}")));
    assert_eq!(status["step"], "welcome");
    assert_eq!(status["needed"], json!(true), "an empty session is a first run");
    assert_eq!(status["decided"], json!(false));
    assert_eq!(status["budget_s_by_step"]["calibrate"], json!(45));
    assert_eq!(status["total_budget_s"], json!(120));
}

#[test]
fn an_agents_skip_and_finish_wait_for_a_person_and_its_restart_is_refused() {
    let rig = rig();
    for op in ["setup.skip", "setup.finish"] {
        let error = run(&rig, &agent(), op, json!({})).err().unwrap_or_else(|| panic!("{op} waits"));
        assert!(matches!(detail(&error), Some(ErrorDetail::NeedsPerson { .. })), "{op}: {error:?}");
    }
    let restart = run(&rig, &agent(), "setup.restart", json!({}));
    assert_eq!(restart.err().map(|e| e.code), Some(ErrorCode::Denied));
    assert_eq!(rig.store.session().state().setup, None, "nothing changed");
}

#[test]
fn a_persons_skip_ends_setup_keeping_the_step_and_its_undo_puts_never_decided_back() {
    let rig = rig();
    let dry = plan(&rig.services, &person(), "setup.skip", json!({}), true).unwrap_or_else(|e| panic!("{e}"));
    assert!(dry.effect.is_some());
    assert_eq!(rig.store.session().state().setup, None, "a dry run writes nothing");

    let done = plan(&rig.services, &person(), "setup.skip", json!({}), false).unwrap_or_else(|e| panic!("{e}"));
    let status = setup_status(&rig);
    assert_eq!((status["finished"].clone(), status["needed"].clone()), (json!(true), json!(false)));
    let undo = done.effect.unwrap_or_else(|| panic!("an undo")).undo;
    assert_eq!(undo, OpCall { op: "setup.restore".into(), args: json!({"progress": null}) });
    run(&rig, &person(), &undo.op, undo.args).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(rig.store.session().state().setup, None);
    assert_eq!(setup_status(&rig)["needed"], json!(true));
}

#[test]
fn finish_is_honoured_only_on_the_go_live_check_and_restart_goes_back_to_welcome() {
    let rig = rig();
    let at = |step: &str| SetupProgress::new(step, vec!["calibrate".into()], false);
    rig.store.session().set_setup(Some(at("input"))).unwrap_or_else(|e| panic!("{e}"));
    let early = run(&rig, &person(), "setup.finish", json!({})).err().unwrap_or_else(|| panic!("refused"));
    assert_eq!(early.code, ErrorCode::Conflict);
    assert!(early.to_string().contains("input"), "{early}");

    rig.store.session().set_setup(Some(at("go_live"))).unwrap_or_else(|e| panic!("{e}"));
    run(&rig, &person(), "setup.finish", json!({})).unwrap_or_else(|e| panic!("{e}"));
    let status = setup_status(&rig);
    assert_eq!(status["finished"], json!(true));
    assert_eq!(status["skipped_steps"], json!(["calibrate"]), "Go-live lists it as Later");
    let again = run(&rig, &person(), "setup.finish", json!({})).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(again["changed"], json!(false), "finishing twice is a no-op");

    let restarted = plan(&rig.services, &person(), "setup.restart", json!({}), false).unwrap_or_else(|e| panic!("{e}"));
    let status = setup_status(&rig);
    assert_eq!((status["step"].clone(), status["finished"].clone()), (json!("welcome"), json!(false)));
    assert_eq!(status["skipped_steps"], json!([]));
    let undo = restarted.effect.unwrap_or_else(|| panic!("an undo")).undo;
    run(&rig, &person(), &undo.op, undo.args).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(setup_status(&rig)["finished"], json!(true), "undo puts the finished setup back");
}

#[test]
fn the_flow_and_the_ops_share_one_progress_through_the_session() {
    let rig = rig();
    let store = SessionSetupStore(Arc::clone(&rig.store) as Arc<dyn Settings>);
    assert_eq!(store.load(), Progress::default(), "never decided resumes at Welcome");
    let at_output = Progress { step: Step::Output, skipped: vec![Step::Calibrate], finished: false };
    store.save(&at_output).unwrap_or_else(|e| panic!("{e}"));
    let status = setup_status(&rig);
    assert_eq!((status["step"].clone(), status["position"].clone()), (json!("output"), json!(4)));
    assert_eq!(status["skipped_steps"], json!(["calibrate"]));
    run(&rig, &person(), "setup.skip", json!({})).unwrap_or_else(|e| panic!("{e}"));
    assert!(store.load().finished, "the panel sees the agent's (approved) skip");
    // A step id from a newer build resumes at Welcome; an unknown skipped id is dropped.
    let newer = SetupProgress::new("dance", vec!["dance".into(), "input".into()], false);
    rig.store.session().set_setup(Some(newer)).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(store.load(), Progress { step: Step::Welcome, skipped: vec![Step::Input], finished: false });
}
