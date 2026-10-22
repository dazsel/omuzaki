//! The client, the service and the ops against [`FakeObs`]: connecting with the stored password, the status, scenes and
//! sources, every action with the request it sends, idempotency, near-match errors, reconnecting with backoff, a wrong
//! password never retried, remote hosts refused, bounded events, and no password in any output.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, Grants, OpCtx, Scope, Services, ServicesBuilder, find};
use omuzaki_secrets_api::prelude::{SecretLabel, SecretStore, SecretValue};
use omuzaki_secrets_mock::MemorySecrets;
use omuzaki_settings_api::service::{Layer, Settings, SettingsExt as _};
use omuzaki_settings_mock::MemorySettings;
use serde_json::{Value, json};

use crate::client::{ClientConfig, EVENT_BACKLOG, EventHub, ObsClient, ObsError};
use crate::protocol::{GetSceneList, ObsEvent};
use crate::service::{ObsService, ObsStatus, backoff};
use crate::testing::{FakeConfig, FakeObs, Output};

const PASSWORD: &str = "test-only-password";

fn locked() -> FakeObs {
    FakeObs::start(FakeConfig { password: Some(PASSWORD.into()), ..FakeConfig::default() })
}

fn rig(service: &Arc<ObsService>, port: u16, password: bool) -> Services {
    let secrets = Arc::new(MemorySecrets::new());
    if password {
        secrets.set(&SecretLabel::new("obs/websocket").unwrap(), SecretValue::new(PASSWORD)).unwrap();
    }
    let settings = Arc::new(MemorySettings::with_clock(Arc::new(FakeClock::new(Seconds(0.0)))));
    settings
        .set::<crate::settings::Port>(
            &port,
            Layer::Base,
            omuzaki_ops_api::Mode::Apply,
            &Actor::Person { name: "host".into() },
        )
        .unwrap();
    ServicesBuilder::new()
        .insert::<ObsService>(Arc::clone(service))
        .insert::<dyn SecretStore>(secrets)
        .insert::<dyn Settings>(settings)
        .build()
}

fn run(name: &str, args: Value, services: &Services, dry_run: bool) -> Result<Value, String> {
    crate::link();
    let op = (find(name).unwrap().decode)(args).map_err(|e| e.message)?;
    let (doc, clock, actor) = (MemDoc::new(), FakeClock::new(Seconds(0.0)), Actor::Person { name: "host".into() });
    let ctx = OpCtx::new(&doc, &actor, &clock).with_services(services).with_dry_run(dry_run);
    op.plan(&ctx)
        .map(|p| p.output.unwrap_or(Value::Null))
        .map_err(|e| format!("{}: {}", e.message, e.hint.unwrap_or_default()))
}

fn call(name: &str, args: Value, services: &Services) -> Result<Value, String> {
    run(name, args, services, false)
}

