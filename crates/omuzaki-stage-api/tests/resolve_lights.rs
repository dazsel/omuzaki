//! The light resolve (`Lighting::resolve_into`): the design's studio rig (D3) and candle (§4) as numbers, the subject
//! default and the missing-member error, the capacity, the falloff under a change of stature (D4), the yaw, and zero
//! allocations (D10).

#![allow(clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::f32::consts::FRAC_PI_2;

use omuzaki_core::glam::{Affine3A, Vec3};
use omuzaki_render_api::limits::MAX_LIGHTS;
use omuzaki_render_api::{Colour, Light};
use omuzaki_stage_api::resolve::{AnchorFrame, LightBuf};
use omuzaki_stage_api::{Anchor, LightDef, LightId, LightShape, Lighting, MemberId, SceneError};
use proptest::prelude::*;
use proptest::test_runner::RngSeed;

fn id(name: &str) -> LightId {
    LightId::new(name).unwrap()
}

fn directional(azimuth: f32, elevation: f32, anchor: Anchor) -> LightDef {
    LightDef {
        role: None,
        colour: Colour::WHITE,
        intensity: 1.0,
        shadow: false,
        anchor,
        shape: LightShape::Directional { azimuth, elevation },
    }
}

fn point(position: [f32; 3], range: f32, intensity: f32, anchor: Anchor) -> LightDef {
    LightDef {
        role: None,
        colour: Colour::WHITE,
        intensity,
        shadow: false,
        anchor,
        shape: LightShape::Point { position, range },
    }
}

fn rig(lights: Vec<(&str, LightDef)>) -> Lighting {
    let mut lighting = Lighting::default();
    for (name, def) in lights {
        lighting.lights.insert(id(name), def);
    }
    lighting
}

fn figure(height_m: f32) -> AnchorFrame {
    AnchorFrame { root: Affine3A::IDENTITY, height_m }
}

/// A lookup with no cast: every member is unknown, so the subject is the 1 m figure at the origin.
fn no_cast(_: &Anchor) -> Option<AnchorFrame> {
    None
}

/// A lookup where every member is a 1 m figure at the origin.
fn figure_for_members(anchor: &Anchor) -> Option<AnchorFrame> {
    match anchor {
        Anchor::Member(_) => Some(figure(1.0)),
        _ => None,
    }
}

fn resolve(lighting: &Lighting, frames: &dyn Fn(&Anchor) -> Option<AnchorFrame>) -> Result<Vec<Light>, SceneError> {
    let mut out = LightBuf::new();
    lighting.resolve_into(frames, &mut out)?;
    Ok(out.as_slice().to_vec())
}

/// The azimuth and elevation (degrees) that point at `p`, in `LightShape::Directional`'s convention, exactly.
fn angles_of(p: [f32; 3]) -> (f32, f32) {
    let v = Vec3::from_array(p).normalize();
    (v.x.atan2(v.z).to_degrees(), v.y.asin().to_degrees())
}

/// Within `rel` of `want`, relative to `want`.
fn close(got: f32, want: f32, rel: f32) -> bool {
    (got - want).abs() <= rel * want.abs()
}

/// The three.js point falloff (render-api D10): intensity over `max(d², 0.01)`, times a window that reaches zero at range.
fn falloff(light: &Light, p: Vec3) -> f32 {
    let Light::Point { position, range, intensity, .. } = *light else { panic!("not a point light") };
    let d = position.distance(p);
    let window = (1.0 - (d / range).powi(4)).clamp(0.0, 1.0);
    intensity * window * window / d.powi(2).max(0.01)
}

const STUDIO: [(&str, [f32; 3]); 3] =
    [("key", [1.6, 3.0, 2.4]), ("rim", [-2.2, 2.0, -2.5]), ("fill", [-2.4, 1.2, 2.0])];

#[test]
fn the_studio_rig_resolves_to_the_normalised_positions_of_design_d3() {
    let lighting = rig(STUDIO
        .iter()
        .map(|(name, p)| {
            let (azimuth, elevation) = angles_of(*p);
            (*name, directional(azimuth, elevation, Anchor::Subject))
        })
        .collect());

    let lights = resolve(&lighting, &no_cast).unwrap();

    // The ambient first, then the lights in id order: candle is absent here, so fill, key, rim.
    assert!(matches!(lights[0], Light::Hemisphere { .. }));
    for (i, name) in ["fill", "key", "rim"].iter().enumerate() {
        let (_, p) = STUDIO.iter().find(|(n, _)| n == name).unwrap();
        let Light::Directional { to_light, .. } = lights[i + 1] else { panic!("{name} is not directional") };
        let want = Vec3::from_array(*p).normalize();
        assert!(to_light.abs_diff_eq(want, 1e-5), "{name}: {to_light:?} vs {want:?}");
    }
    assert_eq!(lights.len(), 4);
}

