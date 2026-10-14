//! Round trips of every message against hand-written JSON fixtures (from the documented field lists), the
//! validator tables, and the error-id mapping.

// Fixture helpers `expect` on purpose (a bad fixture must fail loudly), and the response tests name their
// results `a`, `s`, `m`... to stay readable next to the JSON they check.
#![allow(clippy::expect_used, clippy::many_single_char_names, clippy::needless_pass_by_value)]

use omuzaki_vts::PLUGIN_DEVELOPER;
use omuzaki_vts::PLUGIN_NAME;
use omuzaki_vts::limits::{CustomParamName, FadeTime, LimitError, ParamValue};
use omuzaki_vts::protocol::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

fn frame(message_type: &str, data: Value) -> Value {
    json!({
        "apiName": "VTubeStudioPublicAPI", "apiVersion": "1.0", "requestID": "id1",
        "messageType": message_type, "data": data
    })
}

/// Parses `wire`, checks the type tag, and returns the typed envelope after a serialise/parse round trip.
fn round_trip<T: Message + Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(wire: &Value) -> Envelope<T> {
    let env: Envelope<T> = serde_json::from_value(wire.clone()).expect("fixture parses");
    assert_eq!(env.message_type, T::MESSAGE_TYPE);
    assert!(env.is_current());
    let again: Envelope<T> =
        serde_json::from_value(serde_json::to_value(&env).expect("serialises")).expect("re-parses");
    assert_eq!(env, again);
    env
}

/// For requests we send: the serialised form must equal the fixture exactly.
fn exact<T: Message + Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(wire: &Value) -> T {
    let env = round_trip::<T>(wire);
    assert_eq!(&serde_json::to_value(&env).expect("serialises"), wire);
    env.data
}

#[test]
fn requests_serialise_exactly() {
    exact::<ApiStateRequest>(&frame("APIStateRequest", json!({})));
    exact::<StatisticsRequest>(&frame("StatisticsRequest", json!({})));
    exact::<CurrentModelRequest>(&frame("CurrentModelRequest", json!({})));
    exact::<InputParameterListRequest>(&frame("InputParameterListRequest", json!({})));
    let t = exact::<AuthenticationTokenRequest>(&frame(
        "AuthenticationTokenRequest",
        json!({"pluginName": "Omuzaki", "pluginDeveloper": "Omuzaki Studio"}),
    ));
    assert_eq!(t, AuthenticationTokenRequest::omuzaki());
    let a = exact::<AuthenticationRequest>(&frame(
        "AuthenticationRequest",
        json!({"pluginName": "Omuzaki", "pluginDeveloper": "Omuzaki Studio", "authenticationToken": "tok-1"}),
    ));
    assert_eq!(a, AuthenticationRequest::omuzaki("tok-1"));
    let c = exact::<ParameterCreationRequest>(&frame(
        "ParameterCreationRequest",
        json!({"parameterName": "OmuzakiMouthOpen", "explanation": "Mouth", "min": 0.0, "max": 1.0, "defaultValue": 0.0}),
    ));
    assert_eq!(c.parameter_name.as_str(), "OmuzakiMouthOpen");
    exact::<ParameterValueRequest>(&frame("ParameterValueRequest", json!({"name": "FaceAngleX"})));
    let i = exact::<InjectParameterDataRequest>(&frame(
        "InjectParameterDataRequest",
        json!({"faceFound": true, "mode": "add", "parameterValues": [
            {"id": "FaceAngleX", "value": 12.5}, {"id": "OmuzakiMouthOpen", "value": 0.7, "weight": 0.8}]}),
    ));
    assert_eq!(i.mode, InjectMode::Add);
    exact::<HotkeysInCurrentModelRequest>(&frame("HotkeysInCurrentModelRequest", json!({})));
    exact::<HotkeysInCurrentModelRequest>(&frame("HotkeysInCurrentModelRequest", json!({"modelID": "abc"})));
    exact::<HotkeyTriggerRequest>(&frame("HotkeyTriggerRequest", json!({"hotkeyID": "h1", "itemInstanceID": "i1"})));
    exact::<ExpressionStateRequest>(&frame(
        "ExpressionStateRequest",
        json!({"details": true, "expressionFile": "a.exp3.json"}),
    ));
    let e = exact::<ExpressionActivationRequest>(&frame(
        "ExpressionActivationRequest",
        json!({"expressionFile": "a.exp3.json", "fadeTime": 0.5, "active": true}),
    ));
    assert!((e.fade_time.seconds() - 0.5).abs() < f64::EPSILON);
    let s = exact::<EventSubscriptionRequest>(&frame(
        "EventSubscriptionRequest",
        json!({"eventName": "ModelLoadedEvent", "subscribe": true, "config": {}}),
    ));
    assert_eq!(s, EventSubscriptionRequest::model_loaded(true));
    exact::<EventSubscriptionRequest>(&frame(
        "EventSubscriptionRequest",
        json!({"eventName": "ModelLoadedEvent", "subscribe": false, "config": {"modelID": ["0123456789abcdef0123456789abcdef"]}}),
    ));
}

