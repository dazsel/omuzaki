//! The bridge against `MockVts` (#935, #727, #728): every wait is on the bridge's poll counter or the mock's records,
//! never a sleep, and time is the mock's fake clock.

// Test setup may panic: a panic is a clear failure.
#![allow(clippy::unwrap_used, clippy::panic, clippy::unchecked_time_subtraction)]

use std::net::UdpSocket;

use omuzaki_anim_api::channel::{Channel, ChannelBuf};
use serde_json::json;

use super::*;
use crate::hotkeys::{Row, Target};
use crate::testing::{Event, FakeClock, MockConfig, MockHotkey, MockModel, MockVts};

const WAIT: Duration = Duration::from_secs(10);
const FRAME_60: Duration = Duration::from_nanos(16_666_667);

fn model() -> MockModel {
    MockModel {
        name: "Kit".into(),
        hotkeys: vec![
            MockHotkey { id: "h-smug".into(), name: "Smug".into(), on_screen_button: None },
            MockHotkey { id: "h-wave".into(), name: "Wave".into(), on_screen_button: Some(1) },
        ],
        expressions: vec!["Cry".into()],
        parameters: vec!["MouthOpen".into(), "MouthSmile".into(), "ParamAngleX".into()],
    }
}

struct Rig {
    mock: MockVts,
    clock: FakeClock,
    tokens: Arc<MemoryTokens>,
    client: Option<VtsClient>,
    map: ChannelMap,
    /// What the mock had received when the current client started.
    received_before: usize,
}

impl Rig {
    fn new() -> Self {
        Self::with_map(ChannelMap::builtin().unwrap())
    }

    fn with_map(map: ChannelMap) -> Self {
        let clock = FakeClock::default();
        let config = MockConfig { model: Some(model()), clock: Arc::new(clock.clone()), ..MockConfig::default() };
        let mock = MockVts::start(&config).unwrap();
        let mut rig =
            Self { mock, clock, tokens: Arc::new(MemoryTokens::default()), client: None, map, received_before: 0 };
        rig.spawn();
        rig
    }

    fn spawn(&mut self) {
        self.client = None;
        self.received_before = self.mock.received().len();
        let config = ClientConfig { port: Some(self.mock.port()), ..ClientConfig::default() };
        let client = VtsClient::spawn(config, self.tokens.clone(), Arc::new(self.clock.clone()), &self.map).unwrap();
        self.client = Some(client);
    }

    fn client(&self) -> &VtsClient {
        self.client.as_ref().unwrap()
    }

    fn connected(&self) -> Connected {
        match self.client().status().wait_for(WAIT, |s| matches!(s, BridgeStatus::Connected(_))) {
            Some(BridgeStatus::Connected(c)) => c,
            other => panic!("not connected: {other:?} (now {:?})", self.client().status().get()),
        }
    }

    fn connect(&self) -> Connected {
        self.client().connect();
        self.connected()
    }

    /// Advances the fake clock and waits until the bridge has looked at the new time.
    fn step(&self, by: Duration) {
        self.clock.advance(by);
        assert!(self.client().status().wait_polls(2, WAIT), "the bridge thread stopped polling");
        // The mock stamps a request with the fake clock when it reads it: wait until it has read everything sent,
        // so no request is stamped with a later step's time.
        let sent = usize::try_from(self.client().status().sent()).unwrap();
        let deadline = Instant::now() + WAIT;
        while self.mock.received().len() < self.received_before + sent {
            assert!(Instant::now() < deadline, "the mock stopped reading");
            std::thread::yield_now();
        }
    }

    fn count(&self, kind: &str) -> usize {
        self.mock.received().iter().filter(|r| r.message_type == kind).count()
    }

    /// Waits until the mock has received `n` messages of `kind`.
    fn until_count(&self, kind: &str, n: usize) {
        let deadline = Instant::now() + WAIT;
        while self.count(kind) < n {
            assert!(Instant::now() < deadline, "{kind}: {} of {n}", self.count(kind));
            assert!(self.client().status().wait_polls(1, WAIT));
        }
    }

    fn push(&self, jaw: f32) {
        let mut buf = ChannelBuf::default();
        buf.set(Channel::JAW_OPEN, jaw);
        self.client().injector().push(&buf, true);
    }
}

