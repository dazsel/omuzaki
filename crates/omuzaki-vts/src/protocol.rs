//! The wire messages: the [`Envelope`] every message travels in, the error payload, and the request, response and
//! event data for exactly the messages the bridge uses (it asks for no optional permissions).
//!
//! Every response type ignores fields it does not know, because the API adds fields without a version bump.

use serde::{Deserialize, Serialize};

use crate::limits::{CustomParamName, FadeTime, ParamValue};

/// The `apiName` every message carries.
pub const API_NAME: &str = "VTubeStudioPublicAPI";
/// The `apiVersion` this crate speaks; VTS raises it only for incompatible changes.
pub const API_VERSION: &str = "1.0";
/// The `messageType` of an error response.
pub const API_ERROR_TYPE: &str = "APIError";
/// The event name for [`EventSubscriptionRequest::model_loaded`].
pub const MODEL_LOADED_EVENT: &str = "ModelLoadedEvent";

/// A message data type with a fixed `messageType`.
pub trait Message {
    /// The `messageType` string on the wire.
    const MESSAGE_TYPE: &'static str;
}

/// The frame around every message, in both directions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope<T> {
    /// Always [`API_NAME`].
    pub api_name: String,
    /// Always [`API_VERSION`] today.
    pub api_version: String,
    /// Echoed in the response; 1 to 64 ASCII characters.
    #[serde(rename = "requestID")]
    pub request_id: String,
    /// Which message `data` is.
    pub message_type: String,
    /// The payload. VTS always sends it, as an empty object when there is nothing to say.
    pub data: T,
    /// Milliseconds since the Unix epoch, on responses only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
}

impl<T: Message> Envelope<T> {
    /// Wraps `data` for sending, with the `messageType` taken from its type.
    pub fn new(request_id: impl Into<String>, data: T) -> Self {
        Self {
            api_name: API_NAME.into(),
            api_version: API_VERSION.into(),
            request_id: request_id.into(),
            message_type: T::MESSAGE_TYPE.into(),
            data,
            timestamp: None,
        }
    }
}

impl<T> Envelope<T> {
    /// True when the frame speaks the API this crate was written for.
    pub fn is_current(&self) -> bool {
        self.api_name == API_NAME && self.api_version == API_VERSION
    }

    /// True when this is an error response (its `data` is an [`ApiError`], whatever `T` is).
    pub fn is_error(&self) -> bool {
        self.message_type == API_ERROR_TYPE
    }
}

/// The data of an `APIError` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("VTube Studio error {error_id}: {message}")]
pub struct ApiError {
    /// The numeric error id (`ErrorID` in the VTS repo).
    #[serde(rename = "errorID")]
    pub error_id: i32,
    /// VTS's human-readable text.
    pub message: String,
}

impl ApiError {
    /// The error classified for the bridge.
    pub fn kind(&self) -> ErrorKind {
        ErrorKind::from_id(self.error_id)
    }
}

/// The error ids the bridge acts on; every other id is [`ErrorKind::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Not authenticated (8), or the user denied or revoked API access (50): fetch a token again.
    NotAuthenticated,
    /// `set` mode on a parameter another plugin controls (454): retry with `add`, or tell the user.
    ParamOwnedByOtherPlugin,
    /// A parameter that does not exist (453, 500).
    UnknownParam,
    /// A value outside ±1,000,000 or not a number (451).
    ValueOutOfRange,
    /// The hotkey queue is full (200) or this hotkey is still cooling down (203): back off.
    HotkeyBusy,
    /// A hotkey (201) or an expression (652) needs a model and none is loaded.
    ModelNotLoaded,
    /// Any other id.
    Other(i32),
}

impl ErrorKind {
    /// Classifies a raw error id.
    pub fn from_id(id: i32) -> Self {
        match id {
            8 | 50 => Self::NotAuthenticated,
            454 => Self::ParamOwnedByOtherPlugin,
            453 | 500 => Self::UnknownParam,
            451 => Self::ValueOutOfRange,
            200 | 203 => Self::HotkeyBusy,
            201 | 652 => Self::ModelNotLoaded,
            other => Self::Other(other),
        }
    }
}

