//! The wire messages: the `{"op", "d"}` [`Frame`], the typed [`Message`], the supported requests with their
//! responses, and [`ObsEvent`].
//!
//! Field names are camelCase on the wire. Unknown fields are ignored, so a newer server does not break us; an
//! unknown `op` is [`Message::Unknown`] and an unknown event is [`ObsEvent::Other`].

use crate::limits::{EventSubscription, RPC_VERSION};
use serde::de::DeserializeOwned;
use serde::ser::Error as _;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A message could not be turned into the typed form.
///
/// The text never carries wire data beyond the op or a server-chosen type name, so an `Identify` that fails to parse
/// cannot leak its authentication string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    /// The data of a message with this `op` is missing a field or has a field of the wrong type.
    #[error("the data of the message with op {op} is malformed")]
    BadData {
        /// The op of the message.
        op: u8,
    },
    /// The data of an event of this type is malformed.
    #[error("the data of the {event_type} event is malformed")]
    BadEvent {
        /// The `eventType` of the event.
        event_type: String,
    },
    /// The response to this request has no usable data.
    #[error("the response to the {request_type} request is malformed")]
    BadResponse {
        /// The `requestType` of the response.
        request_type: String,
    },
}

/// The op of each message the crate types.
mod op {
    pub const HELLO: u8 = 0;
    pub const IDENTIFY: u8 = 1;
    pub const IDENTIFIED: u8 = 2;
    pub const REIDENTIFY: u8 = 3;
    pub const EVENT: u8 = 5;
    pub const REQUEST: u8 = 6;
    pub const REQUEST_RESPONSE: u8 = 7;
}

/// One WebSocket text message: `{"op": n, "d": {...}}`.
///
/// `Debug` hides the data of an `Identify` (op 1), which holds the authentication string.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    /// The message type.
    pub op: u8,
    /// The message data.
    pub d: Value,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("Frame");
        s.field("op", &self.op);
        if self.op == op::IDENTIFY {
            s.field("d", &"<redacted>");
        } else {
            s.field("d", &self.d);
        }
        s.finish()
    }
}

/// The authentication challenge in `Hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AuthChallenge {
    /// The server's challenge.
    pub challenge: String,
    /// The server's salt.
    pub salt: String,
}

impl AuthChallenge {
    /// A challenge from its two parts.
    #[must_use]
    pub fn new(challenge: impl Into<String>, salt: impl Into<String>) -> Self {
        Self { challenge: challenge.into(), salt: salt.into() }
    }
}

/// Op 0, server to client: the first message after connecting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Hello {
    /// The server plugin's version.
    pub obs_web_socket_version: String,
    /// The newest RPC version the server speaks.
    pub rpc_version: u32,
    /// Present when the server needs a password.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authentication: Option<AuthChallenge>,
}

impl Hello {
    /// A `Hello` for a server version, with or without a challenge.
    #[must_use]
    pub fn new(version: impl Into<String>, authentication: Option<AuthChallenge>) -> Self {
        Self { obs_web_socket_version: version.into(), rpc_version: RPC_VERSION, authentication }
    }
}

/// Op 1, client to server: the answer to `Hello`.
///
/// `Debug` never shows the authentication string.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Identify {
    /// The RPC version the client speaks.
    pub rpc_version: u32,
    /// The answer from [`auth_response`](crate::auth::auth_response); absent when the server sent no challenge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authentication: Option<String>,
    /// The events to receive; the server's default (all) when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_subscriptions: Option<u32>,
}

impl Identify {
    /// An `Identify` for [`RPC_VERSION`] with no authentication and the server's default subscriptions.
    #[must_use]
    pub fn new() -> Self {
        Self { rpc_version: RPC_VERSION, authentication: None, event_subscriptions: None }
    }

    /// Sets the authentication string.
    #[must_use]
    pub fn with_authentication(mut self, authentication: String) -> Self {
        self.authentication = Some(authentication);
        self
    }

    /// Sets the event subscriptions.
    #[must_use]
    pub fn with_subscriptions(mut self, subscriptions: EventSubscription) -> Self {
        self.event_subscriptions = Some(subscriptions.0);
        self
    }
}

