//! Round trips against hand-written JSON fixtures (from the documented field lists), the limits tables, and the
//! tolerance rules.

#![allow(clippy::expect_used, clippy::needless_pass_by_value)]

use omuzaki_obs::limits::{CloseCode, DEFAULT_PORT, EventSubscription, RPC_VERSION, STATUS_RESOURCE_NOT_FOUND};
use omuzaki_obs::protocol::*;
use serde_json::{Value, json};

fn frame(op: u8, d: Value) -> Frame {
    Frame { op, d }
}

/// Types `wire`, checks it, and checks that it serialises back to the same JSON.
fn round_trip(wire: Value) -> Message {
    let f: Frame = serde_json::from_value(wire.clone()).expect("frame parses");
    let m = Message::from_frame(&f).expect("message parses");
    assert_eq!(serde_json::to_value(m.to_frame()).expect("serialises"), wire);
    m
}

#[test]
fn constants() {
    assert_eq!(DEFAULT_PORT, 4455);
    assert_eq!(RPC_VERSION, 1);
}

#[test]
fn hello_with_and_without_authentication() {
    let bare = json!({"op": 0, "d": {"obsWebSocketVersion": "5.5.2", "rpcVersion": 1}});
    assert_eq!(round_trip(bare), Message::Hello(Hello::new("5.5.2", None)));
    let auth = json!({"op": 0, "d": {"obsWebSocketVersion": "5.5.2", "rpcVersion": 1,
        "authentication": {"challenge": "c", "salt": "s"}}});
    assert_eq!(round_trip(auth), Message::Hello(Hello::new("5.5.2", Some(AuthChallenge::new("c", "s")))));
}

#[test]
fn identify_with_only_the_rpc_version() {
    let m = round_trip(json!({"op": 1, "d": {"rpcVersion": 1}}));
    assert_eq!(m, Message::Identify(Identify::new()));
    let full = Identify::new().with_authentication("a".into()).with_subscriptions(EventSubscription::SCENES);
    assert_eq!(
        round_trip(json!({"op": 1, "d": {"rpcVersion": 1, "authentication": "a", "eventSubscriptions": 4}})),
        Message::Identify(full)
    );
}

#[test]
fn identified_and_reidentify() {
    assert_eq!(round_trip(json!({"op": 2, "d": {"negotiatedRpcVersion": 1}})), Message::Identified(Identified::new(1)));
    assert_eq!(round_trip(json!({"op": 3, "d": {}})), Message::Reidentify(Reidentify::default()));
    assert_eq!(
        round_trip(json!({"op": 3, "d": {"eventSubscriptions": 64}})),
        Message::Reidentify(Reidentify::new(EventSubscription::OUTPUTS))
    );
}

#[test]
fn request_and_response() {
    let req = RequestMsg::new("r1", &SetCurrentProgramScene { scene_name: "Main".into() });
    let wire = json!({"op": 6, "d": {"requestType": "SetCurrentProgramScene", "requestId": "r1",
        "requestData": {"sceneName": "Main"}}});
    assert_eq!(round_trip(wire), Message::Request(req));
    let none = RequestMsg::new("r2", &StartStream {});
    assert_eq!(none.request_data, None);
    assert_eq!(none.request_type, "StartStream");

    let failed = json!({"op": 7, "d": {"requestType": "GetSceneItemId", "requestId": "r3",
        "requestStatus": {"result": false, "code": 600, "comment": "No scene items were found"}}});
    let Message::RequestResponse(resp) = round_trip(failed) else { panic!("not a response") };
    assert!(!resp.request_status.result);
    assert_eq!(resp.request_status.code, STATUS_RESOURCE_NOT_FOUND);
    assert_eq!(resp.request_status.comment.as_deref(), Some("No scene items were found"));
    assert!(resp.data::<GetSceneItemId>().is_err());
}