/// Declares messages whose `data` is an empty object, each its own type so each has its own `messageType`.
macro_rules! empty_message {
    ($($(#[$doc:meta])* $ty:ident => $name:literal),+ $(,)?) => {
        $(
            $(#[$doc])*
            #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
            pub struct $ty {}
            impl Message for $ty { const MESSAGE_TYPE: &'static str = $name; }
        )+
    };
}

empty_message! {
    /// `APIStateRequest`: is the API on, and is this session authenticated.
    ApiStateRequest => "APIStateRequest",
    /// `StatisticsRequest`.
    StatisticsRequest => "StatisticsRequest",
    /// `CurrentModelRequest`.
    CurrentModelRequest => "CurrentModelRequest",
    /// `InputParameterListRequest`: avoid at 60 fps or more, the answer is large.
    InputParameterListRequest => "InputParameterListRequest",
    /// `InjectParameterDataResponse`: the injection was accepted.
    InjectParameterDataResponse => "InjectParameterDataResponse",
    /// `ExpressionActivationResponse`: the expression changed.
    ExpressionActivationResponse => "ExpressionActivationResponse",
}

macro_rules! message {
    ($($ty:ty => $name:literal),+ $(,)?) => {
        $(impl Message for $ty { const MESSAGE_TYPE: &'static str = $name; })+
    };
}

/// `APIStateResponse` data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiStateResponse {
    /// API access is switched on in VTS.
    pub active: bool,
    /// The VTS version.
    pub v_tube_studio_version: String,
    /// This session has authenticated.
    pub current_session_authenticated: bool,
}

/// `AuthenticationTokenRequest` data: asks the user, in a VTS popup, to allow the plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthenticationTokenRequest {
    /// [`crate::PLUGIN_NAME`].
    pub plugin_name: String,
    /// [`crate::PLUGIN_DEVELOPER`].
    pub plugin_developer: String,
    /// A base64 PNG or JPG of exactly 128x128 pixels, shown in the popup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_icon: Option<String>,
}

impl AuthenticationTokenRequest {
    /// The request with the fixed Omuzaki identity and no icon.
    pub fn omuzaki() -> Self {
        Self {
            plugin_name: crate::PLUGIN_NAME.into(),
            plugin_developer: crate::PLUGIN_DEVELOPER.into(),
            plugin_icon: None,
        }
    }
}

/// `AuthenticationTokenResponse` data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthenticationTokenResponse {
    /// The token (at most 64 ASCII characters) to store and reuse in later sessions.
    pub authentication_token: String,
}

/// `AuthenticationRequest` data: authenticates this session with a stored token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthenticationRequest {
    /// Must match the name used to get the token.
    pub plugin_name: String,
    /// Must match the developer used to get the token.
    pub plugin_developer: String,
    /// The stored token.
    pub authentication_token: String,
}

impl AuthenticationRequest {
    /// The request with the fixed Omuzaki identity and `token`.
    pub fn omuzaki(token: impl Into<String>) -> Self {
        Self {
            plugin_name: crate::PLUGIN_NAME.into(),
            plugin_developer: crate::PLUGIN_DEVELOPER.into(),
            authentication_token: token.into(),
        }
    }
}

/// `AuthenticationResponse` data. A bad or revoked token gives `authenticated: false`, not an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthenticationResponse {
    /// The session is authenticated.
    pub authenticated: bool,
    /// VTS's explanation.
    #[serde(default)]
    pub reason: String,
}

/// `StatisticsResponse` data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatisticsResponse {
    /// Milliseconds since VTS started.
    pub uptime: u64,
    /// VTS's render frame rate.
    pub framerate: u32,
    /// The VTS version.
    pub v_tube_studio_version: String,
    /// Plugins the user has allowed.
    pub allowed_plugins: u32,
    /// Plugins connected now.
    pub connected_plugins: u32,
    /// VTS was started through Steam.
    pub started_with_steam: bool,
    /// Window width in pixels.
    pub window_width: u32,
    /// Window height in pixels.
    pub window_height: u32,
    /// The window is fullscreen.
    pub window_is_fullscreen: bool,
}

/// `CurrentModelResponse` data. With no model loaded every field but `model_loaded` is empty or zero.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CurrentModelResponse {
    /// A model is loaded and not mid-load.
    pub model_loaded: bool,
    /// The model's display name.
    pub model_name: String,
    /// The model's unique id.
    #[serde(rename = "modelID")]
    pub model_id: String,
    /// The `.vtube.json` file name.
    pub vts_model_name: String,
    /// The `.model3.json` file name.
    #[serde(rename = "live2DModelName")]
    pub live2d_model_name: String,
    /// How many `Live2D` parameters the model has.
    #[serde(rename = "numberOfLive2DParameters")]
    pub number_of_live2d_parameters: u32,
    /// The model has a physics file.
    pub has_physics_file: bool,
}

/// One tracking parameter as VTS reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputParameter {
    /// The parameter name.
    pub name: String,
    /// `VTube Studio` for built-ins, otherwise the creating plugin.
    pub added_by: String,
    /// The current value.
    pub value: f64,
    /// The default lower bound used when mapping.
    pub min: f64,
    /// The default upper bound used when mapping.
    pub max: f64,
    /// The value when nothing drives it.
    pub default_value: f64,
}

