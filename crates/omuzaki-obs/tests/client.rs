//! The client and the ops against a fake OBS server on a local port: the password handshake, scene switching with its
//! event, the stream and the recording, a wrong password, and the server going away.

#![allow(clippy::unwrap_used)] // helpers outside `#[test]` fns: a panic is a clear test failure

use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_obs::auth::auth_response;
use omuzaki_obs::client::{ObsClient, ObsError};
use omuzaki_obs::limits::CloseCode;
use omuzaki_obs::protocol::{
    AuthChallenge, Frame, GetCurrentProgramScene, GetSceneList, Hello, Identified, Message, ObsEvent, OutputState,
    RequestStatus, ResponseMsg, SetCurrentProgramScene,
};
use omuzaki_obs::service::ObsService;
use omuzaki_ops_api::{Actor, OpCtx, Services, ServicesBuilder, find};
use omuzaki_secrets_api::prelude::{SecretLabel, SecretStore, SecretValue};
use omuzaki_secrets_mock::MemorySecrets;
use serde_json::{Value, json};
use tungstenite::protocol::CloseFrame;
use tungstenite::protocol::frame::coding::CloseCode as WsCloseCode;
use tungstenite::{Message as WsMessage, Utf8Bytes, WebSocket};

const PASSWORD: &str = "test-only-password";
const SALT: &str = "c2FsdA==";
const CHALLENGE: &str = "Y2hhbGxlbmdl";

/// What the fake OBS holds.
struct State {
    scenes: Vec<String>,
    program: String,
    streaming: bool,
    recording: bool,
}

