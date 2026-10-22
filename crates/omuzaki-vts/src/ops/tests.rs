//! The `vts.*` ops and the service against `MockVts`: connecting with the token kept in the secret store, the status, a
//! suggested mapping set and triggered, an expression switched on and released, the checks that name what to do, dry
//! runs that touch nothing, the perform loop's feed reaching VTS, and no token in any answer.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use omuzaki_anim_api::channel::{Channel, ChannelBuf};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, DocRead as _, DocWrite as _, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, OpCtx, Services, ServicesBuilder, find};
use omuzaki_secrets_api::prelude::{SecretLabel, SecretStore};
use omuzaki_secrets_mock::MemorySecrets;
use serde_json::{Value, json};

use crate::client::{BridgeStatus, SecretTokens};
use crate::clock::SystemClock;
use crate::inject::ChannelMap;
use crate::service::VtsService;
use crate::testing::{MockConfig, MockHotkey, MockModel, MockVts};

const WAIT: Duration = Duration::from_secs(10);

fn model() -> MockModel {
    MockModel {
        name: "Kit".into(),
        hotkeys: vec![
            MockHotkey { id: "h-smug".into(), name: "Smug face".into(), on_screen_button: None },
            MockHotkey { id: "h-wave".into(), name: "Wave".into(), on_screen_button: Some(1) },
        ],
        expressions: vec!["Cry".into()],
        parameters: vec!["MouthOpen".into(), "ParamAngleX".into()],
    }
}

struct Rig {
    mock: MockVts,
    service: Arc<VtsService>,
    secrets: Arc<MemorySecrets>,
    services: Services,
    /// The project: a run applies its op's patch here, as the executor would.
    doc: RefCell<MemDoc>,
    /// The patches applied, newest last, for undo.
    applied: RefCell<Vec<Patch>>,
}

impl Rig {
    fn new() -> Self {
        let config = MockConfig { model: Some(model()), clock: Arc::new(SystemClock::new()), ..MockConfig::default() };
        let mock = MockVts::start(&config).unwrap();
        let secrets = Arc::new(MemorySecrets::new());
        let tokens = Arc::new(SecretTokens(Arc::clone(&secrets) as Arc<dyn SecretStore>));
        let service = Arc::new(
            VtsService::with(ChannelMap::builtin().unwrap(), Arc::new(SystemClock::new())).with_tokens(tokens),
        );
        let services = ServicesBuilder::new().insert::<VtsService>(Arc::clone(&service)).build();
        Self { mock, service, secrets, services, doc: RefCell::default(), applied: RefCell::default() }
    }

    fn run(&self, name: &str, args: Value, dry_run: bool) -> Result<Value, String> {
        crate::link();
        self.run_as(name, args, dry_run, &Actor::Person { name: "host".into() })
    }

    fn run_as(&self, name: &str, args: Value, dry_run: bool, actor: &Actor) -> Result<Value, String> {
        let op = (find(name).unwrap().decode)(args).map_err(|e| e.message)?;
        let clock = FakeClock::new(Seconds(0.0));
        let plan = {
            let doc = self.doc.borrow();
            let ctx = OpCtx::new(&*doc, actor, &clock).with_services(&self.services).with_dry_run(dry_run);
            op.plan(&ctx)
        };
        let answer = plan
            .map(|p| {
                if !dry_run && !p.patch.is_empty() {
                    self.doc.borrow_mut().apply(&p.patch).unwrap();
                    self.applied.borrow_mut().push(p.patch);
                }
                p.output.unwrap_or(Value::Null)
            })
            .map_err(|e| format!("{}: {}", e.message, e.hint.unwrap_or_default()));
        // The token never reaches an answer or an error.
        if let Some(token) = self.token() {
            assert!(!format!("{answer:?}").contains(&token), "{name} leaked the token");
        }
        answer
    }

    fn call(&self, name: &str, args: Value) -> Result<Value, String> {
        self.run(name, args, false)
    }

    /// Undoes the newest applied patch, as the executor's undo does.
    fn undo(&self) {
        let patch = self.applied.borrow_mut().pop().unwrap();
        self.doc.borrow_mut().apply(&patch.inverse()).unwrap();
    }

