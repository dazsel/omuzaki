//! Painting on a character's parts (#2575), end to end on the in-memory runner with the mock character's kinds: a
//! texture layer made on the shirt, strokes, fills and erasing on it, each one undoable step; the build turns the
//! painting into a texture drawn over every material of the shirt (and only the shirt), first among its overlays; and
//! the refusals an agent can act on.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_char_api::{
    BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, RegistryBuilder, TextureName,
    paths,
};
use omuzaki_doc_api::{DocPath, DocRead};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, Grants, Mode, OpError, Request, Runner};
use omuzaki_ops_mock::MockRunner;
use omuzaki_render_api::{Mapping, TextureId};
use serde_json::{Value, json};

struct Rig {
    runner: MockRunner<MemDoc>,
}

fn person() -> Actor {
    Actor::Person { name: "tester".into() }
}

fn mock() -> CharId {
    CharId::new("mock").unwrap()
}

impl Rig {
    /// The mock character (`body`, `head`, `hair`, `shirt`, `vest`) stored as `characters/mock`.
    fn new() -> Self {
        omuzaki_paint_ops::link();
        omuzaki_char_mock::link();
        let mut node = json!({});
        omuzaki_char_mock::mock_character().unwrap().to_node(&mut node).unwrap();
        Self { runner: MockRunner::new(MemDoc::from_value(json!({ "characters": { "mock": node } }))) }
    }

    fn run(&self, op: &str, args: Value) -> Result<Value, OpError> {
        let request = Request {
            op: op.into(),
            args,
            mode: Mode::Apply,
            actor: person(),
            grants: Grants::person(),
            cause: None,
            tx: None,
            base: None,
        };
        Ok(self.runner.run(request)?.plan.output.unwrap_or(Value::Null))
    }

    fn undo(&self) {
        assert!(self.runner.undo(&person(), &Grants::person()).unwrap().is_some(), "there is a step to undo");
    }

    fn character(&self) -> CharacterDef {
        let node = self.runner.get(&paths::character(&mock()), &Grants::person()).unwrap().expect("stored");
        CharacterDef::from_node(&node).unwrap()
    }

    fn build(&self) -> BuiltCharacter {
        RegistryBuilder
            .build(&mock(), &self.character(), &BuildOptions::default(), &BuildServices::default())
            .unwrap_or_else(|e| panic!("the mock character doesn't build: {e}"))
    }

    fn design(&self) -> Option<Value> {
        self.runner.get(&paths::design(&mock()), &Grants::person()).unwrap()
    }
}

/// The premultiplied RGBA of texel `(x, y)`.
fn texel(built: &BuiltCharacter, name: &str, x: usize, y: usize) -> [u8; 4] {
    let t = &built.textures[&TextureName::new(name).unwrap()];
    let i = (y * t.desc.size.x as usize + x) * 4;
    t.rgba8[i..i + 4].try_into().unwrap()
}

/// The materials the meshes of `part` draw with.
fn materials_of(built: &BuiltCharacter, part: &str) -> Vec<omuzaki_char_api::MaterialName> {
    let mut names: Vec<_> =
        built.meshes.iter().filter(|m| m.part.as_str() == part).map(|m| m.mesh.material.clone()).collect();
    names.sort();
    names.dedup();
    names
}

#[test]
fn a_stroke_on_the_shirt_paints_a_texture_over_its_materials_and_undoes_in_one_step() {
    let rig = Rig::new();
    let made =
        rig.run("part.paint.new", json!({ "character": "mock", "part": "shirt", "width": 64, "height": 32 })).unwrap();
    assert_eq!(made["layer"], "paint");
    let drawn = rig
        .run(
            "part.paint.stroke",
            json!({ "character": "mock", "part": "shirt", "strokes": [
                { "brush": "hard_round", "colour": "#ff0000", "size": 8, "points": [[0, 16], [64, 16]], "tag": "stripe" }
            ] }),
        )
        .unwrap();
    assert_eq!((drawn["paint_layer"].as_str(), drawn["total"].as_u64()), (Some("paint"), Some(1)));

    let built = rig.build();
    let stripe = texel(&built, "paint-shirt", 32, 16);
    assert_eq!(stripe, [255, 0, 0, 255], "the stripe is painted, opaque red");
    assert_eq!(texel(&built, "paint-shirt", 32, 2), [0, 0, 0, 0], "above it the surface shows through");
    let shirt = materials_of(&built, "shirt");
    assert!(!shirt.is_empty(), "the shirt has materials");
    for (name, spec) in &built.materials {
        let painted = spec.painted.as_ref().map(TextureName::as_str);
        if shirt.contains(name) {
            assert_eq!(painted, Some("paint-shirt"), "{name} is the shirt's");
            let texture = TextureId::from_parts(7, 1);
            let material = spec
                .to_material(
                    &|t| (t.as_str() == "paint-shirt").then_some(texture).or(Some(TextureId::from_parts(1, 1))),
                    &|_| Some(TextureId::from_parts(2, 1)),
                )
                .unwrap();
            assert_eq!((material.overlays[0].texture, material.overlays[0].mapping), (texture, Mapping::Uv));
            assert!(material.validate().is_ok(), "{name} is a valid material");
        } else {
            assert_eq!(painted, None, "{name} isn't the shirt's");
        }
    }

    rig.undo();
    let built = rig.build();
    assert_eq!(texel(&built, "paint-shirt", 32, 16), [0, 0, 0, 0], "undo took the stroke off");
    rig.undo();
    assert!(rig.design().is_none(), "undo took the texture layer off, and the design with it");
    assert!(!rig.build().textures.keys().any(|t| t.as_str() == "paint-shirt"));
}

