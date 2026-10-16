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
//! Turning a value into text is the third. Rhai builds the whole text before any limit looks at it, and a value can
//! hold more text than the output or string limit allows (a map's keys are the budget's, not the data limits'). Every
//! path Rhai has for it (listed at [`install_formatting`]) is replaced here by one that builds into a sink that stops
//! at the limit, answers the text cut there, and never fails, so Rhai never falls back to formatting a value itself.
//!
//! `pad` and `replace` are registered again here under the same signatures, which take precedence over the package's,
//! and check the projected size first. Every other function of the packages that builds a bigger value (`+` on arrays
//! and strings, `split`, `keys`, `to_chars`, `filter`) is bounded by what the run already holds, and stays within one
//! operation of the budget: it adds at most a copy of what the script could already see.

use std::collections::HashSet;
use std::fmt::{self, Write};
use std::mem::size_of;
use std::ops::{Range, RangeInclusive};

use rhai::{
    Array, Blob, Dynamic, Engine, EvalAltResult, FLOAT, FloatWrapper, FnPtr, FuncRegistration, INT, ImmutableString,
    Map, NativeCallContext, Position,
};

use crate::host::{Stop, terminate_at};
use crate::{LimitKind, Limits, budget};

/// The heap a value's parts take, by the layout of the types rhai 1.26.1 builds them from. Measured against the counting
/// allocator by the tests below, which fail if any of these counts less than the value really holds.
mod layout {
    use std::mem::size_of;

    use rhai::{Dynamic, FnPtr, Identifier, Instant};

    /// One element of an array or blob buffer, or a curried argument: a `Dynamic`.
    pub(super) const SLOT: usize = size_of::<Dynamic>();
    /// The box an array, blob or map sits in inside a `Dynamic` (a `Vec` or `BTreeMap` header).
    pub(super) const CONTAINER_BOX: usize = size_of::<Vec<Dynamic>>();
    /// A shared string: the `Arc`'s two counts and the `SmartString` in it; its text is counted on top.
    pub(super) const STRING: usize = 2 * size_of::<usize>() + size_of::<Identifier>();
    /// A function pointer's box.
    pub(super) const FN_PTR: usize = size_of::<FnPtr>();
    /// A timestamp's box.
    pub(super) const TIMESTAMP: usize = size_of::<Instant>();
    /// A custom type is a box holding a box of the value: the fat pointer, then the value.
    pub(super) const VARIANT_BOX: usize = 2 * size_of::<usize>();
    /// A shared value: the `Arc`'s two counts and the lock around the inner `Dynamic`.
    pub(super) const SHARED: usize = 2 * size_of::<usize>() + size_of::<std::sync::RwLock<Dynamic>>();
    /// A map's B-tree node, counted at the size of the larger kind (an internal node: eleven keys and values, twelve
    /// edges, the parent pointer and two counts).
    const MAP_NODE: usize =
        11 * (size_of::<Identifier>() + size_of::<Dynamic>()) + 12 * size_of::<usize>() + 2 * size_of::<usize>();

    /// A function pointer's curried values: a `ThinVec`, its two counts and a buffer that grows by doubling from four.
    pub(super) fn curry(len: usize) -> usize {
        if len == 0 {
            return 0;
        }
        (2 * size_of::<usize>()).saturating_add(len.max(4).next_power_of_two().saturating_mul(SLOT))
    }

    /// The nodes of a map of `len` entries: every node but the root holds at least five (the B-tree's minimum), so
    /// there are at most `len / 5` of them plus the root, and inner nodes add a fifth more at most.
    pub(super) fn map_nodes(len: usize) -> usize {
        if len == 0 {
            return 0;
        }
        let leaves = len.div_ceil(5).saturating_add(1);
        leaves.saturating_add(leaves.div_ceil(5)).saturating_mul(MAP_NODE)
    }
}

/// How much a value holds, in the terms the limits and the budget use.
///
/// The counts mirror Rhai's own size check (`calc_data_sizes` in `eval/data_check.rs`): an array element is one slot
/// and a blob inside an array or map is one slot per byte plus one, a map value is one entry, and a string's bytes count
/// toward the string limit. `bytes` is the heap the value takes, for every kind of value Rhai 1.26.1 has (`Union` in
/// `types/dynamic.rs`): the containers' buffers and nodes, strings and keys, blobs, function pointers and what they
/// capture, timestamps, the ranges a script can make, and shared values, each shared value once however often it is
/// reached. A copy of a shared value shares it, so counting it in every copy counts more than a copy costs, never less.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct Held {
    /// Array slots, nested ones and blob bytes included.
    slots: usize,
    /// Map entries, nested ones included.
    entries: usize,
    /// Bytes of strings held as values.
    strings: usize,
    /// Heap bytes.
    bytes: usize,
}

