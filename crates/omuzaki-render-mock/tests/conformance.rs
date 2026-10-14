//! The mock passes the same conformance suite the real renderer must pass.

use omuzaki_render_api::Renderer;
use omuzaki_render_mock::{MockRenderer, conformance};

#[test]
fn the_mock_passes_every_conformance_check() {
    let mut renderer = MockRenderer::new();
    if let Err(failure) = conformance::run_all(&mut renderer) {
        panic!("conformance failed: {failure}");
    }
}

#[test]
fn each_check_passes_alone_on_a_fresh_renderer() {
    for (name, check) in conformance::CHECKS {
        let mut renderer = MockRenderer::new();
        if let Err(failure) = check(&mut renderer) {
            panic!("{name}: {failure}");
        }
    }
}

#[test]
fn the_mock_claims_and_passes_the_dynamic_and_morph_checks() {
    let mut renderer = MockRenderer::new();
    let caps = renderer.capabilities().clone();
    assert!(caps.dynamic_meshes && caps.morph_targets && caps.max_morph_targets > 0);
    if let Err(failure) = conformance::run_capable(&mut renderer) {
        panic!("dynamic and morph conformance failed: {failure}");
    }
}

#[test]
fn the_mock_passes_the_uv_transform_checks() {
    let mut renderer = MockRenderer::new();
    if let Err(failure) = conformance::run_uv_transform(&mut renderer) {
        panic!("uv transform conformance failed: {failure}");
    }
    for (name, check) in conformance::UV_TRANSFORM_CHECKS {
        let mut renderer = MockRenderer::new();
        if let Err(failure) = check(&mut renderer) {
            panic!("{name}: {failure}");
        }
    }
}

#[test]
fn each_dynamic_and_morph_check_passes_alone_on_a_fresh_renderer() {
    for (name, check) in conformance::DYNAMIC_CHECKS.iter().chain(conformance::MORPH_CHECKS) {
        let mut renderer = MockRenderer::new();
        if let Err(failure) = check(&mut renderer) {
            panic!("{name}: {failure}");
        }
    }
}