#[test]
fn fill_erase_opacity_and_clear_change_the_painting_as_they_say() {
    let rig = Rig::new();
    rig.run("part.paint.new", json!({ "character": "mock", "part": "shirt", "width": 32, "height": 32 })).unwrap();
    rig.run("part.paint.fill", json!({ "character": "mock", "part": "shirt", "fill": "#0000ff" })).unwrap();
    assert_eq!(texel(&rig.build(), "paint-shirt", 16, 16), [0, 0, 255, 255], "the fill covers it all");
    rig.run(
        "part.paint.erase",
        json!({ "character": "mock", "part": "shirt", "points": [[0, 16], [32, 16]], "size": 6 }),
    )
    .unwrap();
    let built = rig.build();
    assert_eq!(texel(&built, "paint-shirt", 16, 16)[3], 0, "erased along the line");
    assert_eq!(texel(&built, "paint-shirt", 16, 2), [0, 0, 255, 255], "and only there");
    rig.run(
        "part.paint.fill",
        json!({ "character": "mock", "part": "shirt", "region": { "kind": "rect", "x": 0, "y": 24, "w": 32, "h": 8 }, "fill": "#00ff00" }),
    )
    .unwrap();
    assert_eq!(texel(&rig.build(), "paint-shirt", 4, 28), [0, 255, 0, 255], "a region fill");

    rig.run("part.paint.opacity", json!({ "character": "mock", "part": "shirt", "opacity": 0.5 })).unwrap();
    let half = texel(&rig.build(), "paint-shirt", 16, 2);
    assert!((126..=129).contains(&half[3]) && half[2] == half[3], "half the texture layer shows: {half:?}");

    let got = rig.run("part.paint.get", json!({ "character": "mock", "part": "shirt" })).unwrap();
    assert_eq!((got["width"].as_u64(), got["opacity"].as_f64()), (Some(32), Some(0.5)));
    assert_eq!(got["layers"][0]["marks"], 3);

    let cleared = rig.run("part.paint.clear", json!({ "character": "mock", "part": "shirt" })).unwrap();
    assert_eq!(cleared["removed"], 3);
    assert_eq!(texel(&rig.build(), "paint-shirt", 16, 2), [0, 0, 0, 0], "cleared");
}

#[test]
fn each_mark_is_appended_in_place_rather_than_rewriting_the_design() {
    let rig = Rig::new();
    rig.run("part.paint.new", json!({ "character": "mock", "part": "shirt", "width": 16, "height": 16 })).unwrap();
    rig.run("part.paint.fill", json!({ "character": "mock", "part": "shirt", "fill": "#ffffff" })).unwrap();
    let record = rig.runner.records().pop().expect("the fill was recorded");
    let changes = &record.patch.changes;
    assert_eq!(changes.len(), 1);
    let path = changes[0].path.to_string();
    assert!(path.ends_with("/kind/painting/layers/paint/marks/0"), "one mark appended at {path}");
    let marks = DocPath::parse(&path).unwrap();
    assert!(
        MemDoc::from_value(rig.runner.get(&DocPath::root(), &Grants::person()).unwrap().unwrap()).get(&marks).is_some()
    );
}

#[test]
fn the_refusals_name_what_to_fix() {
    let rig = Rig::new();
    let code = |e: OpError| (e.field.unwrap_or_default(), e.message);
    let (field, _) = code(rig.run("part.paint.new", json!({ "character": "mock", "part": "cape" })).unwrap_err());
    assert_eq!(field, "part");
    let (field, _) = code(rig.run("part.paint.new", json!({ "character": "nobody", "part": "shirt" })).unwrap_err());
    assert_eq!(field, "character");
    let (field, _) =
        code(rig.run("part.paint.new", json!({ "character": "mock", "part": "shirt", "width": 5000 })).unwrap_err());
    assert_eq!(field, "width");
    let (field, message) =
        code(rig.run("part.paint.stroke", json!({ "character": "mock", "part": "shirt", "strokes": [] })).unwrap_err());
    assert_eq!(field, "layer", "{message}");

    rig.run("part.paint.new", json!({ "character": "mock", "part": "shirt", "width": 64, "height": 64 })).unwrap();
    let (field, _) = code(
        rig.run(
            "part.paint.new",
            json!({ "character": "mock", "part": "shirt", "layer": "more", "width": 32, "height": 32 }),
        )
        .unwrap_err(),
    );
    assert_eq!(field, "width", "every texture layer of a part is one size");
    let (field, _) = code(
        rig.run(
            "part.paint.fill",
            json!({ "character": "mock", "part": "shirt", "fill": "#ffffff", "paint_layer": "ink" }),
        )
        .unwrap_err(),
    );
    assert_eq!(field, "paint_layer");
    let (field, _) = code(
        rig.run("part.paint.opacity", json!({ "character": "mock", "part": "shirt", "opacity": 2.0 })).unwrap_err(),
    );
    assert_eq!(field, "opacity");
}