#[test]
fn connect_reads_the_stored_password_and_the_port_setting() {
    let fake = locked();
    let service = Arc::new(ObsService::new());
    let services = rig(&service, fake.port(), true);
    let dry = run("obs.connect", json!({}), &services, true).unwrap();
    assert_eq!((dry["port"].as_u64(), dry["password_stored"].as_bool()), (Some(u64::from(fake.port())), Some(true)));
    assert!(service.client().is_none(), "a dry run connects nothing");
    let connected = call("obs.connect", json!({}), &services).unwrap();
    assert_eq!(connected["connected"], true);
    let status = call("obs.status", json!({}), &services).unwrap();
    assert_eq!((status["state"].as_str(), status["host"].as_str()), (Some("connected"), Some("127.0.0.1")));
    assert_eq!(status["program_scene"], "Main");
    assert_eq!((status["stream_active"].as_bool(), status["replay_active"].as_bool()), (Some(false), Some(false)));
    assert_eq!(call("obs.scenes", json!({}), &services).unwrap()["scenes"], json!(["Main", "Starting soon", "BRB"]));
    let sources = call("obs.sources", json!({ "scene": "Main" }), &services).unwrap();
    let names: Vec<&str> = sources["sources"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Camera", "Avatar", "Alerts"]);
    let missing = call("obs.sources", json!({ "scene": "Mian" }), &services).unwrap_err();
    assert!(missing.contains("did you mean Main"), "{missing}");
    for out in [&dry, &connected, &status, &sources] {
        assert!(!out.to_string().contains(PASSWORD));
    }
    assert_eq!(call("obs.disconnect", json!({}), &services).unwrap()["disconnected"], true);
    assert_eq!(call("obs.status", json!({}), &services).unwrap()["state"], "disconnected");
}

#[test]
fn connect_reads_only_obs_secrets_and_a_remote_host_is_the_persons_call() {
    let fake = locked();
    let service = Arc::new(ObsService::new());
    let services = rig(&service, fake.port(), true);
    let other = run("obs.connect", json!({ "password": "connector/openai" }), &services, true).unwrap_err();
    assert!(other.contains("not an OBS secret"), "{other}");

    // With remote links allowed, an agent may still not aim the challenge answer at a host of its choosing.
    let settings = services.get::<dyn Settings>().unwrap();
    let person = Actor::Person { name: "host".into() };
    settings.set::<crate::settings::AllowRemote>(&true, Layer::Base, omuzaki_ops_api::Mode::Apply, &person).unwrap();
    crate::link();
    let agent = Actor::Agent { id: "agent".into(), model: None };
    let op = (find("obs.connect").unwrap().decode)(json!({ "host": "192.0.2.1" })).unwrap();
    let (doc, clock) = (MemDoc::new(), FakeClock::new(Seconds(0.0)));
    let ctx = OpCtx::new(&doc, &agent, &clock).with_services(&services).with_dry_run(true);
    let refused = op.plan(&ctx).unwrap_err();
    assert!(refused.message.contains("another computer"), "{}", refused.message);
    // A person may, and so may an agent going to the host the person set up.
    assert!(run("obs.connect", json!({ "host": "192.0.2.1" }), &services, true).is_ok());
    let loopback = (find("obs.connect").unwrap().decode)(json!({ "host": "127.0.0.1" })).unwrap();
    assert!(loopback.plan(&ctx).is_ok());
}

#[test]
fn each_action_sends_its_request_and_changes_obs() {
    let fake = FakeObs::start(FakeConfig::default());
    let service = Arc::new(ObsService::new());
    let services = rig(&service, fake.port(), false);
    call("obs.connect", json!({}), &services).unwrap();

    call("obs.scene.set", json!({ "scene": "BRB" }), &services).unwrap();
    assert_eq!(fake.state().program, "BRB");
    call("obs.source.set_visible", json!({ "scene": "Main", "source": "Alerts", "visible": false }), &services)
        .unwrap();
    assert!(!fake.state().scenes[0].items[2].enabled);
    call("obs.source.set_visible", json!({ "scene": "Main", "source": "Alerts", "visible": true }), &services).unwrap();
    assert!(fake.state().scenes[0].items[2].enabled);

    let first = call("obs.stream.start", json!({}), &services).unwrap();
    let second = call("obs.stream.start", json!({}), &services).unwrap();
    assert_eq!((first["already"].as_bool(), second["already"].as_bool()), (Some(false), Some(true)));
    assert_eq!(fake.count("StartStream"), 2, "the second is one request OBS refuses as already running");
    assert_eq!(fake.state().stream, Output::Started);
    call("obs.stream.stop", json!({}), &services).unwrap();
    assert_eq!(call("obs.stream.stop", json!({}), &services).unwrap()["already"], true);

    call("obs.record.start", json!({}), &services).unwrap();
    let stopped = call("obs.record.stop", json!({}), &services).unwrap();
    assert!(stopped["path"].as_str().unwrap().starts_with(crate::testing::FAKE_FOLDER));

    let not_running = call("obs.replay.save", json!({}), &services).unwrap_err();
    assert!(not_running.contains("obs.replay.start"), "{not_running}");
    call("obs.replay.start", json!({}), &services).unwrap();
    let saved = call("obs.replay.save", json!({}), &services).unwrap();
    assert!(saved["path"].as_str().unwrap().contains("replay"));
    call("obs.replay.stop", json!({}), &services).unwrap();
    assert_eq!(fake.state().replay, Output::Stopped);

    let near =
        call("obs.source.set_visible", json!({ "scene": "Main", "source": "Alrets", "visible": true }), &services)
            .unwrap_err();
    assert!(near.contains("did you mean Alerts"), "{near}");
    let scene = call("obs.scene.set", json!({ "scene": "brb " }), &services).unwrap_err();
    assert!(scene.contains("did you mean BRB"), "{scene}");
}

#[test]
fn every_action_needs_broadcast_and_a_dry_run_sends_nothing() {
    crate::link();
    let actions = [
        "obs.connect",
        "obs.disconnect",
        "obs.scene.set",
        "obs.source.set_visible",
        "obs.stream.start",
        "obs.stream.stop",
        "obs.record.start",
        "obs.record.stop",
        "obs.replay.start",
        "obs.replay.stop",
        "obs.replay.save",
    ];
    for name in actions {
        let meta = (find(name).unwrap().meta)();
        assert_eq!(meta.scope, Scope::Broadcast, "{name}");
        assert_eq!(meta.undo, omuzaki_ops_api::Undo::None, "{name}");
    }
    for name in ["obs.status", "obs.scenes", "obs.sources"] {
        assert_eq!((find(name).unwrap().meta)().scope, Scope::Read, "{name}");
    }
    assert!(Grants::person().allows(Scope::Broadcast));
    assert!(!Grants::agent_default().allows(Scope::Broadcast));
    assert!(Grants::agent_default().with(Scope::Broadcast).allows(Scope::Broadcast));

    let fake = FakeObs::start(FakeConfig::default());
    let service = Arc::new(ObsService::new());
    let services = rig(&service, fake.port(), false);
    call("obs.connect", json!({}), &services).unwrap();
    let before = fake.received().len();
    for (name, args) in [("obs.stream.start", json!({})), ("obs.scene.set", json!({ "scene": "BRB" }))] {
        assert_eq!(run(name, args, &services, true).unwrap()["applied"], false);
    }
    assert_eq!(fake.received().len(), before);
}

#[test]
fn a_restarted_obs_is_reconnected_on_the_backoff() {
    let mut fake = FakeObs::start(FakeConfig::default());
    let service = ObsService::with_backoff_unit(Duration::from_millis(10));
    service.connect(ClientConfig::at("127.0.0.1", fake.port()), None).unwrap();
    let events = service.subscribe();
    fake.down();
    // Down for long enough to walk the whole schedule: 10 + 20 + 40 + 80 + 160 + 300 + 300 ms.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while service.retries().len() < 7 && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    let units: Vec<u128> = service.retries().iter().take(7).map(|d| d.as_millis() / 10).collect();
    assert_eq!(units, [1, 2, 4, 8, 16, 30, 30]);
    assert!(matches!(service.status(), ObsStatus::Retrying { .. } | ObsStatus::Connecting));
    fake.up();
    while service.client().is_none() && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(matches!(service.status(), ObsStatus::Connected { .. }), "{:?}", service.status());
    // The subscription made before the drop hears the new connection.
    fake.set_program_scene("BRB");
    assert_eq!(
        events.recv_timeout(Duration::from_secs(5)),
        Some(ObsEvent::CurrentProgramSceneChanged { scene_name: "BRB".into() })
    );
    assert_eq!((backoff(0), backoff(5), backoff(99)), (1, 30, 30));
}

#[test]
fn a_session_a_person_ended_in_obs_is_not_reconnected() {
    let fake = FakeObs::start(FakeConfig::default());
    let service = ObsService::with_backoff_unit(Duration::from_millis(5));
    service.connect(ClientConfig::at("127.0.0.1", fake.port()), None).unwrap();
    fake.kick();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !matches!(service.status(), ObsStatus::Failed { .. }) && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    let ObsStatus::Failed { reason } = service.status() else { panic!("{:?}", service.status()) };
    assert!(reason.contains("obs.connect"), "{reason}");
    assert!(service.retries().is_empty(), "no reconnect after a kick");
    assert!(service.client().is_none());
    // A person connects again.
    service.connect(ClientConfig::at("127.0.0.1", fake.port()), None).unwrap();
    assert!(matches!(service.status(), ObsStatus::Connected { .. }));
}

#[test]
fn a_wrong_password_is_never_retried_and_never_shown() {
    let fake = locked();
    let service = ObsService::with_backoff_unit(Duration::from_millis(5));
    let error =
        service.connect(ClientConfig::at("127.0.0.1", fake.port()), Some(SecretValue::new("not-it"))).unwrap_err();
    assert!(matches!(error, ObsError::Auth(_)), "{error:?}");
    assert!(matches!(service.status(), ObsStatus::AuthFailed { .. }));
    std::thread::sleep(Duration::from_millis(100));
    assert!(service.retries().is_empty(), "a refused password is not retried");
    let none = service.connect(ClientConfig::at("127.0.0.1", fake.port()), None).unwrap_err();
    assert!(none.to_string().contains("none is stored"), "{none}");
    for shown in [format!("{error}"), format!("{error:?}"), format!("{:?}", service.status()), format!("{service:?}")] {
        assert!(!shown.contains("not-it"), "{shown}");
    }
}

#[test]
fn another_computer_needs_allow_remote() {
    let error = ObsClient::open(&ClientConfig::at("192.0.2.1", 4455), None, Arc::new(EventHub::new())).unwrap_err();
    assert!(matches!(error, ObsError::RemoteNotAllowed(_)), "{error:?}");
    let fake = FakeObs::start(FakeConfig::default());
    let local = ObsClient::connect("localhost", fake.port(), None).unwrap();
    assert!(!local.unencrypted());
}

#[test]
fn a_subscriber_that_never_reads_keeps_the_newest_events() {
    let fake = FakeObs::start(FakeConfig::default());
    let client = ObsClient::connect("127.0.0.1", fake.port(), None).unwrap();
    let events = client.subscribe();
    for i in 0..300 {
        fake.set_program_scene(if i % 2 == 0 { "BRB" } else { "Main" });
    }
    // A request after the events: its answer comes after them, so they have all been handed out.
    client.request(&GetSceneList {}).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while events.lagged() < 44 && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(events.lagged(), 300 - EVENT_BACKLOG as u64);
    let mut kept = 0;
    while events.try_recv().is_some() {
        kept += 1;
    }
    assert_eq!(kept, EVENT_BACKLOG);
}

#[test]
fn a_slow_answer_times_out_and_the_next_call_is_fine() {
    let fake = FakeObs::start(FakeConfig::default());
    let config =
        ClientConfig { request_timeout: Duration::from_millis(150), ..ClientConfig::at("127.0.0.1", fake.port()) };
    let client = ObsClient::open(&config, None, Arc::new(EventHub::new())).unwrap();
    fake.delay("GetSceneList", Duration::from_millis(400));
    assert!(matches!(client.request(&GetSceneList {}), Err(ObsError::Timeout(_))));
    let version = client.request(&crate::protocol::GetVersion {}).unwrap();
    assert_eq!(version.obs_web_socket_version, crate::testing::FAKE_VERSION);
}

/// A document whose scene `stage` casts `members` (member id, character) and whose characters have the given
/// `sources/avatar` terms (`None`: no row).
fn cast_doc(members: &[(&str, &str)], terms: &[(&str, Option<omuzaki_io_api::CommercialUsage>)]) -> MemDoc {
    let cast: serde_json::Map<String, Value> =
        members.iter().map(|(m, c)| ((*m).to_owned(), json!({ "character": c }))).collect();
    let characters: serde_json::Map<String, Value> = terms
        .iter()
        .map(|(c, usage)| {
            let node = usage.map_or_else(
                || json!({ "name": c }),
                |usage| {
                    let terms =
                        omuzaki_io_api::UseTerms { commercial_usage: usage, ..omuzaki_io_api::UseTerms::default() };
                    json!({ "name": c, "sources": { super::golive::AVATAR_PART: { "schema": 1, "terms": terms } } })
                },
            );
            ((*c).to_owned(), node)
        })
        .collect();
    MemDoc::from_value(json!({ "scenes": { "stage": { "cast": cast } }, "characters": characters }))
}

fn start_stream(doc: &MemDoc, services: &Services, dry_run: bool) -> Value {
    crate::link();
    let op = (find("obs.stream.start").unwrap().decode)(json!({})).unwrap();
    let (clock, actor) = (FakeClock::new(Seconds(0.0)), Actor::Person { name: "host".into() });
    let ctx = OpCtx::new(doc, &actor, &clock).with_services(services).with_dry_run(dry_run);
    op.plan(&ctx).unwrap().output.unwrap()
}

#[test]
fn going_live_with_a_non_profit_model_warns_and_still_starts_the_stream() {
    use omuzaki_io_api::CommercialUsage::{PersonalNonProfit, PersonalProfit};
    assert_eq!(super::golive::AVATAR_PART, omuzaki_avatar_api::anchors::AVATAR_PART);
    let fake = FakeObs::start(FakeConfig::default());
    let service = Arc::new(ObsService::new());
    let services = rig(&service, fake.port(), false);
    call("obs.connect", json!({}), &services).unwrap();

    // One non-profit model among a profit-allowed one, a kit character (no row) and a repeat: one warning, naming it.
    let doc = cast_doc(
        &[("a", "ava"), ("b", "bea"), ("c", "kit"), ("d", "ava")],
        &[("ava", Some(PersonalNonProfit)), ("bea", Some(PersonalProfit)), ("kit", None)],
    );
    let dry = start_stream(&doc, &services, true);
    assert_eq!(dry["warnings"].as_array().unwrap().len(), 1, "{dry}");
    assert_eq!(fake.count("StartStream"), 0, "a dry run sends nothing");
    let live = start_stream(&doc, &services, false);
    let warnings = live["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{live}");
    assert_eq!(warnings[0]["rule"], "rights.perform_non_profit");
    assert_eq!(warnings[0]["severity"], "warning");
    assert_eq!(warnings[0]["location"], "characters/ava/sources/avatar");
    assert!(warnings[0]["message"].as_str().unwrap().contains("`ava`"));
    assert_eq!(fake.count("StartStream"), 1, "the notice never blocks");
    assert_eq!(fake.state().stream, Output::Started);
    call("obs.stream.stop", json!({}), &services).unwrap();

    // Two non-profit models: two warnings, in cast order.
    let doc =
        cast_doc(&[("a", "zoe"), ("b", "ava")], &[("ava", Some(PersonalNonProfit)), ("zoe", Some(PersonalNonProfit))]);
    let both = start_stream(&doc, &services, false);
    let at: Vec<&str> = both["warnings"].as_array().unwrap().iter().map(|w| w["location"].as_str().unwrap()).collect();
    assert_eq!(at, ["characters/zoe/sources/avatar", "characters/ava/sources/avatar"]);
    call("obs.stream.stop", json!({}), &services).unwrap();

    // Nothing to say: the output is exactly what it was before the notice.
    let doc = cast_doc(&[("a", "bea"), ("b", "kit")], &[("bea", Some(PersonalProfit)), ("kit", None)]);
    let quiet = start_stream(&doc, &services, false);
    assert_eq!(
        quiet,
        json!({ "applied": true, "output": "stream", "output_active": true, "state": "started", "already": false })
    );
}
