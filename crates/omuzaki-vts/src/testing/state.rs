//! The mock's protocol state and request handling; no I/O here, so the rules are plain data in, plain data out.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::MockConfig;
use crate::limits::{HOTKEY_MIN_FRAMES, HOTKEY_QUEUE, KEEPALIVE, MAX_PLUGIN_PARAMS, PARAM_VALUE_LIMIT};
use crate::protocol::{
    API_ERROR_TYPE, API_NAME, API_VERSION, ApiError, ApiStateResponse, AuthenticationRequest, AuthenticationResponse,
    AuthenticationTokenRequest, AuthenticationTokenResponse, CurrentModelResponse, Envelope, Expression,
    ExpressionActivationRequest, ExpressionActivationResponse, ExpressionStateRequest, ExpressionStateResponse, Hotkey,
    HotkeyTriggerRequest, HotkeyTriggerResponse, HotkeysInCurrentModelResponse, InjectMode, InjectParameterDataRequest,
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
const API_NAME_INVALID: i32 = 3;
const API_VERSION_INVALID: i32 = 4;
const UNKNOWN_MESSAGE: i32 = 2;
const UNKNOWN_HOTKEY: i32 = 202;
const UNKNOWN_EXPRESSION: i32 = 651;
const PARAM_LIMIT: i32 = 450;
const PARAM_EXISTS: i32 = 452;

/// Who VTS reports as the owner of its own (built-in) parameters.
const BUILT_IN_OWNER: &str = "VTube Studio";

/// A hotkey of the mock model: triggered by `id`, or by `name` ignoring case.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MockHotkey {
    /// The hotkey's unique id.
    pub id: String,
    /// The hotkey's display name.
    pub name: String,
    /// The on-screen button it is bound to, 1 to 8. `None`, or a number outside that range, is unbound (-1).
    pub on_screen_button: Option<u8>,
}

/// The model VTS has loaded. An expression's file is `<name>.exp3.json`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MockModel {
    /// The model's name.
    pub name: String,
    /// The hotkeys.
    pub hotkeys: Vec<MockHotkey>,
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

