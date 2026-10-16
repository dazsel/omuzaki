//! Rhai's native functions that loop over a value and call back into the engine for each element, replaced with
//! versions that stop with the run.
//!
//! A run stops at an operation tick (`Run::progress`), and a stop due at the first tick of a call made from native code
//! is held for that one tick, because it may be the tick of Rhai's own `to_string` call, where a stop is swallowed and
//! the whole value formatted instead. Reading every place rhai 1.26.1's loaded packages call back into the engine
//! (`call_native_fn_raw`, `call_fn_raw`, `FnPtr::call_raw`) finds two kinds of loop that the hold alone can't stop
//! cleanly:
//!
//! - `sort` (`sort_by`) and `dedup` with a comparer, and `dedup()`, which compares with `==`, swallow every error of the
//!   calls they make and carry on, cloning both elements for each comparison;
//! - `contains` (and so `in`), `index_of(value)` and `==` and `!=` on arrays and maps make calls that have no tick of
//!   their own after the first, so a stop due at the last element's tick would still be held when they return, and the
//!   next tick could be the `to_string` call of an interpolated part (`${[m, a.contains(x)]}`). `contains` and
//!   `index_of` also clone the value they look for once per element.
//!
//! The versions here make the same calls, in the same order, with the same results, and check the run
//! (`Run::check`) before each element and before they return. Every other loop in the packages calls a closure,
//! whose body ticks after its first tick (a closure must have a body, `closures.rs`), and passes its errors on.

use std::cmp::Ordering;

use rhai::{Array, Dynamic, Engine, EvalAltResult, FnPtr, FuncRegistration, INT, Map, NativeCallContext, Position};

use crate::host::run_of;

/// The name Rhai compares elements with.
const EQUALS: &str = "==";

/// Registers the loops in place of the package's.
pub(crate) fn install(engine: &mut Engine) {
    engine.register_fn("contains", |context: NativeCallContext<'_>, array: &mut Array, value: Dynamic| {
        index_of(&context, array, &value, 0).map(|found| found >= 0)
    });
    engine.register_fn("index_of", |context: NativeCallContext<'_>, array: &mut Array, value: Dynamic| {
        index_of(&context, array, &value, 0)
    });
    engine.register_fn("index_of", |context: NativeCallContext<'_>, array: &mut Array, value: Dynamic, start: INT| {
        index_of(&context, array, &value, start)
    });
    engine.register_fn("==", |context: NativeCallContext<'_>, first: &mut Array, second: Array| {
        arrays_equal(&context, first, second)
    });
    engine.register_fn("!=", |context: NativeCallContext<'_>, first: &mut Array, second: Array| {
        arrays_equal(&context, first, second).map(|equal| !equal)
    });
    engine.register_fn("==", |context: NativeCallContext<'_>, first: &mut Map, second: Map| {
        maps_equal(&context, first, second)
    });
    engine.register_fn("!=", |context: NativeCallContext<'_>, first: &mut Map, second: Map| {
        maps_equal(&context, first, second).map(|equal| !equal)
    });
    // These change the array, so they are not pure, as with the package's: a constant array can't be sorted.
    FuncRegistration::new("dedup")
        .with_purity(false)
        .register_into_engine(engine, |context: NativeCallContext<'_>, array: &mut Array| dedup(&context, array, None));
    FuncRegistration::new("dedup")
        .with_purity(false)
        .register_into_engine(engine, |context: NativeCallContext<'_>, array: &mut Array, comparer: FnPtr| {
            dedup(&context, array, Some(&comparer))
        });
    for name in ["sort", "sort_by"] {
        FuncRegistration::new(name)
            .with_purity(false)
            .register_into_engine(engine, |context: NativeCallContext<'_>, array: &mut Array, comparer: FnPtr| {
                sort(&context, array, &comparer)
            });
    }
}

/// `a == b` for one element, as the package's loops compare them: through `==`, and, when there is no `==` for the
/// two, `false` for values of different types and an error for values of the same type.
fn element_equals(
    context: &NativeCallContext<'_>,
    first: &mut Dynamic,
    second: &mut Dynamic,
) -> Result<bool, Box<EvalAltResult>> {
    let same_type = first.type_id() == second.type_id();
    match context.call_native_fn_raw(EQUALS, true, &mut [first, second]) {
        Ok(equal) => Ok(equal.as_bool().unwrap_or(false)),
        Err(error) => match *error {
            EvalAltResult::ErrorFunctionNotFound(ref signature, ..) if signature.starts_with(EQUALS) && !same_type => {
                Ok(false)
            }
            _ => Err(error),
        },
    }
}