/// `InputParameterListResponse` data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputParameterListResponse {
    /// A model is loaded.
    pub model_loaded: bool,
    /// The model's display name.
    pub model_name: String,
    /// The model's unique id.
    #[serde(rename = "modelID")]
    pub model_id: String,
    /// Parameters created by plugins.
    pub custom_parameters: Vec<InputParameter>,
    /// VTS's own tracking parameters.
    pub default_parameters: Vec<InputParameter>,
}

/// `ParameterCreationRequest` data. Repeating it from the same plugin updates `min`, `max` and `default_value`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParameterCreationRequest {
    /// The new parameter's name.
    pub parameter_name: CustomParamName,
    /// What it does, shown to the user; under 256 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    /// The lower bound used when the user creates a mapping.
    pub min: ParamValue,
    /// The upper bound used when the user creates a mapping.
    pub max: ParamValue,
    /// The value when nothing drives it.
    pub default_value: ParamValue,
}

/// `ParameterCreationResponse` data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParameterCreationResponse {
    /// The created parameter's name.
    pub parameter_name: String,
}

/// `ParameterValueRequest` data: the value of one parameter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParameterValueRequest {
    /// The parameter name, default or custom.
    pub name: String,
}

/// `ParameterValueResponse` data: the parameter itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParameterValueResponse(pub InputParameter);

/// How injected values combine with what is already driving a parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InjectMode {
    /// Override it; only one plugin may do this per parameter at a time.
    #[default]
    Set,
    /// Add to it; any number of plugins may.
    Add,
}

/// One parameter value to inject.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InjectedValue {
    /// The parameter name, default or custom.
    pub id: String,
    /// The value.
    pub value: ParamValue,
    /// 0 to 1: how much of the injected value replaces face tracking (`set` mode only; default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<f64>,
}

/// `InjectParameterDataRequest` data. Send one batched request per frame, and re-send within the keep-alive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InjectParameterDataRequest {
    /// Tells VTS to treat the face as found, so it does not play the tracking-lost animation.
    #[serde(default)]
    pub face_found: bool,
    /// Override or add.
    #[serde(default)]
    pub mode: InjectMode,
    /// The values.
    pub parameter_values: Vec<InjectedValue>,
}

/// `HotkeysInCurrentModelRequest` data. Both fields are optional; the default asks for the current model.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeysInCurrentModelRequest {
    /// Another model's id.
    #[serde(default, rename = "modelID", skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// A `Live2D` item's file name (ignored when `model_id` is set).
    #[serde(default, rename = "live2DItemFileName", skip_serializing_if = "Option::is_none")]
    pub live2d_item_file_name: Option<String>,
}

/// One hotkey of a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hotkey {
    /// The hotkey's name.
    pub name: String,
    /// The action type, for example `ToggleExpression`; kept as text because VTS adds types over time.
    #[serde(rename = "type")]
    pub kind: String,
    /// VTS's description of the action.
    pub description: String,
    /// The expression, animation or model file it uses, or empty.
    pub file: String,
    /// The unique id to trigger it by.
    #[serde(rename = "hotkeyID")]
    pub hotkey_id: String,
    /// The on-screen button 1 to 8, or -1 for none.
    #[serde(rename = "onScreenButtonID")]
    pub on_screen_button_id: i32,
}

/// `HotkeysInCurrentModelResponse` data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeysInCurrentModelResponse {
    /// The model is loaded.
    pub model_loaded: bool,
    /// The model's display name.
    pub model_name: String,
    /// The model's unique id.
    #[serde(rename = "modelID")]
    pub model_id: String,
    /// Its hotkeys; empty with no model.
    pub available_hotkeys: Vec<Hotkey>,
}

/// `HotkeyTriggerRequest` data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyTriggerRequest {
    /// The hotkey's id or (case-insensitive) name.
    #[serde(rename = "hotkeyID")]
    pub hotkey_id: String,
    /// A `Live2D` item instance to trigger it in, instead of the model.
    #[serde(default, rename = "itemInstanceID", skip_serializing_if = "Option::is_none")]
    pub item_instance_id: Option<String>,
}

/// `HotkeyTriggerResponse` data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyTriggerResponse {
    /// The id of the hotkey that ran.
    #[serde(rename = "hotkeyID")]
    pub hotkey_id: String,
}

/// `ExpressionStateRequest` data.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpressionStateRequest {
    /// Also fill `used_in_hotkeys` and `parameters`.
    #[serde(default)]
    pub details: bool,
    /// One `.exp3.json` file; empty or absent lists all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expression_file: Option<String>,
}

/// A hotkey that uses an expression.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyRef {
    /// The hotkey's name.
    pub name: String,
    /// The hotkey's id.
    pub id: String,
}