/// What one plugin last fed into one parameter, and when.
struct Feed {
    mode: InjectMode,
    value: f64,
    weight: f64,
    at: Duration,
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
    start_epoch_ms: i64,
    /// Each issued token with the `(pluginName, pluginDeveloper)` it was issued to.
    tokens: BTreeMap<String, (String, String)>,
    issued: u32,
    next_session: u64,
    sessions: BTreeMap<u64, Session>,
    custom: BTreeMap<String, CustomParam>,
    /// The live feeds, keyed by (parameter, plugin).
    feeds: BTreeMap<(String, String), Feed>,
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
    pub fn new(config: &MockConfig) -> Self {
        Self {
            approve_token: config.approve_token,
            model: config.model.clone(),
            framerate: config.framerate.max(1),
            start_epoch_ms: config.start_epoch_ms,
            claimed: BTreeSet::new(),
            received: Vec::new(),
            events: Vec::new(),
            tokens: BTreeMap::new(),
            issued: 0,
            next_session: 0,
            sessions: BTreeMap::new(),
            custom: BTreeMap::new(),
            feeds: BTreeMap::new(),
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

    /// Lapses every feed not repeated for [`KEEPALIVE`] by `now` (research line 63, README "Feeding in data for default
    /// or custom parameters": re-send "at least once every second").
    pub fn settle(&mut self, now: Duration) {
        let lapsed: Vec<_> = self
            .feeds
            .iter()
            .filter(|&(_, feed)| now.saturating_sub(feed.at) >= KEEPALIVE)
            .map(|(key, feed)| (key.clone(), feed.at))
            .collect();
        for (key, at) in lapsed {
            self.feeds.remove(&key);
            self.events.push(Event::keepalive_lapse(key.0, at + KEEPALIVE));
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
                let reply = Self::check_envelope(&env)
                    .and_then(|()| self.dispatch(session, &env.message_type, env.data.clone(), now));
                (env.request_id, reply)
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
            timestamp: Some(self.start_epoch_ms.saturating_add(i64::try_from(now.as_millis()).unwrap_or(i64::MAX))),
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
                uptime: u64::try_from(now.as_millis()).unwrap_or(u64::MAX),
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
            "InjectParameterDataRequest" => self.inject(session, data, now),
            "HotkeysInCurrentModelRequest" => self.hotkeys(),
            "HotkeyTriggerRequest" => self.trigger(&parse(data)?, now),
            "ExpressionStateRequest" => self.expression_state(&parse(data)?),
            "ExpressionActivationRequest" => self.activate(&parse(data)?),
            other => Err(error(UNKNOWN_MESSAGE, &format!("the mock does not speak {other}"))),
        }
    }

    fn token(&mut self, request: &AuthenticationTokenRequest) -> Reply {
        if !self.approve_token {
            return Err(error(TOKEN_DENIED, "the user denied the token request"));
        }
        self.issued += 1;
        let authentication_token = format!("mock-token-{}", self.issued);
        let identity = (request.plugin_name.clone(), request.plugin_developer.clone());
        self.tokens.insert(authentication_token.clone(), identity);
        ok(&AuthenticationTokenResponse { authentication_token })
    }

    /// A token only works for the plugin name and developer it was issued to (research line 43, section 5: the token
    /// is tied to both).
    fn authenticate(&mut self, session: u64, request: &AuthenticationRequest) -> Reply {
        let identity = (request.plugin_name.clone(), request.plugin_developer.clone());
        let valid = self.tokens.get(&request.authentication_token) == Some(&identity);
        if let Some(s) = self.sessions.get_mut(&session) {
            s.authenticated = valid;
            s.plugin.clone_from(&request.plugin_name);
        }
        let reason = if valid {
            ""
        } else {
            "the token is not valid for this plugin (revoked, never issued, or issued to another)"
        };
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

    /// The (min, max, default) of a parameter: its own for a custom one, VTS's usual -1..=1 for a built-in.
    fn range(&self, name: &str) -> (f64, f64, f64) {
        self.custom.get(name).map_or((-1.0, 1.0, 0.0), |c| (c.min, c.max, c.default))
    }

    /// The parameter as VTS lists it, with the value composed from the live feeds.
    fn input(&self, name: &str) -> InputParameter {
        let (min, max, default_value) = self.range(name);
        InputParameter {
            name: name.into(),
            added_by: self.custom.get(name).map_or(BUILT_IN_OWNER, |c| c.owner.as_str()).into(),
            value: self.value_of(name).unwrap_or(default_value),
            min,
            max,
            default_value,
        }
    }

    /// The value a parameter has now: the `set` feed blended with the user's tracking by its `weight`, plus every `add`
    /// feed (research line 62: `add` stacks across plugins, `weight` "blends with the user's real face tracking").
    /// The README gives no formula, so the blend is linear and the mock's tracking input is the parameter's default.
    pub fn value_of(&self, name: &str) -> Option<f64> {
        if !self.is_param(name) {
            return None;
        }
        let (_, _, tracking) = self.range(name);
        let mut base = tracking;
        let mut added = 0.0;
        for feed in self.feeds.iter().filter(|((param, _), _)| param == name).map(|(_, feed)| feed) {
            match feed.mode {
                InjectMode::Set => base = tracking * (1.0 - feed.weight) + feed.value * feed.weight,
                InjectMode::Add => added += feed.value,
            }
        }
        Some(base + added)
    }

    fn parameter_list(&self) -> InputParameterListResponse {
        let model = self.model.clone().unwrap_or_default();
        InputParameterListResponse {
            model_loaded: self.model.is_some(),
            model_name: model.name.clone(),
            model_id: model.name,
            custom_parameters: self.custom.keys().map(|n| self.input(n)).collect(),
            default_parameters: model.parameters.iter().map(|n| self.input(n)).collect(),
        }
    }

    fn is_param(&self, name: &str) -> bool {
        self.custom.contains_key(name) || self.model.as_ref().is_some_and(|m| m.parameters.iter().any(|p| p == name))
    }

    /// At most 100 custom parameters per plugin: research `docs/research/2026-10-vts-plugin-api-terms.md` line 60,
    /// from the public `DenchiSoft/VTubeStudio` README ("Adding new tracking parameters"). The creating plugin may
    /// repeat the request to change the range and default; a name owned by another plugin or built in is refused.
    fn create_param(&mut self, session: u64, request: &ParameterCreationRequest) -> Reply {
        let owner = self.sessions.get(&session).map(|s| s.plugin.clone()).unwrap_or_default();
        let name = request.parameter_name.as_str();
        let (min, max, default) = (request.min.get(), request.max.get(), request.default_value.get());
        let own = self.custom.get(name).map(|c| c.owner == owner);
        if own == Some(false) || (own.is_none() && self.is_param(name)) {
            return Err(error(PARAM_EXISTS, "a parameter with this name exists"));
        }
        if own.is_none() && self.custom.values().filter(|c| c.owner == owner).count() >= MAX_PLUGIN_PARAMS {
            return Err(error(PARAM_LIMIT, &format!("a plugin may create {MAX_PLUGIN_PARAMS} custom parameters")));
        }
        self.custom.insert(name.to_owned(), CustomParam { owner, min, max, default });
        ok(&ParameterCreationResponse { parameter_name: name.to_owned() })
    }

    fn param_value(&self, request: &ParameterValueRequest) -> Reply {
        if !self.is_param(&request.name) {
            return Err(error(UNKNOWN_PARAM, "unknown parameter"));
        }
        ok(&ParameterValueResponse(self.input(&request.name)))
    }

    /// A parameter not fed for 1 s reverts: research line 63, README "Feeding in data for default or custom
    /// parameters" (see [`State::settle`]). `set` on a parameter another plugin controls errors, `add` stacks: line 62.
    ///
    /// All-or-nothing: one bad value, unknown parameter or claimed parameter in `set` mode rejects the request.
    fn inject(&mut self, session: u64, data: Value, now: Duration) -> Reply {
        let out_of_range = data["parameterValues"].as_array().is_some_and(|values| {
            values.iter().any(|v| v["value"].as_f64().is_some_and(|x| !x.is_finite() || x.abs() > PARAM_VALUE_LIMIT))
        });
        if out_of_range {
            return Err(error(VALUE_OUT_OF_RANGE, "a value is outside the allowed range"));
        }
        let request: InjectParameterDataRequest = parse(data)?;
        let plugin = self.sessions.get(&session).map(|s| s.plugin.clone()).unwrap_or_default();
        for v in &request.parameter_values {
            if !self.is_param(&v.id) {
                return Err(error(UNKNOWN_PARAM, &format!("unknown parameter {}", v.id)));
            }
            let held_by_other = self
                .feeds
                .iter()
                .any(|((param, owner), feed)| *param == v.id && *owner != plugin && feed.mode == InjectMode::Set);
            if request.mode == InjectMode::Set && (self.claimed.contains(&v.id) || held_by_other) {
                return Err(error(PARAM_OWNED, &format!("{} is controlled by another plugin", v.id)));
            }
        }
        for v in &request.parameter_values {
            let weight = v.weight.filter(|w| w.is_finite()).map_or(1.0, |w| w.clamp(0.0, 1.0));
            let feed = Feed { mode: request.mode, value: v.value.get(), weight, at: now };
            self.feeds.insert((v.id.clone(), plugin.clone()), feed);
        }
        ok(&InjectParameterDataResponse {})
    }

    /// Real VTS refuses a frame for another API or another version before it looks at the request.
    fn check_envelope(frame: &Envelope<Value>) -> Result<(), ApiError> {
        if frame.api_name != API_NAME {
            return Err(error(API_NAME_INVALID, &format!("apiName must be {API_NAME}")));
        }
        if frame.api_version != API_VERSION {
            return Err(error(API_VERSION_INVALID, &format!("apiVersion must be {API_VERSION}")));
        }
        Ok(())
    }

    fn loaded_model(&self, id: i32) -> Result<&MockModel, ApiError> {
        self.model.as_ref().ok_or_else(|| error(id, "no model is loaded"))
    }

    fn hotkeys(&self) -> Reply {
        // With no model VTS answers with `modelLoaded: false` and no hotkeys, not an error.
        let Some(model) = &self.model else {
            return ok(&HotkeysInCurrentModelResponse {
                model_loaded: false,
                model_name: String::new(),
                model_id: String::new(),
                available_hotkeys: Vec::new(),
            });
        };
        let available_hotkeys = model
            .hotkeys
            .iter()
            .map(|h| Hotkey {
                name: h.name.clone(),
                kind: "TriggerAnimation".into(),
                description: String::new(),
                file: String::new(),
                hotkey_id: h.id.clone(),
                on_screen_button_id: h.on_screen_button.filter(|b| (1..=8).contains(b)).map_or(-1, i32::from),
            })
            .collect();
        ok(&HotkeysInCurrentModelResponse {
            model_loaded: true,
            model_name: model.name.clone(),
            model_id: model.name.clone(),
            available_hotkeys,
        })
    }

    /// Refuses a hotkey inside its 5-frame cooldown at the reported framerate, and any trigger when 32 are queued
    /// (research line 66, README "Requesting execution of hotkeys"). VTS plays one per frame.
    fn trigger(&mut self, request: &HotkeyTriggerRequest, now: Duration) -> Reply {
        let key = &request.hotkey_id;
        // The real API takes the hotkey's id or its name (ignoring case); cooldowns follow the hotkey, not the spelling.
        let hotkeys = &self.loaded_model(NO_MODEL)?.hotkeys;
        let found =
            hotkeys.iter().find(|h| h.id == *key).or_else(|| hotkeys.iter().find(|h| h.name.eq_ignore_ascii_case(key)));
        let id = &found.ok_or_else(|| error(UNKNOWN_HOTKEY, "unknown hotkey"))?.id.clone();
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

    fn expression_state(&self, request: &ExpressionStateRequest) -> Reply {
        // With no model VTS answers with `modelLoaded: false` and no expressions, not an error.
        let Some(model) = &self.model else {
            return ok(&ExpressionStateResponse {
                model_loaded: false,
                model_name: String::new(),
                model_id: String::new(),
                expressions: Vec::new(),
            });
        };
        // An absent or empty `expressionFile` lists every expression (`protocol.rs`, `ExpressionStateRequest`).
        let wanted = |name: &String| {
            request.expression_file.as_deref().is_none_or(|file| file.is_empty() || file == format!("{name}.exp3.json"))
        };
        let expressions = model
            .expressions
            .iter()
            .filter(|name| wanted(name))
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