/// The first index at or after `start` (from the end when negative, as Rhai reads it) whose element equals `value`, or
/// -1 (`index_of` and, as `index >= 0`, `contains`).
fn index_of(
    context: &NativeCallContext<'_>,
    array: &mut Array,
    value: &Dynamic,
    start: INT,
) -> Result<INT, Box<EvalAltResult>> {
    let run = run_of(context)?;
    let from = start_of(array.len(), start);
    for (index, item) in array.iter_mut().enumerate().skip(from) {
        run.check()?;
        // A copy for each comparison, as the package makes: `==` on arrays and maps takes its second side by value.
        let equal = element_equals(context, item, &mut value.clone())?;
        if equal {
            run.check()?;
            return Ok(INT::try_from(index).unwrap_or(INT::MAX));
        }
    }
    run.check()?;
    Ok(-1)
}

/// Where a search from `start` begins in an array of `len`: Rhai's `calc_offset_len` (`eval/target.rs`) for a start
/// alone, which counts a negative start from the end and clamps one past the end to the end.
fn start_of(len: usize, start: INT) -> usize {
    if start < 0 {
        let back = usize::try_from(start.unsigned_abs()).unwrap_or(usize::MAX);
        len - back.min(len)
    } else {
        usize::try_from(start).map_or(len, |start| start.min(len))
    }
}

/// `first == second` for arrays: the same length and every pair of elements equal.
fn arrays_equal(
    context: &NativeCallContext<'_>,
    first: &mut Array,
    mut second: Array,
) -> Result<bool, Box<EvalAltResult>> {
    if first.len() != second.len() {
        return Ok(false);
    }
    let run = run_of(context)?;
    for (left, right) in first.iter_mut().zip(second.iter_mut()) {
        run.check()?;
        if !element_equals(context, left, right)? {
            run.check()?;
            return Ok(false);
        }
    }
    run.check()?;
    Ok(true)
}

/// `first == second` for maps: the same keys, and every pair of values equal through `==`, whose errors pass on.
fn maps_equal(context: &NativeCallContext<'_>, first: &mut Map, mut second: Map) -> Result<bool, Box<EvalAltResult>> {
    if first.len() != second.len() {
        return Ok(false);
    }
    let run = run_of(context)?;
    for (key, left) in first.iter_mut() {
        run.check()?;
        let Some(right) = second.get_mut(key) else {
            run.check()?;
            return Ok(false);
        };
        let equal = context.call_native_fn_raw(EQUALS, true, &mut [left, right])?.as_bool().unwrap_or(false);
        if !equal {
            run.check()?;
            return Ok(false);
        }
    }
    run.check()?;
    Ok(true)
}

/// `dedup()` (with `==`) and `dedup(comparer)`: drop each element its predecessor's comparison calls equal. The package
/// calls `comparer(previous, element)` and treats an error as "not equal"; here a run that must stop calls nothing more.
fn dedup(
    context: &NativeCallContext<'_>,
    array: &mut Array,
    comparer: Option<&FnPtr>,
) -> Result<(), Box<EvalAltResult>> {
    if array.is_empty() {
        return Ok(());
    }
    let run = run_of(context)?;
    array.dedup_by(|element, previous| {
        if run.halted() {
            return false;
        }
        let answer = match comparer {
            Some(comparer) => comparer.call_raw(context, None, [previous.clone(), element.clone()]),
            None => context.call_fn_raw(EQUALS, false, false, &mut [&mut previous.clone(), &mut element.clone()]),
        };
        answer.ok().and_then(|equal| equal.as_bool().ok()).unwrap_or(false)
    });
    run.check()
}

/// `sort(comparer)`: the package's order, where the comparer answers a number (its sign) or a boolean (`true` for
/// "before"), and anything else or an error orders by type. Here a run that must stop calls nothing more and keeps the
/// elements in place.
fn sort(context: &NativeCallContext<'_>, array: &mut Array, comparer: &FnPtr) -> Result<(), Box<EvalAltResult>> {
    if array.len() <= 1 {
        return Ok(());
    }
    let run = run_of(context)?;
    // A comparer that is not a total order can make the standard sort panic; the package reports that as an error.
    let sorted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        array.sort_by(|first, second| {
            if run.halted() {
                return Ordering::Equal;
            }
            comparer
                .call_raw(context, None, [first.clone(), second.clone()])
                .ok()
                .and_then(|answer| {
                    answer.as_int().or_else(|_| answer.as_bool().map(|before| if before { -1 } else { 1 })).ok()
                })
                .map_or_else(|| first.type_id().cmp(&second.type_id()), |order| order.cmp(&0))
        });
    }));
    run.check()?;
    sorted.map_err(|_| EvalAltResult::ErrorRuntime("error in comparer for sorting".into(), Position::NONE).into())
}
