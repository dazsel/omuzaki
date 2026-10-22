//! The fake against a raw WebSocket client: the handshake with and without a password, scenes and their events by
//! subscription, the output state machines and their failures, faults, and seeded determinism.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::TcpStream;
use std::time::{Duration, Instant};

use super::*;
use crate::protocol::{GetSceneList, Identify, RequestMsg, RequestType, SaveReplayBuffer, SetCurrentProgramScene};

const PASSWORD: &str = "test-only-password";

struct Raw {
    ws: Socket,
    hello: Hello,
    next: u64,
    /// Events read while waiting for an answer.
    early: Vec<ObsEvent>,
}

impl Raw {
    fn open(port: u16) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let (mut ws, _) = tungstenite::client(format!("ws://127.0.0.1:{port}/"), stream).unwrap();
        let Ok(Some(Message::Hello(hello))) = read(&mut ws) else { panic!("no hello") };
        Self { ws, hello, next: 1, early: Vec::new() }
    }

    /// Identifies; `None` when the server closed instead.
    fn identify(mut self, password: Option<&str>, subscriptions: EventSubscription) -> Option<Self> {
        let mut identify = Identify::new().with_subscriptions(subscriptions);
        if let (Some(p), Some(c)) = (password, &self.hello.authentication) {
            identify = identify.with_authentication(auth_response(p, &c.salt, &c.challenge));
        }
        assert!(send(&mut self.ws, &Message::Identify(identify)));
        match read(&mut self.ws) {
            Ok(Some(Message::Identified(_))) => Some(self),
            _ => None,
        }
    }

    fn call<R: RequestType>(&mut self, request: &R) -> ResponseMsg {
        let id = self.next.to_string();
        self.next += 1;
        assert!(send(&mut self.ws, &Message::Request(RequestMsg::new(id.clone(), request).unwrap())));
        loop {
            match read(&mut self.ws) {
                Ok(Some(Message::RequestResponse(r))) if r.request_id == id => return r,
                Ok(Some(Message::Event(e))) => self.early.push(ObsEvent::from_msg(&e).unwrap()),
                Ok(_) => {}
                Err(()) => panic!("closed while waiting for {}", R::NAME),
            }
        }
    }

    /// The events that arrive within `wait`.
    fn events(&mut self, wait: Duration) -> Vec<ObsEvent> {
        self.ws.get_ref().set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        let until = Instant::now() + wait;
        let mut out = std::mem::take(&mut self.early);
        while Instant::now() < until {
            match read(&mut self.ws) {
                Ok(Some(Message::Event(e))) => out.push(ObsEvent::from_msg(&e).unwrap()),
                Ok(_) => {}
                Err(()) => break,
            }
        }
        out
    }

    fn closed(&mut self) -> bool {
        self.ws.get_ref().set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        loop {
            match self.ws.read() {
                Ok(WsMessage::Close(_)) | Err(_) => return true,
                Ok(_) => {}
            }
        }
    }
}

fn with_password() -> FakeConfig {
    FakeConfig { password: Some(PASSWORD.into()), ..FakeConfig::default() }
}

#[test]
fn the_handshake_asks_for_the_password_only_when_one_is_set() {
    let open = FakeObs::start(FakeConfig::default());
    let raw = Raw::open(open.port());
    assert!(raw.hello.authentication.is_none());
    assert!(raw.identify(None, EventSubscription::ALL).is_some());

    let locked = FakeObs::start(with_password());
    let raw = Raw::open(locked.port());
    assert!(raw.hello.authentication.is_some());
    assert!(raw.identify(Some(PASSWORD), EventSubscription::ALL).is_some());
}

#[test]
fn a_wrong_answer_is_closed_with_authentication_failed() {
    let fake = FakeObs::start(with_password());
    let mut raw = Raw::open(fake.port());
    let identify = Identify::new().with_authentication("not-the-answer".into());
    assert!(send(&mut raw.ws, &Message::Identify(identify)));
    match raw.ws.read() {
        Ok(WsMessage::Close(Some(frame))) => assert_eq!(u16::from(frame.code), 4009),
        other => panic!("expected the 4009 close, got {other:?}"),
    }
}

