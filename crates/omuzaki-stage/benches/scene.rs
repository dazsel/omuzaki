//! Benchmark: scene resolve (design D10 budgets it at 0.2 ms or less) and, with #274, the per-frame scene fill.

// Bench helpers may panic: a panic is a clear failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "../tests/scene/templates.rs"]
mod templates;

use std::collections::BTreeMap;
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_char_api::CharId;
use omuzaki_render_api::Colour;
use omuzaki_stage::scene::landmarks::Landmarks;
use omuzaki_stage::scene::resolve::{Figure, ResolvedScene};
use omuzaki_stage_api::{
    Anchor, CameraDef, CameraId, CastMember, Framing, LightDef, LightId, LightShape, MemberId, SceneDef, SceneSettings,
};

const ASPECT: f32 = 16.0 / 9.0;

fn light(name: &str, azimuth: f32, elevation: f32, anchor: Anchor) -> (LightId, LightDef) {
    let def = LightDef {
        role: None,
        colour: Colour::WHITE,
        intensity: 1.0,
        shadow: false,
        anchor,
        shape: LightShape::Directional { azimuth, elevation },
    };
    (LightId::new(name).unwrap(), def)
}

/// One member, D11's three cameras and the default lighting with three lights.
fn scene() -> SceneDef {
    let mut def = SceneDef::new(SceneSettings::named("Bench"));
    def.cast.insert(MemberId::new("kiri").unwrap(), CastMember::new(CharId::new("t").unwrap()));
    let full = CameraDef { yaw: 30.0, pitch: 6.0, ..CameraDef::preset(Framing::Full) };
    for (name, camera) in
        [("full", full), ("bust", CameraDef::preset(Framing::Bust)), ("face", CameraDef::preset(Framing::Face))]
    {
        def.cameras.insert(CameraId::new(name).unwrap(), camera);
    }
    for (name, azimuth, elevation, anchor) in [
        ("key", 33.7, 46.1, Anchor::Subject),
        ("fill", -50.2, 21.0, Anchor::Subject),
        ("rim", -138.7, 31.0, Anchor::Stage),
    ] {
        let (id, light) = light(name, azimuth, elevation, anchor);
        def.lighting.lights.insert(id, light);
    }
    def
}

fn resolve(c: &mut Criterion) {
    // The mock character has no eye joints, which a face shot needs, so the figure is the standard template.
    let landmarks = Landmarks::measure(&templates::build("standard")).expect("the template measures");
    let figure = Figure::new(&CastMember::new(CharId::new("t").unwrap()), landmarks);
    let figures = BTreeMap::from([(MemberId::new("kiri").unwrap(), figure)]);

    let def = scene();
    let mut out = ResolvedScene::new();
    out.resolve_into(&def, &figures, ASPECT).expect("the scene resolves");
    c.bench_function("resolve/unchanged", |b| {
        b.iter(|| black_box(out.resolve_into(black_box(&def), &figures, ASPECT).expect("resolves")));
    });

    let front = CameraId::new("full").unwrap();
    let (mut a, mut b_def) = (scene(), scene());
    a.cameras.get_mut(&front).unwrap().yaw = 30.0;
    b_def.cameras.get_mut(&front).unwrap().yaw = 35.0;
    let mut flip = false;
    c.bench_function("resolve/camera_yaw", |b| {
        b.iter(|| {
            flip = !flip;
            let def = if flip { &a } else { &b_def };
            black_box(out.resolve_into(black_box(def), &figures, ASPECT).expect("resolves"))
        });
    });
}

criterion_group!(benches, resolve);
criterion_main!(benches);
