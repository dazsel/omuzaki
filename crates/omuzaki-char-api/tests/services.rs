//! The build services (decision G5): a part reads assets and paints textures only through what the app injected, and
//! says exactly what is missing when it wasn't. A test-only rasteriser and asset map stand in for `omuzaki-face` and the
//! store, so this crate's tests need no implementation or mock crate.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_char_api::{
    AssetError, AssetSource, BuildCtx, BuildError, BuildOptions, BuildServices, BuiltCharacter, CharId,
    CharacterBuilder, CharacterDef, ContentHash, PartCategory, PartDef, PartId, PartKind, PartKindId, PartMeta,
    PartOutput, Published, Quality, RegistryBuilder, TextureData, TextureName,
};
use omuzaki_core::glam::UVec2;
use omuzaki_core::rng::Rng;
use omuzaki_face_api::{
    Check, DrawList, FaceError, FaceLayerSpec, FaceProfile, FaceRasterizer, FaceRect, Palette, Raster, Validate,
};
use omuzaki_render_api::{AlphaEncoding, ColourSpace, TextureDesc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

// ---- test doubles for the two services

/// Writes `[x, y, px_per_m, alpha]` into every pixel: the output shows the density and orientation the part's request
/// turned into, and (for alpha < 255) the premultiplied-to-straight conversion.
struct CoordinateRaster {
    alpha: u8,
}

impl FaceRasterizer for CoordinateRaster {
    // The test sizes are tiny, so every value fits a byte.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn rasterize(
        &mut self,
        spec: &FaceLayerSpec,
        _list: &DrawList,
        px_per_m: f32,
        out: &mut Raster,
    ) -> Result<(), FaceError> {
        assert_eq!(spec.px_per_m, px_per_m, "the spec carries the density the texture is drawn at");
        let (w, h) = spec.size_px();
        *out = Raster::new(w, h);
        for (i, px) in out.pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = (i % w as usize, i / w as usize);
            *px = [x as u8, y as u8, px_per_m as u8, self.alpha];
        }
        Ok(())
    }
}

/// A rasteriser that can't draw anything.
struct Refuses;

impl FaceRasterizer for Refuses {
    fn rasterize(&mut self, _: &FaceLayerSpec, _: &DrawList, _: f32, _: &mut Raster) -> Result<(), FaceError> {
        Err(FaceError::Raster("no gradients here".to_owned()))
    }
}

#[derive(Default)]
struct Assets(BTreeMap<ContentHash, Arc<[u8]>>);

impl Assets {
    fn with(bytes: &[u8]) -> (Self, ContentHash) {
        let hash = ContentHash::of(bytes);
        (Self(BTreeMap::from([(hash, Arc::from(bytes))])), hash)
    }
}

impl AssetSource for Assets {
    fn get(&self, hash: &ContentHash) -> Result<Arc<[u8]>, AssetError> {
        self.0.get(hash).cloned().ok_or(AssetError::NotFound(*hash))
    }
}

fn with_raster(alpha: u8) -> BuildServices {
    BuildServices::default().with_rasterizer(move || Box::new(CoordinateRaster { alpha }))
}

// ---- two kinds in the shape a garment and the head would have

fn cloth_rect() -> FaceRect {
    FaceRect { x0: 0.0, x1: 0.5, y0: 0.0, y1: 0.25 }
}

#[derive(Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct ClothParams {
    /// Make the texture straight alpha instead of the premultiplied the rasteriser draws.
    straight: bool,
}

impl Validate for ClothParams {
    fn check(&self, _: &mut Check) {}
}

/// Asks for a 16 x 8 px texture over `cloth_rect()`.
struct Cloth;

impl PartKind for Cloth {
    type Params = ClothParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "probe-cloth",
            summary: "Paint a cloth texture.",
            category: PartCategory::Garment,
            requires: &[],
        }
    }

    fn build(params: &ClothParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let mut desc = TextureDesc::colour(UVec2::ONE);
        desc.alpha = if params.straight { AlphaEncoding::Straight } else { AlphaEncoding::Premultiplied };
        let texture = ctx.paint_texture(UVec2::new(16, 8), cloth_rect(), &DrawList::new(), desc)?;
        Ok(PartOutput::new().with_textures(vec![(
            TextureName::new("probe-cloth-weave").map_err(|e| ctx.fail(e.to_string()))?,
            texture,
        )]))
    }
}
omuzaki_char_api::register_part_kind!(Cloth);