#[test]
fn inject_defaults_to_set_without_weight() {
    let d: InjectParameterDataRequest =
        serde_json::from_value(json!({"parameterValues": [{"id": "FaceAngleX", "value": 1}]})).expect("parses");
    assert_eq!(d.mode, InjectMode::Set);
    assert!(!d.face_found);
    assert_eq!(d.parameter_values[0].weight, None);
}

#[test]
fn responses_round_trip_and_ignore_unknown_fields() {
    let ts = |mut v: Value| {
        v["timestamp"] = json!(1_625_405_710_728_i64);
        v["extraTopLevel"] = json!(1);
        v
    };
    let r = round_trip::<ApiStateResponse>(&ts(frame(
        "APIStateResponse",
        json!({"active": true, "vTubeStudioVersion": "1.9.0", "currentSessionAuthenticated": false, "future": [1]}),
    )));
    assert_eq!(r.timestamp, Some(1_625_405_710_728));
    assert!(r.data.active);
    round_trip::<AuthenticationTokenResponse>(&ts(frame(
        "AuthenticationTokenResponse",
        json!({"authenticationToken": "t"}),
    )));
    let a = round_trip::<AuthenticationResponse>(&ts(frame(
        "AuthenticationResponse",
        json!({"authenticated": false, "reason": "no"}),
    )));
    assert!(!a.data.authenticated);
    let s = round_trip::<StatisticsResponse>(&ts(frame(
        "StatisticsResponse",
        json!({"uptime": 1_439_384, "framerate": 73, "vTubeStudioVersion": "1.9.0", "allowedPlugins": 7, "connectedPlugins": 2,
               "startedWithSteam": true, "windowWidth": 1031, "windowHeight": 812, "windowIsFullscreen": false}),
    )));
    assert_eq!(s.data.framerate, 73);
    let m = round_trip::<CurrentModelResponse>(&ts(frame(
        "CurrentModelResponse",
        json!({"modelLoaded": true, "modelName": "M", "modelID": "id", "vtsModelName": "M.vtube.json",
               "live2DModelName": "M.model3.json", "numberOfLive2DParameters": 29, "hasPhysicsFile": true,
               "modelLoadTime": 3021, "modelPosition": {"positionX": -0.1}}),
    )));
    assert_eq!(m.data.number_of_live2d_parameters, 29);
    let unloaded = round_trip::<CurrentModelResponse>(&frame("CurrentModelResponse", json!({"modelLoaded": false})));
    assert_eq!(unloaded.data, CurrentModelResponse::default());
    let p = json!({"name": "FaceAngleX", "addedBy": "VTube Studio", "value": 45.78, "min": -30.0, "max": 30.0, "defaultValue": 0.0});
    let list = round_trip::<InputParameterListResponse>(&ts(frame(
        "InputParameterListResponse",
        json!({"modelLoaded": true, "modelName": "M", "modelID": "id", "customParameters": [p.clone()], "defaultParameters": [p.clone()]}),
    )));
    assert_eq!(list.data.default_parameters[0].added_by, "VTube Studio");
    round_trip::<ParameterCreationResponse>(&ts(frame(
        "ParameterCreationResponse",
        json!({"parameterName": "OmuzakiMouthOpen"}),
    )));
    let v = round_trip::<ParameterValueResponse>(&ts(frame("ParameterValueResponse", p)));
    assert_eq!(v.data.0.name, "FaceAngleX");
    round_trip::<InjectParameterDataResponse>(&ts(frame("InjectParameterDataResponse", json!({}))));
    let h = round_trip::<HotkeysInCurrentModelResponse>(&ts(frame(
        "HotkeysInCurrentModelResponse",
        json!({"modelLoaded": true, "modelName": "M", "modelID": "id", "availableHotkeys": [
            {"name": "h", "type": "ToggleExpression", "description": "d", "file": "a.exp3.json", "hotkeyID": "h1",
             "keyCombination": [], "onScreenButtonID": 8}]}),
    )));
    assert_eq!(h.data.available_hotkeys[0].kind, "ToggleExpression");
    assert_eq!(h.data.available_hotkeys[0].on_screen_button_id, 8);
    round_trip::<HotkeyTriggerResponse>(&ts(frame("HotkeyTriggerResponse", json!({"hotkeyID": "h1"}))));
    let x = round_trip::<ExpressionStateResponse>(&ts(frame(
        "ExpressionStateResponse",
        json!({"modelLoaded": true, "modelName": "M", "modelID": "id", "expressions": [
            {"name": "a", "file": "a.exp3.json", "active": false, "deactivateWhenKeyIsLetGo": false,
             "autoDeactivateAfterSeconds": false, "secondsRemaining": 0,
             "usedInHotkeys": [{"name": "h", "id": "h1"}], "parameters": [{"name": "P", "value": 0}]}]}),
    )));
    assert_eq!(x.data.expressions[0].used_in_hotkeys[0].id, "h1");
    let bare = round_trip::<ExpressionStateResponse>(&frame(
        "ExpressionStateResponse",
        json!({"modelLoaded": true, "modelName": "M", "modelID": "id", "expressions": [
            {"name": "a", "file": "a.exp3.json", "active": true, "deactivateWhenKeyIsLetGo": false,
             "autoDeactivateAfterSeconds": false, "secondsRemaining": 0}]}),
    ));
    assert_eq!(bare.data.expressions[0].parameters, []);
    round_trip::<ExpressionActivationResponse>(&ts(frame("ExpressionActivationResponse", json!({}))));
    round_trip::<EventSubscriptionResponse>(&ts(frame(
        "EventSubscriptionResponse",
        json!({"subscribedEventCount": 1, "subscribedEvents": ["ModelLoadedEvent"]}),
    )));
    let ev = round_trip::<ModelLoadedEvent>(&ts(frame(
        "ModelLoadedEvent",
        json!({"modelLoaded": true, "modelName": "M", "modelID": "165131471d8a4e42aae01a9738f255ef"}),
    )));
    assert!(ev.data.model_loaded);
}

