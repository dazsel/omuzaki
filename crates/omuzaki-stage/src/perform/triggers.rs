//! Expression triggers, control side (PF-25; spec v3 §22): which key shows which expression.
//!
//! The ops name a key and an expression. This module keeps the table of up to [`MAX_TRIGGERS`] bindings and bakes each
//! expression to a [`TriggerSlot`] here, so the perform thread receives finished slots and only runs the fades
//! ([`Triggers`](omuzaki_anim::Triggers)).

use omuzaki_anim::{MAX_TRIGGERS, TriggerError, TriggerFades, TriggerMode, TriggerSlot, Triggers, expression_slot};
use omuzaki_anim_api::Layer;
use omuzaki_anim_api::ChannelBuf;
use omuzaki_face_api::{ExpressionId, ExpressionSet};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use unicode_bidi::{BidiClass, bidi_class};

use super::host::PerformError;

/// The most characters a key may have.
pub const MAX_KEY_CHARS: usize = 32;

/// The slots a rebake changed, ready to send: the new values of every slot, and which of them differ from what the
/// perform thread holds. A slot that is not marked is not touched, so a key held or toggled across a reload stays on.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Rebaked {
    slots: [Option<TriggerSlot>; MAX_TRIGGERS],
    /// Bit `i` is set when slot `i` changed.
    changed: u32,
}

impl Rebaked {
    /// The changed slots by index, with their new values (`None` empties the slot).
    pub fn changes(&self) -> impl Iterator<Item = (usize, Option<&TriggerSlot>)> {
        self.slots.iter().enumerate().filter(|(index, _)| self.changed >> index & 1 == 1).map(|(index, slot)| (index, slot.as_ref()))
    }

    /// Marks slot `index` as changed to `slot`.
    fn set(&mut self, index: usize, slot: Option<&TriggerSlot>) {
        if let Some(entry) = self.slots.get_mut(index) {
            *entry = slot.copied();
            self.changed |= 1 << index;
        } else {
            debug_assert!(false, "trigger slot {index} is out of range");
        }
    }
}

/// The perform thread's triggers: the fades ([`Triggers`]) plus the slots they were given and which slot a press
/// wants, which `Triggers` keeps private. With them a rebake can keep a press across new values, and a reset can start
/// the fades over and keep the bindings. Nothing here allocates.
pub(super) struct TriggerLayer {
    triggers: Triggers,
    fades: TriggerFades,
    /// The slots as last set, so a reset can put them back.
    slots: [Option<TriggerSlot>; MAX_TRIGGERS],
    /// The slot a press wants, as `Triggers` holds it: set by a press, cleared by a toggle off, a hold's release or
    /// replacing the slot.
    wanted: Option<usize>,
    /// The slot on the layer, fading in, held or fading out: `Triggers` keeps it private, so it is mirrored here by its
    /// own rule (see [`step`](Self::step)). A key switch leaves the outgoing slot here while `wanted` names the next.
    shown: Option<usize>,
    /// The shown slot was replaced while it showed: what fades out is the old binding's expression, and the slot now
    /// belongs to a new binding that has not been pressed, so it is not listed active.
    shown_replaced: bool,
    /// The generation of the binding each slot holds, as the control side numbered it (0: none).
    generations: [u64; MAX_TRIGGERS],
}

impl TriggerLayer {
    /// No slots, nothing pressed.
    pub(super) fn new(fades: TriggerFades) -> Self {
        Self { triggers: Triggers::new(fades), fades, slots: [None; MAX_TRIGGERS], wanted: None, shown: None, shown_replaced: false, generations: [0; MAX_TRIGGERS] }
    }

    /// Advances the fade by `dt` and writes the trigger layer.
    ///
    /// The shown slot follows `Triggers`: when nothing shows, the wanted slot is put on the layer at the start of the
    /// step; when the layer ends the step empty, nothing shows.
    pub(super) fn step(&mut self, dt: f32, out: &mut Layer) {
        if self.shown.is_none()
            && let Some(index) = self.wanted
            && matches!(self.slots.get(index), Some(Some(_)))
        {
            self.shown = Some(index);
            self.shown_replaced = false;
        }
        self.triggers.step(dt, out);
        if out.weight <= 0.0 {
            self.shown = None;
            self.shown_replaced = false;
        }
    }