#[derive(Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct ReaderParams {
    /// The asset to read, as 64 hex characters.
    hash: String,
}

impl Validate for ReaderParams {
    fn check(&self, _: &mut Check) {}
}

/// Reads one asset and reports its size, the way the head reads a traced radius map.
struct Reader;

impl PartKind for Reader {
    type Params = ReaderParams;

    fn meta() -> PartMeta {
        PartMeta { id: "probe-reader", summary: "Read an asset.", category: PartCategory::Head, requires: &[] }
    }

    fn build(params: &ReaderParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let hash: ContentHash = params.hash.parse().map_err(|e| ctx.fail(format!("{e}")))?;
        let bytes = ctx.asset_or_fail(&hash)?;
        Ok(PartOutput::new().with_warnings(vec![format!("read {} bytes", bytes.len())]))
    }
}
omuzaki_char_api::register_part_kind!(Reader);

// ---- helpers

fn character(kind: &str, params: serde_json::Value) -> CharacterDef {
    let part =
        PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer: 0, attach: None, params };
    CharacterDef::new("Test".to_owned(), omuzaki_char_api::BodyTemplateId::new("soft-sd").unwrap())
        .with_seed(7)
        .with_palette(Palette::default())
        .with_skin_tone(None)
        .with_parts(BTreeMap::from([
            (PartId::new("body").unwrap(), common::body_part()),
            (PartId::new("probe").unwrap(), part),
        ]))
        .with_face(FaceProfile::default())
        .with_weight_brushes(BTreeMap::new())
}

fn build(def: &CharacterDef, services: &BuildServices) -> Result<BuiltCharacter, BuildError> {
    RegistryBuilder.build(&CharId::new("probe").unwrap(), def, &BuildOptions::default(), services)
}

fn weave(built: &BuiltCharacter) -> &TextureData {
    &built.textures[&TextureName::new("probe-cloth-weave").unwrap()]
}