/// A value nested deeper than this is not measured, and counts as larger than any limit: padding with it is refused.
const MAX_DEPTH: usize = 256;

impl Held {
    /// What can't be measured (a value nested too deep, a shared value locked by its writer, a custom type of unknown
    /// layout): more than any limit, so whatever would copy it is refused.
    const UNBOUNDED: Self = Self { slots: usize::MAX, entries: usize::MAX, strings: usize::MAX, bytes: usize::MAX };

    /// What `value` holds.
    fn of(value: &Dynamic) -> Self {
        Self::walk(value, 0, &mut HashSet::new())
    }

    /// What `value`, at `depth`, holds; `seen` is the shared values counted so far, by the address of their content.
    fn walk(value: &Dynamic, depth: usize, seen: &mut HashSet<usize>) -> Self {
        if depth > MAX_DEPTH {
            return Self::UNBOUNDED;
        }
        if value.is_shared() {
            // A writer holds the lock: the content can't be read, so it can't be measured.
            let Some(content) = value.read_lock::<Dynamic>() else { return Self::UNBOUNDED };
            if !seen.insert(std::ptr::from_ref::<Dynamic>(&content) as usize) {
                return Self::default();
            }
            let mut held = Self::walk(&content, depth + 1, seen);
            held.bytes = held.bytes.saturating_add(layout::SHARED);
            return held;
        }
        if let Some(array) = value.read_lock::<Array>() {
            let buffer = array.capacity().saturating_mul(layout::SLOT);
            let mut held = Self { bytes: layout::CONTAINER_BOX.saturating_add(buffer), ..Self::default() };
            for item in array.iter() {
                held.slots = held.slots.saturating_add(1);
                held.add(Self::walk(item, depth + 1, seen));
            }
            held
        } else if let Some(blob) = value.read_lock::<Blob>() {
            let bytes = layout::CONTAINER_BOX.saturating_add(blob.capacity());
            Self { slots: blob.len().saturating_add(1), bytes, ..Self::default() }
        } else if let Some(map) = value.read_lock::<Map>() {
            let nodes = layout::CONTAINER_BOX.saturating_add(layout::map_nodes(map.len()));
            let mut held = Self { bytes: nodes, ..Self::default() };
            for (key, item) in map.iter() {
                held.entries = held.entries.saturating_add(1);
                held.bytes = held.bytes.saturating_add(key.len());
                held.add(Self::walk(item, depth + 1, seen));
            }
            held
        } else if let Some(text) = value.read_lock::<ImmutableString>() {
            Self { strings: text.len(), bytes: layout::STRING.saturating_add(text.len()), ..Self::default() }
        } else if let Some(function) = value.read_lock::<FnPtr>() {
            let name = layout::STRING.saturating_add(function.fn_name().len());
            let curried = layout::curry(function.curry().len());
            let mut held =
                Self { bytes: layout::FN_PTR.saturating_add(name).saturating_add(curried), ..Self::default() };
            for item in function.iter_curry() {
                held.add(Self::walk(item, depth + 1, seen));
            }
            held
        } else if value.is_timestamp() {
            Self { bytes: layout::TIMESTAMP, ..Self::default() }
        } else if value.is::<Range<INT>>() || value.is::<RangeInclusive<INT>>() {
            Self { bytes: layout::VARIANT_BOX.saturating_add(size_of::<RangeInclusive<INT>>()), ..Self::default() }
        } else if value.is_unit() || value.is_bool() || value.is_char() || value.is_int() || value.is_float() {
            // Held in the `Dynamic` itself.
            Self::default()
        } else {
            Self::UNBOUNDED
        }
    }

    fn add(&mut self, other: Self) {
        self.slots = self.slots.saturating_add(other.slots);
        self.entries = self.entries.saturating_add(other.entries);
        self.strings = self.strings.saturating_add(other.strings);
        self.bytes = self.bytes.saturating_add(other.bytes);
    }

