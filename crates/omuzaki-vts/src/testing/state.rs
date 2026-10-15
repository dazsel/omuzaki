//! The mock's protocol state and request handling; no I/O here, so the rules are plain data in, plain data out.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::limits::{HOTKEY_MIN_FRAMES, HOTKEY_QUEUE, KEEPALIVE, MAX_PLUGIN_PARAMS, PARAM_VALUE_LIMIT};
use crate::protocol::{
    API_ERROR_TYPE, API_NAME, API_VERSION, ApiError, ApiStateResponse, AuthenticationRequest, AuthenticationResponse,
    AuthenticationTokenRequest, AuthenticationTokenResponse, CurrentModelResponse, Envelope, Expression,
    ExpressionActivationRequest, ExpressionActivationResponse, ExpressionStateResponse, Hotkey, HotkeyTriggerRequest,
    HotkeyTriggerResponse, HotkeysInCurrentModelResponse, InjectMode, InjectParameterDataRequest,
    InjectParameterDataResponse, InputParameter, InputParameterListResponse, Message, ParameterCreationRequest,
    ParameterCreationResponse, ParameterValueRequest, ParameterValueResponse, StatisticsResponse,
};

// Ids the crate classifies (`ErrorKind`) use the documented values; the rest are the mock's own and only say "refused".
const NOT_AUTHENTICATED: i32 = 8;
const TOKEN_DENIED: i32 = 50;
const QUEUE_FULL: i32 = 200;
const NO_MODEL: i32 = 201;
const HOTKEY_COOLDOWN: i32 = 203;
const VALUE_OUT_OF_RANGE: i32 = 451;
const UNKNOWN_PARAM: i32 = 453;
const PARAM_OWNED: i32 = 454;
const EXPRESSION_NO_MODEL: i32 = 652;
const BAD_REQUEST: i32 = 1;
const UNKNOWN_MESSAGE: i32 = 2;
const UNKNOWN_HOTKEY: i32 = 202;
const UNKNOWN_EXPRESSION: i32 = 651;
const PARAM_LIMIT: i32 = 450;
const PARAM_EXISTS: i32 = 452;

/// The model VTS has loaded. A hotkey's id is its name; an expression's file is `<name>.exp3.json`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MockModel {
    /// The model's name.
    pub name: String,
    /// The hotkey names; each is also the hotkey's id.
    pub hotkeys: Vec<String>,
    /// The expression names.
    pub expressions: Vec<String>,
    /// The `Live2D` parameters, which can be injected like VTS's default parameters.
    pub parameters: Vec<String>,
}

/// One request the mock received.
#[derive(Debug, Clone, PartialEq)]
pub struct Received {
    /// When it was handled, by the injected clock.
    pub at: Duration,
    /// The `requestID`, empty when the frame was not a valid envelope.
    pub request_id: String,
    /// The `messageType`, empty when the frame was not a valid envelope.
    pub message_type: String,
    /// The `data` object; the raw text as a JSON string when the frame was not a valid envelope.
    pub data: Value,
}

/// Something the mock saw that is not a reply.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event {
    /// A parameter was not fed for [`KEEPALIVE`] and went back to its previous controller.
    #[non_exhaustive]
    KeepaliveLapse {
        /// The parameter.
        parameter: String,
        /// When the lapse happened: the last feed plus [`KEEPALIVE`].
        at: Duration,
    },
}

impl Event {
    /// A [`Event::KeepaliveLapse`].
    pub fn keepalive_lapse(parameter: impl Into<String>, at: Duration) -> Self {
        Self::KeepaliveLapse { parameter: parameter.into(), at }
    }
}

struct Session {
    authenticated: bool,
    plugin: String,
}

struct CustomParam {
    owner: String,
    min: f64,
    max: f64,
    default: f64,
}

pub(super) struct State {
    pub approve_token: bool,
    pub model: Option<MockModel>,
    pub framerate: u32,
    pub claimed: BTreeSet<String>,
    pub received: Vec<Received>,
    pub events: Vec<Event>,
    tokens: BTreeSet<String>,
    issued: u32,
    next_session: u64,
    sessions: BTreeMap<u64, Session>,
    custom: BTreeMap<String, CustomParam>,
    values: BTreeMap<String, f64>,
    last_fed: BTreeMap<String, Duration>,
    active_expressions: BTreeSet<String>,
    last_trigger: BTreeMap<String, Duration>,
    queue: VecDeque<Duration>,
}

