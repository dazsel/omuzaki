//! Bindings (design §5): what a configured chord does, as the feeds hand it out. The chord itself never leaves
//! `omuzaki-input`; a binding is known outside it by its [`BindingId`], its name, its device class and its
//! [`BindingAction`]. The one stated exception is [`ChordLabel`], the chord's display text for the binding editor.

use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::event::DeviceClass;

/// The most bindings in the table at once.
pub const MAX_BINDINGS: usize = 64;

/// A binding, stable for its life (from the binding store).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct BindingId(pub u32);

/// A press, a release or an auto-repeat.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Edge {
    /// The binding's chord went down.
    Press,
    /// It went up: the end of what its press started.
    Release,
    /// The OS repeated it while held.
    Repeat,
}

/// A locomotion intent (design §7.1).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    /// Walk in a direction, `x` right and `y` forward, each in `-127..=127`; held move intents add up.
    #[non_exhaustive]
    Move {
        /// Rightward.
        x: i8,
        /// Forward.
        y: i8,
    },
    /// Run while held.
    Run,
    /// Jump.
    Jump,
    /// Sit, or stand up again.
    Sit,
    /// Play emote `n` of the character's set.
    Emote(u8),
}

impl Intent {
    /// [`Intent::Move`].
    pub const fn moving(x: i8, y: i8) -> Self {
        Self::Move { x, y }
    }
}

/// What a binding does. Owned by the binding store; set by `input.binding.set_action`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BindingAction {
    /// Run an operation (#469), by the binding dispatcher, as the person.
    #[non_exhaustive]
    Op {
        /// The operation's name.
        op: String,
        /// Its arguments.
        args: serde_json::Value,
    },
    /// Run an operation that takes a `down` argument (such as `trigger.press`) on the press with `down: true`, and
    /// again on the release with `down: false` (#469), by the binding dispatcher, as the person: an expression shown
    /// while the key, pad or button is held.
    #[non_exhaustive]
    Hold {
        /// The operation's name.
        op: String,
        /// Its arguments, without `down`.
        args: serde_json::Value,
    },
    /// Press a PF-25 trigger slot (#478).
    Trigger(u8),
    /// A locomotion intent, routed to the frame thread's [`IntentFeed`](crate::feed::IntentFeed), never to the
    /// dispatcher.
    Intent(Intent),
}

impl BindingAction {
    /// [`BindingAction::Op`].
    pub fn op(op: impl Into<String>, args: serde_json::Value) -> Self {
        Self::Op { op: op.into(), args }
    }

    /// [`BindingAction::Hold`].
    pub fn hold(op: impl Into<String>, args: serde_json::Value) -> Self {
        Self::Hold { op: op.into(), args }
    }

    /// Its intent, when it is one.
    pub const fn as_intent(&self) -> Option<Intent> {
        match self {
            Self::Intent(intent) => Some(*intent),
            Self::Op { .. } | Self::Hold { .. } | Self::Trigger(_) => None,
        }
    }

    /// The operation it runs and that operation's arguments (without `down`, for a hold), when it runs one.
    pub fn op_call(&self) -> Option<(&str, &serde_json::Value)> {
        match self {
            Self::Op { op, args } | Self::Hold { op, args } => Some((op, args)),
            Self::Trigger(_) | Self::Intent(_) => None,
        }
    }
}

/// A resolved binding event, ready to run: the stable id and a borrow of the action its press started (design §5).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct BindingHit<'a> {
    /// The binding.
    pub id: BindingId,
    /// What to run: for a release, the action its press started, whatever the table says now.
    pub action: &'a BindingAction,
    /// Press, release or repeat.
    pub edge: Edge,
    /// When.
    pub at: Seconds,
}

/// A resolved locomotion intent event, for the frame thread.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct IntentEvent {
    /// The binding that sent it.
    pub id: BindingId,
    /// The intent its press started.
    pub intent: Intent,
    /// Press, release or repeat.
    pub edge: Edge,
    /// When.
    pub at: Seconds,
}

/// What `input.binding.list` shows of a binding: never its chord.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BindingInfo {
    /// The binding.
    pub id: BindingId,
    /// Its name, as the person or a preset gave it.
    pub name: String,
    /// The device class its chord is on.
    pub device: DeviceClass,
    /// What it does.
    pub action: BindingAction,
    /// Whether another binding shares its chord.
    pub conflict: bool,
}

/// A configured chord rendered as display text ("Ctrl+Shift+F9"), for the binding editor to show: the one stated
/// exception to "no key identity in a public type" (design §3 rule 1).
///
/// It is confined three ways: only [`ChordStore`](crate::feed::ChordStore) returns one, and that store is handed to the
/// binding editor alone; it has no serde and no `Display`, and its `Debug` prints `<chord>`, so it cannot be
/// serialised, logged or put in an event by accident; the text is read only through [`render`](Self::render).
#[derive(Clone, PartialEq, Eq)]
pub struct ChordLabel(String);

impl ChordLabel {
    /// A label with `text`. Only the hub makes one, from a chord it holds.
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// Hands the text to `f`, for the editor to draw.
    pub fn render(&self, f: &mut dyn FnMut(&str)) {
        f(&self.0);
    }
}

impl std::fmt::Debug for ChordLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<chord>")
    }
}
