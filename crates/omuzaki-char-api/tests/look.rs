//! The look through the real pipeline (LK-06): material classes collected by the assembler, `apply_look` on a built
//! character (non-cumulative, materials only, the change list the stage updates from), and the preset registry with
//! its did-you-mean.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use omuzaki_char_api::{
    BuildCtx, BuildError, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef,
    ClassLook, LookDef, LookError, LookParams, LookPreset, MaterialClass, MaterialName, MaterialSpec, MeshData,
    OutlineColour, PartCategory, PartKind, PartMesh, PartMeta, PartOutput, RegistryBuilder, SceneLook,
    find_look_preset, look_presets,
};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_face_api::{Check, Validate};
use omuzaki_render_api::{Outline, Shading, Skin, Toon};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct Params {
    /// Declare a class for a material the part doesn't build.
    stray_class: bool,
}

impl Validate for Params {
    fn check(&self, _: &mut Check) {}
}

fn name(s: &str) -> MaterialName {
    MaterialName::new(s).unwrap()
}

fn mesh(material: &str) -> PartMesh {
    PartMesh {
        name: material.to_owned(),
        geometry: MeshData {
            positions: vec![Vec3::ZERO, Vec3::X, Vec3::Y],
            normals: vec![Vec3::Z; 3],
            uvs: vec![Vec2::ZERO; 3],
            indices: vec![0, 1, 2],
            ..MeshData::default()
        },
        joint_bones: Vec::new(),
        material: name(material),
        casts_shadow: true,
        render_order: 0,
        dynamic: false,
    }
}

/// A toon material with a line, like the parts build: `skin` adds the skin treatment.
fn toon(base: u32, skin: bool) -> MaterialSpec {
    let mut spec = MaterialSpec::toon(Colour::hex(base));
    if let Shading::Toon(t) = &mut spec.shading {
        t.softness = 0.2;
        t.skin = skin.then(Skin::default);
    }
    spec.outline = Some(Outline { colour: Colour::hex(0x0020_1010), width: 0.0016 });
    spec
}

/// A body part: one skin material.
struct LookBody;

impl PartKind for LookBody {
    type Params = Params;

    fn meta() -> PartMeta {
        PartMeta { id: "look-body", summary: "A skin triangle.", category: PartCategory::Body, requires: &[] }
    }

    fn build(_: &Params, _: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        Ok(PartOutput {
            meshes: vec![mesh("look-skin")],
            materials: vec![(name("look-skin"), toon(0x00f9_d3c5, true))],
            ..PartOutput::default()
        })
    }
}
omuzaki_char_api::register_part_kind!(LookBody);

/// A garment with cloth and a gold button it declares metal.
struct LookCoat;

impl PartKind for LookCoat {
    type Params = Params;

    fn meta() -> PartMeta {
        PartMeta { id: "look-coat", summary: "Cloth and a button.", category: PartCategory::Garment, requires: &[] }
    }

    fn build(params: &Params, _: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let declared = if params.stray_class { "look-lining" } else { "look-button" };
        Ok(PartOutput {
            meshes: vec![mesh("look-cloth"), mesh("look-button")],
            materials: vec![
                (name("look-cloth"), toon(0x0023_2028, false)),
                (name("look-button"), toon(0x00dc_b66c, false)),
            ],
            material_classes: vec![(name(declared), MaterialClass::Metal)],
            ..PartOutput::default()
        })
    }
}
omuzaki_char_api::register_part_kind!(LookCoat);

/// A garment that defines a material no mesh of its own uses.
struct LookSpare;

impl PartKind for LookSpare {
    type Params = Params;

    fn meta() -> PartMeta {
        PartMeta {
            id: "look-spare",
            summary: "A material and no mesh.",
            category: PartCategory::Garment,
            requires: &[],
        }
    }

    fn build(_: &Params, _: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        Ok(PartOutput { materials: vec![(name("look-spare"), toon(0x0010_1010, false))], ..PartOutput::default() })
    }
}
omuzaki_char_api::register_part_kind!(LookSpare);

/// An accessory with a mesh in a material another part defines.
struct LookBorrow;

impl PartKind for LookBorrow {
    type Params = Params;

