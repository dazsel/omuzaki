//! The cloth solver suite with the quality preset (`SolverConfig::quality`, the default); `cloth_live.rs` runs the same
//! tests with the live preset.

use omuzaki_char::cloth::solver::SolverConfig;

mod suite;

/// The solver config under test.
fn preset() -> SolverConfig {
    SolverConfig::quality()
}
