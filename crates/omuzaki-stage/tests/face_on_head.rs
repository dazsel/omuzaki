//! The painted face on the kit character's head, through the stage and the wgpu renderer on the software adapter: the
//! character `character.create` makes (the standard template with its body, head and hair, and the built-in expression
//! library), framed on the face. An expression changes the face on the head, and the face's Overlay layer (the sweat
//! drop, the gloom's hatch), which sits in front of the hair, reaches it too.
//!
//! With `OMUZAKI_STAGE_PNG` set, it also writes one still per expression of a contact set (`head-<name>.png`), the
//! visual evidence of the face spine.
#![cfg(feature = "wgpu")]
// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "the evidence test writes its stills into the folder CI gives it")]

use std::collections::BTreeMap;

use omuzaki_char::template::registry;
use omuzaki_char_api::{
    BodyTemplateId, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, PartDef,
    PartId, PartKindId, RegistryBuilder, find_kind,
};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::UVec2;
use omuzaki_face_api::{ExpressionId, ExpressionSet, Palette, PaletteSlot};
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_render_api::{Image, Quality};
use omuzaki_stage::ops::render::still::{Shot, StillLight, StillSpec, encode_png, render_stills};
use omuzaki_stage::recipe::LightRig;
use omuzaki_stage::scene::frame::frame;
use omuzaki_stage::scene::landmarks::Landmarks;
use omuzaki_stage_api::{CameraDef, Framing};

/// A part of `kind` with `params` normalised through its kind, as `character.create` makes it.
fn part(kind: &str, params: &serde_json::Value) -> PartDef {
    let params = (find_kind(kind).expect("a registered kind").check)(params).expect("the kind takes its defaults");
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer: 0, attach: None, params }
}

/// The kit character: the standard template's body, head and hair, the starting skin, and the expression library.
fn kit() -> BuiltCharacter {
    omuzaki_stage::link();
    let defaults = registry::find("standard").expect("the standard template").defaults();
    let mut face = defaults.face.clone();
    face.expressions = ExpressionSet::library().expect("the library reads");
    let parts: BTreeMap<PartId, PartDef> = [
        ("body", part("body", &serde_json::to_value(&defaults.body).unwrap())),
        ("head", part("head", &serde_json::to_value(&defaults.head).unwrap())),
        ("hair", part("hair", &(find_kind("hair").unwrap().defaults)())),
    ]
    .into_iter()
    .map(|(id, part)| (PartId::new(id).unwrap(), part))
    .collect();
    let skin = [
        ("skin", Colour::rgb(0xf9, 0xd3, 0xc5)),
        ("skin_shade", Colour::rgb(0xf2, 0xb4, 0xab)),
        ("skin_line", Colour::rgb(0xc9, 0x8a, 0x80)),
    ];
    let palette = Palette(skin.into_iter().map(|(slot, colour)| (PaletteSlot::new(slot).unwrap(), colour)).collect());
    let def = CharacterDef::new("Kit".to_owned(), BodyTemplateId::new("standard").unwrap())
        .with_seed(1)
        .with_palette(palette)
        .with_skin_tone(None)
        .with_parts(parts)
        .with_face(face)
        .with_weight_brushes(BTreeMap::new());
    RegistryBuilder
        .build(&CharId::new("kit").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .expect("the kit character builds")
}

/// One face-framed still of `kit` per name (`rest` is the neutral face), `side` pixels square.
fn stills(names: &[&str], side: u32) -> Vec<Image> {
    let built = kit();
    let landmarks = Landmarks::measure(&built).unwrap();
    let camera = frame(Framing::Face, &landmarks, &CameraDef::preset(Framing::Face), 1.0).unwrap();
    let rig = LightRig::load("viewer-studio").unwrap();
    let light = StillLight::new(rig.to_lights().unwrap(), rig.background(), rig.post(false));
    let spec = StillSpec::new(light, UVec2::splat(side), Quality::offline());
    let shots: Vec<Shot> = names
        .iter()
        .map(|name| match *name {
            "rest" => Shot::new(camera),
            name => Shot::new(camera).with_expression(ExpressionId::new(name).unwrap(), 1.0),
        })
        .collect();
    let mut renderer = WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software })
        .expect("a software adapter (install mesa-vulkan-drivers)");
    render_stills(&mut renderer, built, &spec, &shots).unwrap().into_iter().map(|still| still.image).collect()
}

/// Pixels whose largest channel differs by more than 8 levels (a light blue drop on pale skin is a small difference).
fn changed(a: &Image, b: &Image) -> usize {
    a.rgba8.chunks(4).zip(b.rgba8.chunks(4)).filter(|(p, q)| p.iter().zip(*q).any(|(x, y)| x.abs_diff(*y) > 8)).count()
}

#[test]
fn an_expression_and_the_faces_overlay_reach_the_kit_head() {
    let [rest, happy, sweat, again]: [Image; 4] =
        stills(&["rest", "happy", "sweat", "rest"], 256).try_into().expect("four stills");
    assert_eq!(changed(&rest, &again), 0, "a still depends only on its shot");
    // Happy closes the eyes to arcs, blushes and smiles: a large part of a face-framed still changes.
    let happy = changed(&rest, &happy);
    assert!(happy > 1000, "happy changed {happy} pixels of the head");
    // The sweat drop is painted on the Overlay layer only; it shows on the head (or the hair in front of it).
    let sweat = changed(&rest, &sweat);
    assert!(sweat > 40, "the sweat drop changed {sweat} pixels: the Overlay layer doesn't reach the head");
}

#[test]
fn render_the_expressions_on_the_head_when_asked() {
    let Some(dir) = std::env::var_os("OMUZAKI_STAGE_PNG") else {
        return;
    };
    let names = ["rest", "happy", "angry", "sad", "surprised", "smug", "shy", "wink", "blink", "aa", "hearts", "sweat"];
    for (name, image) in names.iter().zip(stills(&names, 512)) {
        std::fs::write(std::path::Path::new(&dir).join(format!("head-{name}.png")), encode_png(&image).unwrap())
            .unwrap();
    }
}
