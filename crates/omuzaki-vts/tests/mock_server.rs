//! Drives the mock `VTube Studio` server with raw JSON over a real loopback WebSocket. The clock is a `FakeClock`, so
//! nothing here sleeps: each test waits for a reply, then moves time.

// A bad fixture or a dead socket must fail the test loudly.
#![allow(clippy::expect_used, clippy::needless_pass_by_value)]

use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use omuzaki_vts::testing::{Event, FakeClock, MockConfig, MockModel, MockVts};
use serde_json::{Value, json};
use tungstenite::{Message, WebSocket};

struct Client {
    ws: WebSocket<TcpStream>,
    next: u32,
}

impl Client {
    fn connect(mock: &MockVts) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", mock.port())).expect("connects");
        // A missing reply fails the test instead of hanging it.
        stream.set_read_timeout(Some(Duration::from_secs(5))).expect("sets a read timeout");
        let (ws, _) = tungstenite::client(mock.url(), stream).expect("handshakes");
        Self { ws, next: 0 }
    }

    /// Sends one request and returns the whole reply frame.
    fn call(&mut self, message_type: &str, data: Value) -> Value {
        self.next += 1;
        let frame = json!({
            "apiName": "VTubeStudioPublicAPI", "apiVersion": "1.0", "requestID": format!("r{}", self.next),
            "messageType": message_type, "data": data
        });
        self.ws.send(Message::text(frame.to_string())).expect("sends");
        let Message::Text(text) = self.ws.read().expect("a reply arrives") else { panic!("a text reply") };
        let reply: Value = serde_json::from_str(&text).expect("the reply is JSON");
        assert_eq!(reply["requestID"], format!("r{}", self.next));
        reply
    }

    /// Requests a token and authenticates with it.
    fn login(&mut self) {
        let token = self
            .call("AuthenticationTokenRequest", json!({"pluginName": "Omuzaki", "pluginDeveloper": "Omuzaki Studio"}));
        let token = token["data"]["authenticationToken"].as_str().expect("a token").to_owned();
        let auth = self.call(
            "AuthenticationRequest",
            json!({"pluginName": "Omuzaki", "pluginDeveloper": "Omuzaki Studio", "authenticationToken": token}),
        );
        assert_eq!(auth["data"]["authenticated"], true);
    }

    fn inject(&mut self, mode: &str, id: &str, value: f64) -> Value {
        self.call(
            "InjectParameterDataRequest",
            json!({"faceFound": true, "mode": mode, "parameterValues": [{"id": id, "value": value}]}),
        )
    }

    fn trigger(&mut self, id: &str) -> Value {
        self.call("HotkeyTriggerRequest", json!({"hotkeyID": id}))
    }
}

fn model(hotkeys: usize) -> MockModel {
    MockModel {
        name: "Kit".into(),
        hotkeys: (0..hotkeys).map(|i| format!("hk{i}")).collect(),
        expressions: vec!["smile".into()],
        parameters: vec!["MouthOpen".into()],
    }
}

fn mock(hotkeys: usize) -> (MockVts, FakeClock) {
    let clock = FakeClock::default();
    let config = MockConfig { model: Some(model(hotkeys)), clock: Arc::new(clock.clone()), ..MockConfig::default() };
    (MockVts::start(config).expect("starts"), clock)
}

fn error_id(reply: &Value) -> i64 {
    assert_eq!(reply["messageType"], "APIError", "{reply}");
    reply["data"]["errorID"].as_i64().expect("an error id")
}

#[test]
fn approve_authenticates_and_deny_does_not() {
    let (mock, _) = mock(1);
    let mut client = Client::connect(&mock);
    assert_eq!(error_id(&client.call("CurrentModelRequest", json!({}))), 8, "requests need a login");
    client.login();
    assert_eq!(client.call("CurrentModelRequest", json!({}))["data"]["modelName"], "Kit");

    mock.set_approve_token(false);
    let denied = client
        .call("AuthenticationTokenRequest", json!({"pluginName": "Omuzaki", "pluginDeveloper": "Omuzaki Studio"}));
    assert_eq!(error_id(&denied), 50);
}

#[test]
fn only_loopback_and_an_ephemeral_port() {
    let (mock, _) = mock(1);
    assert_ne!(mock.port(), 0);
    assert!(mock.url().starts_with("ws://127.0.0.1:"));
}

#[test]
fn revoke_fails_the_next_request_and_voids_the_token() {
    let (mock, _) = mock(1);
    let mut client = Client::connect(&mock);
    client.login();
    mock.revoke();
    assert_eq!(error_id(&client.call("StatisticsRequest", json!({}))), 8);
    let stale = client.call(
        "AuthenticationRequest",
        json!({"pluginName": "Omuzaki", "pluginDeveloper": "Omuzaki Studio", "authenticationToken": "mock-token-1"}),
    );
    assert_eq!(stale["data"]["authenticated"], false);
    client.login();
    assert_eq!(client.call("StatisticsRequest", json!({}))["data"]["framerate"], 60);
}

#[test]
fn set_conflict_errors_and_add_succeeds() {
    let (mock, _) = mock(1);
    let mut client = Client::connect(&mock);
    client.login();
    mock.claim_param("MouthOpen");
    assert_eq!(error_id(&client.inject("set", "MouthOpen", 0.5)), 454);
    assert_eq!(client.inject("add", "MouthOpen", 0.5)["messageType"], "InjectParameterDataResponse");
    assert_eq!(error_id(&client.inject("add", "Nope", 0.5)), 453);
    assert_eq!(error_id(&client.inject("add", "MouthOpen", 2_000_000.0)), 451);
}

