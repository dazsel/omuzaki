//! Drives the mock `VTube Studio` server with raw JSON over a real loopback WebSocket. The clock is a `FakeClock`, so
//! nothing here sleeps: each test waits for a reply, then moves time.

// A bad fixture or a dead socket must fail the test loudly.
#![allow(clippy::expect_used, clippy::needless_pass_by_value)]

use std::net::{TcpListener, TcpStream};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread::{self, Builder};
use std::time::Duration;

use super::{Event, FakeClock, MockConfig, MockHotkey, MockModel, MockVts, Running, Stop, accept_loop};
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
        self.login_as("Omuzaki", "Omuzaki Studio");
    }

    /// Like [`Client::login`], for another plugin identity.
    fn login_as(&mut self, plugin: &str, developer: &str) {
        let token =
            self.call("AuthenticationTokenRequest", json!({"pluginName": plugin, "pluginDeveloper": developer}));
        let token = token["data"]["authenticationToken"].as_str().expect("a token").to_owned();
        let auth = self.call(
            "AuthenticationRequest",
            json!({"pluginName": plugin, "pluginDeveloper": developer, "authenticationToken": token}),
        );
        assert_eq!(auth["data"]["authenticated"], true);
    }

    fn create(&mut self, name: &str, max: f64) -> Value {
        self.call(
            "ParameterCreationRequest",
            json!({"parameterName": name, "min": 0.0, "max": max, "defaultValue": 0.0}),
        )
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
        hotkeys: (0..hotkeys)
            .map(|i| MockHotkey { id: format!("hk{i}"), name: format!("Wave{i}"), ..MockHotkey::default() })
            .collect(),
        expressions: vec!["smile".into()],
        parameters: vec!["MouthOpen".into()],
    }
}

fn mock(hotkeys: usize) -> (MockVts, FakeClock) {
    let clock = FakeClock::default();
    let config = MockConfig { model: Some(model(hotkeys)), clock: Arc::new(clock.clone()), ..MockConfig::default() };
    (MockVts::start(&config).expect("starts"), clock)
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
    assert_eq!(create(&mut client, 0)["messageType"], "ParameterCreationResponse", "updating one at the cap is fine");
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
    // No model is a normal answer with `modelLoaded: false` and an empty list, not an error.
    let list = client.call("HotkeysInCurrentModelRequest", json!({}));
    assert_eq!(list["messageType"], "HotkeysInCurrentModelResponse");
    assert_eq!(list["data"]["modelLoaded"], false);
    assert_eq!(list["data"]["availableHotkeys"], json!([]));
    let state = client.call("ExpressionStateRequest", json!({}));
    assert_eq!(state["messageType"], "ExpressionStateResponse");
    assert_eq!(state["data"]["modelLoaded"], false);
    assert_eq!(state["data"]["expressions"], json!([]));
    // Changing an expression still needs a model.
    let on = client.call("ExpressionActivationRequest", json!({"expressionFile": "smile.exp3.json", "active": true}));
    assert_eq!(error_id(&on), 652);
}

#[test]
fn on_screen_buttons_are_minus_one_unless_the_config_assigns_one_to_eight() {
    let (mock, _) = mock(0);
    let hotkey = |id: &str, button| MockHotkey { id: id.into(), name: id.into(), on_screen_button: button };
    mock.set_model(Some(MockModel {
        hotkeys: vec![
            hotkey("none", None),
            hotkey("one", Some(1)),
            hotkey("eight", Some(8)),
            hotkey("zero", Some(0)),
            hotkey("nine", Some(9)),
        ],
        ..model(0)
    }));
    let mut client = Client::connect(&mock);
    client.login();
    let list = client.call("HotkeysInCurrentModelRequest", json!({}));
    let buttons: Vec<_> = list["data"]["availableHotkeys"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|h| h["onScreenButtonID"].clone())
        .collect();
    assert_eq!(buttons, [json!(-1), json!(1), json!(8), json!(-1), json!(-1)]);
}

