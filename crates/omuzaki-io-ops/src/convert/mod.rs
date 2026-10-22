//! Conversions between the file-side types of `omuzaki-io-api` and the runtime types of the other API crates.
//!
//! Each conversion is a pure function over values (no I/O, no services), so the ops and the gather step call them and
//! tests check them in isolation. Where a conversion cannot be exact it says so: fallible ones return an error, and
//! lossy ones are documented next to the function.

pub mod bones;
pub mod channels;
pub mod colour;
pub mod drivers;
pub mod material;
pub mod texture;
