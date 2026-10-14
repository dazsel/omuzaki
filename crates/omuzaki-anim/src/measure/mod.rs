//! Measurements over a stream of channel frames (spec VF-05): how alive a face looks, as numbers.
//!
//! [`liveliness`] is pure and works on any [`ChannelBuf`](omuzaki_anim_api::ChannelBuf) stream (the life layer's output,
//! a retargeted recording); [`Baseline`] says what a human does, and [`Baseline::compare`] says low, ok or high per
//! metric. The op `measure.liveliness` runs both over a life-layer run or a recorded track.

mod baseline;
mod liveliness;
mod ops;

pub use baseline::{Band, Baseline, BaselineError, Comparison, Verdict};
pub use liveliness::{Liveliness, liveliness};
pub use ops::{MeasureLiveliness, Source};
