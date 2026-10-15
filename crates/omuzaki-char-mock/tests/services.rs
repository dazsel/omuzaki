//! The build services with the real mocks behind them (decision G5): a part reads its stripe colour from `MemAssets`
//! and paints a stripe texture through `omuzaki-face-mock`'s `MockRasterizer`, with nothing but API types between them.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;

use omuzaki_char_api::{
    AssetError, BuildCtx, BuildError, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder,
    CharacterDef, ContentHash, MaterialName, MaterialSpec, MeshData, PartCategory, PartDef, PartId, PartKind,
    PartKindId, PartMesh, PartMeta, PartOutput, RegistryBuilder, TextureData, TextureName,
};
use omuzaki_char_mock::{MemAssets, mock_character};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{UVec2, Vec2, Vec3};
use omuzaki_face_api::{Check, DrawList, FaceRect, Paint, Validate};
use omuzaki_face_mock::MockRasterizer;
use omuzaki_render_api::TextureDesc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// A 0.08 m rect at 64 px is 800 px/m: eight stripes of 0.01 m, 8 px each.
const SIZE: u32 = 64;
const STRIPE_PX: u32 = 8;

#[derive(Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct StripedParams {
    /// The asset holding the stripe colour: three bytes, R G B.
    colour_asset: String,
}

impl Validate for StripedParams {
    fn check(&self, _: &mut Check) {}
}

/// A garment-like part: white cloth with every second stripe in the colour its asset holds.
struct Striped;

impl PartKind for Striped {
    type Params = StripedParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "mock-striped",
            summary: "Paint a striped cloth texture.",
            category: PartCategory::Garment,
            requires: &[],
        }
    }

    fn build(params: &StripedParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let hash: ContentHash = params.colour_asset.parse().map_err(|e| ctx.fail(format!("{e}")))?;
        let bytes = ctx.asset_or_fail(&hash)?;
        let [r, g, b] = <[u8; 3]>::try_from(&*bytes).map_err(|_| ctx.fail("the colour asset is not 3 bytes"))?;

        let rect = FaceRect { x0: 0.0, x1: 0.08, y0: 0.0, y1: 0.08 };
        let mut list = DrawList::new();
        let rect_path = |list: &mut DrawList, x0: f32, x1: f32| {
            let pts = [Vec2::new(x0, rect.y0), Vec2::new(x1, rect.y0), Vec2::new(x1, rect.y1), Vec2::new(x0, rect.y1)];
            list.path().polyline(&pts).close().finish()
        };
        let cloth = rect_path(&mut list, rect.x0, rect.x1);
        list.fill(cloth, Paint::Solid(Colour::rgb(255, 255, 255)));
        for k in 0..4_u8 {
            let x0 = rect.x0 + 0.02 * f32::from(k);
            let stripe = rect_path(&mut list, x0, x0 + 0.01);
            list.fill(stripe, Paint::Solid(Colour::rgb(r, g, b)));
        }
        let texture = ctx.paint_texture(UVec2::splat(SIZE), rect, &list, TextureDesc::colour(UVec2::ONE))?;

        let texture_name = TextureName::new("mock-striped-cloth").map_err(|e| ctx.fail(e.to_string()))?;
        let material_name = MaterialName::new("mock-striped-material").map_err(|e| ctx.fail(e.to_string()))?;
        let mut material = MaterialSpec::toon(Colour::rgb(255, 255, 255));
        material.base_texture = Some(texture_name.clone());
        Ok(PartOutput::new()
            .with_meshes(vec![PartMesh {
                name: "cloth".to_owned(),
                geometry: MeshData {
                    positions: vec![Vec3::ZERO, Vec3::X, Vec3::Y],
                    normals: vec![Vec3::Z; 3],
                    uvs: vec![Vec2::ZERO, Vec2::X, Vec2::Y],
                    indices: vec![0, 1, 2],
                    ..MeshData::default()
                },
                joint_bones: Vec::new(),
                material: material_name.clone(),
                casts_shadow: true,
                render_order: 0,
                dynamic: false,
            }])
            .with_materials(vec![(material_name, material)])
            .with_textures(vec![(texture_name, texture)]))
    }
}
omuzaki_char_api::register_part_kind!(Striped);