    fn meta() -> PartMeta {
        PartMeta {
            id: "look-borrow",
            summary: "A mesh, no material.",
            category: PartCategory::Accessory,
            requires: &[],
        }
    }

    fn build(_: &Params, _: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        Ok(PartOutput { meshes: vec![mesh("look-spare")], ..PartOutput::default() })
    }
}
omuzaki_char_api::register_part_kind!(LookBorrow);

fn character(stray_class: bool) -> CharacterDef {
    let mut def: CharacterDef = serde_json::from_value(json!({
        "name": "Look", "template": "soft-sd",
        "parts": { "body": { "kind": "look-body" }, "coat": { "kind": "look-coat", "params": { "stray_class": stray_class } } }
    }))
    .unwrap();
    def.parts.insert("skeleton".parse().unwrap(), common::body_part());
    def
}

fn built() -> BuiltCharacter {
    RegistryBuilder
        .build(&CharId::new("look").unwrap(), &character(false), &BuildOptions::default(), &BuildServices::default())
        .unwrap()
}

/// Two looks that differ in every class.
fn hard() -> LookParams {
    LookParams {
        all: ClassLook {
            softness_scale: Some(0.0),
            rim_scale: Some(0.0),
            outline_width: Some(0.002),
            ..ClassLook::default()
        },
        metal: ClassLook {
            outline_colour: Some(OutlineColour::Flat(Colour::hex(0x0040_2000))),
            ..ClassLook::default()
        },
        scene: SceneLook { bloom: Some(false), ..SceneLook::default() },
        ..LookParams::default()
    }
}

fn soft() -> LookParams {
    LookParams {
        skin: ClassLook { softness_scale: Some(2.0), receives_shadows: Some(false), ..ClassLook::default() },
        cloth: ClassLook { outline_width_scale: Some(0.5), ..ClassLook::default() },
        ..LookParams::default()
    }
}

#[test]
fn the_assembler_records_declared_and_inferred_classes() {
    let built = built();
    let classes: Vec<_> = built.material_classes.iter().map(|(n, c)| (n.as_str(), *c)).collect();
    assert_eq!(
        classes,
        [
            ("look-button", MaterialClass::Metal),
            ("look-cloth", MaterialClass::Cloth),
            ("look-skin", MaterialClass::Skin)
        ]
    );
    assert_eq!(built.materials, built.part_materials, "a fresh build is in the parts' own look");
    assert!(built.look().is_empty());

    let err = RegistryBuilder
        .build(&CharId::new("look").unwrap(), &character(true), &BuildOptions::default(), &BuildServices::default())
        .unwrap_err();
    assert!(err.to_string().contains("look-lining"), "{err}");
}

#[test]
fn looks_apply_by_class_never_compound_and_report_what_changed() {
    let fresh = built();
    let mut built = built();

    let changed = built.apply_look(&hard()).unwrap();
    assert_eq!(changed.len(), 3, "every material changes: {changed:?}");
    let Shading::Toon(skin) = &built.materials[&name("look-skin")].shading else { panic!("toon") };
    assert_eq!((skin.softness, skin.rim.strength), (0.0, 0.0));
    let button = built.materials[&name("look-button")].outline.unwrap();
    assert_eq!((button.colour, button.width), (Colour::hex(0x0040_2000), 0.002), "metal's flat line");
    let cloth = built.materials[&name("look-cloth")].outline.unwrap();
    assert_eq!((cloth.colour, cloth.width), (Colour::hex(0x0020_1010), 0.002), "cloth keeps its own line colour");
    assert_eq!(built.part_materials, fresh.part_materials, "the parts' materials are kept as built");
    assert_eq!(built.look(), &hard());

    // Soft after hard is soft alone: looks start from the parts' materials every time.
    built.apply_look(&soft()).unwrap();
    let mut soft_only = self::built();
    soft_only.apply_look(&soft()).unwrap();
    assert_eq!(built.materials, soft_only.materials);
    let Shading::Toon(skin) = &built.materials[&name("look-skin")].shading else { panic!("toon") };
    assert_eq!(skin.softness, 0.4);
    assert!(!built.materials[&name("look-skin")].receives_shadows);

    // Back to the empty look: the parts' bytes, and only the materials soft had changed are reported.
    let changed = built.apply_look(&LookParams::default()).unwrap();
    assert_eq!(changed, [name("look-cloth"), name("look-skin")]);
    assert_eq!(built.materials, fresh.materials);
    assert_eq!(built.apply_look(&LookParams::default()).unwrap(), Vec::<MaterialName>::new(), "nothing left to change");

    // Geometry is never touched, and a bad look changes nothing.
    assert_eq!(built.meshes, fresh.meshes);
    let bad = LookParams { hair: ClassLook { rim_scale: Some(-1.0), ..ClassLook::default() }, ..LookParams::default() };
    assert!(matches!(built.apply_look(&bad), Err(LookError::Invalid { .. })));
    assert_eq!(built.materials, fresh.materials);
}