#[test]
fn error_envelope_and_kinds() {
    let wire = frame("APIError", json!({"errorID": 454, "message": "owned"}));
    let env = round_trip::<ApiError>(&wire);
    assert!(env.is_error());
    assert_eq!(env.data.kind(), ErrorKind::ParamOwnedByOtherPlugin);
    assert_eq!(env.data.to_string(), "VTube Studio error 454: owned");
    let table = [
        (8, ErrorKind::NotAuthenticated),
        (50, ErrorKind::NotAuthenticated),
        (454, ErrorKind::ParamOwnedByOtherPlugin),
        (453, ErrorKind::UnknownParam),
        (500, ErrorKind::UnknownParam),
        (451, ErrorKind::ValueOutOfRange),
        (200, ErrorKind::HotkeyBusy),
        (203, ErrorKind::HotkeyBusy),
        (201, ErrorKind::ModelNotLoaded),
        (652, ErrorKind::ModelNotLoaded),
        (0, ErrorKind::Other(0)),
        (999, ErrorKind::Other(999)),
    ];
    for (id, kind) in table {
        assert_eq!(ErrorKind::from_id(id), kind, "id {id}");
    }
}

#[test]
fn foreign_frames_are_not_current() {
    let mut wire = frame("APIStateRequest", json!({}));
    wire["apiVersion"] = json!("2.0");
    let env: Envelope<ApiStateRequest> = serde_json::from_value(wire).expect("parses");
    assert!(!env.is_current());
}

#[test]
fn plugin_identity_is_fixed() {
    assert_eq!(PLUGIN_NAME, "Omuzaki");
    assert_eq!(PLUGIN_DEVELOPER, "Omuzaki Studio");
}

#[test]
fn param_name_table() {
    let ok = ["abcd", "Omuzaki1", &"a".repeat(32)];
    for name in ok {
        assert!(CustomParamName::new(name).is_ok(), "{name}");
    }
    let bad = ["", "abc", &"a".repeat(33), "has space", "under_score", "naïve1"];
    for name in bad {
        assert!(matches!(CustomParamName::new(name), Err(LimitError::ParamName(_))), "{name}");
    }
    assert!(serde_json::from_value::<CustomParamName>(json!("abc")).is_err());
}

#[test]
fn param_value_table() {
    for v in [0.0, 1e6, -1e6, 0.5] {
        assert_eq!(ParamValue::new(v).expect("in range").get().to_bits(), v.to_bits());
    }
    for v in [1_000_001.0, -1_000_001.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(ParamValue::new(v).is_err(), "{v}");
    }
    assert!(serde_json::from_value::<ParamValue>(json!(1_000_001)).is_err());
}

#[test]
fn fade_time_table() {
    let cases = [(-1.0, 0.0), (0.0, 0.0), (0.5, 0.5), (2.0, 2.0), (3.0, 2.0), (f64::NAN, 0.25), (f64::INFINITY, 0.25)];
    for (input, want) in cases {
        assert!((FadeTime::new(input).seconds() - want).abs() < f64::EPSILON, "{input}");
    }
    assert!((FadeTime::default().seconds() - 0.25).abs() < f64::EPSILON);
    let omitted: ExpressionActivationRequest =
        serde_json::from_value(json!({"expressionFile": "a.exp3.json", "active": true})).expect("parses");
    assert!((omitted.fade_time.seconds() - 0.25).abs() < f64::EPSILON);
    let big: FadeTime = serde_json::from_value(json!(9)).expect("parses");
    assert!((big.seconds() - 2.0).abs() < f64::EPSILON);
}

#[test]
fn limits_match_the_documented_numbers() {
    use omuzaki_vts::limits::*;
    assert_eq!(
        (MAX_PLUGIN_PARAMS, HOTKEY_QUEUE, HOTKEY_MIN_FRAMES, DEFAULT_PORT, DISCOVERY_PORT),
        (100, 32, 5, 8001, 47779)
    );
    assert_eq!(KEEPALIVE, std::time::Duration::from_secs(1));
}