impl Default for Identify {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Identify {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identify")
            .field("rpc_version", &self.rpc_version)
            .field("authentication", &self.authentication.as_ref().map(|_| "<redacted>"))
            .field("event_subscriptions", &self.event_subscriptions)
            .finish()
    }
}

/// Op 2, server to client: the session is open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Identified {
    /// The RPC version both sides will use.
    pub negotiated_rpc_version: u32,
}

impl Identified {
    /// An `Identified` for a negotiated version.
    #[must_use]
    pub fn new(negotiated_rpc_version: u32) -> Self {
        Self { negotiated_rpc_version }
    }
}

/// Op 3, client to server: change the event subscriptions of an open session.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Reidentify {
    /// The new subscriptions; the server's default (all) when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_subscriptions: Option<u32>,
}

impl Reidentify {
    /// A `Reidentify` with these subscriptions.
    #[must_use]
    pub fn new(subscriptions: EventSubscription) -> Self {
        Self { event_subscriptions: Some(subscriptions.0) }
    }
}

/// Op 5, server to client: an event, still untyped. [`ObsEvent::from_msg`] types it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct EventMsg {
    /// The event's name.
    pub event_type: String,
    /// The [`EventSubscription`] bit a client needs to receive it.
    pub event_intent: u32,
    /// The event's fields, when it has any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_data: Option<Value>,
}

impl EventMsg {
    /// An event with its subscription bit and fields.
    #[must_use]
    pub fn new(event_type: impl Into<String>, event_intent: EventSubscription, event_data: Option<Value>) -> Self {
        Self { event_type: event_type.into(), event_intent: event_intent.0, event_data }
    }
}

/// Op 6, client to server: a request. [`RequestMsg::new`] builds one from a typed [`RequestType`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct RequestMsg {
    /// The request's name.
    pub request_type: String,
    /// The caller's id; the response echoes it.
    pub request_id: String,
    /// The request's fields, when it has any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_data: Option<Value>,
}

impl RequestMsg {
    /// The message for `request` under `request_id`. A request with no fields has no `requestData`.
    ///
    /// # Errors
    /// The serialisation error of `request`, passed on rather than sent without its data.
    pub fn new<R: RequestType>(request_id: impl Into<String>, request: &R) -> Result<Self, serde_json::Error> {
        let data = serde_json::to_value(request)?;
        let request_data = Some(data).filter(|v| v.as_object().is_none_or(|o| !o.is_empty()));
        Ok(Self { request_type: R::NAME.to_owned(), request_id: request_id.into(), request_data })
    }
}

/// The `requestStatus` of a response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RequestStatus {
    /// Whether the request succeeded.
    pub result: bool,
    /// The status code (100 is success, 600 is "resource not found"; see [`limits`](crate::limits)).
    pub code: u32,
    /// A human-readable reason, on failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

impl RequestStatus {
    /// The status of a request that succeeded.
    #[must_use]
    pub fn success() -> Self {
        Self { result: true, code: crate::limits::STATUS_SUCCESS, comment: None }
    }

    /// The status of a request that failed with `code`.
    #[must_use]
    pub fn failure(code: u32, comment: Option<String>) -> Self {
        Self { result: false, code, comment }
    }
}

/// Op 7, server to client: the answer to a request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ResponseMsg {
    /// The request's name.
    pub request_type: String,
    /// The id of the request this answers.
    pub request_id: String,
    /// Whether and why it worked.
    pub request_status: RequestStatus,
    /// The response's fields, when it has any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_data: Option<Value>,
}

impl ResponseMsg {
    /// A response to the request `request_id` for `R`, with `data` as the typed fields.
    ///
    /// # Errors
    /// The serialisation error of `data`, passed on rather than sent without its data.
    pub fn new<R: RequestType>(
        request_id: impl Into<String>,
        status: RequestStatus,
        data: Option<&R::Response>,
    ) -> Result<Self, serde_json::Error> {
        Ok(Self {
            request_type: R::NAME.to_owned(),
            request_id: request_id.into(),
            request_status: status,
            response_data: data.map(serde_json::to_value).transpose()?,
        })
    }

