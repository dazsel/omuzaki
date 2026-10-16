//! Floats as an agent should read them.
//!
//! A value an op builds from an `f32` and stores with `json!(x)` becomes the `f64` nearest to it, so 0.6 reaches the
//! document, the op log and every result as `0.6000000238418579`. An agent that asked for 0.6 copies that noise back
//! into later calls, and it shows up in diffs. Everything this crate hands an agent therefore passes through [`tidy`]:
//! a number that is exactly an `f32` widened, and that the `f32`'s own shortest decimal spells differently, is written
//! with that shorter decimal.
//!
//! The document is untouched: this is how a value is *shown*. A number that is not exactly an `f32` (every `f64` an op
//! computed itself, with rare exceptions: one that happens to be a short binary fraction is the same in both forms)
//! is shown as it is.

use serde_json::{Number, Value};

/// `value` with every `f32`-widened number inside it written with the `f32`'s shortest decimal.
pub(crate) fn tidied(mut value: Value) -> Value {
    tidy(&mut value);
    value
}

/// As [`tidied`], in place.
pub(crate) fn tidy(value: &mut Value) {
    match value {
        Value::Number(number) => {
            if let Some(short) = shortened(number) {
                *number = short;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(tidy),
        Value::Object(map) => map.values_mut().for_each(tidy),
        Value::Null | Value::Bool(_) | Value::String(_) => {}
    }
}

/// The shorter spelling of `number`, if it is an `f32` widened to `f64` whose `f32` decimal is shorter.
fn shortened(number: &Number) -> Option<Number> {
    if !number.is_f64() {
        return None;
    }
    let wide = number.as_f64()?;
    // Exact comparisons are the point here: "the same number" means the same bits.
    // Narrowing is the point: the value is kept only if it survives the round trip exactly.
    #[allow(clippy::cast_possible_truncation)]
    let narrow = wide as f32;
    if !narrow.is_finite() || f64::from(narrow).to_bits() != wide.to_bits() {
        return None;
    }
    let shortest: f64 = format!("{narrow}").parse().ok()?;
    if shortest.to_bits() == wide.to_bits() {
        return None;
    }
    Number::from_f64(shortest)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_widened_f32_is_written_as_the_f32_spells_it() {
        let widened = json!(0.6_f32);
        assert_eq!(widened.to_string(), "0.6000000238418579", "the noise this module removes");
        assert_eq!(tidied(widened).to_string(), "0.6");
        assert_eq!(tidied(json!(0.1_f32)).to_string(), "0.1");
        assert_eq!(tidied(json!(-12.345_f32)).to_string(), "-12.345");
    }

    #[test]
    fn numbers_that_were_never_noisy_are_left_alone() {
        for value in
            [json!(0.5), json!(3.0), json!(7), json!(-2), json!(0.1), json!(0.6), json!(1.0e-9), json!(u64::MAX)]
        {
            assert_eq!(tidied(value.clone()), value);
        }
        // An f64 an op computed is not exactly an f32, so it keeps all its digits.
        let computed = json!(0.123_456_789_012_345_68_f64);
        assert_eq!(tidied(computed.clone()), computed);
    }

    #[test]
    fn it_reaches_into_objects_and_arrays_and_leaves_text_alone() {
        let value =
            json!({"blend": 0.6_f32, "stops": [0.1_f32, 2], "name": "0.6000000238418579", "on": true, "none": null});
        assert_eq!(
            tidied(value),
            json!({"blend": 0.6, "stops": [0.1, 2], "name": "0.6000000238418579", "on": true, "none": null})
        );
    }

    #[test]
    fn non_finite_and_huge_values_are_not_touched() {
        assert_eq!(tidied(json!(1.0e300)), json!(1.0e300));
        assert_eq!(tidied(json!(f64::MAX)), json!(f64::MAX));
    }
}