#[test]
fn a_rebuild_inherits_the_look_of_the_character_it_replaces() {
    let mut drawn = built();
    drawn.apply_look(&hard()).unwrap();
    let mut rebuilt = built();
    assert_eq!(rebuilt.inherit_look(&drawn).len(), 3, "every material is drawn in the inherited look");
    assert_eq!((rebuilt.look(), &rebuilt.materials), (drawn.look(), &drawn.materials));
}

#[test]
fn no_look_moves_a_palette_colour() {
    let colours = |b: &BuiltCharacter| -> Vec<_> {
        b.materials
            .values()
            .map(|m| match &m.shading {
                Shading::Toon(Toon { shade, emissive, rim, .. }) => (m.base, *shade, *emissive, rim.colour),
                other => panic!("toon expected, got {other:?}"),
            })
            .collect()
    };
    let fresh = built();
    for look in [hard(), soft()] {
        let mut b = built();
        b.apply_look(&look).unwrap();
        assert_eq!(colours(&b), colours(&fresh));
    }
}

// ---- the registry: two presets from this test "crate"

// The registry's signature: a real source can fail to parse its files.
#[allow(clippy::unnecessary_wraps)]
fn load() -> Result<Vec<LookPreset>, String> {
    let preset = |id: &str, params: LookParams| LookPreset {
        id: id.parse().unwrap(),
        name: id.to_owned(),
        summary: "a test preset".to_owned(),
        origin: "this test".to_owned(),
        params,
    };
    Ok(vec![preset("test-hard", hard()), preset("test-soft", soft())])
}
omuzaki_char_api::register_look_presets!(load);

#[test]
fn presets_register_resolve_and_suggest_the_nearest_id() {
    let ids: Vec<_> = look_presets().unwrap().into_iter().map(|p| p.id.to_string()).collect();
    assert_eq!(ids, ["test-hard", "test-soft"]);
    assert_eq!(find_look_preset("test-soft").unwrap().params, soft());

    match find_look_preset("test-sofft") {
        Err(e @ LookError::UnknownPreset { .. }) => {
            assert!(e.to_string().contains("did you mean `test-soft`?"), "{e}");
            assert!(e.to_string().contains("test-hard, test-soft"), "{e}");
        }
        other => panic!("{other:?}"),
    }
    let far = find_look_preset("watercolour").unwrap_err().to_string();
    assert!(!far.contains("did you mean"), "{far}");

    // The look node: the preset's parameters with the person's over them.
    let node = json!({"preset": "test-hard", "overrides": {"all": {"outline_width": 0.003}, "scene": {"bloom": true}}});
    let look = LookDef::from_node(Some(&node)).unwrap().resolve().unwrap();
    assert_eq!(look.class(MaterialClass::Cloth).outline_width, Some(0.003));
    assert_eq!(look.class(MaterialClass::Cloth).softness_scale, Some(0.0), "from the preset");
    assert_eq!(look.scene.bloom, Some(true));
    let missing_default = LookDef::default().resolve().unwrap_err();
    assert!(matches!(missing_default, LookError::UnknownPreset { ref id, .. } if id == "soft-sd"), "{missing_default}");
}

