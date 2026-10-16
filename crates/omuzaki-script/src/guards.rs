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
//! Turning a value into text (`print`, `debug`, `to_string`, `to_debug`, `log`) is the third: Rhai builds the whole text
//! before any limit looks at it, and a value can hold more text than the output or string limit allows (a map's keys
//! are the budget's, not the data limits'). The text is built here into a sink that stops at the limit instead.
//!
//! Both are registered again here under the same signatures, which take precedence over the package's, and check the
//! projected size first. Every other function of the packages that builds a bigger value (`+` on arrays and strings,
//! `split`, `keys`, `to_chars`, `filter`) is bounded by what the run already holds, and stays within one operation of
//! the budget: it adds at most a copy of what the script could already see.

use std::fmt::{self, Write};

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
    install_formatting(engine, limits);
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

/// A text being built that stops taking more at `room` bytes, so formatting a huge value costs `room` and no more.
pub(crate) struct Bounded {
    text: String,
    room: usize,
    /// Set when something did not fit.
    pub(crate) cut: bool,
}

impl Bounded {
    pub(crate) fn new(room: usize) -> Self {
        Self { text: String::new(), room, cut: false }
    }

    pub(crate) fn into_text(self) -> String {
        self.text
    }
}

impl Write for Bounded {
    fn write_str(&mut self, part: &str) -> fmt::Result {
        let left = self.room.saturating_sub(self.text.len());
        if part.len() <= left {
            self.text.push_str(part);
            return Ok(());
        }
        // Keep what fits, on a character boundary, and refuse the rest so the formatter stops walking the value.
        let mut end = left;
        while !part.is_char_boundary(end) {
            end -= 1;
        }
        self.text.push_str(&part[..end]);
        self.cut = true;
        Err(fmt::Error)
    }
}

/// An element of an array or map as Rhai prints it: a string in quotes, a character plain, a container in its
/// brackets, anything else in its debug form. Rhai's own text for a container is made of these.
fn write_element(sink: &mut Bounded, value: &Dynamic) -> fmt::Result {
    if let Some(array) = value.read_lock::<Array>() {
        sink.write_str("[")?;
        for (index, item) in array.iter().enumerate() {
            if index > 0 {
                sink.write_str(", ")?;
            }
            write_element(sink, item)?;
        }
        sink.write_str("]")
    } else if let Some(map) = value.read_lock::<Map>() {
        sink.write_str("#{")?;
        for (index, (key, item)) in map.iter().enumerate() {
            if index > 0 {
                sink.write_str(", ")?;
            }
            write!(sink, "{key:?}: ")?;
            write_element(sink, item)?;
        }
        sink.write_str("}")
    } else if let Some(character) = value.read_lock::<char>() {
        sink.write_char(*character)
    } else {
        write!(sink, "{value:?}")
    }
}

/// `value` as text (`debug` for the debug form), at most `room` bytes of it, and whether more was left out.
pub(crate) fn format_bounded(value: &Dynamic, debug: bool, room: usize) -> (String, bool) {
    let mut sink = Bounded::new(room);
    let _ = if value.is_array() || value.is_map() {
        write_element(&mut sink, value)
    } else if debug {
        write!(sink, "{value:?}")
    } else {
        write!(sink, "{value}")
    };
    let cut = sink.cut;
    (sink.into_text(), cut)
}

/// Registers bounded `print`, `debug`, `to_string` and `to_debug` for any value, an array and a map (the spellings
/// Rhai's own versions of them have for containers). `print` and `debug` answer the text cut at the output limit plus
/// one byte, which is enough for the output to see it is over and mark itself truncated; `to_string` and `to_debug`
/// (which interpolation and `+` use) refuse a text over the string limit, as Rhai does after building it.
pub(crate) fn install_formatting(engine: &mut Engine, limits: &Limits) {
    let output_room = limits.output_bytes.saturating_add(1);
    let string_room = limits.string_bytes;
    for (name, debug, output) in
        [("print", false, true), ("debug", true, true), ("to_string", false, false), ("to_debug", true, false)]
    {
        let room = if output { output_room } else { string_room };
        let answer =
            move |context: &NativeCallContext<'_>, value: &Dynamic| -> Result<ImmutableString, Box<EvalAltResult>> {
                let (text, cut) = format_bounded(value, debug, room);
                if cut && !output {
                    return Err(too_large("Length of string", context.call_position()));
                }
                Ok(text.into())
            };
        engine.register_fn(name, move |context: NativeCallContext<'_>, value: &mut Dynamic| answer(&context, value));
        engine.register_fn(name, move |context: NativeCallContext<'_>, array: &mut Array| {
            let held = Dynamic::from_array(std::mem::take(array));
            let result = answer(&context, &held);
            *array = held.into_array().unwrap_or_default();
            result
        });
        engine.register_fn(name, move |context: NativeCallContext<'_>, map: &mut Map| {
            let held = Dynamic::from_map(std::mem::take(map));
            let result = answer(&context, &held);
            *map = held.try_cast::<Map>().unwrap_or_default();
            result
        });
    }
}
