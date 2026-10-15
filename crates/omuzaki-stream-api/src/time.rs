//! Pipeline time: [`Ticks`] and exact conversion between [`Timebase`]s.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::StreamError;

/// Nanoseconds on the pipeline clock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Ticks(pub i64);

/// A rational unit of time: one tick is `num / den` seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Timebase {
    /// Numerator.
    pub num: u32,
    /// Denominator; zero is invalid.
    pub den: u32,
}

impl Timebase {
    /// Converts `value` from this timebase to `to`, rounding to nearest with ties away from zero.
    ///
    /// The product is computed in 128 bits, so any `i64` input is exact.
    ///
    /// # Errors
    /// [`StreamError::Config`] if either denominator or `to.num` is zero, or the result does not fit in `i64`.
    pub fn rescale(self, value: i64, to: Timebase) -> Result<i64, StreamError> {
        if self.den == 0 || to.den == 0 || to.num == 0 {
            return Err(StreamError::Config(format!("invalid timebase {self:?} -> {to:?}")));
        }
        let top = i128::from(value) * i128::from(self.num) * i128::from(to.den);
        let bottom = i128::from(self.den) * i128::from(to.num);
        let half = bottom / 2;
        let rounded = if top >= 0 { (top + half) / bottom } else { (top - half) / bottom };
        i64::try_from(rounded).map_err(|_| StreamError::Config("rescaled time overflows i64".into()))
    }
}
