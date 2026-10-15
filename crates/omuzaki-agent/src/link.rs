//! [`McpClientLink`]: the host's one MCP client, over an in-process duplex (D1).
//!
//! The host thread is a plain thread, so every call here starts the request on the app's tokio runtime and hands back a
//! [`Pending`] the thread polls. Nothing here blocks on its own, which is what lets Stop abandon a call in flight.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use omuzaki_agent_api::error::HostError;
use omuzaki_llm_api::message::ImageMime;
use omuzaki_llm_api::provider::RuntimeHandle;
use omuzaki_llm_api::stream::Cancel;
use rmcp::model::{
    CallToolRequest, CallToolRequestParams, CancelledNotification, CancelledNotificationParam, ClientCapabilities,
    ClientConfig, ClientRequest, ContentBlock, GetPromptRequestParams, Implementation, JsonObject, ProtocolVersion,
    ReadResourceRequestParams, ResourceContents, ServerResult,
};
use rmcp::service::{NotificationContext, PeerRequestOptions, RoleClient, RunningService, ServiceError};
use rmcp::{ClientHandler, ClientLifecycleMode, ClientServiceExt};
use serde_json::Value;

/// The `_meta` key `omuzaki-mcp` puts on each tool to name what a call will do (D2.5).
const KIND_KEY: &str = "omuzaki/kind";
/// How long a call abandoned by Stop is still awaited, so a late result can be recorded.
const LATE_WAIT: Duration = Duration::from_secs(60);
/// How often an in-flight request looks at its cancel flag.
const CANCEL_POLL: Duration = Duration::from_millis(2);

/// What a tool does when called, from the server's `_meta` (D2.5).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum ToolKind {
    /// Changes the document: joins the answer's transaction.
    Edit,
    /// Reads only.
    Query,
    /// A job that finishes as its own step.
    Job,
    /// A driver job, which joins the transaction and never commits it.
    DriverJob,
    /// A server built-in (undo, redo, log, transactions).
    Builtin,
    /// The server named no kind (an older server).
    Unknown,
}

/// One tool of the server's `tools/list`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ToolInfo {
    pub name: String,
    pub title: Option<String>,
    pub description: String,
    pub input_schema: Value,
    pub kind: ToolKind,
    pub read_only: bool,
}

/// One block of a tool result as the host sees it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ReplyBlock {
    Text(String),
    Image {
        mime: ImageMime,
        bytes: Vec<u8>,
    },
    /// A block the model cannot take (audio, an unreadable blob), as a note.
    Note(String),
}

/// A finished `tools/call`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ToolReply {
    pub blocks: Vec<ReplyBlock>,
    pub is_error: bool,
    pub structured: Option<Value>,
}

/// Why a request to the server did not produce a result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LinkError {
    /// The server refused at protocol level; the text is its own, hint included.
    Protocol(String),
    /// The caller cancelled the request.
    Cancelled,
    /// The link is down.
    Closed,
}

impl From<LinkError> for HostError {
    fn from(error: LinkError) -> Self {
        match error {
            LinkError::Protocol(text) => HostError::Mcp(text),
            LinkError::Cancelled => HostError::Mcp("cancelled".into()),
            LinkError::Closed => HostError::SessionEnded,
        }
    }
}

fn link_error(error: &ServiceError) -> LinkError {
    match error {
        ServiceError::McpError(data) => LinkError::Protocol(data.message.to_string()),
        ServiceError::TransportClosed => LinkError::Closed,
        other => LinkError::Protocol(other.to_string()),
    }
}

/// A request in flight on the runtime.
pub(crate) struct Pending<T> {
    rx: Receiver<T>,
}

impl<T> Pending<T> {
    /// The result if it has arrived, waiting at most `wait` (real time).
    pub(crate) fn poll(&self, wait: Duration) -> Option<Result<T, LinkError>> {
        match self.rx.recv_timeout(wait) {
            Ok(value) => Some(Ok(value)),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(Err(LinkError::Closed)),
        }
    }
}

/// Called with a result that arrived after its call was cancelled (Stop, D4).
pub(crate) type LateFn = Box<dyn FnOnce(Result<ToolReply, LinkError>) + Send>;

/// What the server told the client changed.
#[derive(Default)]
struct Changes {
    prompts: AtomicBool,
}

struct Handler {
    changes: Arc<Changes>,
}

impl ClientHandler for Handler {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(ClientCapabilities::default(), Implementation::new("omuzaki-assistant", "1.0"))
            .with_protocol_version(ProtocolVersion::LATEST_WITH_INITIALIZE)
    }

    async fn on_prompt_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.changes.prompts.store(true, Ordering::Release);
    }
}

struct Inner {
    rt: tokio::runtime::Handle,
    service: Mutex<Option<RunningService<RoleClient, Handler>>>,
    peer: rmcp::service::Peer<RoleClient>,
    changes: Arc<Changes>,
    instructions: Option<String>,
}

/// The client half of the in-process MCP link. Clones share one connection.
#[derive(Clone)]
pub struct McpClientLink {
    inner: Arc<Inner>,
}