    /// The typed response fields for request `R`. A response with no data reads as an empty object, so requests with
    /// no response fields parse.
    ///
    /// # Errors
    /// [`ProtocolError::BadResponse`] when the data is missing a field or has one of the wrong type.
    pub fn data<R: RequestType>(&self) -> Result<R::Response, ProtocolError> {
        let data = self.response_data.clone().unwrap_or_else(|| Value::Object(Map::new()));
        serde_json::from_value(data).map_err(|_| ProtocolError::BadResponse { request_type: self.request_type.clone() })
    }
}

/// A typed message.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Message {
    /// Op 0.
    Hello(Hello),
    /// Op 1.
    Identify(Identify),
    /// Op 2.
    Identified(Identified),
    /// Op 3.
    Reidentify(Reidentify),
    /// Op 5.
    Event(EventMsg),
    /// Op 6.
    Request(RequestMsg),
    /// Op 7.
    RequestResponse(ResponseMsg),
    /// Any other op (the batch ops 8 and 9 among them); the data is dropped.
    Unknown {
        /// The op.
        op: u8,
    },
}

fn parse<T: DeserializeOwned>(op: u8, d: &Value) -> Result<T, ProtocolError> {
    serde_json::from_value(d.clone()).map_err(|_| ProtocolError::BadData { op })
}

impl Message {
    /// Types a frame by its op. An unknown op is [`Message::Unknown`], not an error.
    ///
    /// # Errors
    /// [`ProtocolError::BadData`] when the data of a known op does not fit its type.
    pub fn from_frame(f: &Frame) -> Result<Self, ProtocolError> {
        Ok(match f.op {
            op::HELLO => Self::Hello(parse(f.op, &f.d)?),
            op::IDENTIFY => Self::Identify(parse(f.op, &f.d)?),
            op::IDENTIFIED => Self::Identified(parse(f.op, &f.d)?),
            op::REIDENTIFY => Self::Reidentify(parse(f.op, &f.d)?),
            op::EVENT => Self::Event(parse(f.op, &f.d)?),
            op::REQUEST => Self::Request(parse(f.op, &f.d)?),
            op::REQUEST_RESPONSE => Self::RequestResponse(parse(f.op, &f.d)?),
            op => Self::Unknown { op },
        })
    }

    /// The frame for this message. [`Message::Unknown`] has no data, so it becomes an empty object.
    ///
    /// # Errors
    /// The serialisation error of the message's data, passed on rather than sent as `null`.
    pub fn to_frame(&self) -> Result<Frame, serde_json::Error> {
        let (op, d) = match self {
            Self::Hello(m) => (op::HELLO, serde_json::to_value(m)?),
            Self::Identify(m) => (op::IDENTIFY, serde_json::to_value(m)?),
            Self::Identified(m) => (op::IDENTIFIED, serde_json::to_value(m)?),
            Self::Reidentify(m) => (op::REIDENTIFY, serde_json::to_value(m)?),
            Self::Event(m) => (op::EVENT, serde_json::to_value(m)?),
            Self::Request(m) => (op::REQUEST, serde_json::to_value(m)?),
            Self::RequestResponse(m) => (op::REQUEST_RESPONSE, serde_json::to_value(m)?),
            Self::Unknown { op } => (*op, Value::Object(Map::new())),
        };
        Ok(Frame { op, d })
    }
}

/// Supertrait that seals [`RequestType`]: only the requests in this crate can implement it.
///
/// Sealing means every request and response type is one whose serialisation is known to succeed, so the constructors
/// never see a request that cannot be sent. An outside type could carry a map with non-string keys, which
/// `serde_json` refuses to serialise.
mod private {
    /// The seal.
    pub trait Sealed {}
}

/// A request this crate knows: its wire name and the type of its response fields.
///
/// The trait is sealed: a request type outside this crate cannot implement it.
///
/// ```compile_fail,E0277
/// #[derive(serde::Serialize)]
/// struct Outside {}
///
/// impl omuzaki_obs::protocol::RequestType for Outside {
///     const NAME: &'static str = "Outside";
///     type Response = omuzaki_obs::protocol::Empty;
/// }
/// ```
pub trait RequestType: Serialize + private::Sealed {
    /// The `requestType` on the wire.
    const NAME: &'static str;
    /// The `responseData` fields. A request with none uses [`Empty`].
    type Response: Serialize + DeserializeOwned;
}