    /// `self` plus `copies` of `each`, each in a new array slot (in a buffer that may double as it grows).
    fn plus(mut self, copies: usize, each: Self) -> Self {
        self.slots = self.slots.saturating_add(copies.saturating_mul(each.slots.saturating_add(1)));
        self.entries = self.entries.saturating_add(copies.saturating_mul(each.entries));
        self.strings = self.strings.saturating_add(copies.saturating_mul(each.strings));
        let slot = 2 * layout::SLOT;
        self.bytes = self.bytes.saturating_add(copies.saturating_mul(each.bytes.saturating_add(slot)));
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
    // `pad` changes the array, so it is not pure: a constant array can't be padded (as with the package's).
    FuncRegistration::new("pad").with_purity(false).register_into_engine(
        engine,
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
            let each = Held::of(&item);
            let mut have = Held::default();
            for existing in array.iter() {
                have.slots = have.slots.saturating_add(1);
                have.add(Held::of(existing));
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
            let grows = after.bytes.saturating_sub(have.bytes);
            if budget::used_here().saturating_add(grows) > pad_limits.memory_bytes {
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
    // `replace` changes the string, so it is not pure: a constant can't be changed (as with the package's).
    let mutating = || FuncRegistration::new("replace").with_purity(false);
    let first = by_string;
    mutating().register_into_engine(
        engine,
        move |context: NativeCallContext<'_>,
              text: &mut ImmutableString,
              find: ImmutableString,
              with: ImmutableString| { first(&context, text, &find, &with) },
    );
    let second = by_string;
    mutating().register_into_engine(
        engine,
        move |context: NativeCallContext<'_>, text: &mut ImmutableString, find: char, with: ImmutableString| {
            second(&context, text, &find.to_string(), &with)
        },
    );
    let third = by_string;
    mutating().register_into_engine(
        engine,
        move |context: NativeCallContext<'_>, text: &mut ImmutableString, find: ImmutableString, with: char| {
            third(&context, text, &find, &with.to_string())
        },
    );
    mutating().register_into_engine(
        engine,
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

/// Which of Rhai's two forms of a value: the text form (`print`, `to_string`, `${...}`, `+` with a string) or the
/// debug form (`debug`, `to_debug`, and every element of an array or map).
#[derive(Clone, Copy)]
pub(crate) enum Form {
    Text,
    Debug,
}

/// `value` in `form`, as Rhai's own functions for it write it (`print_debug_functions` in rhai's
/// `packages/string_basic.rs`), into `sink`. An error only says the sink is full.
fn write_value(sink: &mut Bounded, value: &Dynamic, form: Form) -> fmt::Result {
    if let Some(array) = value.read_lock::<Array>() {
        write_array(sink, &array)
    } else if let Some(map) = value.read_lock::<Map>() {
        write_map(sink, &map)
    } else if let Some(text) = value.read_lock::<ImmutableString>() {
        write_string(sink, &text, form)
    } else if let Some(character) = value.read_lock::<char>() {
        sink.write_char(*character)
    } else if let Some(function) = value.read_lock::<FnPtr>() {
        write_fn_ptr(sink, &function)
    } else if value.is_unit() {
        write_unit(sink, form)
    } else {
        // Numbers, booleans and ranges: Rhai writes them with the value's own `Display` or `Debug`.
        match form {
            Form::Text => write!(sink, "{value}"),
            Form::Debug => write!(sink, "{value:?}"),
        }
    }
}

/// `[a, b]`, each element in its debug form (`format_array`).
fn write_array(sink: &mut Bounded, array: &Array) -> fmt::Result {
    sink.write_str("[")?;
    for (index, item) in array.iter().enumerate() {
        if index > 0 {
            sink.write_str(", ")?;
        }
        write_value(sink, item, Form::Debug)?;
    }
    sink.write_str("]")
}

/// `#{"k": v}`, each value in its debug form (`format_map`).
fn write_map(sink: &mut Bounded, map: &Map) -> fmt::Result {
    sink.write_str("#{")?;
    for (index, (key, item)) in map.iter().enumerate() {
        if index > 0 {
            sink.write_str(", ")?;
        }
        write!(sink, "{key:?}: ")?;
        write_value(sink, item, Form::Debug)?;
    }
    sink.write_str("}")
}

/// A string as itself, or quoted and escaped in the debug form (`print_string`, `debug_string`).
fn write_string(sink: &mut Bounded, text: &str, form: Form) -> fmt::Result {
    match form {
        Form::Text => sink.write_str(text),
        Form::Debug => write!(sink, "{text:?}"),
    }
}

/// `Fn(name)` in both forms (`debug_fn_ptr`, and the any-value `to_string`): never the captured values.
fn write_fn_ptr(sink: &mut Bounded, function: &FnPtr) -> fmt::Result {
    write!(sink, "{function}")
}

/// Nothing, or `()` in the debug form (`print_unit`, `debug_unit`).
fn write_unit(sink: &mut Bounded, form: Form) -> fmt::Result {
    match form {
        Form::Text => Ok(()),
        Form::Debug => sink.write_str("()"),
    }
}

/// A float as Rhai writes one (`print_f64`, `debug_f64`).
fn write_float(sink: &mut Bounded, number: FLOAT, form: Form) -> fmt::Result {
    match form {
        Form::Text => write!(sink, "{}", FloatWrapper::new(number)),
        Form::Debug => write!(sink, "{:?}", FloatWrapper::new(number)),
    }
}

/// A map as JSON, as Rhai's `to_json` writes it (`format_map_as_json` in rhai's `api/json.rs`).
fn write_json_map(sink: &mut Bounded, map: &Map) -> fmt::Result {
    sink.write_str("{")?;
    for (index, (key, item)) in map.iter().enumerate() {
        if index > 0 {
            sink.write_str(",")?;
        }
        write!(sink, "{key:?}:")?;
        write_json(sink, item)?;
    }
    sink.write_str("}")
}

/// One value of [`write_json_map`] (`format_dynamic_as_json`).
fn write_json(sink: &mut Bounded, value: &Dynamic) -> fmt::Result {
    if let Some(map) = value.read_lock::<Map>() {
        write_json_map(sink, &map)
    } else if let Some(array) = value.read_lock::<Array>() {
        sink.write_str("[")?;
        for (index, item) in array.iter().enumerate() {
            if index > 0 {
                sink.write_str(",")?;
            }
            write_json(sink, item)?;
        }
        sink.write_str("]")
    } else if let Some(blob) = value.read_lock::<Blob>() {
        sink.write_str("[")?;
        for (index, byte) in blob.iter().enumerate() {
            if index > 0 {
                sink.write_str(",")?;
            }
            write!(sink, "{byte}")?;
        }
        sink.write_str("]")
    } else if let Some(function) = value.read_lock::<FnPtr>() {
        if !function.is_curried() {
            return write!(sink, "{:?}", function.fn_name());
        }
        write!(sink, "[{:?}", function.fn_name())?;
        for item in function.iter_curry() {
            sink.write_str(",")?;
            write_json(sink, item)?;
        }
        sink.write_str("]")
    } else if value.is_unit() {
        sink.write_str("null")
    } else {
        write!(sink, "{value:?}")
    }
}

/// `value` as text in `form`, at most `room` bytes of it, and whether more was left out.
pub(crate) fn format_bounded(value: &Dynamic, form: Form, room: usize) -> (String, bool) {
    #[cfg(test)]
    FORMATS.with(|count| count.set(count.get() + 1));
    let mut sink = Bounded::new(room);
    let _ = write_value(&mut sink, value, form); // an error only says the sink is full, which `cut` records
    let cut = sink.cut;
    (sink.into_text(), cut)
}

/// `value` as Rhai's `Display` writes it, at most `room` bytes of it.
pub(crate) fn display_bounded(value: &Dynamic, room: usize) -> String {
    let mut sink = Bounded::new(room);
    let _ = write!(sink, "{value}"); // as in `format_bounded`
    sink.into_text()
}

#[cfg(test)]
thread_local! {
    /// How many times a value was formatted here on this thread, by a function of [`install_formatting`] or by `log`;
    /// the coverage test of the formatting paths reads it.
    pub(crate) static FORMATS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// One way of answering a text to Rhai: the form, the most bytes, and whether a text cut there stops the run.
#[derive(Clone, Copy)]
struct Formatter {
    form: Form,
    room: usize,
    /// A text cut at the string limit ends the run with `Limit(StringSize)`; one cut at the output limit is the
    /// output's to report (its "output truncated" line), and the run goes on.
    cut_stops: bool,
}

impl Formatter {
    /// The text `write` produces, cut at the room. Never fails: Rhai calls these from places where it swallows an
    /// error and formats the whole value itself instead. A run that must stop gets an empty text, and one whose text
    /// was cut at the string limit is marked to stop; either way it stops at its next tick (`Run::progress`).
    fn answer(
        self,
        context: &NativeCallContext<'_>,
        write: impl FnOnce(&mut Bounded) -> fmt::Result,
    ) -> ImmutableString {
        #[cfg(test)]
        FORMATS.with(|count| count.set(count.get() + 1));
        let run = crate::host::run_of(context).ok();
        if let Some(run) = &run {
            run.confirm_format();
        }
        if run.as_ref().is_some_and(|run| run.stopping()) {
            return ImmutableString::new();
        }
        let mut sink = Bounded::new(self.room);
        let _ = write(&mut sink); // an error only says the sink is full, which `cut` records
        if let (true, true, Some(run)) = (sink.cut, self.cut_stops, &run) {
            run.note_stop(Stop::Limit(LimitKind::StringSize));
        }
        sink.into_text().into()
    }
}

/// Registers bounded versions of every function Rhai turns a value into text with. Read from rhai 1.26.1 with the
/// packages a run loads (core, logic, more-string, array, map, math), they are:
///
/// - `${...}`: the evaluator (`Expr::InterpolatedString` in `eval/expr.rs`) writes a string part as it is and calls
///   `to_string` for any other through `print_with_func`;
/// - `print(x)` and `debug(x)`, and `x.to_string()` and `x.to_debug()`: `print_debug_functions` in
///   `packages/string_basic.rs`, with versions for any value and for a string, a character, a boolean, `()`, a float,
///   a function pointer (`debug` and `to_debug` only), an array and a map (whose elements it formats with `to_debug`);
/// - `s + x`, `x + s`, `s += x` and `s.append(x)` for a value that is not a string, a character or `()` (which have
///   versions that only join text): `string_functions` in `packages/string_more.rs`, through `print_with_func`;
/// - `m.to_json()`: `packages/map_basic.rs`, through `format_map_as_json` in `api/json.rs`.
///
/// `print_with_func` calls the function and, if that call fails in any way, formats the whole value with its `Display`
/// or `Debug` instead. The call can fail at its own first tick (Rhai's operation limit, which a run turns off, or the
/// progress callback; `Run::progress` never stops there), in the function (these never fail), or when Rhai checks the
/// size of the result (these answer at most the string limit). Rhai's exact-type versions are found before any version
/// that takes any value, so each is registered here for the same types; for one signature, a function registered on the
/// engine is found before a package's. Integers, ranges and blobs (`string.to_blob()`) reach the any-value version:
/// the blob package, which has blob versions, isn't loaded. (A timestamp, an `f32` and the other integer types have
/// versions in packages that aren't loaded either, and a run can't make one.)
///
/// `print` and `debug` cut at the output limit plus one byte, which is enough for the output to see it is over and mark
/// itself truncated (or at the string limit if that is smaller); the others cut at the string limit and flag the run.
pub(crate) fn install_formatting(engine: &mut Engine, limits: &Limits) {
    let string = Formatter { form: Form::Text, room: limits.string_bytes, cut_stops: true };
    let output_room = limits.output_bytes.saturating_add(1);
    let output =
        if output_room < string.room { Formatter { room: output_room, cut_stops: false, ..string } } else { string };
    for (name, formatter) in [
        ("print", output),
        ("debug", Formatter { form: Form::Debug, ..output }),
        ("to_string", string),
        ("to_debug", Formatter { form: Form::Debug, ..string }),
    ] {
        register_formatter(engine, name, formatter);
    }
    install_concatenation(engine, string);
    engine.register_fn("to_json", move |context: NativeCallContext<'_>, map: &mut Map| {
        string.answer(&context, |sink| write_json_map(sink, map))
    });
}

/// Registers `name` for any value and for each type Rhai's package has its own version of (see
/// [`install_formatting`]). Values are taken by reference where they can be large, so nothing is copied to format it.
fn register_formatter(engine: &mut Engine, name: &str, formatter: Formatter) {
    let form = formatter.form;
    engine.register_fn(name, move |context: NativeCallContext<'_>, value: &mut Dynamic| {
        formatter.answer(&context, |sink| write_value(sink, value, form))
    });
    engine.register_fn(name, move |context: NativeCallContext<'_>, text: &mut ImmutableString| {
        formatter.answer(&context, |sink| write_string(sink, text, form))
    });
    engine.register_fn(name, move |context: NativeCallContext<'_>, character: char| {
        formatter.answer(&context, |sink| sink.write_char(character))
    });
    engine.register_fn(name, move |context: NativeCallContext<'_>, value: bool| {
        formatter.answer(&context, |sink| write!(sink, "{value}"))
    });
    engine.register_fn(name, move |context: NativeCallContext<'_>, (): ()| {
        formatter.answer(&context, |sink| write_unit(sink, form))
    });
    engine.register_fn(name, move |context: NativeCallContext<'_>, number: FLOAT| {
        formatter.answer(&context, |sink| write_float(sink, number, form))
    });
    engine.register_fn(name, move |context: NativeCallContext<'_>, function: &mut FnPtr| {
        formatter.answer(&context, |sink| write_fn_ptr(sink, function))
    });
    engine.register_fn(name, move |context: NativeCallContext<'_>, array: &mut Array| {
        formatter.answer(&context, |sink| write_array(sink, array))
    });
    engine.register_fn(name, move |context: NativeCallContext<'_>, map: &mut Map| {
        formatter.answer(&context, |sink| write_map(sink, map))
    });
}