fn character(colour_asset: ContentHash) -> CharacterDef {
    let part = PartDef {
        kind: PartKindId::new("mock-striped").unwrap(),
        parent: None,
        enabled: true,
        layer: 0,
        attach: None,
        params: json!({ "colour_asset": colour_asset.to_string() }),
    };
    // The skeleton comes from a body part.
    let body = PartDef {
        kind: PartKindId::new("mock-body").unwrap(),
        parent: None,
        enabled: true,
        layer: 0,
        attach: None,
        params: json!({}),
    };
    // The mock body paints its skin from the mock character's palette slots.
    CharacterDef::new("Stripes", omuzaki_char_api::BodyTemplateId::new("soft-sd").unwrap())
        .with_seed(1)
        .with_palette(mock_character().unwrap().palette)
        .with_parts(BTreeMap::from([(PartId::new("body").unwrap(), body), (PartId::new("shirt").unwrap(), part)]))
}

fn build(def: &CharacterDef, services: &BuildServices) -> Result<BuiltCharacter, BuildError> {
    RegistryBuilder.build(&CharId::new("stripes").unwrap(), def, &BuildOptions::default(), services)
}

fn services(assets: MemAssets) -> BuildServices {
    BuildServices::default().with_assets(assets).with_rasterizer(|| Box::new(MockRasterizer))
}

fn pixel(t: &TextureData, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * t.desc.size.x + x) * 4) as usize;
    [t.rgba8[i], t.rgba8[i + 1], t.rgba8[i + 2], t.rgba8[i + 3]]
}

#[test]
fn a_part_paints_a_stripe_texture_through_the_mock_rasteriser() {
    let mut assets = MemAssets::new();
    let colour = assets.put(&[200, 30, 60]);
    let (def, services) = (character(colour), services(assets));

    let built = build(&def, &services).unwrap();
    let texture = &built.textures[&TextureName::new("mock-striped-cloth").unwrap()];
    texture.validate().unwrap();
    assert_eq!(texture.desc.size, UVec2::splat(SIZE));
    for y in [0, 13, SIZE - 1] {
        for x in 0..SIZE {
            let want = if (x / STRIPE_PX).is_multiple_of(2) { [200, 30, 60, 255] } else { [255, 255, 255, 255] };
            assert_eq!(pixel(texture, x, y), want, "pixel ({x}, {y}): stripes run top to bottom");
        }
    }
    let coloured = (0..SIZE).filter(|&x| pixel(texture, x, 5) == [200, 30, 60, 255]).count();
    assert_eq!(coloured, usize::try_from(SIZE / 2).unwrap(), "every second stripe is coloured");
    assert_eq!(
        built.materials[&MaterialName::new("mock-striped-material").unwrap()].base_texture,
        Some(TextureName::new("mock-striped-cloth").unwrap()),
        "the material names the painted texture"
    );

    let again = build(&def, &services).unwrap();
    assert_eq!(again.textures, built.textures, "byte-identical on two builds");
}

#[test]
fn the_stripe_colour_follows_the_asset() {
    let mut assets = MemAssets::new();
    let (red, blue) = (assets.put(&[255, 0, 0]), assets.put(&[0, 0, 255]));
    let services = services(assets);
    let first_pixel = |hash| {
        let built = build(&character(hash), &services).unwrap();
        pixel(built.textures.values().next().unwrap(), 0, 0)
    };
    assert_eq!((first_pixel(red), first_pixel(blue)), ([255, 0, 0, 255], [0, 0, 255, 255]));
}

#[test]
fn a_part_says_which_service_the_build_lacks() {
    let mut assets = MemAssets::new();
    let colour = assets.put(&[1, 2, 3]);
    let def = character(colour);

    let no_rasteriser = BuildServices::default().with_assets(assets.clone());
    assert_eq!(
        build(&def, &no_rasteriser).unwrap_err(),
        BuildError::Missing { part: PartId::new("shirt").unwrap(), needs: "a 2D rasteriser" }
    );

    let no_assets = BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer));
    let err = build(&def, &no_assets).unwrap_err();
    assert!(
        matches!(&err, BuildError::Asset { part, error: AssetError::Unavailable } if part.as_str() == "shirt"),
        "{err}"
    );
    assert!(err.to_string().contains("no asset reader"), "{err}");
}

#[test]
fn a_missing_colour_asset_is_a_typed_asset_error_naming_the_part() {
    let mut assets = MemAssets::new();
    assets.put(&[1, 2, 3]);
    let absent = ContentHash::of(b"not in the map");
    let err = build(&character(absent), &services(assets)).unwrap_err();
    assert_eq!(
        err,
        BuildError::Asset { part: PartId::new("shirt").unwrap(), error: AssetError::NotFound(absent) },
        "{err}"
    );
    assert!(err.to_string().contains("part 'shirt'") && err.to_string().contains(&absent.to_string()), "{err}");
}
