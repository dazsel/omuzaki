//! Cloth: an XPBD particle-sheet solver and the runtime that binds it to garments.
//!
//! # Portable maths
//!
//! Contact replays must be bit-identical on every platform (ADR 0009, #1585). The platform's C library rounds
//! `sin`, `exp` and `cbrt` differently from one OS to the next, so every transcendental here goes through the
//! pure-Rust `libm` crate: `libm::expf` (the solver's damping decay), `libm::sin` (the wind gust) and `libm::cbrtf`
//! (a rig collider's scale). `sqrt`, `+ - * /` and `abs` are correctly rounded IEEE operations and stay as they are.
//! Nothing here uses `mul_add`, and Rust never fuses a multiply and an add on its own. A new transcendental on a
//! contact or solver path must use `libm` too.

pub mod bake;
pub mod collide;
pub mod constraints;
pub mod contact;
pub mod quality;
pub mod runtime;
pub mod solver;
