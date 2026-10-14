//! Capability profiles: every profile either runs the render conformance suite on a device holding exactly its
//! limits, or fails at creation with the full list of what it lacks. The device never gets the adapter's own limits, so
//! a feature a laptop lacks fails on this machine too.

// Integration tests may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_render::{AdapterPreference, CapabilityProfile, HeadlessOptions, WgpuRenderer};
use omuzaki_render_api::{Pass, RenderError, Renderer};
use omuzaki_render_mock::conformance::CHECKS;

/// The checks that need a pass the renderer may not have yet (as in `conformance.rs`).
const NEEDS_PASS: &[(&str, Pass)] =
    &[("outlines hug the silhouette", Pass::Outlines), ("blended instances sort back to front", Pass::Transparent)];

fn open(profile: CapabilityProfile) -> Result<WgpuRenderer, RenderError> {
    WgpuRenderer::headless_with_profile(&HeadlessOptions { adapter: AdapterPreference::Software }, profile)
}

/// What opening `profile` did: a renderer, or the reason the profile cannot run (unmet requirements), or the adapter
/// cannot provide it.
enum Outcome {
    Opened(Box<WgpuRenderer>),
    /// The renderer needs more than the profile offers; the message lists all of it.
    Refused(String),
    /// The adapter lacks something the profile asks for (a smaller GPU than the profile): nothing to test here.
    AdapterTooSmall,
}

fn try_open(profile: CapabilityProfile) -> Outcome {
    match open(profile) {
        Ok(renderer) => Outcome::Opened(Box::new(renderer)),
        Err(RenderError::Backend(message)) if message.contains("unmet") => Outcome::Refused(message),
        Err(RenderError::Backend(message)) if message.contains("may not meet it") => Outcome::AdapterTooSmall,
        Err(other) => panic!("{profile}: {other}"),
    }
}

#[test]
fn a_profile_that_meets_the_requirements_opens_a_renderer_that_passes_the_conformance_checks() {
    let mut ran = Vec::new();
    for profile in CapabilityProfile::ALL {
        let met = profile.unmet_requirements().is_empty();
        match (try_open(profile), met) {
            (Outcome::Opened(mut renderer), true) => {
                let capabilities = renderer.capabilities().clone();
                for (name, check) in CHECKS
                    .iter()
                    .filter(|(name, _)| !NEEDS_PASS.iter().any(|&(n, pass)| n == *name && !capabilities.supports(pass)))
                {
                    if let Err(failure) = check(&mut *renderer) {
                        panic!("{profile}: {name}: {failure}");
                    }
                }
                ran.push(profile);
            }
            (Outcome::AdapterTooSmall, true) => eprintln!("{profile}: the adapter cannot provide it; skipped"),
            (Outcome::Refused(_), true) => panic!("{profile}: refused although it meets every requirement"),
            (Outcome::Opened(_), false) => panic!("{profile}: opened although requirements are unmet"),
            (Outcome::Refused(_) | Outcome::AdapterTooSmall, false) => {}
        }
    }
    assert!(ran.contains(&CapabilityProfile::WebGpu), "lavapipe must satisfy WebGPU's default limits: ran {ran:?}");
}

#[test]
fn a_profile_that_falls_short_is_refused_with_every_unmet_requirement() {
    for profile in CapabilityProfile::ALL {
        let expected = profile.unmet_requirements();
        if expected.is_empty() {
            continue;
        }
        let Outcome::Refused(message) = try_open(profile) else { panic!("{profile} must be refused") };
        assert!(message.contains(&format!("{} unmet", expected.len())), "{message}");
        for item in &expected {
            assert!(message.contains(&item.to_string()), "{item} missing from {message}");
        }
    }
}

#[test]
fn the_device_has_the_profiles_limits_and_not_the_adapters() {
    for (profile, side) in [(CapabilityProfile::WebGpu, 8192), (CapabilityProfile::Native, 8192)] {
        let Outcome::Opened(renderer) = try_open(profile) else { continue };
        assert_eq!(renderer.capabilities().max_texture_size, side, "{profile}");
    }
}

#[test]
fn the_webgpu_profile_has_no_gpu_timings_because_browsers_expose_no_timestamps_between_passes() {
    let Outcome::Opened(renderer) = try_open(CapabilityProfile::WebGpu) else { return };
    assert!(!renderer.capabilities().gpu_timings, "browsers expose no timestamps between passes");
}