    /// Bit `i` is set for slot `i` when it is wanted (faded toward) or shown (fading in, held or fading out). During a
    /// switch from A to B, A stays set until it has faded out, and B is set at once. A slot whose binding was replaced
    /// mid-fade is not set for the replacement: its old expression fades out, but nothing has pressed the new one.
    pub(super) fn active_mask(&self) -> u32 {
        let shown = self.shown.filter(|_| !self.shown_replaced);
        self.wanted.into_iter().chain(shown).fold(0, |mask, index| mask | 1 << index)
    }

    /// The generation of the binding in each active slot, 0 for a slot that is not active: what the handle matches
    /// against the generation of the binding it lists.
    pub(super) fn active_generations(&self) -> [u64; MAX_TRIGGERS] {
        let mask = self.active_mask();
        std::array::from_fn(|index| if mask >> index & 1 == 1 { self.generations[index] } else { 0 })
    }

    /// Key down on `index`: a toggle flips it, a hold shows it. An empty slot is ignored.
    pub(super) fn press(&mut self, index: usize) {
        let Some(Some(slot)) = self.slots.get(index) else { return };
        self.wanted = match slot.mode {
            TriggerMode::Toggle if self.wanted == Some(index) => None,
            _ => Some(index),
        };
        self.triggers.press(index);
    }

    /// Key up on `index`: a hold hides it, a toggle does nothing.
    pub(super) fn release(&mut self, index: usize) {
        let held = matches!(self.slots.get(index), Some(Some(slot)) if slot.mode == TriggerMode::Hold);
        if held && self.wanted == Some(index) {
            self.wanted = None;
        }
        self.triggers.release(index);
    }

    /// Replaces slot `index` (`None` empties it). Replacing a slot ends the press that shows it, and what was shown
    /// fades out first.
    ///
    /// # Errors
    /// The slot errors of [`Triggers::set_slot`]; nothing changes then.
    pub(super) fn set(
        &mut self,
        index: usize,
        slot: Option<&TriggerSlot>,
        generation: u64,
    ) -> Result<(), TriggerError> {
        self.set_values(index, slot)?;
        if let Some(entry) = self.generations.get_mut(index) {
            *entry = generation;
        }
        // Another binding now owns the slot: what still fades out belongs to the old one.
        if self.shown == Some(index) {
            self.shown_replaced = true;
        }
        Ok(())
    }

    /// [`set`](Self::set) for the same binding with new values (a rebake): the key is the same, so what fades out is
    /// still its expression and stays listed active.
    fn set_values(&mut self, index: usize, slot: Option<&TriggerSlot>) -> Result<(), TriggerError> {
        self.triggers.set_slot(index, slot.copied())?;
        if let Some(entry) = self.slots.get_mut(index) {
            *entry = slot.copied();
        }
        if self.wanted == Some(index) {
            self.wanted = None;
        }
        Ok(())
    }

    /// Applies a rebake. A slot whose values changed fades its old values out and, if its key is still pressed, fades
    /// the new ones in, so a held or toggled expression survives a reload that changes it.
    ///
    /// A slot that comes back empty (its expression left the set) ends its press for good. While it is empty the
    /// control side does not send the key's presses or releases, so nothing says whether the key is still down when the
    /// expression returns, and showing it again could stick: the key shows it again the next time it is pressed.
    pub(super) fn rebake(&mut self, changes: &Rebaked) {
        for (index, slot) in changes.changes() {
            let pressed = self.wanted == Some(index);
            let result = self.set_values(index, slot);
            debug_assert_eq!(result, Ok(()), "the control side sent a slot the table refuses");
            if pressed && slot.is_some() {
                self.press(index);
            }
        }
    }

    /// Starts the fades over with nothing pressed, keeping the slots: whatever was shown is gone at once, so the next
    /// step writes no trigger layer.
    pub(super) fn reset(&mut self) {
        self.triggers = Triggers::new(self.fades);
        self.wanted = None;
        self.shown = None;
        self.shown_replaced = false;
        for (index, slot) in self.slots.iter().enumerate() {
            let result = self.triggers.set_slot(index, *slot);
            debug_assert_eq!(result, Ok(()), "a slot the table accepted is refused on reset");
        }
    }
}

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

