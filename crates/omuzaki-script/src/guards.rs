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

use std::fmt::{self, Write};

use rhai::{
    Array, Dynamic, Engine, EvalAltResult, FLOAT, FloatWrapper, FnPtr, INT, ImmutableString, Map, NativeCallContext,
    Position,
};

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
/// engine is found before a package's. Integers and ranges have only the any-value version. (A blob, a timestamp, an
/// `f32` and the other integer types have their own versions too, but a run can't hold one: their packages aren't
/// loaded and nothing converts to them.)
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
    for name in ["+=", "append"] {
        engine.register_fn(name, move |context: NativeCallContext<'_>, string: &mut ImmutableString, item: Dynamic| {
            *string = join(string, &format(&context, &item)).into();
        });
    }
    engine.register_fn(
        "+",
        move |context: NativeCallContext<'_>, item: &mut Dynamic, string: &str| -> ImmutableString {
            join(&format(&context, item), string).into()
        },
    );
}
