//! The agent sessions and approvals contract (design `agent-sessions.md` §11, ADR 0020): every new type survives a
//! serde round trip and has a JSON Schema, old JSON still decodes, `Grants::intersect` never widens, and
//! `OpCtx::require_person` answers the same in a dry-run.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fmt::Debug;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, DocVersion, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{
    Actor, ApprovalId, ApprovalRef, ApprovalRequest, Budget, ClashKind, Decision, EndReason, Ended, ErrorCode,
    ErrorDetail, Event, Grants, JobId, KeptTx, Limits, Mode, Need, OpCtx, OpError, OpRecord, Outcome, PauseReason,
    PendingApproval, Plan, Principals, Request, Resolution, Resolved, Scope, SessionChange, SessionId, SessionInfo,
    SessionSpec, SessionState, SessionTx, SessionUndo, Sessions, Spend, StopStep, Stopped, Transport, TxId,
};
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

fn person() -> Actor {
    Actor::Person { name: "alice".into() }
}

fn agent() -> Actor {
    Actor::Agent { id: "claude-code#1".into(), model: Some("claude-opus-5-5".into()) }
}

fn spend() -> Spend {
    Spend { usd_micros: 1_500_000, tokens: 12, seconds: 3 }
}

fn budget() -> Budget {
    Budget { usd_micros: 2_000_000, tokens: Some(1000), seconds: None }
}

fn kept() -> KeptTx {
    KeptTx { tx: TxId(4), label: "tweak".into(), error: OpError::conflict("changed") }
}

fn undo_result() -> SessionUndo {
    SessionUndo {
        step: Some(TxId(9)),
        reverted: vec![TxId(3), TxId(2)],
        skipped: vec![kept()],
        abandoned: vec![TxId(5)],
        outside: vec![TxId(1)],
    }
}

fn ended() -> Ended {
    Ended { abandoned: vec![TxId(5)], kept: vec![kept()], withdrawn: vec![ApprovalId(2)], cancelled: vec![] }
}

fn info() -> SessionInfo {
    SessionInfo {
        id: SessionId(1),
        actor: agent(),
        on_behalf_of: person(),
        transport: Transport::Http { token: "ci".into() },
        client: "claude-code".into(),
        model: Some("claude-opus-5-5".into()),
        parent: Some(SessionId(0)),
        role: Some("designer".into()),
        grants: Grants::agent_default().with_fs_root("/work"),
        fence: vec![DocPath::parse("characters/a").unwrap()],
        budget: budget(),
        spent: spend(),
        limits: Limits::default(),
        state: SessionState::Paused { by: person(), reason: PauseReason::Budget },
        open_tx: Some(SessionTx { tx: TxId(6), label: "open".into() }),
        pending: vec![ApprovalId(2)],
        steps: 3,
        started_at: Seconds(1.0),
        last_active_at: Seconds(2.5),
    }
}

fn pending() -> PendingApproval {
    PendingApproval {
        id: ApprovalId(2),
        session: SessionId(1),
        actor: agent(),
        request: Some(ApprovalRequest {
            op: "export.publish".into(),
            args: json!({"to": "x"}),
            cause: None,
            preview: Plan::answer(json!("ok")),
        }),
        need: Need::Scope { scope: Scope::Publish },
        batch: "1:export.publish:publish".into(),
        requested_at: Seconds(4.0),
    }
}

fn record() -> OpRecord {
    OpRecord::new(1, "test.set", json!({}), agent(), TxId(1), Seconds(0.5), Patch::new(), DocVersion(3))
        .with_on_behalf_of(person())
        .with_approval(ApprovalRef::new(ApprovalId(2), person()))
}

/// `value` encodes to JSON that decodes to an equal value, and its type has a JSON Schema.
fn round_trips<T: Serialize + DeserializeOwned + JsonSchema + PartialEq + Debug>(value: &T) {
    let json = serde_json::to_value(value).unwrap();
    let back: T = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(&back, value, "{json}");
    let schema = serde_json::to_value(schemars::schema_for!(T)).unwrap();
    assert!(schema.is_object() && schema.as_object().is_some_and(|s| !s.is_empty()), "{}", std::any::type_name::<T>());
}