#[test]
fn approval_connects_with_the_model_and_the_next_session_reuses_the_token() {
    let mut rig = Rig::new();
    assert_eq!(rig.client().status().get(), BridgeStatus::Disconnected, "no token: waits for a person");
    let connected = rig.connect();
    assert_eq!(connected.model, Some(ModelInfo { id: "Kit".into(), name: "Kit".into() }));
    assert_eq!(connected.fps, 60);
    assert_eq!(connected.hotkeys.len(), 2);
    assert_eq!(rig.count("ParameterCreationRequest"), 76, "one custom parameter per channel");
    assert!(rig.tokens.get("default").is_some(), "the token is stored");
    assert_eq!(rig.mock.setup_failures(), Vec::<String>::new());
    // A new bridge with the stored token connects on its own, with no popup.
    rig.spawn();
    rig.connected();
    assert_eq!(rig.count("AuthenticationTokenRequest"), 1);
    assert_eq!(rig.count("AuthenticationRequest"), 2, "one per session: the new token, then the stored one");
}

#[test]
fn a_denied_popup_is_not_asked_again() {
    let rig = Rig::new();
    rig.mock.set_approve_token(false);
    rig.client().connect();
    assert_eq!(rig.client().status().wait_for(WAIT, |s| *s == BridgeStatus::Denied), Some(BridgeStatus::Denied));
    for _ in 0..60 {
        rig.step(Duration::from_secs(1));
    }
    assert_eq!(rig.count("AuthenticationTokenRequest"), 1, "60 s of fake time, no second popup");
    assert_eq!(rig.client().status().get(), BridgeStatus::Denied);
}

#[test]
fn a_revoke_stops_everything_and_forgets_the_token() {
    let rig = Rig::new();
    rig.connect();
    rig.push(0.5);
    rig.until_count("InjectParameterDataRequest", 1);
    rig.mock.revoke();
    rig.push(0.6);
    rig.step(FRAME_60);
    let revoked = rig.client().status().wait_for(WAIT, |s| matches!(s, BridgeStatus::Revoked { .. }));
    assert_eq!(revoked, Some(BridgeStatus::Revoked { expressions_may_be_on: Vec::new() }));
    let sent = rig.mock.received().len();
    for i in 0..30 {
        rig.push(f32::from(u8::try_from(i).unwrap()) / 30.0);
        rig.step(Duration::from_millis(100));
    }
    assert_eq!(rig.mock.received().len(), sent, "nothing is sent after the refusal");
    assert_eq!(rig.tokens.get("default"), None, "the token is gone");
    assert!(rig.mock.received().iter().rev().any(|r| r.message_type == "InjectParameterDataRequest"));
}

#[test]
fn a_restarted_vts_is_reconnected_on_the_backoff_without_a_popup() {
    let mut rig = Rig::new();
    rig.connect();
    rig.mock.stop().unwrap();
    let waiting = rig.client().status().wait_for(WAIT, |s| matches!(s, BridgeStatus::Connecting { attempt: 1 }));
    assert_eq!(waiting, Some(BridgeStatus::Connecting { attempt: 1 }));
    rig.mock.restart().unwrap();
    rig.step(Duration::from_millis(900));
    assert!(matches!(rig.client().status().get(), BridgeStatus::Connecting { .. }), "not before the first wait");
    rig.step(Duration::from_millis(100));
    rig.connected();
    assert_eq!(rig.count("AuthenticationTokenRequest"), 1, "reconnecting never shows the popup");
}

#[test]
fn a_host_off_this_computer_is_refused() {
    let config = ClientConfig { host: "192.168.1.20".into(), port: Some(8001), ..ClientConfig::default() };
    let map = ChannelMap::builtin().unwrap();
    let refused = VtsClient::spawn(config, Arc::new(MemoryTokens::default()), Arc::new(FakeClock::default()), &map);
    assert_eq!(refused.err(), Some(ClientError::NotLoopback("192.168.1.20".into())));
    for host in ["127.0.0.1", "::1", "[::1]", "localhost"] {
        assert!(ClientConfig { host: host.into(), ..ClientConfig::default() }.is_loopback(), "{host}");
    }
}

