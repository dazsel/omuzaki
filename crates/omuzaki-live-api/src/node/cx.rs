//! What a node may do: [`NodeCx`] in [`Node::handle`](super::Node::handle), [`EvalCx`] in
//! [`Node::eval`](super::Node::eval). Both are thin wrappers over a host the engine (or the harness) implements, so
//! helpers stay out of the host traits and the traits stay object-safe.

use std::sync::Arc;

use omuzaki_core::rng::Rng;
use omuzaki_core::time::Seconds;
use omuzaki_l10n_api::Text;
use serde_json::Value;

use super::{CallId, NodeError, WakeId};
use crate::event::{LiveEvent, UtcMillis};
use crate::ids::{EventKind, GroupId, VarName};
use crate::value::{PortType, Val};

/// The reads every node may make, in [`Node::eval`](super::Node::eval) as in `handle`. Implemented by the engine and
/// by the harness.
pub trait EvalHost {
    /// The step's time on the runner clock.
    fn now(&self) -> Seconds;
    /// The step's wall time.
    fn utc(&self) -> UtcMillis;
    /// The value input `port`, resolved in the current token and coerced to the port's type (design D3).
    ///
    /// # Errors
    /// [`NodeError::unknown_port`] for a port that isn't a value input; a type error if a JSON value doesn't fit.
    fn input(&mut self, port: &str) -> Result<Val, NodeError>;
    /// A variable's value, if it is declared.
    fn var(&self, name: &VarName) -> Option<Val>;
    /// A group's runtime state, if it is declared.
    fn group_enabled(&self, group: &GroupId) -> Option<bool>;
}

/// Everything a node may do while handling an input. Implemented by the engine and by the harness; nodes use it
/// through [`NodeCx`].
pub trait NodeHost: EvalHost {
    /// This node's own random stream (design D6).
    fn rng(&mut self) -> &mut Rng;
    /// Sets the value output `port` in the current token.
    ///
    /// # Errors
    /// [`NodeError::unknown_port`] for a port that isn't a value output the node sets; a type error for a value that
    /// doesn't fit it or breaks a value rule.
    fn set(&mut self, port: &str, value: Val) -> Result<(), NodeError>;
    /// Fires the event output `port`: its wires' destinations run next, depth-first (design D5).
    ///
    /// # Errors
    /// [`NodeError::unknown_port`] for a port that isn't an event output.
    fn fire(&mut self, port: &str) -> Result<(), NodeError>;
    /// Sets the `error` output (when the node has one) to `why` in English, records `why` for people, and fires
    /// `port`.
    ///
    /// # Errors
    /// As [`fire`](Self::fire).
    fn fail(&mut self, port: &str, why: Text) -> Result<(), NodeError>;
    /// Asks for an [`Input::Wake`](super::Input::Wake) at `at` (now, if `at` is past), in the current token.
    ///
    /// # Errors
    /// [`NodeError::limit`] past [`MAX_WAKES_PER_NODE`](crate::limits::MAX_WAKES_PER_NODE) pending wakes.
    fn wake_at(&mut self, at: Seconds) -> Result<WakeId, NodeError>;
    /// Cancels a pending wake; nothing happens if it already ran or was cancelled.
    fn cancel_wake(&mut self, id: WakeId);
    /// Asks for an op run in the show's session (design D7). The reply arrives later as
    /// [`Input::Reply`](super::Input::Reply), in the current token.
    ///
    /// # Errors
    /// [`NodeError::limit`] past [`MAX_CALLS_IN_FLIGHT`](crate::limits::MAX_CALLS_IN_FLIGHT); the node then fires
    /// `failed`.
    fn run_op(&mut self, op: &str, args: Value) -> Result<CallId, NodeError>;
    /// Writes a variable, type-checked against its declaration (design D10).
    ///
    /// # Errors
    /// A type error naming the variable, or one that isn't declared.
    fn set_var(&mut self, name: &VarName, value: Val) -> Result<(), NodeError>;
    /// Switches a group's runtime state; it takes effect at the end of the step (design D12).
    ///
    /// # Errors
    /// An error for a group the show doesn't declare.
    fn switch_group(&mut self, group: &GroupId, on: bool) -> Result<(), NodeError>;
    /// Emits an event from this node, one `event.emit` hop on from the current token's root (design D8).
    ///
    /// # Errors
    /// [`NodeError::limit`] past [`MAX_EMIT_HOPS`](crate::limits::MAX_EMIT_HOPS); a type error for a payload that
    /// isn't an object or is too large.
    fn emit(&mut self, kind: EventKind, payload: Value) -> Result<(), NodeError>;
    /// The current token's root event.
    fn event(&self) -> &LiveEvent;
}