/// The response fields of a request that has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Empty {}

macro_rules! request {
    ($(#[$doc:meta])* $name:ident { $($(#[$fdoc:meta])* $field:ident: $ty:ty),* $(,)? } => $resp:ty) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub struct $name { $($(#[$fdoc])* pub $field: $ty),* }
        impl private::Sealed for $name {}
        impl RequestType for $name {
            const NAME: &'static str = stringify!($name);
            type Response = $resp;
        }
    };
}

macro_rules! response {
    ($(#[$doc:meta])* $name:ident { $($(#[$fdoc:meta])* $field:ident: $ty:ty),* $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub struct $name { $($(#[$fdoc])* pub $field: $ty),* }
    };
}

response! {
    /// The response to [`GetVersion`].
    GetVersionResponse {
        /// The OBS Studio version.
        obs_version: String,
        /// The server plugin's version.
        obs_web_socket_version: String,
        /// The RPC version in use.
        rpc_version: u32,
        /// The request names this server supports.
        #[serde(default)]
        available_requests: Vec<String>,
        /// The image formats the screenshot requests accept.
        #[serde(default)]
        supported_image_formats: Vec<String>,
        /// The platform, for example `linux`.
        platform: String,
        /// A description of the platform.
        platform_description: String,
    }
}
response! {
    /// The response to [`GetSceneList`]. The scene objects are kept as JSON: the document does not list their fields.
    GetSceneListResponse {
        /// The program scene's name; `null` for a non-main canvas.
        current_program_scene_name: Option<String>,
        /// The preview scene's name; `null` outside studio mode.
        current_preview_scene_name: Option<String>,
        /// The scenes.
        scenes: Vec<Value>,
    }
}
response! {
    /// The response to [`GetCurrentProgramScene`].
    GetCurrentProgramSceneResponse {
        /// The program scene's name.
        scene_name: String,
        /// The program scene's UUID.
        scene_uuid: String,
    }
}
response! {
    /// The response to [`GetSceneItemList`]. The item objects are kept as JSON: the document does not list their fields.
    GetSceneItemListResponse {
        /// The scene's items.
        scene_items: Vec<Value>,
    }
}
response! {
    /// The response to [`GetSceneItemId`].
    GetSceneItemIdResponse {
        /// The item's numeric id.
        scene_item_id: i64,
    }
}
response! {
    /// The response to [`GetStreamStatus`].
    GetStreamStatusResponse {
        /// Whether the stream is live.
        output_active: bool,
        /// Whether the stream is reconnecting.
        output_reconnecting: bool,
        /// The stream's running time as `HH:MM:SS.mmm`.
        output_timecode: String,
        /// The stream's running time in milliseconds.
        output_duration: u64,
        /// The congestion, 0 to 1.
        output_congestion: f64,
        /// The bytes sent.
        output_bytes: u64,
        /// The frames skipped.
        output_skipped_frames: u64,
        /// The frames total.
        output_total_frames: u64,
    }
}
response! {
    /// The response to [`GetRecordStatus`].
    GetRecordStatusResponse {
        /// Whether a recording is running.
        output_active: bool,
        /// Whether it is paused.
        output_paused: bool,
        /// The recording's running time as `HH:MM:SS.mmm`.
        output_timecode: String,
        /// The recording's running time in milliseconds.
        output_duration: u64,
        /// The bytes written.
        output_bytes: u64,
    }
}
response! {
    /// The response to [`StopRecord`].
    StopRecordResponse {
        /// The file the recording was written to.
        output_path: String,
    }
}
response! {
    /// The response to [`GetReplayBufferStatus`].
    GetReplayBufferStatusResponse {
        /// Whether the replay buffer is running.
        output_active: bool,
    }
}
response! {
    /// The response to [`GetLastReplayBufferReplay`].
    GetLastReplayBufferReplayResponse {
        /// The file the last replay was saved to.
        saved_replay_path: String,
    }
}

request! {
    /// Asks for the versions and capabilities of OBS and the server.
    GetVersion {} => GetVersionResponse
}
request! {
    /// Asks for the scenes.
    GetSceneList {} => GetSceneListResponse
}
request! {
    /// Asks for the program scene.
    GetCurrentProgramScene {} => GetCurrentProgramSceneResponse
}
request! {
    /// Switches the program scene.
    SetCurrentProgramScene {
        /// The scene to switch to.
        scene_name: String,
    } => Empty
}
request! {
    /// Asks for the items of a scene.
    GetSceneItemList {
        /// The scene.
        scene_name: String,
    } => GetSceneItemListResponse
}
request! {
    /// Asks for the id of the item showing a source in a scene.
    GetSceneItemId {
        /// The scene.
        scene_name: String,
        /// The source the item shows.
        source_name: String,
    } => GetSceneItemIdResponse
}
request! {
    /// Shows or hides a scene item.
    SetSceneItemEnabled {
        /// The scene.
        scene_name: String,
        /// The item.
        scene_item_id: i64,
        /// Whether the item is visible.
        scene_item_enabled: bool,
    } => Empty
}
request! {
    /// Asks for the stream's state.
    GetStreamStatus {} => GetStreamStatusResponse
}
request! {
    /// Starts streaming.
    StartStream {} => Empty
}
request! {
    /// Stops streaming.
    StopStream {} => Empty
}
request! {
    /// Asks for the recording's state.
    GetRecordStatus {} => GetRecordStatusResponse
}
request! {
    /// Starts recording.
    StartRecord {} => Empty
}
request! {
    /// Stops recording.
    StopRecord {} => StopRecordResponse
}
request! {
    /// Asks whether the replay buffer is running.
    GetReplayBufferStatus {} => GetReplayBufferStatusResponse
}
request! {
    /// Starts the replay buffer.
    StartReplayBuffer {} => Empty
}
request! {
    /// Stops the replay buffer.
    StopReplayBuffer {} => Empty
}
request! {
    /// Saves the replay buffer to a file.
    SaveReplayBuffer {} => Empty
}
request! {
    /// Asks for the file of the last saved replay.
    GetLastReplayBufferReplay {} => GetLastReplayBufferReplayResponse
}

/// The state of an output (stream, record, replay buffer) in an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputState {
    /// `OBS_WEBSOCKET_OUTPUT_STARTING`.
    #[serde(rename = "OBS_WEBSOCKET_OUTPUT_STARTING")]
    Starting,
    /// `OBS_WEBSOCKET_OUTPUT_STARTED`.
    #[serde(rename = "OBS_WEBSOCKET_OUTPUT_STARTED")]
    Started,
    /// `OBS_WEBSOCKET_OUTPUT_STOPPING`.
    #[serde(rename = "OBS_WEBSOCKET_OUTPUT_STOPPING")]
    Stopping,
    /// `OBS_WEBSOCKET_OUTPUT_STOPPED`.
    #[serde(rename = "OBS_WEBSOCKET_OUTPUT_STOPPED")]
    Stopped,
    /// `OBS_WEBSOCKET_OUTPUT_RECONNECTING`.
    #[serde(rename = "OBS_WEBSOCKET_OUTPUT_RECONNECTING")]
    Reconnecting,
    /// `OBS_WEBSOCKET_OUTPUT_RECONNECTED`.
    #[serde(rename = "OBS_WEBSOCKET_OUTPUT_RECONNECTED")]
    Reconnected,
    /// `OBS_WEBSOCKET_OUTPUT_PAUSED`.
    #[serde(rename = "OBS_WEBSOCKET_OUTPUT_PAUSED")]
    Paused,
    /// `OBS_WEBSOCKET_OUTPUT_RESUMED`.
    #[serde(rename = "OBS_WEBSOCKET_OUTPUT_RESUMED")]
    Resumed,
    /// `OBS_WEBSOCKET_OUTPUT_UNKNOWN`, and any state a newer server adds.
    #[serde(rename = "OBS_WEBSOCKET_OUTPUT_UNKNOWN", other)]
    Unknown,
}

/// The events the bridge acts on; every other event is [`ObsEvent::Other`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "eventType", content = "eventData", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum ObsEvent {
    /// The program scene changed.
    CurrentProgramSceneChanged {
        /// The new program scene.
        scene_name: String,
    },
    /// The stream's state changed.
    StreamStateChanged {
        /// Whether the output is active.
        output_active: bool,
        /// The new state.
        output_state: OutputState,
    },
    /// The recording's state changed.
    RecordStateChanged {
        /// Whether the output is active.
        output_active: bool,
        /// The new state.
        output_state: OutputState,
        /// The file written; `null` until the recording has stopped.
        output_path: Option<String>,
    },
    /// The replay buffer's state changed.
    ReplayBufferStateChanged {
        /// Whether the output is active.
        output_active: bool,
        /// The new state.
        output_state: OutputState,
    },
    /// A replay was saved.
    ReplayBufferSaved {
        /// The file written.
        saved_replay_path: String,
    },
    /// A scene item was shown or hidden.
    SceneItemEnableStateChanged {
        /// The scene.
        scene_name: String,
        /// The item.
        scene_item_id: i64,
        /// Whether the item is now visible.
        scene_item_enabled: bool,
    },
    /// OBS is shutting down.
    ExitStarted,
    /// An event this crate does not type; its data is dropped.
    #[serde(skip)]
    Other {
        /// The `eventType`.
        event_type: String,
    },
}