#[test]
fn the_fixture_rigs_angles_agree_with_their_positions_to_1e3() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/scene/lighting.toml");
    let text = std::fs::read_to_string(path).unwrap();
    let lighting: Lighting = toml::from_str(&text).unwrap();

    let lights = resolve(&lighting, &figure_for_members).unwrap();

    // The candle (a member point light) sorts first, so fill, key and rim follow at 2, 3 and 4.
    assert!(matches!(lights[1], Light::Point { .. }));
    for (i, (name, p)) in [("fill", STUDIO[2].1), ("key", STUDIO[0].1), ("rim", STUDIO[1].1)].into_iter().enumerate() {
        let Light::Directional { to_light, casts_shadow, .. } = lights[i + 2] else {
            panic!("{name} is not directional")
        };
        let want = Vec3::from_array(p).normalize();
        assert!(to_light.abs_diff_eq(want, 1e-3), "{name}: the TOML angles drifted from the positions: {to_light:?}");
        assert_eq!(casts_shadow, name == "key", "{name}: only the key casts a shadow");
    }
}

#[test]
fn the_candle_of_design_4_resolves_on_aiko_and_on_kiri() {
    let lighting = rig(vec![("candle", point([1.501, 1.283, -1.19], 7.246, 5.187, Anchor::Subject))]);
    // (subject height in metres, expected position, range, intensity)
    let cases = [(0.966, [1.45, 1.239, -1.15], 7.0, 4.84), (1.714, [2.573, 2.198, -2.040], 12.42, 15.24)];

    for (height, position, range, intensity) in cases {
        let frames = move |_: &Anchor| Some(figure(height));
        let lights = resolve(&lighting, &frames).unwrap();

        let Light::Point { position: got, range: got_range, intensity: got_intensity, .. } = lights[1] else {
            panic!("the candle is not a point light")
        };
        for (g, w) in got.to_array().into_iter().zip(position) {
            assert!(close(g, w, 1e-3), "{height} m: position {got:?} vs {position:?}");
        }
        assert!(close(got_range, range, 1e-3), "{height} m: range {got_range} vs {range}");
        assert!(close(got_intensity, intensity, 1e-3), "{height} m: intensity {got_intensity} vs {intensity}");
    }
}

/// Points on the subject, at least 0.3 subject units from the candle, so the falloff's 0.01 floor never applies.
const SUBJECT_POINTS: [[f32; 3]; 3] = [[0.0, 1.0, 0.0], [0.4, 0.9, 0.2], [-0.3, 1.5, 0.1]];

fn config() -> ProptestConfig {
    // A fixed seed unless PROPTEST_RNG_SEED asks otherwise, so a failure reproduces (as omuzaki-doc's properties do).
    let mut config = ProptestConfig { failure_persistence: None, ..ProptestConfig::default() };
    if matches!(config.rng_seed, RngSeed::Random) {
        config.rng_seed = RngSeed::Fixed(0x5EED);
    }
    config
}

proptest! {
    #![proptest_config(config())]
    #[test]
    fn the_falloff_at_corresponding_points_is_the_same_at_any_stature(height in 0.5f32..=2.0) {
        let lighting = rig(vec![("candle", point([1.501, 1.283, -1.19], 7.246, 5.187, Anchor::Subject))]);
        let lit_at = |height: f32, q: [f32; 3]| {
            let frames = move |_: &Anchor| Some(figure(height));
            let lights = resolve(&lighting, &frames).unwrap();
            falloff(&lights[1], Vec3::from_array(q) * height)
        };
        for q in SUBJECT_POINTS {
            let (want, got) = (lit_at(1.0, q), lit_at(height, q));
            prop_assert!(close(got, want, 1e-4), "{q:?} at {height} m: {got} vs {want}");
        }
    }
}

#[test]
fn a_members_yaw_turns_its_lights_and_the_stage_ignores_it() {
    let turned = AnchorFrame { root: Affine3A::from_rotation_y(FRAC_PI_2), height_m: 1.0 };
    let lighting = rig(vec![
        ("member", directional(0.0, 0.0, Anchor::Member(MemberId::new("kiri").unwrap()))),
        ("stage", directional(0.0, 0.0, Anchor::Stage)),
    ]);
    let lights = resolve(&lighting, &move |_: &Anchor| Some(turned)).unwrap();

    // A front light (from +Z) turned by +90 degrees about Y comes from +X; the stage's stays put.
    let Light::Directional { to_light: member, .. } = lights[1] else { panic!("member light") };
    let Light::Directional { to_light: stage, .. } = lights[2] else { panic!("stage light") };
    assert!(member.abs_diff_eq(Vec3::X, 1e-5), "member: {member:?}");
    assert!(stage.abs_diff_eq(Vec3::Z, 1e-5), "stage: {stage:?}");
}