#[test]
fn keepalive_lapses_at_one_second_and_not_before() {
    let (mock, clock) = mock(1);
    let mut client = Client::connect(&mock);
    client.login();
    assert_eq!(client.inject("set", "MouthOpen", 0.5)["messageType"], "InjectParameterDataResponse");
    clock.advance(Duration::from_millis(900));
    assert_eq!(mock.events().len(), 0, "no lapse at 0.9 s");
    clock.advance(Duration::from_millis(100));
    assert_eq!(mock.events(), [Event::keepalive_lapse("MouthOpen", Duration::from_secs(1))]);
}

#[test]
fn feeding_again_keeps_the_parameter() {
    let (mock, clock) = mock(1);
    let mut client = Client::connect(&mock);
    client.login();
    for _ in 0..3 {
        client.inject("set", "MouthOpen", 0.5);
        clock.advance(Duration::from_millis(900));
    }
    assert_eq!(mock.events().len(), 0, "a parameter fed every 0.9 s never lapses");
}

#[test]
fn custom_parameters_are_capped_at_100_per_plugin() {
    let (mock, _) = mock(1);
    let mut client = Client::connect(&mock);
    client.login();
    let create = |client: &mut Client, i: usize| {
        client.call(
            "ParameterCreationRequest",
            json!({"parameterName": format!("Custom{i:03}"), "min": 0.0, "max": 1.0, "defaultValue": 0.0}),
        )
    };
    for i in 0..100 {
        assert_eq!(create(&mut client, i)["messageType"], "ParameterCreationResponse");
    }
    assert_eq!(error_id(&create(&mut client, 100)), 450);
    assert_eq!(error_id(&create(&mut client, 0)), 452, "a duplicate name is refused");
}

#[test]
fn a_hotkey_twice_within_five_frames_is_refused() {
    let (mock, clock) = mock(2);
    let mut client = Client::connect(&mock);
    client.login();
    assert_eq!(client.trigger("hk0")["messageType"], "HotkeyTriggerResponse");
    clock.advance(Duration::from_millis(80)); // 4.8 frames at 60 fps
    assert_eq!(error_id(&client.trigger("hk0")), 203);
    assert_eq!(client.trigger("hk1")["messageType"], "HotkeyTriggerResponse", "another hotkey is unaffected");
    clock.advance(Duration::from_millis(4));
    assert_eq!(client.trigger("hk0")["messageType"], "HotkeyTriggerResponse", "5 frames have passed");
    mock.set_framerate(30);
    clock.advance(Duration::from_millis(100));
    assert_eq!(error_id(&client.trigger("hk0")), 203, "5 frames at 30 fps is about 167 ms");
}

#[test]
fn at_most_32_hotkeys_queue() {
    let (mock, clock) = mock(40);
    let mut client = Client::connect(&mock);
    client.login();
    for i in 0..32 {
        assert_eq!(client.trigger(&format!("hk{i}"))["messageType"], "HotkeyTriggerResponse");
    }
    assert_eq!(error_id(&client.trigger("hk32")), 200);
    clock.advance(Duration::from_millis(17)); // one frame plays one queued hotkey
    assert_eq!(client.trigger("hk32")["messageType"], "HotkeyTriggerResponse");
}

#[test]
fn hotkeys_and_expressions_need_a_model() {
    let (mock, _) = mock(1);
    let mut client = Client::connect(&mock);
    client.login();
    let list = client.call("HotkeysInCurrentModelRequest", json!({}));
    assert_eq!(list["data"]["availableHotkeys"][0]["hotkeyID"], "hk0");
    let on = client.call(
        "ExpressionActivationRequest",
        json!({"expressionFile": "smile.exp3.json", "fadeTime": 0.25, "active": true}),
    );
    assert_eq!(on["messageType"], "ExpressionActivationResponse");
    let state = client.call("ExpressionStateRequest", json!({"details": false}));
    assert_eq!(state["data"]["expressions"][0]["active"], true);

    mock.set_model(None);
    assert_eq!(error_id(&client.call("HotkeysInCurrentModelRequest", json!({}))), 201);
    assert_eq!(error_id(&client.call("ExpressionStateRequest", json!({}))), 652);
}

#[test]
fn received_has_arrival_times_from_the_injected_clock() {
    let (mock, clock) = mock(1);
    let mut client = Client::connect(&mock);
    client.call("APIStateRequest", json!({}));
    clock.advance(Duration::from_millis(250));
    client.call("StatisticsRequest", json!({}));
    let seen: Vec<_> = mock.received().into_iter().map(|r| (r.at, r.message_type, r.request_id)).collect();
    assert_eq!(
        seen,
        [
            (Duration::ZERO, "APIStateRequest".to_owned(), "r1".to_owned()),
            (Duration::from_millis(250), "StatisticsRequest".to_owned(), "r2".to_owned()),
        ]
    );
}

#[test]
fn restart_keeps_the_port_and_drops_sessions() {
    let (mut mock, _) = mock(1);
    let port = mock.port();
    let mut client = Client::connect(&mock);
    client.login();
    mock.stop().expect("stops");
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err(), "nothing listens while stopped");
    mock.restart().expect("restarts");
    assert_eq!(mock.port(), port);
    assert!(mock.restart().is_err(), "a running server cannot restart");
    let mut again = Client::connect(&mock);
    assert_eq!(error_id(&again.call("StatisticsRequest", json!({}))), 8, "a new connection is a new session");
}

#[test]
fn a_bad_frame_and_an_unknown_message_get_errors() {
    let (mock, _) = mock(1);
    let mut client = Client::connect(&mock);
    client.ws.send(Message::text("not json")).expect("sends");
    let Message::Text(text) = client.ws.read().expect("a reply") else { panic!("text") };
    let reply: Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(error_id(&reply), 1);
    client.login();
    assert_eq!(error_id(&client.call("NoSuchRequest", json!({}))), 2);
}