/// A value of the asked type, or a type mismatch naming the port.
fn typed<T>(port: &str, value: Val, ty: PortType, take: impl FnOnce(Val) -> Option<T>) -> Result<T, NodeError> {
    let found = value.ty();
    take(value).ok_or_else(|| NodeError::type_mismatch(port, ty, found))
}

/// The typed input reads, shared by [`NodeCx`] and [`EvalCx`].
macro_rules! typed_inputs {
    () => {
        /// The value input `port` (design D3).
        ///
        /// # Errors
        /// As [`EvalHost::input`].
        pub fn input(&mut self, port: &str) -> Result<Val, NodeError> {
            self.host.input(port)
        }

        /// The `bool` input `port`.
        ///
        /// # Errors
        /// As [`input`](Self::input), and a type mismatch naming the port if it isn't a `bool`.
        pub fn input_bool(&mut self, port: &str) -> Result<bool, NodeError> {
            let value = self.host.input(port)?;
            typed(port, value, PortType::Bool, |v| if let Val::Bool(b) = v { Some(b) } else { None })
        }

        /// The `number` input `port`.
        ///
        /// # Errors
        /// As [`input`](Self::input), and a type mismatch naming the port if it isn't a `number`.
        pub fn input_number(&mut self, port: &str) -> Result<f64, NodeError> {
            let value = self.host.input(port)?;
            typed(port, value, PortType::Number, |v| if let Val::Number(x) = v { Some(x) } else { None })
        }

        /// The `text` input `port`.
        ///
        /// # Errors
        /// As [`input`](Self::input), and a type mismatch naming the port if it isn't `text`.
        pub fn input_text(&mut self, port: &str) -> Result<Arc<str>, NodeError> {
            let value = self.host.input(port)?;
            typed(port, value, PortType::Text, |v| if let Val::Text(s) = v { Some(s) } else { None })
        }

        /// The `list` input `port`.
        ///
        /// # Errors
        /// As [`input`](Self::input), and a type mismatch naming the port if it isn't a `list`.
        pub fn input_list(&mut self, port: &str) -> Result<Arc<[Val]>, NodeError> {
            let value = self.host.input(port)?;
            typed(port, value, PortType::List, |v| if let Val::List(items) = v { Some(items) } else { None })
        }

        /// The step's time on the runner clock.
        pub fn now(&self) -> Seconds {
            self.host.now()
        }

        /// The step's wall time.
        pub fn utc(&self) -> UtcMillis {
            self.host.utc()
        }

        /// A variable's value, if it is declared.
        pub fn var(&self, name: &VarName) -> Option<Val> {
            self.host.var(name)
        }

        /// A group's runtime state, if it is declared.
        pub fn group_enabled(&self, group: &GroupId) -> Option<bool> {
            self.host.group_enabled(group)
        }
    };
}

/// What a node may do in [`Node::handle`](super::Node::handle).
pub struct NodeCx<'a> {
    host: &'a mut dyn NodeHost,
}

impl<'a> NodeCx<'a> {
    /// Wraps a host. For the engine and the harness.
    pub fn new(host: &'a mut dyn NodeHost) -> Self {
        Self { host }
    }