    fn token(&self) -> Option<String> {
        self.secrets.get(&SecretLabel::new("vts/default").unwrap()).unwrap().map(|v| v.expose().to_owned())
    }

    /// Connects; the first time on an instance VTS shows its popup (the mock approves it), so the answer says so and
    /// the bridge connects after.
    fn connect(&self) -> Value {
        self.connect_to(json!({ "port": self.mock.port(), "wait_s": 10.0 }))
    }

    fn connect_to(&self, args: Value) -> Value {
        let answer = self.call("vts.connect", args).unwrap();
        assert!(matches!(answer["state"].as_str(), Some("connected" | "awaiting_approval")), "{answer}");
        until("connected", || matches!(self.service.status(), BridgeStatus::Connected(_)));
        self.call("vts.status", json!({})).unwrap()
    }

    fn count(&self, kind: &str) -> usize {
        self.mock.received().iter().filter(|r| r.message_type == kind).count()
    }
}

/// Waits until `done`, yielding between looks: the bridge and the mock answer on their own threads.
fn until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::yield_now();
    }
}

#[test]
fn connect_stores_the_token_and_status_reports_the_model() {
    let rig = Rig::new();
    let dry = rig.run("vts.connect", json!({ "port": rig.mock.port() }), true).unwrap();
    assert_eq!((dry["applied"].as_bool(), dry["token_stored"].as_bool()), (Some(false), Some(false)));
    assert_eq!(rig.service.status(), BridgeStatus::Disconnected, "a dry run starts nothing");
    assert_eq!(rig.mock.received().len(), 0, "and sends nothing");

    let answer = rig.call("vts.connect", json!({ "port": rig.mock.port(), "wait_s": 10.0 })).unwrap();
    assert_eq!(answer["state"], "awaiting_approval", "no token yet: VTS asks the person");
    assert!(answer["hint"].as_str().unwrap().contains("Allow"));
    let answer = rig.connect();
    assert_eq!((answer["connected"].as_bool(), answer["model"]["name"].as_str()), (Some(true), Some("Kit")));
    assert!(rig.token().is_some(), "the token is kept in the secret store");
    let status = rig.call("vts.status", json!({})).unwrap();
    assert_eq!((status["state"].as_str(), status["instance"].as_str()), (Some("connected"), Some("default")));
    assert_eq!(status["hotkeys"].as_array().map(Vec::len), Some(2));
    assert_eq!(status["mapping"], json!([]));
    assert!(status["parameters"].as_u64().unwrap() > 0);

    // Connecting again is a no-op on the same session, and answers at once.
    let again = rig.call("vts.connect", json!({ "port": rig.mock.port() })).unwrap();
    assert_eq!(again["state"], "connected");
    assert_eq!(rig.count("AuthenticationTokenRequest"), 1, "the popup shows once");

    let off = rig.call("vts.disconnect", json!({})).unwrap();
    assert_eq!((off["disconnected"].as_bool(), off["state"].as_str()), (Some(true), Some("disconnected")));
    assert!(rig.call("vts.trigger", json!({ "id": "smug" })).unwrap_err().contains("vts.connect"));
}

