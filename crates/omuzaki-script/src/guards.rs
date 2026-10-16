//! Functions that can copy more than the structural limits bound, checked before they copy.
//!
//! The memory budget is read once per Rhai operation ([`MemoryBudget`](crate::MemoryBudget)), so one native call that
//! copies a value many times could allocate gigabytes before the next read. Rhai's own checks come after the work or
//! don't count what the budget does (a map's keys). Reading Rhai's standard packages for functions that make a result
//! larger than their arguments by more than a constant factor finds two:
//!
//! - `array.pad(len, item)` clones `item` once per added element, and Rhai measures `item` without its map keys;
//! - `string.replace(find, with)` makes a result as long as the matches times `with`, and Rhai builds it before it
//!   checks its length.
//!
//! Both are registered again here under the same signatures, which take precedence over the package's, and check the
//! projected size first. Every other function of the packages that builds a bigger value (`+` on arrays and strings,
//! `split`, `keys`, `to_chars`, `filter`) is bounded by what the run already holds, and stays within one operation of
//! the budget: it adds at most a copy of what the script could already see.

use rhai::{Array, Dynamic, Engine, EvalAltResult, INT, ImmutableString, Map, NativeCallContext, Position};

use crate::host::{Stop, terminate_at};
use crate::{LimitKind, Limits, budget};

/// Bytes counted for a value itself, whatever it holds: the cost of one array slot or map entry.
const NODE_BYTES: usize = 32;

/// How much a value holds, in the terms the limits and the budget use.
#[derive(Clone, Copy, Default)]
struct Held {
    /// Array slots, nested ones included.
    slots: usize,
    /// Map entries, nested ones included.
    entries: usize,
    /// Bytes of strings held as values.
    strings: usize,
    /// Bytes in all: strings, map keys and a fixed cost per slot and entry.
    bytes: usize,
}

/// A value nested deeper than this is not measured, and counts as larger than any limit: padding with it is refused.
const MAX_DEPTH: usize = 256;

impl Held {
    fn of(value: &Dynamic, depth: usize) -> Self {
        let mut held = Self { bytes: NODE_BYTES, ..Self::default() };
        if depth > MAX_DEPTH {
            return Self { slots: usize::MAX, entries: usize::MAX, strings: usize::MAX, bytes: usize::MAX };
        }
        if let Some(array) = value.read_lock::<Array>() {
            for item in array.iter() {
                held.slots = held.slots.saturating_add(1);
                held.add(Self::of(item, depth + 1));
            }
        } else if let Some(map) = value.read_lock::<Map>() {
            for (key, item) in map.iter() {
                held.entries = held.entries.saturating_add(1);
                held.bytes = held.bytes.saturating_add(key.len());
                held.add(Self::of(item, depth + 1));
            }
        } else if let Some(text) = value.read_lock::<ImmutableString>() {
            held.strings = text.len();
            held.bytes = held.bytes.saturating_add(text.len());
        }
        held
    }

    fn add(&mut self, other: Self) {
        self.slots = self.slots.saturating_add(other.slots);
        self.entries = self.entries.saturating_add(other.entries);
        self.strings = self.strings.saturating_add(other.strings);
        self.bytes = self.bytes.saturating_add(other.bytes);
    }

    /// `self` plus `copies` of `each`.
    fn plus(mut self, copies: usize, each: Self) -> Self {
        self.slots = self.slots.saturating_add(copies.saturating_mul(each.slots.saturating_add(1)));
        self.entries = self.entries.saturating_add(copies.saturating_mul(each.entries));
        self.strings = self.strings.saturating_add(copies.saturating_mul(each.strings));
        self.bytes = self.bytes.saturating_add(copies.saturating_mul(each.bytes));
        self
    }
}

#[allow(clippy::unnecessary_box_returns)] // Rhai's callbacks return `Box<EvalAltResult>`
fn too_large(what: &str, at: Position) -> Box<EvalAltResult> {
    EvalAltResult::ErrorDataTooLarge(what.to_owned(), at).into()
}

/// Registers the checked versions of `pad` and `replace`.
pub(crate) fn install(engine: &mut Engine, limits: &Limits) {
    let limits = limits.clone();
    let pad_limits = limits.clone();
    engine.register_fn(
        "pad",
        move |context: NativeCallContext<'_>,
              array: &mut Array,
              len: INT,
              item: Dynamic|
              -> Result<(), Box<EvalAltResult>> {
            let Ok(len) = usize::try_from(len) else { return Ok(()) };
            if len <= array.len() {
                return Ok(());
            }
            let at = context.call_position();
            let extra = len - array.len();
            let each = Held::of(&item, 0);
            let mut have = Held { bytes: 0, ..Held::default() };
            for existing in array.iter() {
                have.slots += 1;
                have.add(Held::of(existing, 1));
            }
            let after = have.plus(extra, each);
            if after.slots > pad_limits.array_len {
                return Err(too_large("Size of array", at));
            }
            if after.entries > pad_limits.map_len {
                return Err(too_large("Size of object map", at));
            }
            if after.strings > pad_limits.string_bytes {
                return Err(too_large("Length of string", at));
            }
            if budget::used_here().saturating_add(each.bytes.saturating_mul(extra)) > pad_limits.memory_bytes {
                return Err(terminate_at(Stop::Limit(LimitKind::Memory), at));
            }
            array.resize(len, item);
            Ok(())
        },
    );
    // The four spellings of `replace`: only a replacement that is a string can make the result longer than a char can.
    let by_string = move |context: &NativeCallContext<'_>,
                          text: &mut ImmutableString,
                          find: &str,
                          with: &str|
          -> Result<(), Box<EvalAltResult>> {
        if text.is_empty() {
            return Ok(());
        }
        let matches = text.matches(find).count();
        let grown = text
            .len()
            .saturating_sub(matches.saturating_mul(find.len()))
            .saturating_add(matches.saturating_mul(with.len()));
        if grown > limits.string_bytes {
            return Err(too_large("Length of string", context.call_position()));
        }
        *text = text.replace(find, with).into();
        Ok(())
    };
    let first = by_string;
    engine.register_fn(
        "replace",
        move |context: NativeCallContext<'_>,
              text: &mut ImmutableString,
              find: ImmutableString,
              with: ImmutableString| { first(&context, text, &find, &with) },
    );
    let second = by_string;
    engine.register_fn(
        "replace",
        move |context: NativeCallContext<'_>, text: &mut ImmutableString, find: char, with: ImmutableString| {
            second(&context, text, &find.to_string(), &with)
        },
    );
    let third = by_string;
    engine.register_fn(
        "replace",
        move |context: NativeCallContext<'_>, text: &mut ImmutableString, find: ImmutableString, with: char| {
            third(&context, text, &find, &with.to_string())
        },
    );
    engine.register_fn(
        "replace",
        move |context: NativeCallContext<'_>, text: &mut ImmutableString, find: char, with: char| {
            by_string(&context, text, &find.to_string(), &with.to_string())
        },
    );
}