#[test]
fn typed_responses() {
    let resp = ResponseMsg::new::<GetSceneItemId>(
        "r",
        RequestStatus::success(),
        Some(&GetSceneItemIdResponse { scene_item_id: 7 }),
    );
    assert_eq!(resp.response_data, Some(json!({"sceneItemId": 7})));
    assert_eq!(resp.data::<GetSceneItemId>().expect("typed").scene_item_id, 7);
    // A request with no response fields parses from a response with no data.
    let empty = ResponseMsg::new::<StartRecord>("r", RequestStatus::success(), None);
    assert_eq!(empty.data::<StartRecord>().expect("empty"), Empty {});
    let stream = json!({"outputActive": true, "outputReconnecting": false, "outputTimecode": "00:00:01.000",
        "outputDuration": 1000, "outputCongestion": 0.5, "outputBytes": 10, "outputSkippedFrames": 0, "outputTotalFrames": 30,
        "somethingNew": 1});
    let parsed: GetStreamStatusResponse = serde_json::from_value(stream).expect("unknown fields are ignored");
    assert!(parsed.output_active);
    let names = [
        GetVersion::NAME,
        GetSceneList::NAME,
        GetCurrentProgramScene::NAME,
        GetStreamStatus::NAME,
        StopRecord::NAME,
        GetLastReplayBufferReplay::NAME,
    ];
    assert_eq!(
        names,
        [
            "GetVersion",
            "GetSceneList",
            "GetCurrentProgramScene",
            "GetStreamStatus",
            "StopRecord",
            "GetLastReplayBufferReplay"
        ]
    );
}

fn event(event_type: &str, intent: u32, data: Option<Value>) -> Value {
    let mut d = json!({"eventType": event_type, "eventIntent": intent});
    if let Some(data) = data {
        d["eventData"] = data;
    }
    json!({"op": 5, "d": d})
}

#[test]
fn every_event_variant_round_trips() {
    let state = "OBS_WEBSOCKET_OUTPUT_STARTED";
    let cases = [
        (
            event("CurrentProgramSceneChanged", 4, Some(json!({"sceneName": "Main", "sceneUuid": "u"}))),
            ObsEvent::CurrentProgramSceneChanged { scene_name: "Main".into() },
        ),
        (
            event("StreamStateChanged", 64, Some(json!({"outputActive": true, "outputState": state}))),
            ObsEvent::StreamStateChanged { output_active: true, output_state: OutputState::Started },
        ),
        (
            event(
                "RecordStateChanged",
                64,
                Some(
                    json!({"outputActive": false, "outputState": "OBS_WEBSOCKET_OUTPUT_STOPPED", "outputPath": "/v/a.mkv"}),
                ),
            ),
            ObsEvent::RecordStateChanged {
                output_active: false,
                output_state: OutputState::Stopped,
                output_path: Some("/v/a.mkv".into()),
            },
        ),
        (
            event(
                "RecordStateChanged",
                64,
                Some(json!({"outputActive": true, "outputState": state, "outputPath": null})),
            ),
            ObsEvent::RecordStateChanged { output_active: true, output_state: OutputState::Started, output_path: None },
        ),
        (
            event(
                "ReplayBufferStateChanged",
                64,
                Some(json!({"outputActive": true, "outputState": "OBS_WEBSOCKET_OUTPUT_PAUSED"})),
            ),
            ObsEvent::ReplayBufferStateChanged { output_active: true, output_state: OutputState::Paused },
        ),
        (
            event("ReplayBufferSaved", 64, Some(json!({"savedReplayPath": "/v/r.mkv"}))),
            ObsEvent::ReplayBufferSaved { saved_replay_path: "/v/r.mkv".into() },
        ),
        (
            event(
                "SceneItemEnableStateChanged",
                128,
                Some(json!({"sceneName": "Main", "sceneUuid": "u", "sceneItemId": 3, "sceneItemEnabled": false})),
            ),
            ObsEvent::SceneItemEnableStateChanged {
                scene_name: "Main".into(),
                scene_item_id: 3,
                scene_item_enabled: false,
            },
        ),
        (event("ExitStarted", 1, None), ObsEvent::ExitStarted),
    ];
    for (wire, expected) in cases {
        let Message::Event(msg) = round_trip(wire) else { panic!("not an event") };
        let typed = ObsEvent::from_msg(&msg).expect("types");
        assert_eq!(typed, expected);
        // The typed form drops fields it does not model (the scene UUIDs), so compare the typed round trip.
        assert_eq!(ObsEvent::from_msg(&typed.to_msg()), Ok(typed.clone()));
        assert_eq!(typed.to_msg().event_type, msg.event_type);
        assert_eq!(typed.intent().0, msg.event_intent);
    }
}