/// A fake OBS: accepts connections on a local port, answers the handshake (with the password challenge) and a few
/// requests, and sends the events a real server would.
struct FakeObs {
    port: u16,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

fn send(ws: &mut WebSocket<TcpStream>, msg: &Message) {
    let text = serde_json::to_string(&msg.to_frame().unwrap()).unwrap();
    ws.send(WsMessage::Text(Utf8Bytes::from(text))).unwrap();
}

fn read(ws: &mut WebSocket<TcpStream>) -> Option<Message> {
    loop {
        match ws.read() {
            Ok(WsMessage::Text(t)) => {
                let frame: Frame = serde_json::from_str(t.as_str()).unwrap();
                return Some(Message::from_frame(&frame).unwrap());
            }
            Ok(WsMessage::Close(_)) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

fn event(ws: &mut WebSocket<TcpStream>, e: &ObsEvent) {
    send(ws, &Message::Event(e.to_msg().unwrap()));
}

fn serve(stream: TcpStream, state: &Mutex<State>) {
    let Ok(mut ws) = tungstenite::accept(stream) else { return };
    send(&mut ws, &Message::Hello(Hello::new("5.5.0", Some(AuthChallenge::new(CHALLENGE, SALT)))));
    let Some(Message::Identify(identify)) = read(&mut ws) else { return };
    if identify.authentication.as_deref() != Some(auth_response(PASSWORD, SALT, CHALLENGE).as_str()) {
        let code = WsCloseCode::from(CloseCode::AuthenticationFailed.as_u16());
        let _ = ws.close(Some(CloseFrame { code, reason: "Authentication failed.".into() }));
        let _ = ws.flush();
        return;
    }
    send(&mut ws, &Message::Identified(Identified::new(1)));
    while let Some(message) = read(&mut ws) {
        let Message::Request(req) = message else { continue };
        let data = req.request_data.clone().unwrap_or(Value::Null);
        let mut events = Vec::new();
        let (status, response) = {
            let mut s = state.lock().unwrap();
            match req.request_type.as_str() {
                "GetSceneList" => {
                    let scenes: Vec<Value> = s
                        .scenes
                        .iter()
                        .rev()
                        .enumerate()
                        .map(|(i, n)| json!({"sceneName": n, "sceneIndex": i}))
                        .collect();
                    (
                        RequestStatus::success(),
                        Some(
                            json!({"currentProgramSceneName": s.program, "currentPreviewSceneName": null, "scenes": scenes}),
                        ),
                    )
                }
                "GetCurrentProgramScene" => {
                    (RequestStatus::success(), Some(json!({"sceneName": s.program, "sceneUuid": "uuid"})))
                }
                "SetCurrentProgramScene" => {
                    let name = data["sceneName"].as_str().unwrap_or_default().to_owned();
                    if s.scenes.contains(&name) {
                        s.program.clone_from(&name);
                        events.push(ObsEvent::CurrentProgramSceneChanged { scene_name: name });
                        (RequestStatus::success(), None)
                    } else {
                        (
                            RequestStatus::failure(600, Some(format!("No source was found by the name of `{name}`."))),
                            None,
                        )
                    }
                }
                "StartStream" | "StopStream" => {
                    s.streaming = req.request_type == "StartStream";
                    let state = if s.streaming { OutputState::Started } else { OutputState::Stopped };
                    events.push(ObsEvent::StreamStateChanged { output_active: s.streaming, output_state: state });
                    (RequestStatus::success(), None)
                }
                "GetStreamStatus" => (
                    RequestStatus::success(),
                    Some(
                        json!({"outputActive": s.streaming, "outputReconnecting": false, "outputTimecode": "00:00:00.000",
                        "outputDuration": 0, "outputCongestion": 0.0, "outputBytes": 0, "outputSkippedFrames": 0, "outputTotalFrames": 0}),
                    ),
                ),
                "StartRecord" => {
                    s.recording = true;
                    (RequestStatus::success(), None)
                }
                "StopRecord" => {
                    s.recording = false;
                    (RequestStatus::success(), Some(json!({"outputPath": "take.mkv"})))
                }
                "GetRecordStatus" => (
                    RequestStatus::success(),
                    Some(json!({"outputActive": s.recording, "outputPaused": false, "outputTimecode": "00:00:00.000",
                        "outputDuration": 0, "outputBytes": 0})),
                ),
                _ => (RequestStatus::failure(204, Some("unknown request".into())), None),
            }
        };
        let mut response_msg =
            ResponseMsg::new::<GetCurrentProgramScene>(req.request_id.clone(), status, None).unwrap();
        response_msg.request_type.clone_from(&req.request_type);
        response_msg.response_data = response;
        send(&mut ws, &Message::RequestResponse(response_msg));
        for e in &events {
            event(&mut ws, e);
        }
    }
}

impl FakeObs {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(State {
            scenes: vec!["Main".into(), "Starting soon".into(), "BRB".into()],
            program: "Starting soon".into(),
            streaming: false,
            recording: false,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (s, flag) = (Arc::clone(&state), Arc::clone(&stop));
        let accept = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if flag.load(Ordering::Acquire) {
                    return;
                }
                let Ok(stream) = stream else { continue };
                let s = Arc::clone(&s);
                std::thread::spawn(move || serve(stream, &s));
            }
        });
        Self { port, state, stop, accept: Some(accept) }
    }

    /// Stops accepting (existing connections end when their client goes).
    fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(a) = self.accept.take() {
            a.join().unwrap();
        }
    }
}

impl Drop for FakeObs {
    fn drop(&mut self) {
        self.stop();
    }
}

#[test]
fn the_client_identifies_with_the_password_and_switches_scenes() {
    let obs = FakeObs::start();
    let client = ObsClient::connect("127.0.0.1", obs.port, Some(PASSWORD)).unwrap();
    assert_eq!(client.info().rpc_version, 1);
    let events = client.subscribe();
    let list = client.request(&GetSceneList {}).unwrap();
    assert_eq!(list.current_program_scene_name.as_deref(), Some("Starting soon"));
    assert_eq!(list.scenes.len(), 3);
    client.request(&SetCurrentProgramScene { scene_name: "Main".into() }).unwrap();
    assert_eq!(obs.state.lock().unwrap().program, "Main");
    let e = events.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(e, ObsEvent::CurrentProgramSceneChanged { scene_name: "Main".into() });
    let refused = client.request(&SetCurrentProgramScene { scene_name: "Nope".into() }).unwrap_err();
    assert!(matches!(refused, ObsError::Refused { code: 600, .. }), "{refused:?}");
}

#[test]
fn a_wrong_or_missing_password_is_an_auth_error_that_never_shows_the_password() {
    let obs = FakeObs::start();
    let wrong = ObsClient::connect("127.0.0.1", obs.port, Some("not-it")).unwrap_err();
    assert!(matches!(wrong, ObsError::Auth(_)), "{wrong:?}");
    assert!(!wrong.to_string().contains("not-it"));
    let missing = ObsClient::connect("127.0.0.1", obs.port, None).unwrap_err();
    assert!(matches!(missing, ObsError::Auth(_)), "{missing:?}");
}

#[test]
fn nothing_listening_is_unreachable() {
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let e = ObsClient::connect("127.0.0.1", port, None).unwrap_err();
    assert!(matches!(e, ObsError::Unreachable { .. }), "{e:?}");
}

fn call(name: &str, args: Value, services: &Services) -> Result<Value, String> {
    omuzaki_obs::link();
    let op = (find(name).unwrap().decode)(args).map_err(|e| e.message)?;
    let (doc, clock, actor) = (MemDoc::new(), FakeClock::new(Seconds(0.0)), Actor::Person { name: "host".into() });
    let ctx = OpCtx::new(&doc, &actor, &clock).with_services(services);
    op.plan(&ctx).map(|p| p.output.unwrap_or(Value::Null)).map_err(|e| e.message)
}

#[test]
fn the_ops_connect_with_the_stored_password_and_drive_obs() {
    let obs = FakeObs::start();
    let service = Arc::new(ObsService::new());
    let secrets = Arc::new(MemorySecrets::new());
    secrets.set(&SecretLabel::new("obs/websocket").unwrap(), SecretValue::new(PASSWORD)).unwrap();
    let services =
        ServicesBuilder::new().insert::<ObsService>(Arc::clone(&service)).insert::<dyn SecretStore>(secrets).build();

    assert_eq!(call("obs.status", json!({}), &services).unwrap()["connected"], false);
    let connected = call("obs.connect", json!({ "port": obs.port }), &services).unwrap();
    assert_eq!(connected["connected"], true);
    assert!(!connected.to_string().contains(PASSWORD), "the answer never carries the password");

    let scenes = call("obs.scenes", json!({}), &services).unwrap();
    assert_eq!(scenes["scenes"], json!(["Main", "Starting soon", "BRB"]));
    call("obs.set_scene", json!({ "scene": "BRB" }), &services).unwrap();
    call("obs.stream", json!({ "action": "start" }), &services).unwrap();
    call("obs.record", json!({ "action": "start" }), &services).unwrap();
    let status = call("obs.status", json!({}), &services).unwrap();
    assert_eq!(
        (status["program_scene"].clone(), status["streaming"].clone(), status["recording"].clone()),
        (json!("BRB"), json!(true), json!(true))
    );
    let stopped = call("obs.record", json!({ "action": "stop" }), &services).unwrap();
    assert_eq!(stopped["file"], "take.mkv");
    call("obs.stream", json!({ "action": "stop" }), &services).unwrap();
    assert!(!obs.state.lock().unwrap().streaming);

    assert_eq!(call("obs.disconnect", json!({}), &services).unwrap()["disconnected"], true);
    let e = call("obs.set_scene", json!({ "scene": "Main" }), &services).unwrap_err();
    assert!(e.contains("closed"), "{e}");
}
