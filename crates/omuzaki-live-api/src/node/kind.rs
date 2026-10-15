//! What a node kind is ([`NodeKind`], [`NodeMeta`]), what one running instance does ([`Node`]) and what it is called
//! with ([`Input`]).

use omuzaki_ops_api::OpError;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{EvalCx, NodeCx, NodeError, ParamErrors, Ports};
use crate::event::LiveEvent;
use crate::ids::EventKind;
use crate::need::Need;
use crate::value::Val;

/// A kind's identity: its id, summary, class and palette category. Built with [`NodeMeta::new`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct NodeMeta {
    /// The kind's id: `family.name` (a [`NodeKindId`](crate::ids::NodeKindId)).
    pub id: &'static str,
    /// One line on what it does, in English: the `node-<id>` text.
    pub summary: &'static str,
    /// How the engine runs it.
    pub class: NodeClass,
    /// Where the editor's palette lists it.
    pub category: Category,
}

impl NodeMeta {
    /// The kind `id`, doing `summary`.
    pub fn new(id: &'static str, summary: &'static str, class: NodeClass, category: Category) -> Self {
        Self { id, summary, class, category }
    }
}

/// How the engine runs a kind. Closed on purpose (design D21): the engine's dispatch matches it exhaustively.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NodeClass {
    /// Listens to event kinds ([`NodeKind::listens`]) and starts tokens.
    Trigger,
    /// No event ports; every output is live; [`Node::handle`] is never called.
    Pure,
    /// Everything else.
    Active,
}

/// The editor palette's groups, fixed in the API like the library's sections.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Category {
    /// Nodes that start on events.
    Triggers,
    /// Waits, branches, sequences.
    Flow,
    /// And, or, not.
    Logic,
    /// Arithmetic, ranges, easing.
    Math,
    /// Formatting and parsing text.
    Text,
    /// Reading and writing variables.
    Variables,
    /// Timers, schedules, stopwatches.
    Time,
    /// Conditions on the world: files, processes, audio, dates.
    Conditions,
    /// Ops, files, the network, programs.
    Actions,
    /// Chat commands and the chat reader.
    Chat,
    /// Switching groups.
    Groups,
    /// Emitting and matching events.
    Events,
    /// Kinds plugins add.
    Plugin,
}

impl Category {
    /// The name in message ids and documents (`node-category-<name>`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Category::Triggers => "triggers",
            Category::Flow => "flow",
            Category::Logic => "logic",
            Category::Math => "math",
            Category::Text => "text",
            Category::Variables => "variables",
            Category::Time => "time",
            Category::Conditions => "conditions",
            Category::Actions => "actions",
            Category::Chat => "chat",
            Category::Groups => "groups",
            Category::Events => "events",
            Category::Plugin => "plugin",
        }
    }
}

/// A node type. The type holds one instance's parameters and state; [`register_node!`](crate::register_node) makes it
/// known to the engine, the editor and agents.
pub trait NodeKind: Node + Sized + 'static {
    /// The parameters. By convention `#[serde(deny_unknown_fields)]`, with a doc comment on every field (it is the
    /// `node-<id>--<key>` text). Missing top-level keys take their values from `Default`.
    type Params: Serialize + DeserializeOwned + JsonSchema + Default + Send + Sync + 'static;

    /// The kind's identity.
    fn meta() -> NodeMeta;

    /// The ports for these parameters (switch cases, `text.format` placeholders and `op.run` arguments are dynamic).
    fn ports(params: &Self::Params) -> Ports;

    /// The event kinds a trigger listens to. Empty for other classes.
    fn listens(params: &Self::Params) -> Vec<EventKind> {
        let _ = params;
        Vec::new()
    }

    /// What running it needs beyond the base grants (design D11): static, from the parameters only.
    fn needs(params: &Self::Params) -> Vec<Need> {
        let _ = params;
        Vec::new()
    }

    /// Range and consistency checks the schema can't express.
    ///
    /// # Errors
    /// Each problem, at its path.
    fn check(params: &Self::Params) -> Result<(), ParamErrors> {
        let _ = params;
        Ok(())
    }

    /// A new instance.
    fn new(params: Self::Params) -> Self;
}

/// One running instance. The engine never calls it re-entrantly and never from two threads.
pub trait Node: Send + 'static {
    /// Handles one input. A node matches the inputs it handles, returns `Ok(())` by name for each current input it
    /// ignores on purpose, and returns [`NodeError::unhandled`] from its wildcard arm, which only an input added after
    /// it was written can reach (design D21).
    ///
    /// # Errors
    /// A [`NodeError`] for a misuse or a fault: the engine drops that token's branch, counts it and shows it.
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError>;

    /// A live output's value, from the state and the inputs. Must not change anything.
    ///
    /// # Errors
    /// [`NodeError::not_live`] for a port that isn't a live output (the default), or any read error.
    fn eval(&self, port: &str, cx: &mut EvalCx<'_>) -> Result<Val, NodeError> {
        let _ = cx;
        Err(NodeError::not_live(port))
    }
}

/// What a node is called with. Struct-like variants are built with the constructors and matched with `..`.
#[derive(Debug)]
#[non_exhaustive]
pub enum Input<'a> {
    /// The show started, or this instance was (re)created while it runs.
    Start,
    /// The show is stopping; outputs fired now are ignored.
    Stop,
    /// A pulse on an event input, in the current token.
    #[non_exhaustive]
    Event {
        /// The input.
        port: &'a str,
    },
    /// An event this trigger listens to, in a new token.
    #[non_exhaustive]
    Trigger {
        /// The event.
        event: &'a LiveEvent,
    },
    /// A wake this node asked for, in the token that asked.
    #[non_exhaustive]
    Wake {
        /// The wake.
        id: WakeId,
    },
    /// An op call came back, in the token that made it.
    #[non_exhaustive]
    Reply {
        /// The call.
        call: CallId,
        /// The op's output, or why it failed.
        outcome: Result<Option<Value>, OpError>,
    },
}

impl<'a> Input<'a> {
    /// A pulse on `port`.
    pub fn event(port: &'a str) -> Self {
        Input::Event { port }
    }

    /// `event`, to a trigger.
    pub fn trigger(event: &'a LiveEvent) -> Self {
        Input::Trigger { event }
    }

    /// The wake `id` is due.
    pub fn wake(id: WakeId) -> Self {
        Input::Wake { id }
    }

    /// The call `call` came back with `outcome`.
    pub fn reply(call: CallId, outcome: Result<Option<Value>, OpError>) -> Self {
        Input::Reply { call, outcome }
    }

    /// The variant's name (`start`, `event`, `reply`), for error text.
    pub fn name(&self) -> &'static str {
        match self {
            Input::Start => "start",
            Input::Stop => "stop",
            Input::Event { .. } => "event",
            Input::Trigger { .. } => "trigger",
            Input::Wake { .. } => "wake",
            Input::Reply { .. } => "reply",
        }
    }
}

/// A wake a node asked for, unique within a run.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct WakeId(pub u64);

/// An op call a node made, unique within a run.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct CallId(pub u64);
