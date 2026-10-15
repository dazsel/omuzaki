//! Expression triggers, control side (PF-25; spec v3 §22): which key shows which expression.
//!
//! The ops name a key and an expression. This module keeps the table of up to [`MAX_TRIGGERS`] bindings and bakes each
//! expression to a [`TriggerSlot`] here, so the perform thread receives finished slots and only runs the fades
//! ([`Triggers`](omuzaki_anim::Triggers)).

use omuzaki_anim::{MAX_TRIGGERS, TriggerMode, TriggerSlot, expression_slot};
use omuzaki_anim_api::ChannelBuf;
use omuzaki_face_api::{ExpressionId, ExpressionSet};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::host::PerformError;

/// The most characters a key may have.
pub const MAX_KEY_CHARS: usize = 32;

/// The baked slots, one per trigger slot index; `None` leaves the slot empty.
pub type Baked = [Option<TriggerSlot>; MAX_TRIGGERS];

/// A key bound to an expression.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TriggerBinding {
    /// The key, as it was last bound: 1 to 32 printable characters. Keys compare case-insensitively.
    pub key: String,
    /// The expression the key shows.
    pub expression: ExpressionId,
    /// Whether a press toggles the expression or shows it only while the key is down.
    pub mode: TriggerMode,
}

/// What a bind did: the slot it took, and whether it replaced a binding of the same key.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BindOutcome {
    /// The trigger slot index the key now uses.
    pub slot: usize,
    /// Whether the key was already bound, so this bind replaced its binding.
    pub replaced: bool,
}

/// A binding as listed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TriggerInfo {
    /// The binding.
    pub binding: TriggerBinding,
    /// Whether the expression is shown, or fading in or out.
    pub active: bool,
    /// Whether the expression is missing from the set, or overrides no channel: pressing the key then does nothing.
    pub missing: bool,
}

/// Checks a key: 1 to [`MAX_KEY_CHARS`] characters, none of them control characters.
///
/// # Errors
/// [`PerformError::InvalidKey`] otherwise.
pub fn check_key(key: &str) -> Result<(), PerformError> {
    let count = key.chars().count();
    if (1..=MAX_KEY_CHARS).contains(&count) && key.chars().all(|c| !c.is_control()) {
        Ok(())
    } else {
        Err(PerformError::InvalidKey)
    }
}

/// Whether two keys name the same key: compared character by character, ignoring case, so `F1` and `f1` are one key.
pub fn same_key(a: &str, b: &str) -> bool {
    a.chars().flat_map(char::to_lowercase).eq(b.chars().flat_map(char::to_lowercase))
}

/// A bound key and what the table knows about it.
#[derive(Clone, Debug)]
pub(super) struct Bound {
    /// The binding, as the caller gave it.
    pub(super) binding: TriggerBinding,
    /// Whether the expression is missing or shows nothing (see [`TriggerInfo::missing`]).
    pub(super) missing: bool,
}

/// The control side's table: the bindings by slot, the set they bake against and the rest face they blend onto.
#[derive(Clone, Debug)]
pub(super) struct TriggerBook {
    expressions: ExpressionSet,
    rest: ChannelBuf,
    slots: [Option<Bound>; MAX_TRIGGERS],
}

impl TriggerBook {
    /// An empty table with no expressions: every bind is missing until the set arrives.
    pub(super) fn new() -> Self {
        Self { expressions: ExpressionSet::default(), rest: ChannelBuf::NEUTRAL, slots: std::array::from_fn(|_| None) }
    }

    /// The expression library the table bakes against.
    pub(super) fn expressions(&self) -> ExpressionSet {
        self.expressions.clone()
    }

    /// Every bound key, with its slot index.
    pub(super) fn bound(&self) -> impl Iterator<Item = (usize, &Bound)> {
        self.slots.iter().enumerate().filter_map(|(slot, entry)| entry.as_ref().map(|bound| (slot, bound)))
    }

    /// The slot `key` is bound to, with its binding.
    pub(super) fn find(&self, key: &str) -> Option<(usize, &Bound)> {
        self.bound().find(|(_, bound)| same_key(&bound.binding.key, key))
    }

    /// The slot a bind of `key` takes: the slot the key already uses (`replaced`), else the first empty slot.
    ///
    /// # Errors
    /// [`PerformError::TooManyTriggers`] when every slot is taken by other keys.
    pub(super) fn place(&self, key: &str) -> Result<(usize, bool), PerformError> {
        if let Some((slot, _)) = self.find(key) {
            return Ok((slot, true));
        }
        self.slots.iter().position(Option::is_none).map(|slot| (slot, false)).ok_or(PerformError::TooManyTriggers)
    }

    /// Stores `bound` in `slot` (`None` clears it). `slot` comes from [`place`](Self::place) or [`find`](Self::find),
    /// so it is always in range.
    pub(super) fn put(&mut self, slot: usize, bound: Option<Bound>) {
        if let Some(entry) = self.slots.get_mut(slot) {
            *entry = bound;
        } else {
            debug_assert!(false, "trigger slot {slot} is out of range");
        }
    }

    /// Bakes `binding` against the current set: the slot's values (`None` when it shows nothing) and whether it is
    /// missing.
    pub(super) fn bake(&self, binding: &TriggerBinding) -> (Option<TriggerSlot>, bool) {
        bake(&self.expressions, &self.rest, binding)
    }

    /// The table for a new set and rest face: every bound key is re-baked, and the slots to send are returned with it.
    /// `self` is left as it was, so a refused send changes nothing.
    pub(super) fn with_expressions(&self, expressions: ExpressionSet, rest: &ChannelBuf) -> (Self, Baked) {
        let mut next = Self { expressions, rest: *rest, slots: self.slots.clone() };
        let mut baked: Baked = std::array::from_fn(|_| None);
        for (entry, out) in next.slots.iter_mut().zip(baked.iter_mut()) {
            if let Some(bound) = entry {
                let (shown, missing) = bake(&next.expressions, &next.rest, &bound.binding);
                bound.missing = missing;
                *out = shown;
            }
        }
        (next, baked)
    }
}

/// The slot an expression bakes to, and whether the binding is missing. An expression that is not in the set, or that
/// overrides no channel (a slot with an empty mask is refused by the table), shows nothing.
fn bake(expressions: &ExpressionSet, rest: &ChannelBuf, binding: &TriggerBinding) -> (Option<TriggerSlot>, bool) {
    match expression_slot(expressions, &binding.expression, rest, binding.mode) {
        Ok(slot) if !slot.mask.is_empty() => (Some(slot), false),
        Ok(_) | Err(_) => (None, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_one_to_thirty_two_non_control_characters() {
        assert_eq!(check_key(""), Err(PerformError::InvalidKey));
        assert_eq!(check_key(&"k".repeat(MAX_KEY_CHARS)), Ok(()));
        assert_eq!(check_key(&"k".repeat(MAX_KEY_CHARS + 1)), Err(PerformError::InvalidKey));
        assert_eq!(check_key("F\u{7}"), Err(PerformError::InvalidKey));
    }

    #[test]
    fn keys_compare_ignoring_case() {
        assert!(same_key("F1", "f1"));
        assert!(!same_key("F1", "F10"));
    }
}