/// The `eventType` of each typed variant, to tell an unknown event from a malformed known one.
const KNOWN_EVENTS: [&str; 7] = [
    "CurrentProgramSceneChanged",
    "StreamStateChanged",
    "RecordStateChanged",
    "ReplayBufferStateChanged",
    "ReplayBufferSaved",
    "SceneItemEnableStateChanged",
    "ExitStarted",
];

impl ObsEvent {
    /// Types an event. An unknown `eventType` is [`ObsEvent::Other`], not an error.
    ///
    /// # Errors
    /// [`ProtocolError::BadEvent`] when a known event's data is missing a field or has one of the wrong type.
    pub fn from_msg(msg: &EventMsg) -> Result<Self, ProtocolError> {
        if !KNOWN_EVENTS.contains(&msg.event_type.as_str()) {
            return Ok(Self::Other { event_type: msg.event_type.clone() });
        }
        let mut tagged = Map::new();
        tagged.insert("eventType".to_owned(), Value::String(msg.event_type.clone()));
        // An event with no fields may carry `{}`, which the unit variant would reject.
        if let Some(data) = msg.event_data.as_ref().filter(|d| d.as_object().is_none_or(|o| !o.is_empty())) {
            tagged.insert("eventData".to_owned(), data.clone());
        }
        serde_json::from_value(Value::Object(tagged))
            .map_err(|_| ProtocolError::BadEvent { event_type: msg.event_type.clone() })
    }