#[test]
fn session_types_round_trip_and_have_schemas() {
    round_trips(&SessionId(7));
    for transport in [
        Transport::Stdio,
        Transport::Http { token: "t".into() },
        Transport::InApp,
        Transport::Plugin,
        Transport::Automation,
        Transport::Replay,
    ] {
        round_trips(&transport);
    }
    round_trips(&SessionSpec {
        transport: Transport::Stdio,
        client: "codex".into(),
        model: None,
        on_behalf_of: person(),
        grants: Grants::agent_default(),
        budget: Budget::default(),
        limits: Limits::default(),
        parent: None,
        role: None,
        fence: vec![DocPath::parse("characters/a").unwrap()],
    });
    round_trips(&info());
    for state in [
        SessionState::Active,
        SessionState::Paused { by: person(), reason: PauseReason::Pairing },
        SessionState::Ended { reason: EndReason::Revoked { by: person(), credential: true }, at: Seconds(9.0) },
    ] {
        round_trips(&state);
    }
    for reason in [PauseReason::Person, PauseReason::Budget, PauseReason::Undo, PauseReason::Step, PauseReason::Pairing]
    {
        round_trips(&reason);
    }
    for reason in [
        EndReason::Disconnected,
        EndReason::IdleTimeout,
        EndReason::Closed,
        EndReason::Revoked { by: person(), credential: false },
        EndReason::ProjectClosed,
    ] {
        round_trips(&reason);
    }
    round_trips(&budget());
    round_trips(&spend());
    round_trips(&Limits::default());
    round_trips(&ended());
    round_trips(&kept());
    round_trips(&undo_result());
    for change in [
        SessionChange::Grants { by: person() },
        SessionChange::Budget { by: person() },
        SessionChange::Paused,
        SessionChange::Resumed { by: person() },
        SessionChange::Spent,
        SessionChange::Fence { by: person() },
    ] {
        round_trips(&change);
    }
}

#[test]
fn approval_types_round_trip_and_have_schemas() {
    round_trips(&ApprovalId(1));
    for need in [
        Need::Scope { scope: Scope::Publish },
        Need::Person { reason: "model licence".into() },
        Need::Budget { needed: spend(), left: Spend::default() },
        Need::Pairing { client: "claude-code".into() },
        Need::Review { policy: "two-eyes".into() },
    ] {
        round_trips(&need);
    }
    round_trips(&pending());
    for decision in [
        Decision::Approve,
        Decision::ApproveForSession,
        Decision::ApproveForProject,
        Decision::RaiseBudget { budget: budget() },
        Decision::Reject { comment: Some("no".into()) },
    ] {
        round_trips(&decision);
    }
    let outcome = Outcome::new(Plan::default()).with_applied(record());
    for result in [
        Resolved::Applied { outcome },
        Resolved::Failed { error: OpError::invalid("x", "bad") },
        Resolved::Rejected { comment: None },
        Resolved::Withdrawn { reason: EndReason::Closed },
    ] {
        round_trips(&Resolution { id: ApprovalId(2), by: Some(person()), result, at: Seconds(5.0) });
    }
    round_trips(&ApprovalRef::new(ApprovalId(2), person()));
    round_trips(&ApprovalRef::new(ApprovalId(3), person()).with_also(vec![agent()]));
}

#[test]
fn error_detail_round_trips_and_is_absent_when_none() {
    let details = [
        ErrorDetail::MissingScope { scope: Scope::Spend },
        ErrorDetail::NeedsPerson { reason: "r".into() },
        ErrorDetail::PersonOnly { reason: "Settings > Audio".into() },
        ErrorDetail::Queued { approval: ApprovalId(3), need: Need::Person { reason: "r".into() } },
        ErrorDetail::Paused { by: person(), reason: PauseReason::Person },
        ErrorDetail::Ended { reason: EndReason::Disconnected },
        ErrorDetail::NoSession,
        ErrorDetail::RetryAfter { seconds: 1.5, limit: "edits_per_minute=600".into() },
        ErrorDetail::Changed { path: DocPath::parse("a/b").unwrap(), by: agent(), version: DocVersion(8) },
        ErrorDetail::Fenced { path: DocPath::parse("a/b").unwrap(), holder: Some(agent()) },
        ErrorDetail::Fenced { path: DocPath::parse("a/b").unwrap(), holder: None },
    ];
    for detail in details {
        round_trips(&detail);
        round_trips(&OpError::denied("no").with_detail(detail));
    }

    let plain = serde_json::to_value(OpError::denied("no")).unwrap();
    assert!(plain.get("detail").is_none(), "{plain}");
    let with = serde_json::to_value(OpError::denied("no").with_detail(ErrorDetail::NoSession)).unwrap();
    assert_eq!(with["detail"], json!({"kind": "no_session"}));
}