/// `first` and `second` joined in a string of exactly their length: a map's keys are the budget's, so a key built here
/// must not hold the spare capacity a growing `String` would.
fn join(first: &str, second: &str) -> String {
    let mut joined = String::with_capacity(first.len() + second.len());
    joined.push_str(first);
    joined.push_str(second);
    joined
}

/// Registers `string + value`, `string += value`, `string.append(value)` and `value + string` for any value, replacing
/// the package's, which format `value` through `print_with_func` (see [`install_formatting`]).
fn install_concatenation(engine: &mut Engine, text: Formatter) {
    let format = move |context: &NativeCallContext<'_>, item: &Dynamic| {
        text.answer(context, |sink| write_value(sink, item, Form::Text))
    };
    engine.register_fn(
        "+",
        move |context: NativeCallContext<'_>, string: &mut ImmutableString, item: Dynamic| -> ImmutableString {
            join(string, &format(&context, &item)).into()
        },
    );
    // These change the string, so they are not pure (as with the package's).
    for name in ["+=", "append"] {
        FuncRegistration::new(name).with_purity(false).register_into_engine(
            engine,
            move |context: NativeCallContext<'_>, string: &mut ImmutableString, item: Dynamic| {
                *string = join(string, &format(&context, &item)).into();
            },
        );
    }
    engine.register_fn(
        "+",
        move |context: NativeCallContext<'_>, item: &mut Dynamic, string: &str| -> ImmutableString {
            join(&format(&context, item), string).into()
        },
    );
}