impl McpClientLink {
    /// Connects an rmcp client over `io`, the client end of the in-process duplex the app built.
    ///
    /// # Errors
    /// [`HostError::Mcp`] if `rt` is not a tokio runtime handle or the handshake fails.
    pub fn connect(io: tokio::io::DuplexStream, rt: &RuntimeHandle) -> Result<Self, HostError> {
        let rt = rt
            .downcast_ref::<tokio::runtime::Handle>()
            .ok_or_else(|| HostError::Mcp("the runtime handle is not a tokio handle".into()))?
            .clone();
        let changes = Arc::new(Changes::default());
        let handler = Handler { changes: Arc::clone(&changes) };
        let (tx, rx) = channel();
        rt.spawn(async move {
            let service = handler.serve_with_lifecycle(io, ClientLifecycleMode::Initialize).await;
            // The receiver may be gone if `connect` was abandoned; nothing to tell then.
            let _ = tx.send(service);
        });
        let service = rx
            .recv()
            .map_err(|_| HostError::Mcp("the runtime stopped during the handshake".into()))?
            .map_err(|e| HostError::Mcp(format!("the MCP handshake failed: {e}")))?;
        let instructions = service.peer_info().and_then(|info| info.instructions.clone());
        let peer = service.peer().clone();
        Ok(Self { inner: Arc::new(Inner { rt, service: Mutex::new(Some(service)), peer, changes, instructions }) })
    }

    /// The server's `instructions` (System 2 of the request).
    pub(crate) fn instructions(&self) -> Option<&str> {
        self.inner.instructions.as_deref()
    }

    /// True once if the server said its prompt list changed since the last call.
    pub(crate) fn take_prompts_changed(&self) -> bool {
        self.inner.changes.prompts.swap(false, Ordering::AcqRel)
    }

    fn spawn<T: Send + 'static>(&self, work: impl Future<Output = T> + Send + 'static) -> Pending<T> {
        let (tx, rx) = channel();
        self.inner.rt.spawn(async move {
            // A dropped `Pending` means nobody wants the result.
            let _ = tx.send(work.await);
        });
        Pending { rx }
    }

    /// `tools/list`.
    pub(crate) fn list_tools(&self) -> Pending<Result<Vec<ToolInfo>, LinkError>> {
        let peer = self.inner.peer.clone();
        self.spawn(async move {
            let tools = peer.list_all_tools().await.map_err(|e| link_error(&e))?;
            Ok(tools.into_iter().map(tool_info).collect())
        })
    }

    /// `tools/call`. `cancel` abandons the request: the client sends `notifications/cancelled`, and a result that still
    /// arrives within a minute goes to `late`.
    pub(crate) fn call_tool(
        &self,
        name: &str,
        arguments: Option<JsonObject>,
        cancel: Cancel,
        late: Option<LateFn>,
    ) -> Pending<Result<ToolReply, LinkError>> {
        let peer = self.inner.peer.clone();
        let mut params = CallToolRequestParams::new(name.to_owned());
        params.arguments = arguments;
        self.spawn(async move {
            let request = ClientRequest::CallToolRequest(CallToolRequest::new(params));
            let mut handle = peer
                .send_cancellable_request(request, PeerRequestOptions::no_options())
                .await
                .map_err(|e| link_error(&e))?;
            let mut poll = tokio::time::interval(CANCEL_POLL);
            let answer = loop {
                tokio::select! {
                    answer = &mut handle.rx => break answer,
                    _ = poll.tick() => {
                        if cancel.is_cancelled() {
                            let note = CancelledNotificationParam::new(
                                Some(handle.id.clone()),
                                Some("stopped by the person".into()),
                            );
                            // The server may already have finished; a failed notification changes nothing for us.
                            let _ = peer.send_notification(CancelledNotification::new(note).into()).await;
                            if let Some(late) = late {
                                let waited = tokio::time::timeout(LATE_WAIT, &mut handle.rx).await;
                                if let Ok(answer) = waited {
                                    late(reply_of(answer));
                                }
                            }
                            return Err(LinkError::Cancelled);
                        }
                    }
                }
            };
            reply_of(answer)
        })
    }

    /// `resources/read`, as blocks.
    pub(crate) fn read_resource(&self, uri: &str) -> Pending<Result<Vec<ReplyBlock>, LinkError>> {
        let peer = self.inner.peer.clone();
        let params = ReadResourceRequestParams::new(uri.to_owned());
        self.spawn(async move {
            let result = peer.read_resource(params).await.map_err(|e| link_error(&e))?;
            Ok(result.contents.into_iter().map(resource_block).collect())
        })
    }

    /// `prompts/list`, mapped to the panel's type and sorted by name.
    pub(crate) fn list_prompts(&self) -> Pending<Result<Vec<omuzaki_agent_api::prompts::PromptInfo>, LinkError>> {
        let peer = self.inner.peer.clone();
        self.spawn(async move {
            let prompts = peer.list_all_prompts().await.map_err(|e| link_error(&e))?;
            let mut out: Vec<_> = prompts.into_iter().map(prompt_info).collect();
            out.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(out)
        })
    }

    /// `prompts/get`: the text of the first message.
    pub(crate) fn get_prompt(&self, name: &str, arguments: JsonObject) -> Pending<Result<String, LinkError>> {
        let peer = self.inner.peer.clone();
        let mut params = GetPromptRequestParams::new(name.to_owned());
        params.arguments = Some(arguments);
        self.spawn(async move {
            let result = peer.get_prompt(params).await.map_err(|e| link_error(&e))?;
            let first = result.messages.into_iter().next();
            Ok(first.map(|m| content_text(&m.content)).unwrap_or_default())
        })
    }

    /// Closes the connection. Idempotent.
    pub(crate) fn close(&self) {
        let service = self.inner.service.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(mut service) = service {
            let (tx, rx) = channel();
            self.inner.rt.spawn(async move {
                let _ = service.close().await;
                let _ = tx.send(());
            });
            // Bounded: a runtime that is shutting down never answers.
            let _ = rx.recv_timeout(Duration::from_millis(500));
        }
    }
}