/// True when `c` shows as a mark on screen, so two different keys can always be told apart in a listing: not a
/// control character, not whitespace other than a plain space, and not one of the invisible format characters
/// (soft hyphen, zero-width and bidirectional marks, joiners, the invisible operators, variation selectors, the byte
/// order mark, tag characters) that `char::is_control` lets through.
fn is_printable(c: char) -> bool {
    // The bidirectional formatting classes are the Unicode format characters (soft hyphen, the zero-width and joiner
    // characters, the invisible operators, tags and every later addition), so a newer one is refused with no edit here.
    let format = matches!(
        bidi_class(c),
        BidiClass::BN
            | BidiClass::LRE
            | BidiClass::LRO
            | BidiClass::RLE
            | BidiClass::RLO
            | BidiClass::PDF
            | BidiClass::LRI
            | BidiClass::RLI
            | BidiClass::FSI
            | BidiClass::PDI
    );
    // Invisible too, but not format characters: fillers, variation selectors, the combining grapheme joiner.
    let invisible = matches!(
        c,
        '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{115F}'..='\u{1160}' | '\u{17B4}'..='\u{17B5}'
            | '\u{180B}'..='\u{180F}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}' | '\u{3164}' | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}' | '\u{FFA0}'
            | '\u{FFF9}'..='\u{FFFB}' | '\u{E0000}'..='\u{E0FFF}'
    );
    !c.is_control() && !format && !invisible && (c == ' ' || !c.is_whitespace())
}

/// Checks a key: 1 to [`MAX_KEY_CHARS`] printable characters (see `is_printable`).
///
/// # Errors
/// [`PerformError::InvalidKey`] otherwise.
pub fn check_key(key: &str) -> Result<(), PerformError> {
    let count = key.chars().count();
    if (1..=MAX_KEY_CHARS).contains(&count) && key.chars().all(is_printable) {
        Ok(())
    } else {
        Err(PerformError::InvalidKey)
    }
}

/// A character folded for key comparison: Unicode full case folding, built from the case mappings the standard library
/// has. Upper case first, then lower case of each result, joins what lower-casing alone keeps apart (`Σ`, `σ` and the
/// final `ς`; `ß` and `SS`; `İ` and `i` with a combining dot), and ASCII folds as before. Two exceptions make it the
/// Unicode fold: the dotless `ı` has no fold, so it stays distinct from `I` (its upper case would join them), and
/// `ß`, which the lower-casing step leaves (`ẞ` lower-cases to it), folds to `ss`. Allocation-free.
fn fold(c: char) -> impl Iterator<Item = char> {
    let mut folded = ['\0'; 6];
    let mut len = 0;
    if c == '\u{131}' {
        folded[0] = c;
        len = 1;
    } else {
        for lower in c.to_uppercase().flat_map(char::to_lowercase) {
            let letters: &[char] = if lower == '\u{DF}' { &['s', 's'] } else { &[lower] };
            for letter in letters {
                if let Some(slot) = folded.get_mut(len) {
                    *slot = *letter;
                    len += 1;
                }
            }
        }
    }
    folded.into_iter().take(len)
}

/// Whether two keys name the same key: compared by their case folding (see `fold`), so `F1` and `f1` are one key, and
/// so are `Σ`, `σ` and `ς`, or `ß`, `ẞ` and `SS`; `ı` and `I` are two.
pub fn same_key(a: &str, b: &str) -> bool {
    a.chars().flat_map(fold).eq(b.chars().flat_map(fold))
}

/// A bound key and what the table knows about it.
#[derive(Clone, Debug)]
pub(super) struct Bound {
    /// The binding, as the caller gave it.
    pub(super) binding: TriggerBinding,
    /// Whether the expression is missing or shows nothing (see [`TriggerInfo::missing`]).
    pub(super) missing: bool,
    /// The slot last sent to the perform thread for this key, so a rebake sends only what changed.
    pub(super) slot: Option<TriggerSlot>,
    /// Which binding of this slot this is: a new one for every bind, kept across a rebake. The perform thread publishes
    /// the generation it shows next to the slot's activity, so a listing never credits a new binding with the old one's.
    pub(super) generation: u64,
}

/// The control side's table: the bindings by slot, the set they bake against and the rest face they blend onto.
#[derive(Clone, Debug)]
pub(super) struct TriggerBook {
    expressions: ExpressionSet,
    rest: ChannelBuf,
    slots: [Option<Bound>; MAX_TRIGGERS],
    /// The generation the last bind took.
    generation: u64,
}