#[test]
fn discovery_reads_the_port_from_the_broadcast() {
    let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let broadcast = json!({
        "apiName": "VTubeStudioPublicAPI",
        "apiVersion": "1.0",
        "timestamp": 1_700_000_000_000_i64,
        "messageType": "VTubeStudioAPIStateBroadcast",
        "requestID": "",
        "data": { "active": true, "port": 8002, "instanceID": "a1b2", "windowTitle": "VTube Studio" }
    });
    let inactive = json!({ "apiName": "VTubeStudioPublicAPI", "messageType": "VTubeStudioAPIStateBroadcast",
        "data": { "active": false, "port": 9000, "instanceID": "a1b2" } });
    sender.send_to(inactive.to_string().as_bytes(), listener.local_addr().unwrap()).unwrap();
    sender.send_to(broadcast.to_string().as_bytes(), listener.local_addr().unwrap()).unwrap();
    assert_eq!(discover_on(&listener, WAIT, "default"), Some(8002), "the inactive one is skipped");
    assert_eq!(parse_broadcast(b"not json"), None);
    assert_eq!(parse_broadcast(broadcast.to_string().as_bytes()), Some((8002, "a1b2".into())));
}

#[test]
fn tokens_live_in_the_secret_store_under_the_instance() {
    let store = Arc::new(omuzaki_secrets_mock::MemorySecrets::new());
    let tokens = SecretTokens(store.clone());
    tokens.put("Main PC", "t-1");
    assert_eq!(tokens.get("Main PC").as_deref(), Some("t-1"));
    let labels = store.labels().unwrap();
    assert_eq!(labels.iter().map(SecretLabel::as_str).collect::<Vec<_>>(), ["vts/main-pc"]);
    tokens.delete("Main PC");
    assert_eq!(tokens.get("Main PC"), None);
}

#[test]
fn frames_at_sixty_hertz_send_one_batched_message_each() {
    let rig = Rig::new();
    rig.connect();
    for i in 0..600u16 {
        rig.push(f32::from(i % 100) / 100.0);
        rig.until_count("InjectParameterDataRequest", usize::from(i) + 1);
        rig.step(FRAME_60);
    }
    let injects: Vec<_> =
        rig.mock.received().into_iter().filter(|r| r.message_type == "InjectParameterDataRequest").collect();
    assert!((600..=601).contains(&injects.len()), "{} messages for 600 frames", injects.len());
    for inject in &injects {
        assert_eq!(inject.data["parameterValues"].as_array().unwrap().len(), 76, "every mapped parameter");
        assert_eq!(inject.data["mode"], "set");
        assert_eq!(inject.data["faceFound"], true);
    }
    assert!(rig.mock.param_value("OmuzakiJawOpen").is_some_and(|v| (v - 0.99).abs() < 1e-6), "the last frame's value");
}

#[test]
fn frames_at_240_hertz_are_capped_at_sixty_a_second() {
    let rig = Rig::new();
    rig.connect();
    let quarter = Duration::from_nanos(4_166_667);
    for i in 0..480u16 {
        rig.push(f32::from(i % 2));
        rig.step(quarter);
    }
    let sent = rig.count("InjectParameterDataRequest");
    assert!((100..=121).contains(&sent), "{sent} messages in 2 s of 240 Hz frames");
    let times: Vec<_> = rig
        .mock
        .received()
        .into_iter()
        .filter(|r| r.message_type == "InjectParameterDataRequest")
        .map(|r| r.at)
        .collect();
    assert!(times.windows(2).all(|w| w[1] - w[0] >= Duration::from_nanos(16_666_666)), "never two within a 60th");
}

#[test]
fn without_frames_the_last_values_are_resent_inside_the_keepalive() {
    let rig = Rig::new();
    rig.connect();
    rig.push(0.3);
    rig.until_count("InjectParameterDataRequest", 1);
    for _ in 0..100 {
        rig.step(Duration::from_millis(100));
    }
    let times: Vec<_> = rig
        .mock
        .received()
        .into_iter()
        .filter(|r| r.message_type == "InjectParameterDataRequest")
        .map(|r| r.at)
        .collect();
    assert!(times.len() >= 20, "{} keep-alives in 10 s", times.len());
    assert!(times.windows(2).all(|w| w[1] - w[0] <= Duration::from_millis(500)), "{times:?}");
    assert!(!rig.mock.events().iter().any(|e| matches!(e, Event::KeepaliveLapse { .. })), "no parameter lapsed");
    assert_eq!(rig.mock.param_value("OmuzakiJawOpen").map(|v| (v * 10.0).round()), Some(3.0));
}