#[cfg(test)]
mod tests {
    //! `Held` against the heap a value really takes, measured with the counting allocator.

    use std::time::Instant;

    use rhai::{Array, Dynamic, FnPtr, Map};

    use super::{Held, MAX_DEPTH};
    use crate::budget;

    /// The heap `build`'s value takes: what the thread holds once it is built (the value is kept alive to read it).
    fn footprint(build: impl FnOnce() -> Dynamic) -> (Dynamic, usize) {
        let counting = budget::Counting::start();
        let value = build();
        let held = budget::used_here();
        drop(counting);
        (value, held)
    }

    fn text(len: usize) -> Dynamic {
        Dynamic::from("x".repeat(len))
    }

    fn map_of(entries: usize, value: impl Fn(usize) -> Dynamic) -> Dynamic {
        Dynamic::from_map((0..entries).map(|i| (format!("key {i}").into(), value(i))).collect::<Map>())
    }

    /// A named way to build a value.
    type Builder = (&'static str, fn() -> Dynamic);

    /// One value of every kind Rhai 1.26.1's `Dynamic` holds (`Union` in `types/dynamic.rs`) that a run can measure,
    /// nested ones included. A shared value, a locked one and a custom type have tests of their own.
    fn every_variant() -> Vec<Builder> {
        vec![
            ("unit", || Dynamic::UNIT),
            ("bool", || Dynamic::from(true)),
            ("char", || Dynamic::from('c')),
            ("int", || Dynamic::from(42_i64)),
            ("float", || Dynamic::from(2.5_f64)),
            ("string", || text(1000)),
            ("short string", || text(3)),
            ("array", || Dynamic::from_array((0..100_i64).map(Dynamic::from).collect())),
            ("blob", || Dynamic::from_blob(vec![7; 64 * 1024])),
            ("map of one", || map_of(1, |_| Dynamic::from(1_i64))),
            ("map of a thousand", || map_of(1000, |i| Dynamic::from(i64::try_from(i).unwrap()))),
            ("map of a thousand, inserted one by one", || {
                let mut map = Map::new();
                for i in (0..1000_i64).rev() {
                    map.insert(format!("key {i}").into(), Dynamic::from(i));
                }
                Dynamic::from_map(map)
            }),
            ("function pointer", || {
                let mut pointer = FnPtr::new("f").unwrap();
                pointer.add_curry(text(1000)).add_curry(Dynamic::from_blob(vec![1; 4096]));
                Dynamic::from(pointer)
            }),
            ("timestamp", || Dynamic::from(Instant::now())),
            ("range", || Dynamic::from(0..10_i64)),
            ("inclusive range", || Dynamic::from(0..=10_i64)),
            ("nested", || {
                let blobs: Array = (0..20).map(|_| Dynamic::from_blob(vec![3; 10_000])).collect();
                let inner = map_of(50, |_| Dynamic::from_array(blobs.clone()));
                Dynamic::from_array(vec![inner, text(5000), map_of(10, |_| text(100))])
            }),
        ]
    }

    #[test]
    fn held_counts_the_heap_of_every_variant() {
        for (name, build) in every_variant() {
            let (value, real) = footprint(build);
            let counted = Held::of(&value).bytes;
            assert!(counted >= real, "{name}: counted {counted} bytes of the {real} it holds");
            // A map is counted with its B-tree nodes as empty as they can be (five entries of eleven), so a full one
            // counts up to about three and a half times what it holds.
            assert!(counted <= 4 * real + 2048, "{name}: counted {counted} bytes for {real}");
        }
    }

    /// The counts the limits use match Rhai's own size check (`calc_data_sizes`): a blob is a slot per byte plus one.
    #[test]
    fn held_counts_slots_entries_and_strings_as_rhai_does() {
        let blob = Dynamic::from_blob(vec![0; 65_536]);
        assert_eq!(Held::of(&blob).slots, 65_537);
        let nested = Dynamic::from_array(vec![blob.clone(), map_of(3, |_| text(10)), text(7)]);
        let held = Held::of(&nested);
        assert_eq!((held.slots, held.entries, held.strings), (3 + 65_537, 3, 37));
    }

    #[test]
    fn held_counts_a_shared_value_once_however_often_it_is_reached() {
        let shared = text(100_000).into_shared();
        let twice = Dynamic::from_array(vec![shared.clone(), shared.clone(), shared]);
        let counted = Held::of(&twice).bytes;
        assert!((100_000..200_000).contains(&counted), "counted {counted} bytes for one shared 100,000-byte string");
    }

    /// A type Rhai knows nothing of, holding a megabyte.
    #[derive(Clone)]
    struct Custom(#[allow(dead_code)] Vec<u8>);

    #[test]
    fn held_refuses_what_it_cannot_measure() {
        // A shared value its writer holds locked can't be read, so it can't be measured.
        let shared = text(10).into_shared();
        let locked = Dynamic::from_array(vec![shared.clone()]);
        let mut writer = shared;
        let held_by_writer = writer.write_lock::<Dynamic>().unwrap();
        assert_eq!(Held::of(&locked), Held::UNBOUNDED, "a locked value");
        drop(held_by_writer);
        // A custom type's heap is its own business.
        assert_eq!(Held::of(&Dynamic::from(Custom(vec![0; 1 << 20]))), Held::UNBOUNDED, "a custom type");
        // Nested past the depth that is measured.
        let mut deep = Dynamic::UNIT;
        for _ in 0..=MAX_DEPTH + 1 {
            deep = Dynamic::from_array(vec![deep]);
        }
        assert_eq!(Held::of(&deep), Held::UNBOUNDED, "a value nested too deep");
    }
}