#[test]
fn new_error_codes_have_names() {
    assert_eq!(ErrorCode::Waiting.to_string(), "waiting");
    assert_eq!(ErrorCode::Limited.to_string(), "limited");
    assert_eq!(serde_json::to_value(ErrorCode::Waiting).unwrap(), json!("waiting"));
    assert_eq!(serde_json::to_value(ErrorCode::Limited).unwrap(), json!("limited"));
}

#[test]
fn person_constructors_set_code_and_detail() {
    let e = OpError::needs_person("switch model");
    assert_eq!(e.code, ErrorCode::Denied);
    assert_eq!(e.detail.as_deref(), Some(&ErrorDetail::NeedsPerson { reason: "switch model".into() }));
    let e = OpError::person_only("Settings > Audio");
    assert_eq!(e.code, ErrorCode::Denied);
    assert_eq!(e.detail.as_deref(), Some(&ErrorDetail::PersonOnly { reason: "Settings > Audio".into() }));
}

#[test]
fn old_json_without_the_new_fields_still_decodes() {
    let error: OpError =
        serde_json::from_value(json!({"code": "denied", "message": "m", "field": null, "hint": null})).unwrap();
    assert_eq!(error.detail, None);

    let request: Request = serde_json::from_value(json!({
        "op": "test.set", "args": {}, "mode": "apply", "actor": {"kind": "system"},
        "grants": {"scopes": ["read"], "fs_roots": []}, "cause": null, "tx": null
    }))
    .unwrap();
    assert_eq!(request.base, None);
    assert_eq!(request.mode, Mode::Apply);

    let mut spec = serde_json::to_value(SessionSpec {
        transport: Transport::Stdio,
        client: "c".into(),
        model: None,
        on_behalf_of: person(),
        grants: Grants::agent_default(),
        budget: Budget::default(),
        limits: Limits::default(),
        parent: None,
        role: None,
        fence: vec![],
    })
    .unwrap();
    spec.as_object_mut().unwrap().remove("fence");
    assert_eq!(serde_json::from_value::<SessionSpec>(spec).unwrap().fence, Vec::<DocPath>::new());

    let mut record = serde_json::to_value(record()).unwrap();
    for key in ["on_behalf_of", "approval"] {
        record.as_object_mut().unwrap().remove(key);
    }
    let record: OpRecord = serde_json::from_value(record).unwrap();
    assert_eq!((record.on_behalf_of, record.approval), (None, None));
}

#[test]
fn events_round_trip_and_the_schema_names_every_variant() {
    let events = [
        Event::op_applied(record()),
        Event::tx_committed(TxId(1), "l", person()),
        Event::tx_rolled_back(TxId(1), person(), DocVersion(2)),
        Event::undone(TxId(1), "l", person(), agent(), DocVersion(2)),
        Event::redone(TxId(1), "l", person(), agent(), DocVersion(3)),
        Event::session_opened(info()),
        Event::session_changed(info(), SessionChange::Spent),
        Event::session_ended(info(), ended()),
        Event::session_undone(SessionId(1), person(), undo_result()),
        Event::clash(
            SessionId(1),
            agent(),
            "hair.set_style",
            DocPath::parse("a/b").unwrap(),
            ClashKind::Fenced,
            Some(person()),
            Seconds(7.0),
        ),
        Event::approval_requested(pending()),
        Event::approval_resolved(Resolution {
            id: ApprovalId(2),
            by: None,
            result: Resolved::Withdrawn { reason: EndReason::ProjectClosed },
            at: Seconds(6.0),
        }),
    ];
    let schema = serde_json::to_string(&schemars::schema_for!(Event)).unwrap();
    for event in &events {
        let json = serde_json::to_value(event).unwrap();
        assert_eq!(&serde_json::from_value::<Event>(json.clone()).unwrap(), event);
        let name = json["event"].as_str().unwrap();
        assert!(schema.contains(&format!("\"{name}\"")), "the Event schema lacks {name}");
    }
}