#[test]
fn scenes_switch_and_only_subscribed_clients_hear_it() {
    let fake = FakeObs::start(FakeConfig::default());
    let mut scenes = Raw::open(fake.port()).identify(None, EventSubscription::SCENES).unwrap();
    let mut deaf = Raw::open(fake.port()).identify(None, EventSubscription::NONE).unwrap();
    let list = scenes.call(&GetSceneList {}).data::<GetSceneList>().unwrap();
    assert_eq!(list.current_program_scene_name.as_deref(), Some("Main"));
    let names: Vec<&str> = list.scenes.iter().rev().map(|s| s["sceneName"].as_str().unwrap()).collect();
    assert_eq!(names, ["Main", "Starting soon", "BRB"]);
    assert!(scenes.call(&SetCurrentProgramScene { scene_name: "BRB".into() }).request_status.result);
    assert_eq!(fake.state().program, "BRB");
    let heard = scenes.events(Duration::from_millis(200));
    assert_eq!(heard, [ObsEvent::CurrentProgramSceneChanged { scene_name: "BRB".into() }]);
    assert_eq!(deaf.events(Duration::from_millis(100)), []);
    let missing = scenes.call(&SetCurrentProgramScene { scene_name: "Nope".into() });
    assert_eq!((missing.request_status.result, missing.request_status.code), (false, STATUS_RESOURCE_NOT_FOUND));
}

#[test]
fn outputs_step_through_their_states_and_refuse_what_already_holds() {
    let fake = FakeObs::start(FakeConfig::default());
    let mut raw = Raw::open(fake.port()).identify(None, EventSubscription::OUTPUTS).unwrap();
    assert!(raw.call(&crate::protocol::StartStream {}).request_status.result);
    let again = raw.call(&crate::protocol::StartStream {});
    assert_eq!(again.request_status.code, STATUS_OUTPUT_RUNNING);
    assert!(raw.call(&crate::protocol::StopStream {}).request_status.result);
    let states: Vec<OutputState> = raw
        .events(Duration::from_millis(200))
        .into_iter()
        .filter_map(|e| match e {
            ObsEvent::StreamStateChanged { output_state, .. } => Some(output_state),
            _ => None,
        })
        .collect();
    assert_eq!(states, [OutputState::Starting, OutputState::Started, OutputState::Stopping, OutputState::Stopped]);

    let stopped = raw.call(&SaveReplayBuffer {});
    assert_eq!(stopped.request_status.code, STATUS_OUTPUT_NOT_RUNNING);
    assert!(raw.call(&crate::protocol::StartReplayBuffer {}).request_status.result);
    assert!(raw.call(&SaveReplayBuffer {}).request_status.result);
    let saved = raw.events(Duration::from_millis(200));
    assert!(saved.iter().any(|e| matches!(e, ObsEvent::ReplayBufferSaved { saved_replay_path } if saved_replay_path.starts_with(FAKE_FOLDER))));
}

#[test]
fn faults_drop_restart_delay_and_fail() {
    let mut fake = FakeObs::start(FakeConfig::default());
    let mut raw = Raw::open(fake.port()).identify(None, EventSubscription::ALL).unwrap();
    fake.drop_connection();
    assert!(raw.closed());

    fake.restart();
    let mut raw = Raw::open(fake.port()).identify(None, EventSubscription::ALL).unwrap();
    fake.delay("GetSceneList", Duration::from_millis(300));
    let started = Instant::now();
    raw.call(&GetSceneList {});
    assert!(started.elapsed() >= Duration::from_millis(300));

    fake.fail("StartRecord", 702, "x");
    let failed = raw.call(&crate::protocol::StartRecord {});
    assert_eq!((failed.request_status.code, failed.request_status.comment.as_deref()), (702, Some("x")));
    assert_eq!(fake.count("StartRecord"), 1);
    assert!(fake.received().len() <= MAX_KEPT);
}

#[test]
fn the_same_seed_gives_the_same_challenge() {
    let challenge = |seed| {
        let fake = FakeObs::start(FakeConfig { seed, ..with_password() });
        Raw::open(fake.port()).hello.authentication.unwrap()
    };
    assert_eq!(challenge(7), challenge(7));
    assert_ne!(challenge(7), challenge(8));
}