fn reply_of(
    answer: Result<Result<ServerResult, ServiceError>, tokio::sync::oneshot::error::RecvError>,
) -> Result<ToolReply, LinkError> {
    match answer {
        Ok(Ok(ServerResult::CallToolResult(result))) => Ok(ToolReply {
            is_error: result.is_error == Some(true),
            structured: result.structured_content,
            blocks: result.content.iter().map(block_of).collect(),
        }),
        Ok(Ok(_)) => Err(LinkError::Protocol("the server answered tools/call with something else".into())),
        Ok(Err(e)) => Err(link_error(&e)),
        Err(_) => Err(LinkError::Closed),
    }
}

fn tool_info(tool: rmcp::model::Tool) -> ToolInfo {
    let kind = tool.meta.as_ref().and_then(|m| m.get(KIND_KEY)).and_then(Value::as_str).map_or(
        ToolKind::Unknown,
        |k| match k {
            "edit" => ToolKind::Edit,
            "query" => ToolKind::Query,
            "job" => ToolKind::Job,
            "driver_job" => ToolKind::DriverJob,
            "builtin" => ToolKind::Builtin,
            _ => ToolKind::Unknown,
        },
    );
    ToolInfo {
        name: tool.name.to_string(),
        title: tool.title.clone(),
        description: tool.description.as_deref().unwrap_or_default().to_owned(),
        input_schema: Value::Object((*tool.input_schema).clone()),
        kind,
        read_only: tool.annotations.as_ref().and_then(|a| a.read_only_hint) == Some(true),
    }
}

fn prompt_info(prompt: rmcp::model::Prompt) -> omuzaki_agent_api::prompts::PromptInfo {
    use omuzaki_agent_api::prompts::{PromptArgInfo, PromptInfo};
    let arguments = prompt
        .arguments
        .unwrap_or_default()
        .into_iter()
        .map(|a| PromptArgInfo::new(a.name, a.description.unwrap_or_default(), a.required == Some(true)))
        .collect();
    PromptInfo::new(prompt.name.clone())
        .with_title(prompt.title.unwrap_or(prompt.name))
        .with_description(prompt.description.unwrap_or_default())
        .with_arguments(arguments)
}

fn image_mime(mime: &str) -> Option<ImageMime> {
    match mime {
        "image/png" => Some(ImageMime::Png),
        "image/jpeg" | "image/jpg" => Some(ImageMime::Jpeg),
        "image/webp" => Some(ImageMime::Webp),
        "image/gif" => Some(ImageMime::Gif),
        _ => None,
    }
}

fn decode_image(data: &str, mime: &str) -> ReplyBlock {
    use base64::Engine;
    match (image_mime(mime), base64::engine::general_purpose::STANDARD.decode(data)) {
        (Some(mime), Ok(bytes)) => ReplyBlock::Image { mime, bytes },
        _ => ReplyBlock::Note(format!("[image omitted: unreadable {mime}]")),
    }
}

fn resource_block(contents: ResourceContents) -> ReplyBlock {
    match contents {
        ResourceContents::TextResourceContents { text, .. } => ReplyBlock::Text(text),
        ResourceContents::BlobResourceContents { blob, mime_type, .. } => {
            decode_image(&blob, mime_type.as_deref().unwrap_or_default())
        }
        _ => ReplyBlock::Note("[resource omitted]".into()),
    }
}

fn block_of(block: &ContentBlock) -> ReplyBlock {
    match block {
        ContentBlock::Text(t) => ReplyBlock::Text(t.text.clone()),
        ContentBlock::Image(i) => decode_image(&i.data, &i.mime_type),
        ContentBlock::Audio(_) => ReplyBlock::Note("[audio omitted]".into()),
        ContentBlock::Resource(r) => resource_block(r.resource.clone()),
        ContentBlock::ResourceLink(link) => ReplyBlock::Text(format!("[resource: {}]", link.uri)),
        _ => ReplyBlock::Note("[content omitted]".into()),
    }
}

fn content_text(block: &ContentBlock) -> String {
    match block_of(block) {
        ReplyBlock::Text(t) | ReplyBlock::Note(t) => t,
        ReplyBlock::Image { .. } => String::new(),
    }
}
