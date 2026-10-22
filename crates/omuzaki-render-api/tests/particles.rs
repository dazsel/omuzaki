//! The particle contract (#755, particles.md D2 and D3): the reference evaluation against a numeric integration of the
//! same forces, the pinned random function, the orthonormal basis, and one test per validation rule.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names,
    clippy::float_cmp,
    clippy::type_complexity
)]

use std::f64::consts::TAU;

use omuzaki_core::glam::{DVec3, Mat4, Vec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::particles::{Stream, evaluate, onb, particle_random, sphere_dir};
use omuzaki_render_api::{
    Camera, CellPick, Colour, ColourSet, Edge, EmitterShape, Flutter, Orientation, ParticleParams, ParticleShading,
    ParticleSystemDesc, ParticleSystemId, RenderError, Scene, Span, SpawnBatch, Sprite, SpriteShape, TextureId, limits,
};

fn params() -> ParticleParams {
    ParticleParams {
        lifetime: Span::fixed(10.5),
        speed: Span::new(1.0, 3.0),
        direction: [0.0, 1.0, 0.0],
        spread_deg: 40.0,
        gravity: [0.0, -3.0, 0.0],
        drag: 0.0,
        wind: [0.0; 3],
        flutter: None,
        size: Span::new(0.01, 0.02),
        size_curve: [1.0; 4],
        orientation: Orientation::billboard(Span::new(-30.0, 30.0)),
        colours: ColourSet::one(Colour::WHITE),
        coverage: 1.0,
        shading: ParticleShading::Unlit,
        edge: None,
        glow: 0.0,
        floor: None,
    }
}

fn desc(params: ParticleParams) -> ParticleSystemDesc {
    ParticleSystemDesc { sprite: Sprite::Shape(SpriteShape::Disc), params }
}

fn system() -> ParticleSystemId {
    ParticleSystemId::from_parts(0, 1)
}

/// Integrates `dv/dt = G + k (w - v)` and `dp/dt = v + d(flutter)/dt` with classic RK4 at 1 ms steps, in f64.
fn integrate(p: &ParticleParams, p0: DVec3, v0: DVec3, flutter: Option<(f64, DVec3)>, until: f64) -> DVec3 {
    let g = Vec3::from(p.gravity).as_dvec3();
    let w = Vec3::from(p.wind).as_dvec3();
    let k = f64::from(p.drag);
    let sway = |t: f64| -> DVec3 {
        match (p.flutter, flutter) {
            (Some(f), Some((phase, side))) => {
                let (a, fr) = (f64::from(f.amplitude_m), f64::from(f.frequency_hz));
                side * (a * TAU * fr * (TAU * fr * t + TAU * phase).cos())
            }
            _ => DVec3::ZERO,
        }
    };
    let deriv = |t: f64, _pos: DVec3, vel: DVec3| (vel + sway(t), g + k * (w - vel));
    let (mut pos, mut vel, mut t) = (p0, v0, 0.0);
    let h = 1e-3;
    let steps = (until / h).round() as usize;
    for _ in 0..steps {
        let (k1p, k1v) = deriv(t, pos, vel);
        let (k2p, k2v) = deriv(t + h / 2.0, pos + k1p * (h / 2.0), vel + k1v * (h / 2.0));
        let (k3p, k3v) = deriv(t + h / 2.0, pos + k2p * (h / 2.0), vel + k2v * (h / 2.0));
        let (k4p, k4v) = deriv(t + h, pos + k3p * h, vel + k3v * h);
        pos += (k1p + 2.0 * k2p + 2.0 * k3p + k4p) * (h / 6.0);
        vel += (k1v + 2.0 * k2v + 2.0 * k3v + k4v) * (h / 6.0);
        t += h;
    }
    // The flutter term starts from its own phase, not from zero: add its value at t = 0.
    if let (Some(f), Some((phase, side))) = (p.flutter, flutter) {
        pos += side * (f64::from(f.amplitude_m) * (TAU * phase).sin());
    }
    pos
}

#[test]
fn evaluate_matches_a_numeric_integration_of_the_same_forces_over_ten_seconds() {
    let mut worst = 0.0f64;
    for drag in [0.0, 1.0, 3.0] {
        for wind in [[0.0; 3], [0.8, 0.0, -0.4]] {
            for flutter in [None, Some(Flutter { amplitude_m: 0.1, frequency_hz: 0.7 })] {
                let p = ParticleParams { drag, wind, flutter, ..params() };
                let d = desc(p.clone());
                let batch = SpawnBatch {
                    shape: EmitterShape::sphere(0.3),
                    velocity: Vec3::new(0.2, 0.0, 0.1),
                    count: 8,
                    seed: 99,
                    ..SpawnBatch::new(system(), Mat4::from_translation(Vec3::new(0.0, 1.0, 0.0)))
                };
                for g in 0..8 {
                    let start = evaluate(&d, &batch, g, Seconds(0.0), 1.0).unwrap();
                    // The initial velocity, as D3 defines it, recovered from the zero-age state.
                    let v0 = start.velocity.as_dvec3();
                    let u = |k| f64::from(particle_random(batch.seed, g, k));
                    let side = DVec3::new((TAU * u(Stream::SIDE)).cos(), 0.0, (TAU * u(Stream::SIDE)).sin());
                    let sway0 = flutter
                        .map_or(DVec3::ZERO, |f| side * (f64::from(f.amplitude_m) * (TAU * u(Stream::PHASE)).sin()));
                    let p0 = start.position.as_dvec3() - sway0;
                    let at = evaluate(&d, &batch, g, Seconds(10.0), 1.0).unwrap();
                    let want = integrate(&p, p0, v0, Some((u(Stream::PHASE), side)), 10.0);
                    let err = (at.position.as_dvec3() - want).length();
                    worst = worst.max(err);
                    assert!(err < 1e-4, "drag {drag} wind {wind:?} flutter {flutter:?} particle {g}: off by {err} m");
                }
            }
        }
    }
    println!("worst error over 10 s: {worst:.2e} m");
}

#[test]
fn particle_random_is_pinned() {
    // Golden values from an independent implementation of D3's formula: changing the function changes every golden.
    let pinned: [((u32, u32, u32), u32); 10] = [
        ((0, 0, 0), 8_379_828),
        ((0, 1, 0), 2_322_454),
        ((1, 0, 0), 13_339_483),
        ((0, 0, 1), 8_057_440),
        ((7, 42, 16), 265_819),
        ((0xDEAD_BEEF, 1000, 5), 6_409_127),
        ((123_456, 19_999, 11), 7_190_425),
        ((u32::MAX, u32::MAX, 255), 5_826_454),
        ((42, 7, 12), 1_487_121),
        ((2024, 365, 8), 4_261_710),
    ];
    for ((seed, g, k), want) in pinned {
        let u = particle_random(seed, g, k);
        assert_eq!((u * 16_777_216.0) as u32, want, "particle_random({seed}, {g}, {k})");
        assert!((0.0..1.0).contains(&u));
    }
}

#[test]
fn particle_random_is_uniform_on_every_stream() {
    // Chi-square over 64 buckets, 100,000 draws per stream; 63 degrees of freedom, p = 0.001 at about 103.4.
    for stream in 0..=Stream::CELL {
        let mut buckets = [0u32; 64];
        for g in 0..100_000 {
            buckets[(particle_random(0x5EED, g, stream) * 64.0) as usize] += 1;
        }
        let expected = 100_000.0 / 64.0;
        let chi: f64 = buckets.iter().map(|&n| (f64::from(n) - expected).powi(2) / expected).sum();
        assert!(chi < 103.4, "stream {stream}: chi-square {chi:.1}");
    }
}

#[test]
fn onb_is_orthonormal_everywhere_including_the_poles() {
    let mut normals: Vec<Vec3> =
        (0..10_000).map(|i| sphere_dir(particle_random(1, i, 0), particle_random(1, i, 1))).collect();
    normals.extend([Vec3::Z, -Vec3::Z, Vec3::X, -Vec3::Y, Vec3::new(0.0, 1e-7, -1.0).normalize()]);
    for n in normals {
        let (t1, t2) = onb(n);
        for (a, b) in [(t1, t2), (t1, n), (t2, n)] {
            assert!(a.dot(b).abs() < 1e-6, "{n}: not orthogonal");
        }
        for v in [t1, t2] {
            assert!((v.length() - 1.0).abs() < 1e-6, "{n}: not unit");
        }
        assert!((t1.cross(t2) - n).length() < 1e-5, "{n}: right-handed");
    }
}

#[test]
fn life_density_floor_and_index_bounds_decide_what_exists() {
    let d = desc(ParticleParams { lifetime: Span::fixed(2.0), floor: Some(-5.0), ..params() });
    let batch = SpawnBatch {
        first: 10,
        count: 5,
        interval: 0.5,
        start: Seconds(100.0),
        ..SpawnBatch::new(system(), Mat4::IDENTITY)
    };
    assert!(evaluate(&d, &batch, 9, Seconds(101.0), 1.0).is_none(), "before first");
    assert!(evaluate(&d, &batch, 15, Seconds(101.0), 1.0).is_none(), "after first + count");
    // Particle 12 is born at 101.0 and lives 2 s.
    assert!(evaluate(&d, &batch, 12, Seconds(100.99), 1.0).is_none(), "unborn");
    assert_eq!(evaluate(&d, &batch, 12, Seconds(101.0), 1.0).unwrap().age, 0.0);
    assert!(evaluate(&d, &batch, 12, Seconds(103.01), 1.0).is_none(), "dead");
    assert!(evaluate(&d, &batch, 12, Seconds(102.0), 0.0).is_none(), "density 0 keeps nothing");
    // Thinning keeps the same particles at every step: whoever survives 0.25 survives 0.5.
    let many = SpawnBatch { count: 2000, ..SpawnBatch::new(system(), Mat4::IDENTITY) };
    let kept =
        |density| (0..2000).filter(|&g| evaluate(&d, &many, g, Seconds(0.5), density).is_some()).collect::<Vec<_>>();
    let (quarter, half) = (kept(0.25), kept(0.5));
    assert!(quarter.iter().all(|g| half.contains(g)));
    assert!((900..1100).contains(&half.len()), "{}", half.len());
    // The floor hides what fell through it.
    let falling =
        desc(ParticleParams { lifetime: Span::fixed(10.0), speed: Span::fixed(0.0), floor: Some(-1.0), ..params() });
    let one = SpawnBatch::new(system(), Mat4::IDENTITY);
    assert!(evaluate(&falling, &one, 0, Seconds(0.5), 1.0).is_some());
    assert!(evaluate(&falling, &one, 0, Seconds(1.0), 1.0).is_none(), "1.5 m below at t = 1");
}

#[test]
fn a_pruned_batch_evaluates_its_survivors_as_before() {
    // The emission log drops a stream's dead prefix by advancing `first` and `start` together (D12).
    let d = desc(params());
    let stream = SpawnBatch { interval: 0.01, count: 1000, seed: 5, ..SpawnBatch::new(system(), Mat4::IDENTITY) };
    let pruned = SpawnBatch { first: 400, count: 600, start: stream.birth(400), ..stream };
    for g in [400, 555, 900] {
        let (a, b) = (
            evaluate(&d, &stream, g, Seconds(9.5), 1.0).unwrap(),
            evaluate(&d, &pruned, g, Seconds(9.5), 1.0).unwrap(),
        );
        assert!((a.position - b.position).length() < 1e-4 && (a.age - b.age).abs() < 1e-5, "{g}");
    }
}

#[test]
fn orientation_size_colour_and_cells_follow_d3() {
    let mut p = params();
    p.size = Span::fixed(0.1);
    p.size_curve = [0.0, 1.0, 1.0, 0.0];
    p.lifetime = Span::fixed(3.0);
    p.colours = ColourSet::new(&[Colour::rgb(255, 0, 0), Colour::rgb(0, 0, 255)]).unwrap();
    p.orientation = Orientation::tumble(Span::new(90.0, 180.0));
    let d = ParticleSystemDesc { sprite: Sprite::Shape(SpriteShape::rect(0.5)), params: p.clone() };
    let batch = SpawnBatch { count: 200, scale: 2.0, ..SpawnBatch::new(system(), Mat4::IDENTITY) };
    let mut reds = 0;
    for g in 0..200 {
        let s = evaluate(&d, &batch, g, Seconds(1.0), 1.0).unwrap();
        assert!((s.size - 0.2).abs() < 1e-6, "size × curve(1/3) × scale");
        assert!(
            (s.normal.length() - 1.0).abs() < 1e-5
                && s.normal.dot(s.edges[0]).abs() < 1e-6
                && s.normal.dot(s.edges[1]).abs() < 1e-6
        );
        assert!((s.edges[0].length() - 0.1).abs() < 1e-5 && (s.edges[1].length() - 0.2).abs() < 1e-5, "aspect 0.5");
        reds += usize::from(s.colour.x > 0.5);
    }
    assert!((60..140).contains(&reds), "two colours share the particles: {reds}");
    let born = evaluate(&d, &batch, 0, Seconds(0.0), 1.0).unwrap();
    assert_eq!(born.size, 0.0, "the curve pops particles in");

    p.orientation = Orientation::velocity(0.5);
    let streak = ParticleSystemDesc { sprite: Sprite::Shape(SpriteShape::Streak), params: p.clone() };
    let s = evaluate(&streak, &batch, 3, Seconds(1.0), 1.0).unwrap();
    assert!((s.extent.y - (s.size + 0.5 * s.velocity.length())).abs() < 1e-5 && s.extent.x == s.size);

    let atlas = ParticleSystemDesc { sprite: Sprite::atlas(TextureId::from_parts(0, 1), 4, 2), params: params() };
    let listed = SpawnBatch { count: 300, cells: CellPick::list(&[5, 6]).unwrap(), ..batch };
    let all = SpawnBatch { cells: CellPick::All, ..listed };
    let cells =
        |b: &SpawnBatch| (0..300).map(|g| evaluate(&atlas, b, g, Seconds(1.0), 1.0).unwrap().cell).collect::<Vec<_>>();
    assert!(
        cells(&listed).iter().all(|c| [5, 6].contains(c)) && cells(&listed).contains(&5) && cells(&listed).contains(&6)
    );
    let every = cells(&all);
    assert!((0..8).all(|c| every.contains(&c)) && every.iter().all(|&c| c < 8));
}

#[test]
fn every_emitter_shape_keeps_its_births_inside_it() {
    let d = desc(ParticleParams { speed: Span::fixed(0.0), gravity: [0.0; 3], ..params() });
    let at = Mat4::from_translation(Vec3::new(1.0, 2.0, 3.0));
    for shape in [
        EmitterShape::Point,
        EmitterShape::sphere(0.5),
        EmitterShape::cuboid(Vec3::new(0.1, 0.2, 0.3)),
        EmitterShape::rect(Vec2::new(2.0, 1.0)),
    ] {
        let b = SpawnBatch { shape, count: 500, ..SpawnBatch::new(system(), at) };
        for g in 0..500 {
            let p = evaluate(&d, &b, g, Seconds(0.0), 1.0).unwrap().position - Vec3::new(1.0, 2.0, 3.0);
            let inside = match shape {
                EmitterShape::Point => p.length() < 1e-6,
                EmitterShape::Sphere { radius, .. } => p.length() <= radius + 1e-5,
                EmitterShape::Box { half, .. } => (p.abs() - half).max_element() <= 1e-5,
                EmitterShape::Rect { half, .. } => {
                    p.x.abs() <= half.x + 1e-5 && p.y.abs() < 1e-6 && p.z.abs() <= half.y + 1e-5
                }
            };
            assert!(inside, "{shape:?}: {p}");
        }
    }
}

fn refused(d: &ParticleSystemDesc, needle: &str) {
    match d.validate() {
        Err(RenderError::Invalid { reason, .. }) => assert!(reason.contains(needle), "{needle:?} not in {reason:?}"),
        other => panic!("expected Invalid with {needle:?}, got {other:?}"),
    }
}

#[test]
fn create_refuses_each_rule_of_d2() {
    let ok = desc(params());
    assert_eq!(ok.validate(), Ok(()));
    let with = |f: fn(&mut ParticleParams)| {
        let mut p = params();
        f(&mut p);
        desc(p)
    };
    refused(&with(|p| p.speed = Span::new(f32::NAN, 1.0)), "speed");
    refused(&with(|p| p.size = Span::new(0.2, 0.1)), "size");
    refused(&with(|p| p.lifetime = Span::new(0.0, 1.0)), "lifetime");
    refused(&with(|p| p.lifetime = Span::new(1.0, 21.0)), "lifetime");
    refused(&with(|p| p.speed = Span::new(-1.0, 1.0)), "speed");
    refused(&with(|p| p.direction = [0.0; 3]), "direction");
    refused(&with(|p| p.spread_deg = 181.0), "spread_deg");
    refused(&with(|p| p.gravity = [0.0, f32::INFINITY, 0.0]), "gravity");
    refused(&with(|p| p.drag = -1.0), "drag");
    refused(&with(|p| p.flutter = Some(Flutter { amplitude_m: 2.0, frequency_hz: 1.0 })), "flutter");
    refused(&with(|p| p.size_curve = [1.0, -0.5, 1.0, 1.0]), "size_curve");
    refused(&with(|p| p.orientation = Orientation::velocity(-1.0)), "stretch");
    refused(&with(|p| p.coverage = 1.5), "coverage");
    refused(&with(|p| p.shading = ParticleShading::toon(Colour::WHITE, 2.0, 0.1)), "toon");
    refused(&with(|p| p.edge = Some(Edge { shade: 0.5, width_px: 9.0 })), "edge");
    refused(&with(|p| p.glow = -0.1), "glow");
    refused(&with(|p| p.floor = Some(f32::NAN)), "floor");
    refused(&ParticleSystemDesc { sprite: Sprite::Shape(SpriteShape::Streak), params: params() }, "streak");
    refused(&ParticleSystemDesc { sprite: Sprite::Shape(SpriteShape::rect(20.0)), params: params() }, "aspect");
    refused(&ParticleSystemDesc { sprite: Sprite::Shape(SpriteShape::star(3)), params: params() }, "points");
    let tex = TextureId::from_parts(0, 1);
    refused(&ParticleSystemDesc { sprite: Sprite::atlas(tex, 0, 4), params: params() }, "cells");
    refused(&ParticleSystemDesc { sprite: Sprite::atlas(tex, 128, 64), params: params() }, "cells");
    refused(
        &ParticleSystemDesc {
            sprite: Sprite::atlas(tex, 2, 2),
            params: ParticleParams { edge: Some(Edge { shade: 0.5, width_px: 1.0 }), ..params() },
        },
        "edge",
    );
    assert!(ColourSet::new(&[]).is_none(), "an empty colour set can't be built");
}

#[test]
fn scene_validate_refuses_each_batch_rule_and_the_limits() {
    let mut scene = Scene::new(Camera::default());
    let ok = SpawnBatch::new(system(), Mat4::IDENTITY);
    scene.particles.push(ok);
    assert_eq!(scene.validate(), Ok(()));
    let cases: [(&str, fn(&mut SpawnBatch)); 7] = [
        ("emitter", |b| b.emitter = Mat4::from_translation(Vec3::NAN)),
        ("velocity", |b| b.velocity = Vec3::new(f32::INFINITY, 0.0, 0.0)),
        ("interval", |b| b.interval = -0.1),
        ("scale", |b| b.scale = 0.0),
        ("shape", |b| b.shape = EmitterShape::sphere(-1.0)),
        ("overflows", |b| (b.first, b.count) = (u32::MAX, 2)),
        ("start", |b| b.start = Seconds(f64::NAN)),
    ];
    for (needle, break_it) in cases {
        let mut b = ok;
        break_it(&mut b);
        scene.particles[0] = b;
        let err = scene.validate().unwrap_err().to_string();
        assert!(err.contains(needle), "{needle:?} not in {err:?}");
    }
    scene.particles.clear();
    scene.particles.resize(limits::MAX_SPAWN_BATCHES + 1, ok);
    assert!(matches!(scene.validate(), Err(RenderError::LimitExceeded { what: "spawn batches in a scene", .. })));
    scene.particles.clear();
    scene.particles.push(SpawnBatch { count: limits::MAX_PARTICLES + 1, ..ok });
    assert!(matches!(scene.validate(), Err(RenderError::LimitExceeded { what: "particles in a scene", .. })));
    scene.particles[0].count = limits::MAX_PARTICLES;
    assert_eq!(scene.validate(), Ok(()), "exactly the cap is fine");
}

#[test]
fn clear_keeps_the_particle_capacity() {
    let mut scene = Scene::new(Camera::default());
    scene.particles.resize(1024, SpawnBatch::new(system(), Mat4::IDENTITY));
    let cap = scene.particles.capacity();
    scene.clear();
    assert_eq!(scene.particles.len(), 0);
    assert_eq!(scene.particles.capacity(), cap);
}

#[test]
fn presets_parse_from_the_designs_toml_shape() {
    // §3's confetti `[particles]` table, as JSON.
    let p: ParticleParams = serde_json::from_value(serde_json::json!({
        "lifetime": [3.5, 5.0], "speed": [3.0, 5.0], "direction": [0.0, 1.0, 0.0], "spread_deg": 35.0,
        "gravity": [0.0, -3.0, 0.0], "drag": 3.0, "wind": [0.0, 0.0, 0.0],
        "flutter": { "amplitude_m": 0.05, "frequency_hz": 1.5 }, "size": [0.012, 0.018],
        "size_curve": [0.4, 1.0, 1.0, 0.0], "orientation": { "kind": "tumble", "rate_deg": [360.0, 900.0] },
        "colours": ["#ff5a7a", "#ffc94d", "#5ad1ff", "#9b7bff", "#6ee7a0", "#ffffff"], "coverage": 1.0,
        "shading": { "kind": "toon", "shade": "#b8a8c8", "shift": 0.0, "softness": 0.1 }, "glow": 0.0, "floor": 0.0
    }))
    .unwrap();
    assert_eq!(p.validate(), Ok(()));
    assert_eq!(p.colours.len(), 6);
    let back: ParticleParams = serde_json::from_value(serde_json::to_value(&p).unwrap()).unwrap();
    assert_eq!(back, p, "round-trips");
    let shape: SpriteShape = serde_json::from_value(serde_json::json!({ "shape": "rect", "aspect": 0.6 })).unwrap();
    assert_eq!(shape, SpriteShape::rect(0.6));
    let typo = serde_json::json!({ "lifetime": [1.0, 2.0], "sped": [1.0, 1.0] });
    assert!(serde_json::from_value::<ParticleParams>(typo).is_err(), "unknown keys are refused");
    // §3's worked example: confetti at 5 m/s up with drag 3 and terminal -1 m/s peaks about 1.07 m up at ln 6 / 3 s.
    let up = ParticleParams { speed: Span::fixed(5.0), spread_deg: 0.0, flutter: None, ..p };
    let d = desc(up);
    let b = SpawnBatch::new(system(), Mat4::IDENTITY);
    let peak = evaluate(&d, &b, 0, Seconds(f64::from(6.0f32.ln() / 3.0)), 1.0).unwrap();
    assert!(peak.velocity.y.abs() < 1e-4 && (peak.position.y - (5.0 / 3.0 - 6.0f32.ln() / 3.0)).abs() < 1e-4, "{peak:?}");
}