#[test]
fn events_are_tolerant() {
    // An empty data object on a unit event, and an unknown field, are fine.
    let exit = EventMsg::new("ExitStarted", EventSubscription(1), Some(json!({})));
    assert_eq!(ObsEvent::from_msg(&exit), Ok(ObsEvent::ExitStarted));
    let extra =
        EventMsg::new("ReplayBufferSaved", EventSubscription(64), Some(json!({"savedReplayPath": "p", "x": 1})));
    assert!(ObsEvent::from_msg(&extra).is_ok());
    // An unknown event is `Other`; a known one with bad data is an error.
    let other = EventMsg::new("InputCreated", EventSubscription(8), Some(json!({"a": 1})));
    assert_eq!(ObsEvent::from_msg(&other), Ok(ObsEvent::Other { event_type: "InputCreated".into() }));
    let bad = EventMsg::new("StreamStateChanged", EventSubscription(64), None);
    assert_eq!(ObsEvent::from_msg(&bad), Err(ProtocolError::BadEvent { event_type: "StreamStateChanged".into() }));
    // A state a newer server adds reads as `Unknown`.
    let newer = EventMsg::new(
        "StreamStateChanged",
        EventSubscription(64),
        Some(json!({"outputActive": true, "outputState": "OBS_WEBSOCKET_OUTPUT_NEW"})),
    );
    assert_eq!(
        ObsEvent::from_msg(&newer),
        Ok(ObsEvent::StreamStateChanged { output_active: true, output_state: OutputState::Unknown })
    );
}

#[test]
fn unknown_ops_and_fields_do_not_fail() {
    assert_eq!(Message::from_frame(&frame(8, json!({"requests": []}))), Ok(Message::Unknown { op: 8 }));
    assert_eq!(Message::from_frame(&frame(200, json!(null))), Ok(Message::Unknown { op: 200 }));
    let hello =
        frame(0, json!({"obsWebSocketVersion": "5.6.0", "obsStudioVersion": "31", "rpcVersion": 1, "future": true}));
    assert_eq!(Message::from_frame(&hello), Ok(Message::Hello(Hello::new("5.6.0", None))));
    assert_eq!(Message::from_frame(&frame(2, json!({}))), Err(ProtocolError::BadData { op: 2 }));
}

#[test]
fn event_subscription_bits() {
    assert_eq!(EventSubscription::ALL_PRE_CANVASES.0, 2047);
    assert_eq!(EventSubscription::ALL, EventSubscription::ALL_PRE_CANVASES.union(EventSubscription::CANVASES));
    assert_eq!(EventSubscription::ALL.0, 4095);
    for high in [
        EventSubscription::INPUT_VOLUME_METERS,
        EventSubscription::INPUT_ACTIVE_STATE_CHANGED,
        EventSubscription::INPUT_SHOW_STATE_CHANGED,
        EventSubscription::SCENE_ITEM_TRANSFORM_CHANGED,
    ] {
        assert!(!EventSubscription::ALL.contains(high));
        assert!(high.0 >= 1 << 16);
    }
    assert_eq!(EventSubscription::INPUT_VOLUME_METERS.0, 1 << 16);
    assert_eq!(EventSubscription::UI.0, 1 << 10);
    assert!(EventSubscription::ALL.contains(EventSubscription::OUTPUTS));
}

#[test]
fn close_codes_round_trip() {
    for code in 0..=u16::MAX {
        assert_eq!(CloseCode::from_u16(code).as_u16(), code);
    }
    assert_eq!(CloseCode::from_u16(4009), CloseCode::AuthenticationFailed);
    assert_eq!(CloseCode::from_u16(4010), CloseCode::UnsupportedRpcVersion);
    assert_eq!(CloseCode::from_u16(4001), CloseCode::Other(4001));
    assert_eq!(CloseCode::from_u16(1000), CloseCode::Other(1000));
}
