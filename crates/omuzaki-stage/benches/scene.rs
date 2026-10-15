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
use omuzaki_stage::scene::resolve::{CheckedScene, Figure, ResolvedScene};
use omuzaki_stage_api::{
    Ambient, Anchor, BloomDef, CameraDef, CameraId, CastMember, Framing, LightDef, LightId, LightShape, MemberId,
    SceneDef, SceneSettings,
};

const ASPECT: f32 = 16.0 / 9.0;

fn light(
    name: &str,
    rgb: [u8; 3],
    intensity: f32,
    shadow: bool,
    angles: [f32; 2],
    anchor: Anchor,
) -> (LightId, LightDef) {
    let def = LightDef {
        role: None,
        colour: Colour::rgb(rgb[0], rgb[1], rgb[2]),
        intensity,
        shadow,
        anchor,
        shape: LightShape::Directional { azimuth: angles[0], elevation: angles[1] },
    };
    (LightId::new(name).unwrap(), def)
}

/// The default scene as D11 describes it (`default_scene` isn't built yet): one member, the studio rig's lighting and
/// bloom, the `full`, `bust` and `face` cameras opening on `full`, a shadow ground and ACES.
fn scene() -> SceneDef {
    let mut def = SceneDef::new(SceneSettings::named("Main"));
    def.scene.camera = Some(CameraId::new("full").unwrap());
    def.cast.insert(MemberId::new("kiri").unwrap(), CastMember::new(CharId::new("t").unwrap()));
    let full = CameraDef { yaw: 30.0, pitch: 6.0, ..CameraDef::preset(Framing::Full) };
    for (name, camera) in
        [("full", full), ("bust", CameraDef::preset(Framing::Bust)), ("face", CameraDef::preset(Framing::Face))]
    {
        def.cameras.insert(CameraId::new(name).unwrap(), camera);
    }
    let l = &mut def.lighting;
    l.rig = Some("viewer-studio".into());
    l.exposure = 0.9;
    l.background = Colour::rgb(0x4a, 0x3b, 0x31);
    l.skin_floor = Some(0.48);
    l.ambient = Ambient { sky: Colour::rgb(0xf1, 0xee, 0xf8), ground: Colour::rgb(0x4a, 0x3e, 0x36), intensity: 0.75 };
    l.bloom = Some(BloomDef { strength: 0.32, radius: 0.5, threshold: 0.92 });
    for (name, rgb, intensity, shadow, angles, anchor) in [
        ("key", [0xff, 0xf6, 0xec], 1.7, true, [33.7, 46.1], Anchor::Subject),
        ("fill", [0xff, 0xf0, 0xe0], 0.45, false, [-50.2, 21.0], Anchor::Subject),
        ("rim", [0xdf, 0xe6, 0xff], 0.8, false, [-138.7, 31.0], Anchor::Stage),
    ] {
        let (id, light) = light(name, rgb, intensity, shadow, angles, anchor);
        l.lights.insert(id, light);
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
    let checked = CheckedScene::new(&def).expect("the scene passes its check");
    out.resolve_into(checked, &figures, ASPECT).expect("the scene resolves");
    c.bench_function("resolve/unchanged", |b| {
        b.iter(|| black_box(out.resolve_into(black_box(checked), &figures, ASPECT).expect("resolves")));
    });

    let front = CameraId::new("full").unwrap();
    let (mut a, mut b_def) = (scene(), scene());
    a.cameras.get_mut(&front).unwrap().yaw = 30.0;
    b_def.cameras.get_mut(&front).unwrap().yaw = 35.0;
    let proofs =
        [CheckedScene::new(&a).expect("passes its check"), CheckedScene::new(&b_def).expect("passes its check")];
    let mut flip = false;
    c.bench_function("resolve/camera_yaw", |b| {
        b.iter(|| {
            flip = !flip;
            let proof = proofs[usize::from(flip)];
            black_box(out.resolve_into(black_box(proof), &figures, ASPECT).expect("resolves"))
        });
    });
}

criterion_group!(benches, resolve);
criterion_main!(benches);