#[test]
fn a_point_is_scaled_by_height_then_placed_by_the_root() {
    // A member 2 m tall, turned by +90 degrees and moved 2 m along +X: its unit point (0, 0, 1) is (0, 0, 2) metres,
    // which the turn takes to (2, 0, 0) and the move to (4, 0, 0).
    let root = Affine3A::from_translation(Vec3::X * 2.0) * Affine3A::from_rotation_y(FRAC_PI_2);
    let lighting =
        rig(vec![("lamp", point([0.0, 0.0, 1.0], 1.0, 1.0, Anchor::Member(MemberId::new("kiri").unwrap())))]);
    let lights = resolve(&lighting, &move |_: &Anchor| Some(AnchorFrame { root, height_m: 2.0 })).unwrap();

    let Light::Point { position, range, intensity, .. } = lights[1] else { panic!("lamp") };
    assert!(position.abs_diff_eq(Vec3::new(4.0, 0.0, 0.0), 1e-5), "{position:?}");
    assert!(close(range, 2.0, 1e-6), "range {range}");
    assert!(close(intensity, 4.0, 1e-6), "intensity {intensity}");
}

#[test]
fn with_no_cast_the_subject_is_a_one_metre_figure_at_the_origin() {
    let lighting = rig(vec![
        ("side", directional(90.0, 0.0, Anchor::Subject)),
        ("lamp", point([0.5, 1.0, 0.5], 2.0, 3.0, Anchor::Subject)),
    ]);
    let lights = resolve(&lighting, &no_cast).unwrap();

    // +90 degrees is from +X; the 1 m figure is unscaled, so the lamp keeps its numbers.
    let Light::Directional { to_light, .. } = lights[2] else { panic!("side") };
    assert!(to_light.abs_diff_eq(Vec3::X, 1e-5), "{to_light:?}");
    let Light::Point { position, range, intensity, .. } = lights[1] else { panic!("lamp") };
    assert!(position.abs_diff_eq(Vec3::new(0.5, 1.0, 0.5), 1e-6), "{position:?}");
    assert!(close(range, 2.0, 1e-6) && close(intensity, 3.0, 1e-6));
}

#[test]
fn a_member_the_lookup_does_not_know_is_refused_at_its_light() {
    let lighting =
        rig(vec![("candle", point([1.5, 1.2, -1.2], 7.0, 5.0, Anchor::Member(MemberId::new("kiri").unwrap())))]);

    let err = resolve(&lighting, &no_cast).unwrap_err();

    assert_eq!(err.path, "lighting.lights.candle.anchor");
    assert!(err.rule.contains("member:kiri"), "{}", err.rule);
}

#[test]
fn the_ambient_and_the_lights_stay_within_max_lights() {
    let lights_named = |n: usize| {
        let mut lighting = Lighting::default();
        for i in 0..n {
            lighting.lights.insert(id(&format!("l{i:02}")), directional(0.0, 0.0, Anchor::Stage));
        }
        lighting
    };

    assert_eq!(resolve(&lights_named(MAX_LIGHTS - 1), &no_cast).unwrap().len(), MAX_LIGHTS);
    let err = resolve(&lights_named(MAX_LIGHTS), &no_cast).unwrap_err();
    assert_eq!(err.path, "lighting.lights");
}

#[test]
fn a_directional_light_keeps_its_shadow_and_a_point_light_has_none() {
    let mut lighting = rig(vec![("key", directional(0.0, 45.0, Anchor::Stage))]);
    lighting.lights.get_mut(&id("key")).unwrap().shadow = true;
    lighting.lights.insert(id("lamp"), LightDef { shadow: true, ..point([0.0, 1.0, 0.0], 3.0, 1.0, Anchor::Stage) });

    let lights = resolve(&lighting, &no_cast).unwrap();

    assert!(matches!(lights[2], Light::Point { .. }));
    assert!(matches!(lights[1], Light::Directional { casts_shadow: true, .. }));
}

thread_local! {
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// The allocator only counts and forwards to `System`, so it adds no obligations beyond the trait's.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if WATCHING.with(Cell::get) {
            ALLOCATIONS.with(|n| n.set(n.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if WATCHING.with(Cell::get) {
            ALLOCATIONS.with(|n| n.set(n.get() + 1));
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn resolving_into_a_buffer_allocates_nothing() {
    let lighting = rig(vec![
        ("candle", point([1.501, 1.283, -1.19], 7.246, 5.187, Anchor::Member(MemberId::new("kiri").unwrap()))),
        ("fill", directional(-50.19, 21.01, Anchor::Subject)),
        ("key", directional(33.69, 46.13, Anchor::Subject)),
        ("rim", directional(-138.65, 30.99, Anchor::Stage)),
    ]);
    let mut out = LightBuf::new();
    lighting.resolve_into(&figure_for_members, &mut out).unwrap(); // warm up anything lazy

    WATCHING.with(|w| w.set(true));
    for _ in 0..1_000 {
        lighting.resolve_into(&figure_for_members, &mut out).unwrap();
    }
    WATCHING.with(|w| w.set(false));

    assert_eq!(out.len(), 5);
    assert_eq!(ALLOCATIONS.with(Cell::get), 0, "resolve_into allocated");
}