#[test]
fn limits_default_to_the_accepted_values() {
    let l = Limits::default();
    assert_eq!(
        (l.edits_per_minute, l.edit_burst, l.runs_per_minute, l.run_burst, l.max_pending),
        (600, 100, 3000, 300, 16)
    );
    assert_eq!(Budget::default(), Budget { usd_micros: 0, tokens: None, seconds: None });
}

#[test]
fn sessions_and_principals_are_object_safe() {
    fn _principals(_: &dyn Principals) {}
    fn _sessions(_: &dyn Sessions) {}
}

// -- Grants ---------------------------------------------------------------------------------------------------------

fn scopes(grants: &Grants) -> Vec<Scope> {
    grants.scopes().collect()
}

#[test]
fn intersect_keeps_only_scopes_both_sides_hold() {
    let session = Grants::of([Scope::Read, Scope::EditScene, Scope::Publish]);
    let principal = Grants::of([Scope::Read, Scope::EditScene, Scope::Render]);
    let both = session.intersect(&principal);
    assert_eq!(scopes(&both), [Scope::Read, Scope::EditScene]);
    assert_eq!(both, principal.intersect(&session), "symmetric");
    assert_eq!(scopes(&Grants::person().intersect(&Grants::default())), []);
}

#[test]
fn intersect_keeps_roots_only_when_the_other_side_holds_fs() {
    let session = Grants::agent_default().with_fs_root("/work/art");
    let person = Grants::person();
    assert_eq!(session.intersect(&person).fs_roots(), ["/work/art"], "a person holds fs with no roots");

    let no_fs = Grants::of([Scope::Read]);
    let narrowed = session.intersect(&no_fs);
    assert!(narrowed.fs_roots().is_empty() && !narrowed.allows(Scope::Fs));

    // When both sides name roots, only what the other side covers survives.
    let other = Grants::person().with_fs_root("/work");
    assert_eq!(session.intersect(&other).fs_roots(), ["/work/art"]);
    assert_eq!(other.intersect(&session).fs_roots(), ["/work/art"], "symmetric");
    let elsewhere = Grants::person().with_fs_root("/other");
    assert_eq!(session.intersect(&elsewhere).fs_roots(), [] as [String; 0]);
}

#[test]
fn without_removes_a_scope_and_fs_takes_its_roots() {
    let grants = Grants::person().with_fs_root("/work");
    assert!(!grants.clone().without(Scope::Publish).allows(Scope::Publish));
    let no_fs = grants.without(Scope::Fs);
    assert!(!no_fs.allows(Scope::Fs) && no_fs.fs_roots().is_empty());
}

// -- OpCtx::require_person ------------------------------------------------------------------------------------------

/// What `require_person` answers for `actor` (optionally confirmed by `confirmer`) in a dry-run and a real run.
fn require(actor: &Actor, confirmer: Option<&Actor>) -> [Result<(), OpError>; 2] {
    let (doc, clock) = (MemDoc::new(), FakeClock::new(Seconds(0.0)));
    [true, false].map(|dry_run| {
        let mut ctx = OpCtx::new(&doc, actor, &clock).with_dry_run(dry_run);
        if let Some(person) = confirmer {
            ctx = ctx.with_confirmation(person);
        }
        ctx.require_person("switching to a model with a licence")
    })
}

#[test]
fn require_person_passes_for_a_person() {
    for answer in require(&person(), None) {
        assert_eq!(answer, Ok(()));
    }
}

#[test]
fn require_person_passes_for_an_agent_a_person_confirmed() {
    let alice = person();
    let (doc, clock, agent) = (MemDoc::new(), FakeClock::new(Seconds(0.0)), agent());
    let ctx = OpCtx::new(&doc, &agent, &clock).with_confirmation(&alice);
    assert_eq!(ctx.confirmed_by(), Some(&alice));
    for answer in require(&agent, Some(&alice)) {
        assert_eq!(answer, Ok(()));
    }
}

