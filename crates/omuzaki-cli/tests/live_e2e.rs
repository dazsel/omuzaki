//! The broadcast spine end to end in one process, with the real executor over a saved project: a person connects OBS
//! and Twitch and makes a show whose rule says "when the BRB reward is redeemed, switch OBS to BRB", all as ops; the
//! automation engine runs against the same executor; a viewer's redeem arrives over a fake Twitch `EventSub` socket,
//! and the rule's `op.run` switches the fake OBS to BRB. The switch is in the op log, by the show, with the rule as
//! its cause.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used)]

mod common;

use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use omuzaki_cli::project::SavedProject;
use omuzaki_core::time::SystemClock;
use omuzaki_http_api::{HttpTransport, Method};
use omuzaki_http_mock::FakeHttp;
use omuzaki_live::Automation;
use omuzaki_live_api::event::EventSink;
use omuzaki_obs::service::ObsService;
use omuzaki_obs::testing::{FakeConfig, FakeObs};
use omuzaki_ops::Executor;
use omuzaki_ops_api::{Actor, CauseKind, Grants, Mode, Request, Runner, ServicesBuilder};
use omuzaki_secrets_api::prelude::{SecretLabel, SecretStore, SecretValue};
use omuzaki_secrets_mock::MemorySecrets;
use omuzaki_twitch::helix::Helix;
use omuzaki_twitch::{Endpoints, TwitchService};
use serde_json::{Value, json};
use tungstenite::{Message, Utf8Bytes, WebSocket};

const API: &str = "http://helix.test";
const OAUTH: &str = "http://oauth.test";

fn send(ws: &mut WebSocket<TcpStream>, m: &Value) {
    ws.send(Message::Text(Utf8Bytes::from(m.to_string()))).expect("the fake Twitch sends");
}

/// A fake Twitch `EventSub` socket: one session, a welcome, then one channel-points redeem of `title`.
fn fake_eventsub(title: &'static str) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let url = format!("ws://127.0.0.1:{}/ws", listener.local_addr().expect("its address").port());
    let thread = std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else { return };
        let Ok(mut ws) = tungstenite::accept(stream) else { return };
        send(
            &mut ws,
            &json!({
                "metadata": { "message_id": "w1", "message_type": "session_welcome" },
                "payload": { "session": { "id": "S1", "keepalive_timeout_seconds": 10 } },
            }),
        );
        send(
            &mut ws,
            &json!({
                "metadata": { "message_id": "n1", "message_type": "notification",
                              "subscription_type": "channel.channel_points_custom_reward_redemption.add" },
                "payload": { "event": {
                    "id": "redemption-1", "broadcaster_user_id": "1001", "broadcaster_user_login": "kitdemo",
                    "user_id": "2002", "user_login": "viewer", "user_name": "Viewer", "user_input": "",
                    "reward": { "id": "r1", "title": title, "cost": 100 },
                } },
            }),
        );
        while let Ok(m) = ws.read() {
            if matches!(m, Message::Close(_)) {
                break;
            }
        }
    });
    (url, thread)
}

fn person(runner: &Executor, op: &str, args: Value) -> Value {
    let request = Request {
        op: op.to_owned(),
        args,
        mode: Mode::Apply,
        actor: Actor::Person { name: "host".to_owned() },
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    match runner.run(request) {
        Ok(outcome) => outcome.plan.output.unwrap_or(Value::Null),
        Err(e) => panic!("{op} failed: {} ({:?})", e.message, e.hint),
    }
}

fn until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_twitch_redeem_switches_the_obs_scene_through_the_show() {
    omuzaki_live::link();
    omuzaki_live_nodes::link();
    omuzaki_obs::link();
    omuzaki_twitch::link();

    let obs = FakeObs::start(FakeConfig::default());
    let (socket, twitch_thread) = fake_eventsub("BRB");
    let helix: Arc<dyn HttpTransport> = Arc::new(
        FakeHttp::new()
            .json(
                Method::Get,
                &format!("{OAUTH}/validate"),
                200,
                json!({
                    "client_id": "test-client", "login": "kitdemo", "user_id": "1001",
                    "scopes": ["channel:read:redemptions"], "expires_in": 3600,
                }),
            )
            .json(Method::Post, &format!("{API}/eventsub/subscriptions"), 202, json!({ "data": [] })),
    );

    let automation = Arc::new(Automation::new());
    let sink: Arc<dyn EventSink> = Arc::clone(&automation) as Arc<dyn EventSink>;
    let endpoints = Endpoints { socket, helix: Helix::new(helix, API, OAUTH) };
    let twitch = Arc::new(TwitchService::with_endpoints(endpoints, sink));
    let secrets = Arc::new(MemorySecrets::new());
    secrets
        .set(&SecretLabel::new("twitch/token").expect("a label"), SecretValue::new("test-only-token"))
        .expect("stored");
    let services = ServicesBuilder::new()
        .insert::<ObsService>(Arc::new(ObsService::new()))
        .insert::<Automation>(Arc::clone(&automation))
        .insert::<TwitchService>(Arc::clone(&twitch))
        .insert::<dyn SecretStore>(secrets)
        .build();

    let dir = common::new_project();
    let doc = SavedProject::open(dir.path()).expect("the project opens");
    let runner = Arc::new(
        Executor::new(Box::new(doc), Arc::new(SystemClock::new()))
            .expect("the executor builds")
            .with_services(services),
    );
    automation.attach(&(Arc::clone(&runner) as Arc<dyn Runner>));

    // The person's setup, every step an op.
    person(&runner, "obs.connect", json!({ "port": obs.port() }));
    person(&runner, "show.create", json!({ "id": "main", "name": "Main show" }));
    person(
        &runner,
        "automation.add_graph",
        json!({
            "id": "brb",
            "graph": {
                "graph": { "name": "Be right back" },
                "nodes": {
                    "redeem": { "kind": "on.event", "params": { "kind": "redeem", "where": { "reward.title": "BRB" } } },
                    "switch": {
                        "kind": "op.run",
                        "params": { "op": "obs.scene.set", "args": { "scene": "BRB" } },
                        "in": { "in": "redeem.fired" },
                    },
                },
            },
        }),
    );
    let started = person(&runner, "automation.start", json!({}));
    assert_eq!(started["graphs"].as_array().map(Vec::len), Some(1), "{started}");
    person(&runner, "twitch.connect", json!({}));

    // A viewer redeems BRB on Twitch.
    until("OBS to switch to BRB", || obs.state().program == "BRB");
    let log = runner.log(0, 100, &Grants::person()).expect("the op log");
    let switch = log.iter().find(|r| r.op == "obs.scene.set").expect("the switch is in the op log");
    assert_eq!(switch.args, json!({ "scene": "BRB" }));
    assert_eq!(switch.cause.as_ref().map(|c| c.kind), Some(CauseKind::Rule));
    assert!(!matches!(switch.actor, Actor::Person { .. }), "the show acted, not the person: {:?}", switch.actor);

    let status = person(&runner, "automation.status", json!({}));
    assert_eq!(status["running"]["show"], "main", "{status}");
    person(&runner, "twitch.disconnect", json!({}));
    person(&runner, "automation.stop", json!({}));
    twitch_thread.join().expect("the fake Twitch ends");
}
