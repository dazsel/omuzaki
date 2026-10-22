//! Particles under the capability profiles (#756, particles.md D6): one shader for all three, so there is no WebGL2
//! path to test, only proof. Under `WebGpu` the particle conformance checks pass and the contact sheet matches the
//! same goldens as under `Native`; `WebGl2` is still refused, and for exactly the requirements it lacked before the
//! particles (the renderer's instance and joint storage buffers and the texture size), none of them the particles'.
//! The shader side (no storage buffer in any particle stage, no compute, WebGL2's uniform and texture limits) is
//! checked on the WGSL itself in `pass::particles`.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::path::Path;

use omuzaki_render::{AdapterPreference, CapabilityProfile, HeadlessOptions, WgpuRenderer};
use omuzaki_render_api::{AlphaEncoding, Background, Colour, Image, RenderError};
use omuzaki_render_mock::conformance::CHECKS;
use omuzaki_render_mock::particle_sheet;

fn open(profile: CapabilityProfile) -> Result<WgpuRenderer, RenderError> {
    WgpuRenderer::headless_with_profile(&HeadlessOptions { adapter: AdapterPreference::Software }, profile)
}

/// `image` against `tests/golden/<name>`, the golden `particles.rs` keeps under `Native`, with the same tolerance.
fn matches_golden(image: &Image, name: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/golden/{name}"));
    let golden = image::open(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())).to_rgba8();
    assert_eq!(golden.dimensions(), (image.size.x, image.size.y));
    let diff = support::Diff::of(golden.as_raw(), &image.rgba8);
    assert!(diff.max <= 2, "{name} under webgpu differs from its golden by up to {} levels ({diff})", diff.max);
}

#[test]
fn under_webgpu_the_particle_checks_pass_and_the_sheet_matches_the_native_goldens() {
    let mut renderer = open(CapabilityProfile::WebGpu).expect("lavapipe satisfies WebGPU's default limits");
    let mut ran = 0;
    for (name, check) in CHECKS.iter().filter(|(name, _)| name.starts_with("particle")) {
        if let Err(failure) = check(&mut renderer) {
            panic!("webgpu: {name}: {failure}");
        }
        ran += 1;
    }
    assert_eq!(ran, 16, "every particle check ran");

    let grey = Background::Solid { colour: Colour::hex(0x0080_8080) };
    for (background, alpha, name) in [
        (grey, AlphaEncoding::Straight, "particles-grey.png"),
        (Background::Transparent, AlphaEncoding::Straight, "particles-transparent-straight.png"),
        (Background::Transparent, AlphaEncoding::Premultiplied, "particles-transparent-premultiplied.png"),
    ] {
        let sheet = particle_sheet::sheet(&mut renderer, background, alpha).unwrap();
        matches_golden(&sheet, name);
    }
}

#[test]
fn webgl2_is_refused_only_for_what_it_lacked_before_the_particles() {
    let lacking: Vec<&str> = CapabilityProfile::WebGl2.unmet_requirements().iter().map(|u| u.limit).collect();
    assert_eq!(
        lacking,
        ["max_storage_buffers_per_shader_stage", "max_storage_buffer_binding_size", "max_texture_dimension_2d"]
    );
    let Err(RenderError::Backend(message)) = open(CapabilityProfile::WebGl2) else {
        panic!("webgl2 must be refused while instances and joints are storage buffers")
    };
    assert!(message.contains("3 unmet"), "{message}");
    assert!(!message.to_lowercase().contains("particle"), "the particles add nothing to the list: {message}");
}