impl TriggerBook {
    /// An empty table with no expressions: every bind is missing until the set arrives.
    pub(super) fn new() -> Self {
        Self { expressions: ExpressionSet::default(), rest: ChannelBuf::NEUTRAL, slots: std::array::from_fn(|_| None), generation: 0 }
    }

    /// The expression library the table bakes against.
    pub(super) fn expressions(&self) -> ExpressionSet {
        self.expressions.clone()
    }

    /// Whether the set has `expression`.
    pub(super) fn has_expression(&self, expression: &ExpressionId) -> bool {
        self.expressions.0.contains_key(expression)
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

    /// A generation no earlier binding had, for the next bind.
    pub(super) fn next_generation(&mut self) -> u64 {
        self.generation += 1;
        self.generation
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

    /// The table for a new set and rest face: every bound key is re-baked, and the slots whose values changed are
    /// returned with it. A slot that bakes to the same values is kept, because replacing a slot ends the press that
    /// shows it. `self` is left as it was, so a refused send changes nothing.
    pub(super) fn with_expressions(&self, expressions: ExpressionSet, rest: &ChannelBuf) -> (Self, Rebaked) {
        let mut next = Self { expressions, rest: *rest, slots: self.slots.clone(), generation: self.generation };
        let mut changes = Rebaked::default();
        for (index, entry) in next.slots.iter_mut().enumerate() {
            if let Some(bound) = entry {
                let (shown, missing) = bake(&next.expressions, &next.rest, &bound.binding);
                bound.missing = missing;
                if shown != bound.slot {
                    changes.set(index, shown.as_ref());
                    bound.slot = shown;
                }
            }
        }
        (next, changes)
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
    fn keys_are_one_to_thirty_two_printable_characters() {
        assert_eq!(check_key(""), Err(PerformError::InvalidKey));
        assert_eq!(check_key(&"k".repeat(MAX_KEY_CHARS)), Ok(()));
        assert_eq!(check_key(&"k".repeat(MAX_KEY_CHARS + 1)), Err(PerformError::InvalidKey));
        assert_eq!(check_key("F\u{7}"), Err(PerformError::InvalidKey));
    }

    #[test]
    fn keys_refuse_invisible_and_exotic_whitespace_characters() {
        for hidden in ["\u{200B}", "F\u{200B}1", "\u{FEFF}F1", "\u{00AD}", "\u{202E}F1", "\u{2060}", "\u{E0041}", "F\u{00A0}1", "F\u{2003}1", "\u{1BCA0}", "\u{1D173}", "\u{E0001}", "\u{200C}F"] {
            assert_eq!(check_key(hidden), Err(PerformError::InvalidKey), "{hidden:?}");
        }
        for shown in ["F1", "Ctrl+Shift+A", "Numpad +", "\u{FC}", "\u{3042}", "\u{2318}", "+"] {
            assert_eq!(check_key(shown), Ok(()), "{shown:?}");
        }
    }

    #[test]
    fn keys_compare_ignoring_case() {
        assert!(same_key("F1", "f1"));
        assert!(!same_key("F1", "F10"));
    }

    #[test]
    fn keys_compare_by_unicode_case_folding() {
        // Σ, σ and the final-position ς are one letter in three cases.
        assert!(same_key("\u{3A3}", "\u{3C3}") && same_key("\u{3C3}", "\u{3C2}") && same_key("\u{3A3}", "\u{3C2}"));
        // ß folds to ss.
        assert!(same_key("\u{DF}", "SS") && same_key("Stra\u{DF}e", "STRASSE"));
        // The Turkish dotted capital İ is not an I, and folds the same way whatever its case.
        assert!(!same_key("\u{130}", "I") && !same_key("\u{130}", "i"));
        assert!(same_key("\u{130}", "i\u{307}"));
        // The dotless ı has no case fold: it is not an I, though upper-casing it gives one.
        assert!(!same_key("\u{131}", "I") && !same_key("\u{131}", "i") && same_key("\u{131}", "\u{131}"));
        // The capital sharp s folds like the small one.
        assert!(same_key("\u{1E9E}", "\u{DF}") && same_key("\u{1E9E}", "ss"));
        assert!(!same_key("a", "b"));
    }
}
