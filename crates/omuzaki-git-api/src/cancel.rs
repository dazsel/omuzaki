//! Cooperative cancellation: the [`Cancel`] trait and the two handles every mock and adapter shares, defined once in
//! [`omuzaki_core::cancel`] and re-exported here so these paths stay valid.

pub use omuzaki_core::cancel::{Cancel, CancelFlag, NeverCancel};