#[test]
fn a_frame_for_another_api_or_version_is_refused_before_the_request_is_read() {
    let (mock, _) = mock(1);
    let mut client = Client::connect(&mock);
    let mut send = |name: &str, version: &str| {
        let frame = json!({"apiName": name, "apiVersion": version, "requestID": "x", "messageType": "APIStateRequest", "data": {}});
        client.ws.send(Message::text(frame.to_string())).expect("sends");
        let Message::Text(text) = client.ws.read().expect("a reply arrives") else { panic!("a text reply") };
        serde_json::from_str::<Value>(&text).expect("the reply is JSON")
    };
    assert_eq!(error_id(&send("SomeOtherAPI", "1.0")), 3);
    assert_eq!(error_id(&send("VTubeStudioPublicAPI", "2.0")), 4);
    assert_eq!(send("VTubeStudioPublicAPI", "1.0")["messageType"], "APIStateResponse");
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

#[test]
fn uptime_is_milliseconds_and_the_timestamp_is_epoch_milliseconds() {
    let (mock, clock) = mock(1);
    let mut client = Client::connect(&mock);
    client.login();
    clock.advance(Duration::from_millis(1500));
    let stats = client.call("StatisticsRequest", json!({}));
    assert_eq!(stats["data"]["uptime"], 1500);
    assert_eq!(stats["timestamp"], 1_700_000_000_000_i64 + 1500, "the epoch base plus the elapsed time");
}

#[test]
fn expression_state_honours_the_file_filter_and_rejects_bad_data() {
    let (mock, _) = mock(1);
    let mut client = Client::connect(&mock);
    client.login();
    mock.set_model(Some(MockModel { expressions: vec!["smile".into(), "frown".into()], ..model(1) }));
    let all = client.call("ExpressionStateRequest", json!({"details": false}));
    assert_eq!(all["data"]["expressions"].as_array().map(Vec::len), Some(2));
    let one = client.call("ExpressionStateRequest", json!({"details": false, "expressionFile": "frown.exp3.json"}));
    assert_eq!(one["data"]["expressions"].as_array().map(Vec::len), Some(1));
    assert_eq!(one["data"]["expressions"][0]["name"], "frown");
    assert_eq!(error_id(&client.call("ExpressionStateRequest", json!({"details": "yes"}))), 1);
}

#[test]
fn a_token_only_works_for_the_plugin_it_was_issued_to() {
    let (mock, _) = mock(1);
    let mut client = Client::connect(&mock);
    let token = client
        .call("AuthenticationTokenRequest", json!({"pluginName": "Omuzaki", "pluginDeveloper": "Omuzaki Studio"}));
    let token = token["data"]["authenticationToken"].clone();
    for (name, developer) in [("Omuzaki", "Someone Else"), ("Other Plugin", "Omuzaki Studio")] {
        let auth = client.call(
            "AuthenticationRequest",
            json!({"pluginName": name, "pluginDeveloper": developer, "authenticationToken": token}),
        );
        assert_eq!(auth["data"]["authenticated"], false, "{name} / {developer}");
    }
    let auth = client.call(
        "AuthenticationRequest",
        json!({"pluginName": "Omuzaki", "pluginDeveloper": "Omuzaki Studio", "authenticationToken": token}),
    );
    assert_eq!(auth["data"]["authenticated"], true);
}

#[test]
fn creating_a_parameter_again_updates_it_only_for_its_owner() {
    let (mock, _) = mock(1);
    let (mut owner, mut other) = (Client::connect(&mock), Client::connect(&mock));
    owner.login();
    other.login_as("Other Plugin", "Other Dev");
    assert_eq!(owner.create("Shared01", 1.0)["messageType"], "ParameterCreationResponse");
    assert_eq!(owner.create("Shared01", 5.0)["messageType"], "ParameterCreationResponse", "the owner may update");
    let listed = owner.call("InputParameterListRequest", json!({}));
    assert_eq!(listed["data"]["customParameters"][0]["max"], 5.0);
    assert_eq!(error_id(&other.create("Shared01", 9.0)), 452, "another plugin's parameter");
    assert_eq!(error_id(&owner.create("MouthOpen", 1.0)), 452, "a built-in parameter");
}

#[test]
fn owners_are_reported_in_the_list_and_in_a_single_lookup() {
    let (mock, _) = mock(1);
    let mut client = Client::connect(&mock);
    client.login();
    client.create("Custom001", 1.0);
    let listed = client.call("InputParameterListRequest", json!({}));
    assert_eq!(listed["data"]["defaultParameters"][0]["addedBy"], "VTube Studio");
    assert_eq!(listed["data"]["customParameters"][0]["addedBy"], "Omuzaki");
    let one = |client: &mut Client, name: &str| client.call("ParameterValueRequest", json!({"name": name}));
    assert_eq!(one(&mut client, "Custom001")["data"]["addedBy"], "Omuzaki");
    assert_eq!(one(&mut client, "MouthOpen")["data"]["addedBy"], "VTube Studio");
}

#[test]
fn injections_compose_across_plugins() {
    let (mock, _) = mock(1);
    let (mut a, mut b) = (Client::connect(&mock), Client::connect(&mock));
    a.login();
    b.login_as("Other Plugin", "Other Dev");
    assert_eq!(a.inject("add", "MouthOpen", 0.25)["messageType"], "InjectParameterDataResponse");
    assert_eq!(b.inject("add", "MouthOpen", 0.5)["messageType"], "InjectParameterDataResponse");
    assert_eq!(mock.param_value("MouthOpen"), Some(0.75), "add stacks across plugins");

    let weighted = a.call(
        "InjectParameterDataRequest",
        json!({"faceFound": true, "mode": "set", "parameterValues": [{"id": "MouthOpen", "value": 1.0, "weight": 0.5}]}),
    );
    assert_eq!(weighted["messageType"], "InjectParameterDataResponse");
    assert_eq!(
        mock.param_value("MouthOpen"),
        Some(0.5 * 0.0 + 0.5 * 1.0 + 0.5),
        "tracking 0, weight 0.5, plus b's add"
    );
    assert_eq!(error_id(&b.inject("set", "MouthOpen", 0.1)), 454, "a live set from another plugin owns the parameter");
    let read = a.call("ParameterValueRequest", json!({"name": "MouthOpen"}));
    assert_eq!(read["data"]["value"], 1.0);
}

#[test]
fn hotkeys_resolve_by_id_or_name_ignoring_case_and_share_one_cooldown() {
    let (mock, clock) = mock(2);
    let mut client = Client::connect(&mock);
    client.login();
    let reply = client.trigger("wAvE0");
    assert_eq!(reply["data"]["hotkeyID"], "hk0", "the response carries the canonical id");
    clock.advance(Duration::from_millis(20));
    assert_eq!(error_id(&client.trigger("hk0")), 203, "the id shares the cooldown of the name");
    assert_eq!(error_id(&client.trigger("nope")), 202);
}

#[test]
fn a_failed_handshake_is_recorded_and_a_healthy_run_has_no_failures() {
    let (mut mock, _) = mock(1);
    let mut client = Client::connect(&mock);
    client.call("APIStateRequest", json!({}));
    assert_eq!(mock.setup_failures(), Vec::<String>::new());

    let mut raw = TcpStream::connect(("127.0.0.1", mock.port())).expect("connects");
    std::io::Write::write_all(&mut raw, b"GET / HTTP/1.1\r\n\r\n").expect("writes");
    drop(raw);
    // `stop` joins every connection thread, so the failure is recorded by the time it returns.
    mock.stop().expect("stops cleanly");
    let failures = mock.setup_failures();
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].contains("handshake"), "{failures:?}");
}