    typed_inputs!();

    /// This node's own random stream (design D6).
    pub fn rng(&mut self) -> &mut Rng {
        self.host.rng()
    }

    /// Sets the value output `port` in the current token.
    ///
    /// # Errors
    /// As [`NodeHost::set`].
    pub fn set(&mut self, port: &str, value: impl Into<Val>) -> Result<(), NodeError> {
        self.host.set(port, value.into())
    }

    /// Fires the event output `port`.
    ///
    /// # Errors
    /// As [`NodeHost::fire`].
    pub fn fire(&mut self, port: &str) -> Result<(), NodeError> {
        self.host.fire(port)
    }

    /// Sets `error` to the English text, records `why` for people, and fires `port` (usually `failed`).
    ///
    /// # Errors
    /// As [`NodeHost::fail`].
    pub fn fail(&mut self, port: &str, why: Text) -> Result<(), NodeError> {
        self.host.fail(port, why)
    }

    /// Asks for a wake `after` from now.
    ///
    /// # Errors
    /// [`NodeError::not_finite`] for a time that isn't finite; otherwise as [`NodeHost::wake_at`]. A negative `after`
    /// means now.
    pub fn wake_after(&mut self, after: Seconds) -> Result<WakeId, NodeError> {
        if !after.0.is_finite() {
            return Err(NodeError::not_finite("wake_after"));
        }
        let at = Seconds(self.host.now().0 + after.0.max(0.0));
        self.host.wake_at(at)
    }

    /// Asks for a wake at `at` on the runner clock (now, if `at` is past).
    ///
    /// # Errors
    /// [`NodeError::not_finite`] for a time that isn't finite; otherwise as [`NodeHost::wake_at`].
    pub fn wake_at(&mut self, at: Seconds) -> Result<WakeId, NodeError> {
        if !at.0.is_finite() {
            return Err(NodeError::not_finite("wake_at"));
        }
        self.host.wake_at(at)
    }

    /// Cancels a pending wake.
    pub fn cancel_wake(&mut self, id: WakeId) {
        self.host.cancel_wake(id);
    }

    /// Asks for an op run in the show's session (design D7); the reply arrives later as
    /// [`Input::Reply`](super::Input::Reply), in this token.
    ///
    /// # Errors
    /// As [`NodeHost::run_op`].
    pub fn run_op(&mut self, op: &str, args: Value) -> Result<CallId, NodeError> {
        self.host.run_op(op, args)
    }

    /// Writes a variable, type-checked (design D10).
    ///
    /// # Errors
    /// As [`NodeHost::set_var`].
    pub fn set_var(&mut self, name: &VarName, value: Val) -> Result<(), NodeError> {
        self.host.set_var(name, value)
    }

    /// Switches a group's runtime state, at the end of this step (design D12).
    ///
    /// # Errors
    /// As [`NodeHost::switch_group`].
    pub fn switch_group(&mut self, group: &GroupId, on: bool) -> Result<(), NodeError> {
        self.host.switch_group(group, on)
    }

    /// Emits an event (design D8 counts the hops).
    ///
    /// # Errors
    /// As [`NodeHost::emit`].
    pub fn emit(&mut self, kind: EventKind, payload: Value) -> Result<(), NodeError> {
        self.host.emit(kind, payload)
    }

    /// The current token's root event.
    pub fn event(&self) -> &LiveEvent {
        self.host.event()
    }
}

/// What a node may read in [`Node::eval`](super::Node::eval): inputs, time, variables and groups; nothing it can
/// change.
pub struct EvalCx<'a> {
    host: &'a mut dyn EvalHost,
}

impl<'a> EvalCx<'a> {
    /// Wraps a host. For the engine and the harness.
    pub fn new(host: &'a mut dyn EvalHost) -> Self {
        Self { host }
    }

    typed_inputs!();
}