#[test]
fn a_look_whose_computed_values_overflow_is_refused_and_changes_nothing() {
    let mut built = built();
    built.apply_look(&soft()).unwrap();
    let (look, materials) = (built.look().clone(), built.materials.clone());
    // Each number is finite and >= 0 on its own; their product is not a width the renderer accepts.
    let overflow = LookParams {
        all: ClassLook { outline_width: Some(1e30), outline_width_scale: Some(1e30), ..ClassLook::default() },
        ..LookParams::default()
    };
    let err = built.apply_look(&overflow).unwrap_err();
    assert!(matches!(&err, LookError::Invalid { path, .. } if path.starts_with("materials.look-")), "{err:?}");
    assert_eq!((built.look(), &built.materials), (&look, &materials), "the character is as it was");
    // A rim scale that overflows the strength is refused the same way.
    let big_rim =
        LookParams { all: ClassLook { rim_scale: Some(f32::MAX), ..ClassLook::default() }, ..LookParams::default() };
    let mut loud = self::built();
    for spec in loud.part_materials.values_mut() {
        if let Shading::Toon(t) = &mut spec.shading {
            t.rim.strength = 2.0;
        }
    }
    assert!(loud.apply_look(&big_rim).is_err());
    assert!(loud.look().is_empty());
}

#[test]
fn an_inherited_look_that_overflows_the_rebuilt_parts_warns_and_keeps_their_materials() {
    let mut drawn = built();
    let wide = LookParams {
        all: ClassLook { outline_width_scale: Some(1e30), ..ClassLook::default() },
        ..LookParams::default()
    };
    drawn.apply_look(&wide).unwrap();
    // A rebuild whose parts differ from the ones the look was checked on: lines wide enough to overflow the scale.
    let mut rebuilt = built();
    for spec in rebuilt.part_materials.values_mut() {
        spec.outline = Some(Outline { colour: Colour::BLACK, width: 4e10 });
    }
    let changed = rebuilt.inherit_look(&drawn);
    assert_eq!(rebuilt.materials, rebuilt.part_materials, "the parts' own materials are kept");
    assert_eq!(changed.len(), 3, "{changed:?}");
    let warnings: Vec<_> = rebuilt.report.parts.iter().flat_map(|p| &p.warnings).collect();
    assert_eq!(warnings.len(), 3, "{warnings:?}");
    assert!(warnings.iter().any(|w| w.contains("look-skin") && w.contains("inherited look")), "{warnings:?}");
}

/// The warnings `part` gets when `def` is rebuilt in a look its parts' (too wide) lines overflow.
fn inherit_warnings(def: &CharacterDef, part: &str) -> Vec<String> {
    let build = || {
        RegistryBuilder
            .build(&CharId::new("look").unwrap(), def, &BuildOptions::default(), &BuildServices::default())
            .unwrap()
    };
    let mut drawn = build();
    let wide = LookParams {
        all: ClassLook { outline_width_scale: Some(1e30), ..ClassLook::default() },
        ..LookParams::default()
    };
    drawn.apply_look(&wide).unwrap();
    let mut rebuilt = build();
    for spec in rebuilt.part_materials.values_mut() {
        spec.outline = Some(Outline { colour: Colour::BLACK, width: 4e10 });
    }
    rebuilt.inherit_look(&drawn);
    rebuilt.report.parts.iter().filter(|p| p.part.as_str() == part).flat_map(|p| p.warnings.clone()).collect()
}

#[test]
fn an_inherited_look_warning_goes_to_the_part_that_defines_the_material() {
    let mut def = character(false);
    def.parts.insert("spare".parse().unwrap(), serde_json::from_value(json!({ "kind": "look-spare" })).unwrap());
    // Nothing draws with the material: its owner still gets the warning.
    assert_eq!(inherit_warnings(&def, "spare").len(), 1);
    // The borrower is listed (and so built) before the part that defines the material it draws with.
    def.parts.insert("a-borrow".parse().unwrap(), serde_json::from_value(json!({ "kind": "look-borrow" })).unwrap());
    let spare = inherit_warnings(&def, "spare");
    assert_eq!(spare.len(), 1, "{spare:?}");
    assert!(
        spare[0].contains("part 'spare'") && spare[0].contains("material 'look-spare'") && spare[0].contains("outline"),
        "{spare:?}"
    );
    let borrow = inherit_warnings(&def, "a-borrow");
    assert!(borrow.is_empty(), "the part that only draws with it is not blamed: {borrow:?}");
}