/// One value an expression sets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpressionParameter {
    /// The `Live2D` parameter id.
    pub name: String,
    /// The target value.
    pub value: f64,
}

/// One expression of the current model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Expression {
    /// The file name without `.exp3.json`.
    pub name: String,
    /// The file name.
    pub file: String,
    /// It is on now.
    pub active: bool,
    /// A hotkey turned it on and it ends when the key is released.
    pub deactivate_when_key_is_let_go: bool,
    /// A hotkey turned it on and it ends by itself.
    pub auto_deactivate_after_seconds: bool,
    /// Seconds until it ends, when `auto_deactivate_after_seconds`.
    pub seconds_remaining: f64,
    /// Hotkeys that use it (only with `details`).
    #[serde(default)]
    pub used_in_hotkeys: Vec<HotkeyRef>,
    /// Its parameter targets (only with `details`).
    #[serde(default)]
    pub parameters: Vec<ExpressionParameter>,
}

/// `ExpressionStateResponse` data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpressionStateResponse {
    /// A model is loaded.
    pub model_loaded: bool,
    /// The model's display name.
    pub model_name: String,
    /// The model's unique id.
    #[serde(rename = "modelID")]
    pub model_id: String,
    /// Its expressions; empty with no model.
    pub expressions: Vec<Expression>,
}

/// `ExpressionActivationRequest` data. Hotkeys are the safer way: a user can be left with an expression they cannot end.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpressionActivationRequest {
    /// The `.exp3.json` file name.
    pub expression_file: String,
    /// The fade in; VTS fades out with the same time.
    #[serde(default)]
    pub fade_time: FadeTime,
    /// Turn it on or off.
    pub active: bool,
}

/// The `config` of a `ModelLoadedEvent` subscription.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ModelLoadedConfig {
    /// Only these models (32 hex characters each); empty for every model.
    #[serde(default, rename = "modelID", skip_serializing_if = "Vec::is_empty")]
    pub model_id: Vec<String>,
}

/// `EventSubscriptionRequest` data, for the one event the bridge uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventSubscriptionRequest {
    /// Always [`MODEL_LOADED_EVENT`].
    pub event_name: String,
    /// Subscribe (true) or unsubscribe (false).
    pub subscribe: bool,
    /// The event's filter.
    #[serde(default)]
    pub config: ModelLoadedConfig,
}

impl EventSubscriptionRequest {
    /// Subscribes to, or unsubscribes from, `ModelLoadedEvent` for every model.
    pub fn model_loaded(subscribe: bool) -> Self {
        Self { event_name: MODEL_LOADED_EVENT.into(), subscribe, config: ModelLoadedConfig::default() }
    }
}

/// `EventSubscriptionResponse` data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventSubscriptionResponse {
    /// How many events this session is subscribed to now.
    pub subscribed_event_count: u32,
    /// Their names.
    pub subscribed_events: Vec<String>,
}

/// `ModelLoadedEvent` data: sent when a model loads or unloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelLoadedEvent {
    /// True for a load, false for an unload.
    pub model_loaded: bool,
    /// The model's display name.
    pub model_name: String,
    /// The model's unique id.
    #[serde(rename = "modelID")]
    pub model_id: String,
}

message! {
    ParameterValueResponse => "ParameterValueResponse",
    ApiStateResponse => "APIStateResponse",
    AuthenticationTokenRequest => "AuthenticationTokenRequest",
    AuthenticationTokenResponse => "AuthenticationTokenResponse",
    AuthenticationRequest => "AuthenticationRequest",
    AuthenticationResponse => "AuthenticationResponse",
    StatisticsResponse => "StatisticsResponse",
    CurrentModelResponse => "CurrentModelResponse",
    InputParameterListResponse => "InputParameterListResponse",
    ParameterCreationRequest => "ParameterCreationRequest",
    ParameterCreationResponse => "ParameterCreationResponse",
    ParameterValueRequest => "ParameterValueRequest",
    InjectParameterDataRequest => "InjectParameterDataRequest",
    HotkeysInCurrentModelRequest => "HotkeysInCurrentModelRequest",
    HotkeysInCurrentModelResponse => "HotkeysInCurrentModelResponse",
    HotkeyTriggerRequest => "HotkeyTriggerRequest",
    HotkeyTriggerResponse => "HotkeyTriggerResponse",
    ExpressionStateRequest => "ExpressionStateRequest",
    ExpressionStateResponse => "ExpressionStateResponse",
    ExpressionActivationRequest => "ExpressionActivationRequest",
    EventSubscriptionRequest => "EventSubscriptionRequest",
    EventSubscriptionResponse => "EventSubscriptionResponse",
    ModelLoadedEvent => "ModelLoadedEvent",
    ApiError => "APIError",
}
