//! The provider-neutral request and message model (llm-api D2). History is append-only.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Everything one turn sends. Build it with [`TurnRequest::new`] and the `with_*` setters; it grows without breaking callers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct TurnRequest {
    /// The system prompt, in blocks so a cache breakpoint can sit between them.
    pub system: Vec<SystemBlock>,
    /// The conversation so far. Callers only ever append.
    pub messages: Vec<Message>,
    /// The tools the model may call.
    pub tools: Vec<ToolDef>,
    /// Which tool the model must or may not call.
    pub tool_choice: ToolChoice,
    /// The most output tokens the turn may produce.
    pub max_output: u32,
    /// How hard the model should think.
    pub effort: Effort,
    /// What to cache.
    pub cache: CachePlan,
}

impl TurnRequest {
    /// A request for `messages` that may produce up to `max_output` tokens, with no system prompt, no tools,
    /// `ToolChoice::Auto`, `Effort::Default` and no caching.
    pub fn new(messages: Vec<Message>, max_output: u32) -> Self {
        Self {
            system: Vec::new(),
            messages,
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            max_output,
            effort: Effort::Default,
            cache: CachePlan { conversation: false },
        }
    }

    /// Sets the system prompt.
    #[must_use]
    pub fn with_system(mut self, system: Vec<SystemBlock>) -> Self {
        self.system = system;
        self
    }

    /// Sets the tools the model may call.
    #[must_use]
    pub fn with_tools(mut self, tools: Vec<ToolDef>) -> Self {
        self.tools = tools;
        self
    }

    /// Sets which tool the model must or may not call.
    #[must_use]
    pub fn with_tool_choice(mut self, tool_choice: ToolChoice) -> Self {
        self.tool_choice = tool_choice;
        self
    }

    /// Sets how hard the model should think.
    #[must_use]
    pub fn with_effort(mut self, effort: Effort) -> Self {
        self.effort = effort;
        self
    }

    /// Sets what to cache.
    #[must_use]
    pub fn with_cache(mut self, cache: CachePlan) -> Self {
        self.cache = cache;
        self
    }
}

/// One block of the system prompt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SystemBlock {
    /// The text.
    pub text: String,
    /// Put a cache breakpoint after this block (it covers the tools too).
    pub cache_after: bool,
}

/// Who wrote a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum Role {
    /// The person, or the caller acting for them, including tool results.
    User,
    /// The model.
    Assistant,
}

/// One message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Message {
    /// Who wrote it.
    pub role: Role,
    /// Its blocks, in order.
    pub content: Vec<ContentBlock>,
}

/// An image format a model accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum ImageMime {
    /// `image/png`.
    Png,
    /// `image/jpeg`.
    Jpeg,
    /// `image/webp`.
    Webp,
    /// `image/gif`.
    Gif,
}

/// Where an image came from. A camera image never reaches a non-loopback endpoint (llm-api D7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum ImageOrigin {
    /// A render of the project.
    Render,
    /// A file.
    File,
    /// A screen capture.
    Screen,
    /// A camera frame.
    Camera,
}

/// One block of a message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum ContentBlock {
    /// Plain text.
    Text(String),
    /// An image, at most 768 px on the long side and 5 MB.
    Image {
        /// The encoding of `bytes`.
        mime: ImageMime,
        /// The encoded image.
        bytes: Vec<u8>,
        /// Where it came from.
        origin: ImageOrigin,
    },
    /// The model asks for a tool call.
    ToolUse {
        /// The call id the result answers.
        id: String,
        /// The tool name.
        name: String,
        /// The arguments.
        input: Value,
    },
    /// The answer to a [`ContentBlock::ToolUse`].
    ToolResult {
        /// The id of the call it answers.
        id: String,
        /// The result, `Text` and `Image` blocks only.
        blocks: Vec<ContentBlock>,
        /// The tool failed.
        is_error: bool,
    },
    /// A provider's own block (thinking, redacted thinking), returned untouched and only to that provider.
    Opaque {
        /// The provider that made it.
        provider: String,
        /// The block, byte for byte.
        block: Value,
    },
}

/// A tool the model may call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolDef {
    /// The name, `^[a-zA-Z0-9_-]{1,64}$`.
    pub name: String,
    /// What it does.
    pub description: String,
    /// The JSON Schema of its arguments.
    pub input_schema: Value,
}

/// Which tool the model may call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum ToolChoice {
    /// The model decides.
    Auto,
    /// The model must call some tool.
    Any,
    /// The model must call this tool (structured output).
    Tool(String),
    /// The model may not call a tool.
    None,
}

/// How hard the model should think.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum Effort {
    /// Send nothing; the provider's default.
    Default,
    /// Low.
    Low,
    /// Medium.
    Medium,
    /// High.
    High,
    /// The most the model offers.
    Max,
}

/// What to cache besides the system blocks that ask for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CachePlan {
    /// Also cache the growing conversation (a breakpoint on the last message).
    pub conversation: bool,
}