#[test]
fn a_suggested_mapping_is_set_and_triggers_its_hotkey() {
    let rig = Rig::new();
    rig.connect();
    let suggested = rig.call("vts.suggest", json!({ "ids": ["smug", "wave", "angry"] })).unwrap();
    assert_eq!(suggested["model"], "Kit");
    assert_eq!(suggested["unmatched"], json!(["angry"]));
    let rows = suggested["rows"].clone();
    assert_eq!(
        rows[0],
        json!({ "omuzaki": "smug", "target": { "kind": "hotkey", "id": "h-smug", "name": "Smug face" } })
    );

    let unmapped = rig.call("vts.trigger", json!({ "id": "smug" })).unwrap_err();
    assert!(unmapped.contains("not mapped") && unmapped.contains("vts.suggest"), "{unmapped}");

    rig.run("vts.map", json!({ "rows": rows }), true).unwrap();
    assert!(rig.doc.borrow().root().get("connections").is_none(), "a dry run sets nothing");
    let set = rig.call("vts.map", json!({ "rows": rows })).unwrap();
    assert_eq!(set["rows"].as_array().map(Vec::len), Some(2));
    assert_eq!(rig.call("vts.status", json!({})).unwrap()["mapping"], set["rows"]);
    let stored = rig.doc.borrow().get(&DocPath::root().join("connections").join("vts")).cloned().unwrap();
    assert_eq!(stored["models"]["Kit"], set["rows"], "the mapping is kept in the project");

    let dry = rig.run("vts.trigger", json!({ "id": "smug" }), true).unwrap();
    assert_eq!((dry["applied"].as_bool(), dry["target"]["id"].as_str()), (Some(false), Some("h-smug")));
    assert_eq!(rig.count("HotkeyTriggerRequest"), 0, "a dry run sends nothing");
    let fired = rig.call("vts.trigger", json!({ "id": "smug" })).unwrap();
    assert_eq!(fired["triggered"], "smug");
    until("the hotkey", || rig.count("HotkeyTriggerRequest") == 1);
    let request = rig.mock.received().into_iter().find(|r| r.message_type == "HotkeyTriggerRequest").unwrap();
    assert_eq!(request.data["hotkeyID"], "h-smug");
}

#[test]
fn map_checks_hotkey_ids_and_merges_rows() {
    let rig = Rig::new();
    rig.connect();
    let wrong =
        json!({ "rows": [{ "omuzaki": "smug", "target": { "kind": "hotkey", "id": "nope", "name": "Smug face" } }] });
    let error = rig.call("vts.map", wrong).unwrap_err();
    assert!(error.contains("no hotkey with id nope") && error.contains("has id h-smug"), "{error}");
    let twice = json!({ "rows": [
        { "omuzaki": "a", "target": { "kind": "hotkey", "id": "h-wave", "name": "Wave" } },
        { "omuzaki": "a", "target": { "kind": "hotkey", "id": "h-smug", "name": "Smug face" } },
    ] });
    assert!(rig.call("vts.map", twice).unwrap_err().contains("mapped twice"));

    let wave = json!({ "omuzaki": "wave", "target": { "kind": "hotkey", "id": "h-wave", "name": "Wave" } });
    rig.call("vts.map", json!({ "rows": [wave] })).unwrap();
    let cry = json!({ "omuzaki": "cry", "target": { "kind": "expression", "file": "Cry.exp3.json", "fade_s": 9.0 } });
    let merged = rig.call("vts.map", json!({ "rows": [cry], "merge": true })).unwrap();
    assert_eq!(merged["rows"][0]["omuzaki"], "wave", "merge keeps the other rows");
    assert_eq!(merged["rows"][1]["target"]["fade_s"], 2.0, "the fade is clamped to VTS's 2 s");
    let replaced = rig.call("vts.map", json!({ "rows": [] })).unwrap();
    assert_eq!(replaced["rows"], json!([]));
    assert_eq!(rig.call("vts.status", json!({})).unwrap()["mapping"], json!([]));
    assert!(rig.service.mapping().models.is_empty(), "an emptied model is dropped");

    // Undo is exact, and the bridge follows the project back.
    rig.undo();
    assert_eq!(rig.call("vts.status", json!({})).unwrap()["mapping"], merged["rows"]);
    rig.undo();
    rig.undo();
    assert!(rig.doc.borrow().root().get("connections").is_none(), "undone to the empty project");
    assert_eq!(rig.call("vts.status", json!({})).unwrap()["mapping"], json!([]));

    // Another model's rows can be set before it is loaded; its hotkeys can't be checked yet.
    let other = json!({ "model": "Other", "rows": [{ "omuzaki": "x", "target": { "kind": "hotkey", "id": "h", "name": "X" } }] });
    assert_eq!(rig.call("vts.map", other).unwrap()["model"], "Other");
}