#[test]
fn a_parameter_another_plugin_owns_moves_to_add() {
    let mut map = ChannelMap::builtin().unwrap();
    map.enable_default("MouthOpen", true).unwrap();
    map.enable_default("MouthSmile", true).unwrap();
    let rig = Rig::with_map(map);
    rig.connect();
    rig.mock.claim_param("MouthOpen");
    rig.push(0.4);
    let moved =
        rig.client().status().wait_for(WAIT, |s| matches!(s, BridgeStatus::Connected(c) if !c.conflicts.is_empty()));
    let Some(BridgeStatus::Connected(connected)) = moved else { panic!("no conflict: {moved:?}") };
    assert_eq!(connected.conflicts, ["MouthOpen"]);
    // The refused frame is resent at once in the new modes; settle that, then send one more frame.
    rig.step(FRAME_60);
    assert!(rig.client().status().wait_polls(3, WAIT));
    let before = rig.count("InjectParameterDataRequest");
    rig.push(0.5);
    rig.step(FRAME_60);
    rig.until_count("InjectParameterDataRequest", before + 2);
    let injects: Vec<_> =
        rig.mock.received().into_iter().filter(|r| r.message_type == "InjectParameterDataRequest").collect();
    let (set, add) = (&injects[before].data, &injects[before + 1].data);
    assert_eq!(set["mode"], "set");
    assert!(set["parameterValues"].as_array().unwrap().iter().all(|v| v["id"] != "MouthOpen"));
    assert!(set["parameterValues"].as_array().unwrap().iter().any(|v| v["id"] == "MouthSmile"));
    assert_eq!(add["mode"], "add");
    assert_eq!(add["parameterValues"], json!([{ "id": "MouthOpen", "value": 0.5 }]));
}

fn mapping() -> VtsMapping {
    let mut mapping = VtsMapping::default();
    mapping.models.insert(
        "Kit".into(),
        vec![
            Row { omuzaki: "smug".into(), target: Target::Hotkey { id: "h-smug".into(), name: "Smug".into() } },
            Row { omuzaki: "cry".into(), target: Target::Expression { file: "Cry.exp3.json".into(), fade_s: 0.5 } },
        ],
    );
    mapping
}

#[test]
fn a_hotkey_trigger_waits_out_its_cooldown() {
    let rig = Rig::new();
    rig.connect();
    rig.client().set_mapping(mapping());
    rig.client().trigger("smug");
    rig.until_count("HotkeyTriggerRequest", 1);
    rig.step(FRAME_60);
    rig.client().trigger("smug");
    for _ in 0..3 {
        rig.step(FRAME_60);
    }
    assert_eq!(rig.count("HotkeyTriggerRequest"), 1, "the second waits for five frames");
    rig.step(FRAME_60);
    rig.until_count("HotkeyTriggerRequest", 2);
    rig.client().trigger("unmapped");
    rig.step(FRAME_60 * 10);
    assert_eq!(rig.count("HotkeyTriggerRequest"), 2, "an unmapped id sends nothing");
    let BridgeStatus::Connected(status) = rig.client().status().get() else { panic!() };
    assert_eq!(status.hotkeys_dropped, 0);
}

#[test]
fn an_expression_switched_on_is_switched_off_before_disconnecting() {
    let rig = Rig::new();
    rig.connect();
    rig.client().set_mapping(mapping());
    rig.client().trigger("cry");
    let on = rig
        .client()
        .status()
        .wait_for(WAIT, |s| matches!(s, BridgeStatus::Connected(c) if !c.expressions_on.is_empty()));
    assert!(on.is_some());
    rig.client().disconnect();
    assert_eq!(
        rig.client().status().wait_for(WAIT, |s| *s == BridgeStatus::Disconnected),
        Some(BridgeStatus::Disconnected)
    );
    let activations: Vec<_> =
        rig.mock.received().into_iter().filter(|r| r.message_type == "ExpressionActivationRequest").collect();
    assert_eq!(activations.len(), 2);
    assert_eq!(activations[0].data["active"], true);
    assert_eq!(activations[1].data, json!({ "expressionFile": "Cry.exp3.json", "fadeTime": 0.5, "active": false }));
}