type Reply = Result<(&'static str, Value), ApiError>;

fn error(id: i32, message: &str) -> ApiError {
    ApiError { error_id: id, message: message.into() }
}

fn ok<T: Message + Serialize>(data: &T) -> Reply {
    let value = serde_json::to_value(data).map_err(|e| error(BAD_REQUEST, &e.to_string()))?;
    Ok((T::MESSAGE_TYPE, value))
}

fn parse<T: DeserializeOwned>(data: Value) -> Result<T, ApiError> {
    serde_json::from_value(data).map_err(|e| error(BAD_REQUEST, &e.to_string()))
}

fn frame_nanos(framerate: u32) -> u64 {
    1_000_000_000 / u64::from(framerate.max(1))
}

impl State {
    pub fn new(approve_token: bool, model: Option<MockModel>, framerate: u32) -> Self {
        Self {
            approve_token,
            model,
            framerate: framerate.max(1),
            claimed: BTreeSet::new(),
            received: Vec::new(),
            events: Vec::new(),
            tokens: BTreeSet::new(),
            issued: 0,
            next_session: 0,
            sessions: BTreeMap::new(),
            custom: BTreeMap::new(),
            values: BTreeMap::new(),
            last_fed: BTreeMap::new(),
            active_expressions: BTreeSet::new(),
            last_trigger: BTreeMap::new(),
            queue: VecDeque::new(),
        }
    }

    pub fn open_session(&mut self) -> u64 {
        self.next_session += 1;
        self.sessions.insert(self.next_session, Session { authenticated: false, plugin: String::new() });
        self.next_session
    }

    pub fn close_session(&mut self, id: u64) {
        self.sessions.remove(&id);
    }

    pub fn end_sessions(&mut self) {
        self.sessions.clear();
    }

    pub fn revoke(&mut self) {
        self.tokens.clear();
        self.sessions.values_mut().for_each(|s| s.authenticated = false);
    }

    /// Lapses every parameter not fed for [`KEEPALIVE`] by `now`.
    pub fn settle(&mut self, now: Duration) {
        let lapsed: Vec<_> = self
            .last_fed
            .iter()
            .filter(|&(_, &fed)| now.saturating_sub(fed) >= KEEPALIVE)
            .map(|(n, &t)| (n.clone(), t))
            .collect();
        for (name, fed) in lapsed {
            self.last_fed.remove(&name);
            self.values.remove(&name);
            self.events.push(Event::keepalive_lapse(name, fed + KEEPALIVE));
        }
    }

    /// Handles one text frame and returns the reply frame.
    pub fn handle(&mut self, session: u64, text: &str, now: Duration) -> Result<String, serde_json::Error> {
        self.settle(now);
        let (request_id, reply) = match serde_json::from_str::<Envelope<Value>>(text) {
            Ok(env) => {
                self.received.push(Received {
                    at: now,
                    request_id: env.request_id.clone(),
                    message_type: env.message_type.clone(),
                    data: env.data.clone(),
                });
                (env.request_id, self.dispatch(session, &env.message_type, env.data, now))
            }
            Err(e) => {
                let data = Value::String(text.to_owned());
                self.received.push(Received { at: now, request_id: String::new(), message_type: String::new(), data });
                (String::new(), Err(error(BAD_REQUEST, &e.to_string())))
            }
        };
        let (message_type, data) = match reply {
            Ok(ok) => ok,
            Err(e) => (API_ERROR_TYPE, serde_json::to_value(e)?),
        };
        let env = Envelope {
            api_name: API_NAME.into(),
            api_version: API_VERSION.into(),
            request_id,
            message_type: message_type.into(),
            data,
            timestamp: Some(i64::try_from(now.as_millis()).unwrap_or(i64::MAX)),
        };
        serde_json::to_string(&env)
    }

    fn dispatch(&mut self, session: u64, kind: &str, data: Value, now: Duration) -> Reply {
        let authenticated = self.sessions.get(&session).is_some_and(|s| s.authenticated);
        match kind {
            "APIStateRequest" => ok(&ApiStateResponse {
                active: true,
                v_tube_studio_version: "mock".into(),
                current_session_authenticated: authenticated,
            }),
            "AuthenticationTokenRequest" => self.token(&parse(data)?),
            "AuthenticationRequest" => self.authenticate(session, &parse(data)?),
            _ if !authenticated => Err(error(NOT_AUTHENTICATED, "not authenticated")),
            "StatisticsRequest" => ok(&StatisticsResponse {
                uptime: now.as_secs(),
                framerate: self.framerate,
                v_tube_studio_version: "mock".into(),
                allowed_plugins: 1,
                connected_plugins: u32::try_from(self.sessions.len()).unwrap_or(u32::MAX),
                started_with_steam: false,
                window_width: 1280,
                window_height: 720,
                window_is_fullscreen: false,
            }),
            "CurrentModelRequest" => ok(&self.current_model()),
            "InputParameterListRequest" => ok(&self.parameter_list()),
            "ParameterCreationRequest" => self.create_param(session, &parse(data)?),
            "ParameterValueRequest" => self.param_value(&parse(data)?),
            "InjectParameterDataRequest" => self.inject(data, now),
            "HotkeysInCurrentModelRequest" => self.hotkeys(),
            "HotkeyTriggerRequest" => self.trigger(&parse(data)?, now),
            "ExpressionStateRequest" => self.expression_state(),
            "ExpressionActivationRequest" => self.activate(&parse(data)?),
            other => Err(error(UNKNOWN_MESSAGE, &format!("the mock does not speak {other}"))),
        }
    }

    fn token(&mut self, _request: &AuthenticationTokenRequest) -> Reply {
        if !self.approve_token {
            return Err(error(TOKEN_DENIED, "the user denied the token request"));
        }
        self.issued += 1;
        let authentication_token = format!("mock-token-{}", self.issued);
        self.tokens.insert(authentication_token.clone());
        ok(&AuthenticationTokenResponse { authentication_token })
    }

    fn authenticate(&mut self, session: u64, request: &AuthenticationRequest) -> Reply {
        let valid = self.tokens.contains(&request.authentication_token);
        if let Some(s) = self.sessions.get_mut(&session) {
            s.authenticated = valid;
            s.plugin.clone_from(&request.plugin_name);
        }
        let reason = if valid { "" } else { "the token is not valid (revoked or never issued)" };
        ok(&AuthenticationResponse { authenticated: valid, reason: reason.into() })
    }

    fn current_model(&self) -> CurrentModelResponse {
        let Some(model) = &self.model else { return CurrentModelResponse::default() };
        CurrentModelResponse {
            model_loaded: true,
            model_name: model.name.clone(),
            model_id: model.name.clone(),
            number_of_live2d_parameters: u32::try_from(model.parameters.len()).unwrap_or(u32::MAX),
            ..CurrentModelResponse::default()
        }
    }

    fn input(&self, name: &str, added_by: &str, (min, max, default): (f64, f64, f64)) -> InputParameter {
        InputParameter {
            name: name.into(),
            added_by: added_by.into(),
            value: self.values.get(name).copied().unwrap_or(default),
            min,
            max,
            default_value: default,
        }
    }

    fn parameter_list(&self) -> InputParameterListResponse {
        let model = self.model.clone().unwrap_or_default();
        InputParameterListResponse {
            model_loaded: self.model.is_some(),
            model_name: model.name.clone(),
            model_id: model.name,
            custom_parameters: self
                .custom
                .iter()
                .map(|(n, c)| self.input(n, &c.owner, (c.min, c.max, c.default)))
                .collect(),
            default_parameters: model.parameters.iter().map(|n| self.input(n, "", (-1.0, 1.0, 0.0))).collect(),
        }
    }

    fn is_param(&self, name: &str) -> bool {
        self.custom.contains_key(name) || self.model.as_ref().is_some_and(|m| m.parameters.iter().any(|p| p == name))
    }

    fn create_param(&mut self, session: u64, request: &ParameterCreationRequest) -> Reply {
        let owner = self.sessions.get(&session).map(|s| s.plugin.clone()).unwrap_or_default();
        let name = request.parameter_name.as_str();
        if self.is_param(name) {
            return Err(error(PARAM_EXISTS, "a parameter with this name exists"));
        }
        if self.custom.values().filter(|c| c.owner == owner).count() >= MAX_PLUGIN_PARAMS {
            return Err(error(PARAM_LIMIT, &format!("a plugin may create {MAX_PLUGIN_PARAMS} custom parameters")));
        }
        let (min, max, default) = (request.min.get(), request.max.get(), request.default_value.get());
        self.custom.insert(name.to_owned(), CustomParam { owner, min, max, default });
        ok(&ParameterCreationResponse { parameter_name: name.to_owned() })
    }

    fn param_value(&self, request: &ParameterValueRequest) -> Reply {
        if !self.is_param(&request.name) {
            return Err(error(UNKNOWN_PARAM, "unknown parameter"));
        }
        let range = self.custom.get(&request.name).map_or((-1.0, 1.0, 0.0), |c| (c.min, c.max, c.default));
        ok(&ParameterValueResponse(self.input(&request.name, "", range)))
    }

    /// All-or-nothing: one bad value, unknown parameter or claimed parameter in `set` mode rejects the request.
    fn inject(&mut self, data: Value, now: Duration) -> Reply {
        let out_of_range = data["parameterValues"].as_array().is_some_and(|values| {
            values.iter().any(|v| v["value"].as_f64().is_some_and(|x| !x.is_finite() || x.abs() > PARAM_VALUE_LIMIT))
        });
        if out_of_range {
            return Err(error(VALUE_OUT_OF_RANGE, "a value is outside the allowed range"));
        }
        let request: InjectParameterDataRequest = parse(data)?;
        for v in &request.parameter_values {
            if !self.is_param(&v.id) {
                return Err(error(UNKNOWN_PARAM, &format!("unknown parameter {}", v.id)));
            }
            if request.mode == InjectMode::Set && self.claimed.contains(&v.id) {
                return Err(error(PARAM_OWNED, &format!("{} is controlled by another plugin", v.id)));
            }
        }
        for v in &request.parameter_values {
            self.values.insert(v.id.clone(), v.value.get());
            self.last_fed.insert(v.id.clone(), now);
        }
        ok(&InjectParameterDataResponse {})
    }

    fn loaded_model(&self, id: i32) -> Result<&MockModel, ApiError> {
        self.model.as_ref().ok_or_else(|| error(id, "no model is loaded"))
    }

    fn hotkeys(&self) -> Reply {
        let model = self.loaded_model(NO_MODEL)?;
        let available_hotkeys = model
            .hotkeys
            .iter()
            .enumerate()
            .map(|(i, name)| Hotkey {
                name: name.clone(),
                kind: "TriggerAnimation".into(),
                description: String::new(),
                file: String::new(),
                hotkey_id: name.clone(),
                on_screen_button_id: i32::try_from(i).unwrap_or(i32::MAX),
            })
            .collect();
        ok(&HotkeysInCurrentModelResponse {
            model_loaded: true,
            model_name: model.name.clone(),
            model_id: model.name.clone(),
            available_hotkeys,
        })
    }

    /// Refuses a hotkey inside its 5-frame cooldown, and any trigger when 32 are queued. VTS plays one per frame.
    fn trigger(&mut self, request: &HotkeyTriggerRequest, now: Duration) -> Reply {
        let id = &request.hotkey_id;
        if !self.loaded_model(NO_MODEL)?.hotkeys.contains(id) {
            return Err(error(UNKNOWN_HOTKEY, "unknown hotkey"));
        }
        let frame = Duration::from_nanos(frame_nanos(self.framerate));
        if self.last_trigger.get(id).is_some_and(|&last| now.saturating_sub(last) < frame * HOTKEY_MIN_FRAMES) {
            return Err(error(HOTKEY_COOLDOWN, &format!("a hotkey fires once every {HOTKEY_MIN_FRAMES} frames")));
        }
        while self.queue.front().is_some_and(|&done| done <= now) {
            self.queue.pop_front();
        }
        if self.queue.len() >= HOTKEY_QUEUE {
            return Err(error(QUEUE_FULL, &format!("the queue holds {HOTKEY_QUEUE} hotkeys")));
        }
        let start = self.queue.back().copied().unwrap_or(now).max(now);
        self.queue.push_back(start + frame);
        self.last_trigger.insert(id.clone(), now);
        ok(&HotkeyTriggerResponse { hotkey_id: id.clone() })
    }

    fn expression_state(&self) -> Reply {
        let model = self.loaded_model(EXPRESSION_NO_MODEL)?;
        let expressions = model
            .expressions
            .iter()
            .map(|name| Expression {
                name: name.clone(),
                file: format!("{name}.exp3.json"),
                active: self.active_expressions.contains(name),
                deactivate_when_key_is_let_go: false,
                auto_deactivate_after_seconds: false,
                seconds_remaining: 0.0,
                used_in_hotkeys: Vec::new(),
                parameters: Vec::new(),
            })
            .collect();
        ok(&ExpressionStateResponse {
            model_loaded: true,
            model_name: model.name.clone(),
            model_id: model.name.clone(),
            expressions,
        })
    }

    fn activate(&mut self, request: &ExpressionActivationRequest) -> Reply {
        let model = self.loaded_model(EXPRESSION_NO_MODEL)?;
        let name = model
            .expressions
            .iter()
            .find(|n| format!("{n}.exp3.json") == request.expression_file)
            .cloned()
            .ok_or_else(|| error(UNKNOWN_EXPRESSION, "unknown expression file"))?;
        if request.active {
            self.active_expressions.insert(name);
        } else {
            self.active_expressions.remove(&name);
        }
        ok(&ExpressionActivationResponse {})
    }
}