#[test]
fn an_expression_is_switched_on_and_released() {
    let rig = Rig::new();
    rig.connect();
    let cry = json!({ "omuzaki": "cry", "target": { "kind": "expression", "file": "Cry.exp3.json", "fade_s": 0.5 } });
    rig.call("vts.map", json!({ "rows": [cry] })).unwrap();
    rig.call("vts.trigger", json!({ "id": "cry" })).unwrap();
    until(
        "the expression on",
        || matches!(rig.service.status(), BridgeStatus::Connected(c) if c.expressions_on == ["Cry.exp3.json"]),
    );
    let dry = rig.run("vts.release", json!({ "id": "cry" }), true).unwrap();
    assert_eq!((dry["applied"].as_bool(), dry["on"].as_bool()), (Some(false), Some(true)));
    let released = rig.call("vts.release", json!({ "id": "cry" })).unwrap();
    assert_eq!(released["released"], true);
    until(
        "the expression off",
        || matches!(rig.service.status(), BridgeStatus::Connected(c) if c.expressions_on.is_empty()),
    );
    // The bridge's status can move before the mock has recorded the request it answered.
    until("the mock's record", || rig.count("ExpressionActivationRequest") == 2);
    let activations: Vec<_> =
        rig.mock.received().into_iter().filter(|r| r.message_type == "ExpressionActivationRequest").collect();
    assert_eq!(activations.iter().map(|r| r.data["active"].clone()).collect::<Vec<_>>(), [json!(true), json!(false)]);
}

#[test]
fn the_feed_drives_vts_and_follows_a_new_bridge() {
    let rig = Rig::new();
    let mut feed = rig.service.feed();
    let mut buf = ChannelBuf::default();
    buf.set(Channel::JAW_OPEN, 0.4);
    // No bridge yet: the push goes nowhere.
    feed.push(&buf, true);
    rig.connect();
    feed.push(&buf, true);
    until("the jaw at 0.4", || rig.mock.param_value("OmuzakiJawOpen").is_some_and(|v| (v - 0.4).abs() < 1e-6));

    // A connect on another instance replaces the bridge; the same feed follows it.
    rig.connect_to(json!({ "port": rig.mock.port(), "instance": "second", "wait_s": 10.0 }));
    buf.set(Channel::JAW_OPEN, 0.7);
    feed.push(&buf, true);
    until("the jaw at 0.7", || rig.mock.param_value("OmuzakiJawOpen").is_some_and(|v| (v - 0.7).abs() < 1e-6));
}

#[test]
fn errors_name_what_to_do() {
    let rig = Rig::new();
    for (op, args) in [
        ("vts.trigger", json!({ "id": "smug" })),
        ("vts.release", json!({ "id": "smug" })),
        ("vts.suggest", json!({ "ids": ["smug"] })),
        ("vts.map", json!({ "rows": [] })),
    ] {
        let error = rig.call(op, args).unwrap_err();
        assert!(error.contains("vts.connect"), "{op}: {error}");
    }
    // A dry run never fails for want of VTS: it says what the action would need.
    for (op, args) in [("vts.trigger", json!({ "id": "smug" })), ("vts.suggest", json!({ "ids": ["smug"] }))] {
        let dry = rig.run(op, args, true).unwrap();
        assert_eq!((dry["applied"].as_bool(), dry["connected"].as_bool()), (Some(false), Some(false)), "{op}");
        assert!(dry["hint"].as_str().unwrap().contains("vts.connect"), "{op}: {dry}");
    }
    // The first connect on an instance shows VTS's popup: an agent asks the person first, dry run included.
    let agent = Actor::Agent { id: "agent".into(), model: None };
    for dry_run in [true, false] {
        let error = rig.run_as("vts.connect", json!({ "port": rig.mock.port() }), dry_run, &agent).unwrap_err();
        assert!(error.contains("allow Omuzaki"), "{error}");
    }
    assert!(rig.mock.received().is_empty(), "nothing reached VTS for the agent");
    let error = rig.call("vts.connect", json!({ "wait_s": 99.0 })).unwrap_err();
    assert!(error.contains("outside 0 to 30"), "{error}");
    let status = rig.call("vts.status", json!({})).unwrap();
    assert_eq!((status["state"].as_str(), status["connected"].as_bool()), (Some("disconnected"), Some(false)));
}