/// Runs `f` with a context for part `probe` that carries `services`.
fn with_ctx<T>(services: &BuildServices, f: impl FnOnce(&BuildCtx<'_>) -> T) -> T {
    let (part, palette, face) = (PartId::new("probe").unwrap(), Palette::default(), FaceProfile::default());
    let published = Published::default();
    let template = omuzaki_char_api::BodyTemplateId::new("soft-sd").unwrap();
    let ctx = BuildCtx::new(&part, &template, &palette, &face, Rng::new(0), &published, Quality::Live, services);
    f(&ctx)
}

fn paint(ctx: &BuildCtx<'_>, size: UVec2, rect: FaceRect, desc: TextureDesc) -> Result<TextureData, BuildError> {
    ctx.paint_texture(size, rect, &DrawList::new(), desc)
}

// ---- textures

#[test]
fn a_texture_without_a_rasteriser_is_a_named_error() {
    let err = build(&character("probe-cloth", json!({})), &BuildServices::default()).unwrap_err();
    assert_eq!(err, BuildError::Missing { part: PartId::new("probe").unwrap(), needs: "a 2D rasteriser" });
    assert!(err.to_string().contains("a 2D rasteriser"), "{err}");
}

#[test]
fn a_texture_is_byte_identical_on_every_build_and_drawn_at_the_density_its_size_implies() {
    let (def, services) = (character("probe-cloth", json!({})), with_raster(255));
    let (a, b) = (build(&def, &services).unwrap(), build(&def, &services).unwrap());
    assert_eq!(weave(&a), weave(&b));
    let t = weave(&a);
    t.validate().unwrap();
    assert_eq!((t.desc.size, t.desc.alpha), (UVec2::new(16, 8), AlphaEncoding::Premultiplied), "size_px wins");
    assert_eq!(&t.rgba8[..4], &[0, 0, 32, 255], "16 px over 0.5 m is 32 px/m");
    assert_eq!(&t.rgba8[(8 + 16 * 3) * 4..][..4], &[8, 3, 32, 255], "row 0 is the top of the rect");
}

#[test]
fn straight_alpha_textures_are_unpremultiplied_and_premultiplied_ones_kept() {
    let services = with_raster(128);
    let premultiplied = build(&character("probe-cloth", json!({})), &services).unwrap();
    assert_eq!(&weave(&premultiplied).rgba8[(1 + 16) * 4..][..4], &[1, 1, 32, 128]);
    let straight = build(&character("probe-cloth", json!({ "straight": true })), &services).unwrap();
    // 32 / 128 of full alpha, rounded: the straight colour a blend with alpha 128 needs.
    assert_eq!(&weave(&straight).rgba8[(1 + 16) * 4..][..4], &[2, 2, 64, 128]);
    assert_eq!(weave(&straight).desc.alpha, AlphaEncoding::Straight);
}

#[test]
fn a_texture_that_cannot_be_painted_says_why() {
    let services = with_raster(255);
    let (colour, rect) = (TextureDesc::colour(UVec2::ONE), cloth_rect());
    with_ctx(&services, |ctx| {
        let msg = |r: Result<TextureData, BuildError>| r.unwrap_err().to_string();
        assert!(msg(paint(ctx, UVec2::new(16, 16), rect, colour)).contains("square pixels"));
        let flat = FaceRect { y1: 0.0, ..rect };
        assert!(msg(paint(ctx, UVec2::new(16, 8), flat, colour)).contains("non-empty"));
        let linear = TextureDesc { colour_space: ColourSpace::Linear, ..colour };
        assert!(msg(paint(ctx, UVec2::new(16, 8), rect, linear)).contains("sRGB"));
        assert!(msg(paint(ctx, UVec2::new(0, 8), rect, colour)).contains("zero side"));
    });
    let refusing = BuildServices::default().with_rasterizer(|| Box::new(Refuses));
    with_ctx(&refusing, |ctx| {
        let err = paint(ctx, UVec2::new(16, 8), rect, colour).unwrap_err();
        assert!(matches!(err, BuildError::Part { ref message, .. } if message.contains("no gradients here")), "{err}");
    });
}

// ---- assets

#[test]
fn an_asset_is_read_by_hash_through_the_context() {
    let (assets, hash) = Assets::with(b"traced radius map");
    let services = BuildServices::default().with_assets(assets);
    with_ctx(&services, |ctx| assert_eq!(&*ctx.asset(&hash).unwrap(), b"traced radius map"));
    let built = build(&character("probe-reader", json!({ "hash": hash.to_string() })), &services).unwrap();
    let probe = built.report.parts.iter().find(|p| p.part.as_str() == "probe").unwrap();
    assert_eq!(probe.warnings, ["read 17 bytes"]);
}

#[test]
fn an_absent_asset_is_not_found_and_a_missing_reader_is_unavailable() {
    let (assets, _) = Assets::with(b"something else");
    let absent = ContentHash::of(b"never stored");
    let services = BuildServices::default().with_assets(assets);
    with_ctx(&services, |ctx| assert_eq!(ctx.asset(&absent), Err(AssetError::NotFound(absent))));
    let err = build(&character("probe-reader", json!({ "hash": absent.to_string() })), &services).unwrap_err();
    assert_eq!(
        err,
        BuildError::Asset { part: PartId::new("probe").unwrap(), error: AssetError::NotFound(absent) },
        "{err}"
    );
    assert!(err.to_string().contains("part 'probe'") && err.to_string().contains(&absent.to_string()), "{err}");
    with_ctx(&BuildServices::default(), |ctx| assert_eq!(ctx.asset(&absent), Err(AssetError::Unavailable)));
}

#[test]
fn a_part_that_cannot_read_its_asset_fails_with_the_typed_error_naming_it() {
    let hash = ContentHash::of(b"never stored");
    let params = json!({ "hash": hash.to_string() });
    let err = build(&character("probe-reader", params), &BuildServices::default()).unwrap_err();
    assert!(
        matches!(&err, BuildError::Asset { part, error: AssetError::Unavailable } if part.as_str() == "probe"),
        "{err}"
    );
    assert!(err.to_string().contains("part 'probe'"), "the message names the part: {err}");
}

// ---- the services value

#[test]
fn services_are_cheap_to_share_and_stay_out_of_the_cache_key() {
    fn is_eq<T: Eq>() {}
    is_eq::<BuildOptions>(); // a cache keys on (definition, options): services must not break `Eq`
    let services = with_raster(255).with_assets(Assets::default());
    let copy = services.clone();
    assert_eq!(format!("{copy:?}"), "BuildServices { assets: true, rasterizer: true }");
    assert!(Arc::ptr_eq(services.assets.as_ref().unwrap(), copy.assets.as_ref().unwrap()), "clones share handles");
}