#[test]
fn an_empty_expression_file_lists_every_expression() {
    let (mock, _) = mock(1);
    let mut client = Client::connect(&mock);
    client.login();
    mock.set_model(Some(MockModel { expressions: vec!["smile".into(), "frown".into()], ..model(1) }));
    let reply = client.call("ExpressionStateRequest", json!({"details": false, "expressionFile": ""}));
    assert_eq!(reply["data"]["expressions"].as_array().map(Vec::len), Some(2), "{reply}");
}

#[test]
fn the_value_observer_lets_an_expired_feed_lapse_before_it_reads() {
    let (mock, clock) = mock(1);
    let mut client = Client::connect(&mock);
    client.login();
    client.inject("set", "MouthOpen", 0.5);
    assert_eq!(mock.param_value("MouthOpen"), Some(0.5));
    // No request and no `events` call in between: the observer alone has to notice the lapse.
    clock.advance(Duration::from_secs(1));
    assert_ne!(mock.param_value("MouthOpen"), Some(0.5), "the feed lapsed a second ago");
}

#[test]
fn stop_reports_an_acceptor_that_panicked_even_when_the_wake_up_is_refused() {
    let (mut mock, _) = mock(1);
    mock.stop().expect("stops the real acceptor");
    // A port nothing listens on, so the wake-up connect is refused, as it is once a panicking acceptor drops its
    // listener.
    let addr = TcpListener::bind(("127.0.0.1", 0)).and_then(|l| l.local_addr()).expect("binds");
    #[allow(clippy::panic)]
    let acceptor = Builder::new().spawn(|| panic!("the acceptor panics on purpose")).expect("spawns");
    mock.running = Some(Running {
        addr,
        stopping: Arc::new(AtomicBool::new(false)),
        wake: Arc::new(Mutex::new(None)),
        acceptor,
        conns: Arc::new(Mutex::new(Vec::new())),
    });
    assert!(mock.stop().is_err(), "the panic must not be lost");
}

#[test]
fn a_client_connected_just_before_stop_is_still_served_and_recorded() {
    let (mock, _) = mock(1);
    // Drives the accept loop by hand so the order is exact: the client is already queued when `stop` sets its flag,
    // and the wake-up connection comes after it.
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("binds");
    let addr = listener.local_addr().expect("address");
    let pending = TcpStream::connect(addr).expect("the client connects");
    pending.set_read_timeout(Some(Duration::from_secs(5))).expect("sets a read timeout");
    let stopping = AtomicBool::new(true);
    let wake_up = TcpStream::connect(addr).expect("the wake-up connects");
    let from = Mutex::new(wake_up.local_addr().ok());
    let conns = Mutex::new(Vec::new());
    thread::scope(|scope| {
        let url = format!("ws://{addr}");
        let client = scope.spawn(move || tungstenite::client(url, pending).is_ok());
        accept_loop(&listener, &mock.shared, &Stop { flag: &stopping, wake: &from }, &conns);
        assert!(client.join().expect("client thread"), "the queued client is served, not dropped");
    });
    let conns = std::mem::take(&mut *conns.lock().expect("lock"));
    assert_eq!(conns.len(), 1, "the client's connection was recorded");
    for conn in conns {
        conn.stream.shutdown(std::net::Shutdown::Both).ok();
        conn.thread.join().expect("the connection thread ends");
    }
    assert_eq!(mock.setup_failures(), Vec::<String>::new());
}