    /// The [`EventSubscription`] a client needs to receive this event.
    #[must_use]
    pub fn intent(&self) -> EventSubscription {
        match self {
            Self::CurrentProgramSceneChanged { .. } => EventSubscription::SCENES,
            Self::StreamStateChanged { .. }
            | Self::RecordStateChanged { .. }
            | Self::ReplayBufferStateChanged { .. }
            | Self::ReplayBufferSaved { .. } => EventSubscription::OUTPUTS,
            Self::SceneItemEnableStateChanged { .. } => EventSubscription::SCENE_ITEMS,
            Self::ExitStarted => EventSubscription::GENERAL,
            Self::Other { .. } => EventSubscription::NONE,
        }
    }

    /// The wire event. [`ObsEvent::Other`] keeps its name but has no data and no intent.
    ///
    /// # Errors
    /// The serialisation error of a typed event, passed on rather than sent with a blank name.
    pub fn to_msg(&self) -> Result<EventMsg, serde_json::Error> {
        let event_intent = self.intent().0;
        if let Self::Other { event_type } = self {
            return Ok(EventMsg { event_type: event_type.clone(), event_intent, event_data: None });
        }
        // Every typed variant serialises as `{"eventType": ..., "eventData": ...}`; anything else is a bug here.
        let Value::Object(mut tagged) = serde_json::to_value(self)? else {
            return Err(serde_json::Error::custom("a typed event does not serialise as an object"));
        };
        let Some(Value::String(event_type)) = tagged.remove("eventType") else {
            return Err(serde_json::Error::custom("a typed event has no eventType"));
        };
        Ok(EventMsg { event_type, event_intent, event_data: tagged.remove("eventData") })
    }
}