#[test]
fn require_person_fails_for_an_unconfirmed_agent_the_same_in_a_dry_run() {
    let [dry, real] = require(&agent(), None);
    assert_eq!(dry, real, "dry-run and real run answer alike");
    let error = real.unwrap_err();
    assert_eq!(
        error.detail.as_deref(),
        Some(&ErrorDetail::NeedsPerson { reason: "switching to a model with a licence".into() })
    );
}

#[test]
fn require_person_is_not_satisfied_by_a_non_person_confirmer() {
    for confirmer in [agent(), Actor::System, Actor::Plugin { id: "p".into() }] {
        for answer in require(&agent(), Some(&confirmer)) {
            assert_eq!(answer.unwrap_err().code, ErrorCode::Denied);
        }
    }
    assert_eq!(require(&Actor::System, None)[1].as_ref().unwrap_err().code, ErrorCode::Denied);
}

#[test]
fn a_request_without_base_serialises_it_as_null() {
    let request = Request {
        op: "x".into(),
        args: Value::Null,
        mode: Mode::DryRun,
        actor: Actor::System,
        grants: Grants::default(),
        cause: None,
        tx: None,
        base: Some(DocVersion(4)),
    };
    assert_eq!(serde_json::to_value(&request).unwrap()["base"], json!(4));
}

/// A `Sessions` that overrides neither `request_budget` nor `stop`: both must come back `Unsupported`.
struct Bare;

impl Sessions for Bare {
    fn open(&self, _: SessionSpec) -> Result<SessionInfo, OpError> {
        Err(OpError::internal("unused"))
    }
    fn sessions(&self) -> Vec<SessionInfo> {
        Vec::new()
    }
    fn session(&self, _: SessionId) -> Option<SessionInfo> {
        None
    }
    fn session_of(&self, _: &Actor) -> Option<SessionInfo> {
        None
    }
    fn pause(&self, _: SessionId, _: &Actor) -> Result<SessionInfo, OpError> {
        Err(OpError::internal("unused"))
    }
    fn resume(&self, _: SessionId, _: &Actor) -> Result<SessionInfo, OpError> {
        Err(OpError::internal("unused"))
    }
    fn set_grants(&self, _: SessionId, _: Grants, _: &Actor) -> Result<SessionInfo, OpError> {
        Err(OpError::internal("unused"))
    }
    fn set_budget(&self, _: SessionId, _: Budget, _: &Actor) -> Result<SessionInfo, OpError> {
        Err(OpError::internal("unused"))
    }
    fn charge(&self, _: SessionId, _: Spend) -> Result<SessionInfo, OpError> {
        Err(OpError::internal("unused"))
    }
    fn end(&self, _: SessionId, _: EndReason) -> Result<Ended, OpError> {
        Err(OpError::internal("unused"))
    }
    fn undo_session(&self, _: SessionId, _: &Actor) -> Result<SessionUndo, OpError> {
        Err(OpError::internal("unused"))
    }
    fn approvals(&self) -> Vec<PendingApproval> {
        Vec::new()
    }
    fn resolve(&self, _: ApprovalId, _: Decision, _: &Actor) -> Result<Resolution, OpError> {
        Err(OpError::internal("unused"))
    }
}

#[test]
fn request_budget_and_stop_default_to_unsupported() {
    let err = Bare.request_budget(SessionId(1), spend()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Unsupported);
    let err = Bare.stop(SessionId(1), &person()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Unsupported);
}

fn stopped_golden(name: &str) -> (String, Stopped) {
    let text =
        std::fs::read_to_string(format!("{}/tests/golden/stopped/{name}.json", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let value = serde_json::from_str(&text).unwrap();
    (text, value)
}

#[test]
fn stopped_round_trips_and_its_goldens_are_byte_stable() {
    for (name, expected) in [
        ("pending", Stopped { step: StopStep::Pending, jobs: vec![JobId(1), JobId(2)] }),
        ("committed", Stopped { step: StopStep::Committed(TxId(7)), jobs: vec![] }),
        ("none", Stopped { step: StopStep::None, jobs: vec![] }),
    ] {
        round_trips(&expected);
        let (text, value) = stopped_golden(name);
        assert_eq!(value, expected, "{name}.json decodes to the same value");
        let mut back = serde_json::to_string_pretty(&value).unwrap();
        back.push('\n');
        assert_eq!(back, text, "{name}.json no longer serialises byte for byte");
    }
}
