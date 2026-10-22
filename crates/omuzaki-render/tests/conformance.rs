//! The renderer passes the contract's conformance suite (`omuzaki-render-mock::conformance`), except for the checks whose
//! pass has not been written yet. Each of those is listed with the pass it needs, and the test insists it still fails
//! while that pass is missing from `Capabilities::passes`: the moment a task implements its pass (by deleting the
//! `supported` override in its stub) the check becomes required, with no edit to this file.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use omuzaki_render_api::{Capabilities, Pass, Renderer};
use omuzaki_render_mock::conformance::{self, CHECKS};

/// Checks that need a pass to exist. Changes only when the mock gains a check, never when a pass is implemented.
const NEEDS_PASS: &[(&str, Pass)] =
    &[("outlines hug the silhouette", Pass::Outlines), ("blended instances sort back to front", Pass::Transparent)];

/// The pass `name` is waiting for, if the renderer does not have it yet.
fn missing_pass(capabilities: &Capabilities, name: &str) -> Option<Pass> {
    NEEDS_PASS.iter().find(|(n, pass)| *n == name && !capabilities.supports(*pass)).map(|&(_, pass)| pass)
}

fn counts(r: &dyn Renderer) -> [u32; 5] {
    let s = r.stats();
    [s.meshes, s.textures, s.materials, s.targets, s.environments]
}

#[test]
fn every_check_that_needs_a_pass_names_a_real_check() {
    for (name, _) in NEEDS_PASS {
        assert!(CHECKS.iter().any(|(n, _)| n == name), "no conformance check is called {name:?}");
    }
}

#[test]
fn the_checks_that_should_pass_pass_on_one_renderer_and_leak_nothing() {
    let mut renderer = support::renderer();
    let before = counts(&renderer);
    let capabilities = renderer.capabilities().clone();
    for (name, check) in CHECKS.iter().filter(|(name, _)| missing_pass(&capabilities, name).is_none()) {
        if let Err(failure) = check(&mut renderer) {
            panic!("{name}: {failure}");
        }
    }
    assert_eq!(counts(&renderer), before, "the checks must release everything they make");
}

#[test]
fn checks_that_need_a_missing_pass_still_fail() {
    let capabilities = support::renderer().capabilities().clone();
    for (name, check) in CHECKS {
        let Some(pass) = missing_pass(&capabilities, name) else { continue };
        let mut renderer = support::renderer();
        assert!(
            check(&mut renderer).is_err(),
            "{name:?} passes without the {} pass: remove it from NEEDS_PASS",
            pass.name()
        );
    }
}

#[test]
fn run_all_passes_once_every_needed_pass_exists() {
    let mut renderer = support::renderer();
    if NEEDS_PASS.iter().any(|&(_, pass)| !renderer.capabilities().supports(pass)) {
        return;
    }
    if let Err(failure) = conformance::run_all(&mut renderer) {
        panic!("conformance failed: {failure}");
    }
}

#[test]
fn each_passing_check_also_passes_alone_on_a_fresh_renderer() {
    let capabilities = support::renderer().capabilities().clone();
    for (name, check) in CHECKS.iter().filter(|(name, _)| missing_pass(&capabilities, name).is_none()) {
        let mut renderer = support::renderer();
        if let Err(failure) = check(&mut renderer) {
            panic!("{name} (fresh renderer): {failure}");
        }
    }
}

#[test]
fn the_capability_checks_pass_for_every_capability_the_renderer_claims() {
    let mut renderer = support::renderer();
    assert!(renderer.capabilities().dynamic_meshes, "R2 reports dynamic meshes");
    if let Err(failure) = conformance::run_capable(&mut renderer) {
        panic!("capability checks failed: {failure}");
    }
}

#[test]
fn the_renderer_draws_particles_and_passes_the_particle_checks() {
    let mut renderer = support::renderer();
    assert!(renderer.capabilities().supports(Pass::Particles), "the Particles pass is implemented");
    if let Err(failure) = conformance::run_particles(&mut renderer) {
        panic!("particle checks failed: {failure}");
    }
}

#[test]
fn each_particle_check_passes_alone_on_a_fresh_renderer() {
    for (name, check) in conformance::PARTICLE_CHECKS {
        let mut renderer = support::renderer();
        if let Err(failure) = check(&mut renderer) {
            panic!("{name} (fresh renderer): {failure}");
        }
    }
}
