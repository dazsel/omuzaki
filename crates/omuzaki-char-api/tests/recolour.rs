//! `PartKind::recolour` and `recolour_part` (decision Q6): a palette change reaches a built part without building it
//! again. The default declines, an override must give exactly what a build gives, and a repaint goes through the same
//! rasteriser and the same checks as the build's own.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use omuzaki_char_api::{
    BuildCtx, BuildError, BuildOptions, BuildServices, CharacterDef, MaterialName, MaterialSpec, PartCategory, PartDef,
    PartId, PartKind, PartKindId, PartMeta, PartOutput, RecolourCtx, Recoloured, TextureName, build_part, find_kind,
    recolour_part,
};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{UVec2, Vec2};
use omuzaki_face_api::{
    Check, ColourRef, DrawList, FaceError, FaceLayerSpec, FaceRasterizer, FaceRect, Paint, Palette, PaletteSlot,
    Raster, Validate,
};
use omuzaki_render_api::TextureDesc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

// ---- three kinds: one that declines, one that re-resolves a colour, one that repaints a texture

#[derive(Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct Plain {
    tint: Option<ColourRef>,
}

impl Validate for Plain {
    fn check(&self, _: &mut Check) {}
}

fn material(ctx: &BuildCtx<'_>) -> Result<MaterialName, BuildError> {
    MaterialName::new(format!("probe-{}", ctx.part)).map_err(|e| ctx.fail(e.to_string()))
}

/// Takes the default `recolour`: it bakes a colour but says nothing about how to redo it.
struct Undyed;

impl PartKind for Undyed {
    type Params = Plain;

    fn meta() -> PartMeta {
        PartMeta { id: "probe-undyed", summary: "No recolour.", category: PartCategory::Accessory, requires: &[] }
    }

    fn build(params: &Plain, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let tint = params.tint.as_ref().map(|t| ctx.colour(t)).transpose()?.unwrap_or(Colour::WHITE);
        Ok(PartOutput::new().with_materials(vec![(material(ctx)?, MaterialSpec::toon(tint))]))
    }
}
omuzaki_char_api::register_part_kind!(Undyed);

static DYED_BUILDS: AtomicUsize = AtomicUsize::new(0);

/// A material-only kind: its one colour reaches the output only through its material.
struct Dyed;

impl PartKind for Dyed {
    type Params = Plain;

    fn meta() -> PartMeta {
        PartMeta { id: "probe-dyed", summary: "Recolours.", category: PartCategory::Accessory, requires: &[] }
    }

    fn build(params: &Plain, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        DYED_BUILDS.fetch_add(1, Ordering::SeqCst);
        let tint = params.tint.as_ref().map(|t| ctx.colour(t)).transpose()?.unwrap_or(Colour::WHITE);
        Ok(PartOutput::new().with_materials(vec![(material(ctx)?, MaterialSpec::toon(tint))]))
    }

    fn recolour(params: &Plain, output: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        let [(_, spec)] = output.materials.as_mut_slice() else { return Ok(Recoloured::NeedsRebuild) };
        spec.base = params.tint.as_ref().map(|t| ctx.colour(t)).transpose()?.unwrap_or(Colour::WHITE);
        Ok(Recoloured::Done)
    }
}
omuzaki_char_api::register_part_kind!(Dyed);

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct PlaidParams {
    cloth: ColourRef,
}

impl Default for PlaidParams {
    fn default() -> Self {
        Self { cloth: ColourRef::slot_or("cloth", ColourRef::rgb(200, 200, 200)) }
    }
}

impl Validate for PlaidParams {
    fn check(&self, _: &mut Check) {}
}

const PLAID_RECT: FaceRect = FaceRect { x0: 0.0, x1: 0.08, y0: 0.0, y1: 0.04 };

fn plaid_list(cloth: Colour) -> DrawList {
    let mut list = DrawList::new();
    let pts = [Vec2::ZERO, Vec2::new(0.08, 0.0), Vec2::new(0.08, 0.04), Vec2::new(0.0, 0.04)];
    let path = list.path().polyline(&pts).close().finish();
    list.fill(path, Paint::Solid(cloth));
    list
}

static PLAID_BUILDS: AtomicUsize = AtomicUsize::new(0);

/// A garment-like kind: a procedural texture painted through the injected rasteriser, and the base colour.
struct Plaid;

impl PartKind for Plaid {
    type Params = PlaidParams;

    fn meta() -> PartMeta {
        PartMeta { id: "probe-plaid", summary: "Paints.", category: PartCategory::Garment, requires: &[] }
    }

    fn build(params: &PlaidParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        PLAID_BUILDS.fetch_add(1, Ordering::SeqCst);
        let cloth = ctx.colour(&params.cloth)?;
        let texture =
            ctx.paint_texture(UVec2::new(16, 8), PLAID_RECT, &plaid_list(cloth), TextureDesc::colour(UVec2::ONE))?;
        let tex = TextureName::new("probe-plaid-tex").map_err(|e| ctx.fail(e.to_string()))?;
        let mut spec = MaterialSpec::toon(cloth);
        spec.base_texture = Some(tex.clone());
        Ok(PartOutput::new().with_materials(vec![(material(ctx)?, spec)]).with_textures(vec![(tex, texture)]))
    }

    fn recolour(
        params: &PlaidParams,
        output: &mut PartOutput,
        ctx: &RecolourCtx<'_>,
    ) -> Result<Recoloured, BuildError> {
        let ([(_, spec)], [(_, texture)]) = (output.materials.as_mut_slice(), output.textures.as_mut_slice()) else {
            return Ok(Recoloured::NeedsRebuild);
        };
        let cloth = ctx.colour(&params.cloth)?;
        *texture = ctx.paint_texture(texture.desc.size, PLAID_RECT, &plaid_list(cloth), texture.desc)?;
        spec.base = cloth;
        Ok(Recoloured::Done)
    }
}
omuzaki_char_api::register_part_kind!(Plaid);

/// Fills every pixel with the draw list's fingerprint: the pixels differ exactly when the drawing does.
struct FingerprintRaster;

impl FaceRasterizer for FingerprintRaster {
    fn rasterize(&mut self, spec: &FaceLayerSpec, list: &DrawList, _: f32, out: &mut Raster) -> Result<(), FaceError> {
        let (w, h) = spec.size_px();
        *out = Raster::new(w, h);
        let print = list.fingerprint().to_le_bytes();
        for px in out.pixels.as_chunks_mut::<4>().0 {
            *px = [print[0], print[1], print[2], 255];
        }
        Ok(())
    }
}

fn painter() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(FingerprintRaster))
}

// ---- helpers

/// The kinds count their builds in statics, so the tests that read the counts take turns.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

fn part(kind: &str, params: serde_json::Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer: 0, attach: None, params }
}

fn pid(s: &str) -> PartId {
    PartId::new(s).unwrap()
}

fn palette(cloth: [u8; 3]) -> Palette {
    let mut p = Palette::default();
    p.0.insert(PaletteSlot::new("cloth").unwrap(), Colour::rgb(cloth[0], cloth[1], cloth[2]));
    p
}

fn character(kind: &str, params: serde_json::Value, palette: Palette) -> CharacterDef {
    CharacterDef::new("Test".to_owned(), omuzaki_char_api::BodyTemplateId::new("soft-sd").unwrap())
        .with_seed(7)
        .with_palette(palette)
        .with_skin_tone(None)
        .with_parts(BTreeMap::from([(pid("body"), common::body_part()), (pid("probe"), part(kind, params))]))
        .with_face(omuzaki_face_api::FaceProfile::default())
        .with_weight_brushes(BTreeMap::new())
}

fn build_probe(def: &CharacterDef, services: &BuildServices) -> Result<PartOutput, BuildError> {
    let published = omuzaki_char_api::Published::default();
    build_part(def, &pid("probe"), &BuildOptions::default(), services, &published)
}

/// Everything a part's output holds that a recolour may touch or must leave alone, as exact text.
fn exact(output: &PartOutput) -> String {
    format!("{:?}|{:?}|{:?}|{:?}", output.meshes, output.materials, output.textures, output.warnings)
}

// ---- the contract

#[test]
fn a_kind_that_does_not_override_recolour_declines() {
    let _turn = serial();
    let (old, new) = (palette([10, 20, 30]), palette([200, 100, 50]));
    let params = json!({ "tint": "@cloth" });
    let mut output = build_probe(&character("probe-undyed", params.clone(), old), &BuildServices::default()).unwrap();
    let before = exact(&output);
    let want = Recoloured::NeedsRebuild;
    let def = character("probe-undyed", params, new);
    assert_eq!(recolour_part(&def, &pid("probe"), &BuildServices::default(), &mut output), Ok(want));
    assert_eq!(exact(&output), before, "declining leaves the output alone");
}

#[test]
fn a_recoloured_output_is_exactly_the_output_of_a_build_with_the_new_palette() {
    let _turn = serial();
    let params = json!({ "tint": "@cloth" });
    let services = BuildServices::default();
    let mut output = build_probe(&character("probe-dyed", params.clone(), palette([10, 20, 30])), &services).unwrap();
    let new = character("probe-dyed", params, palette([200, 100, 50]));
    let builds = DYED_BUILDS.load(Ordering::SeqCst);
    assert_eq!(recolour_part(&new, &pid("probe"), &services, &mut output), Ok(Recoloured::Done));
    assert_eq!(DYED_BUILDS.load(Ordering::SeqCst), builds, "a recolour is not a build");
    assert_eq!(exact(&output), exact(&build_probe(&new, &services).unwrap()));
    assert_eq!(output.materials[0].1.base, Colour::rgb(200, 100, 50));
}

#[test]
fn a_repaint_goes_through_the_rasteriser_and_gives_the_pixels_of_a_build() {
    let _turn = serial();
    let (services, params) = (painter(), json!({}));
    let mut output = build_probe(&character("probe-plaid", params.clone(), palette([10, 20, 30])), &services).unwrap();
    let old_pixels = output.textures[0].1.rgba8.clone();
    let new = character("probe-plaid", params, palette([200, 100, 50]));
    let builds = PLAID_BUILDS.load(Ordering::SeqCst);
    assert_eq!(recolour_part(&new, &pid("probe"), &services, &mut output), Ok(Recoloured::Done));
    assert_eq!(PLAID_BUILDS.load(Ordering::SeqCst), builds, "no build ran");
    assert_ne!(output.textures[0].1.rgba8, old_pixels, "the texture was repainted");
    assert_eq!(exact(&output), exact(&build_probe(&new, &services).unwrap()));
    output.textures[0].1.validate().unwrap();
}

#[test]
fn a_repaint_without_a_rasteriser_names_what_is_missing_like_a_build_does() {
    let _turn = serial();
    let mut output = build_probe(&character("probe-plaid", json!({}), palette([1, 2, 3])), &painter()).unwrap();
    let new = character("probe-plaid", json!({}), palette([4, 5, 6]));
    let err = recolour_part(&new, &pid("probe"), &BuildServices::default(), &mut output).unwrap_err();
    assert_eq!(err, BuildError::Missing { part: pid("probe"), needs: "a 2D rasteriser" });
    assert_eq!(build_probe(&new, &BuildServices::default()).unwrap_err(), err, "the build says the same");
}

#[test]
fn a_slot_the_new_palette_lacks_is_a_colour_error_naming_the_part() {
    let _turn = serial();
    let params = json!({ "tint": "@cloth" });
    let services = BuildServices::default();
    let mut output = build_probe(&character("probe-dyed", params.clone(), palette([1, 2, 3])), &services).unwrap();
    let bare = character("probe-dyed", params, Palette::default());
    let err = recolour_part(&bare, &pid("probe"), &services, &mut output).unwrap_err();
    assert!(matches!(&err, BuildError::Colour { part, .. } if part.as_str() == "probe"), "{err:?}");
    assert_eq!(build_probe(&bare, &services).unwrap_err(), err, "the build reports the same error");
}

#[test]
fn recolouring_a_part_that_cannot_be_built_is_the_builds_error() {
    let _turn = serial();
    let services = BuildServices::default();
    let mut output = build_probe(&character("probe-dyed", json!({}), Palette::default()), &services).unwrap();
    let missing =
        recolour_part(&character("probe-dyed", json!({}), Palette::default()), &pid("nobody"), &services, &mut output);
    assert!(matches!(missing, Err(BuildError::Part { ref part, .. }) if part.as_str() == "nobody"), "{missing:?}");

    let ghost = character("probe-no-such-kind", json!({}), Palette::default());
    let err = recolour_part(&ghost, &pid("probe"), &services, &mut output).unwrap_err();
    assert!(matches!(err, BuildError::UnknownKind { .. }), "{err:?}");

    let bad = character("probe-dyed", json!({ "tint": 5 }), Palette::default());
    let err = recolour_part(&bad, &pid("probe"), &services, &mut output).unwrap_err();
    assert!(matches!(&err, BuildError::Params { part, .. } if part.as_str() == "probe"), "{err:?}");
}

#[test]
fn every_registration_carries_the_hook() {
    let _turn = serial();
    let reg = find_kind("probe-dyed").expect("registered");
    let services = BuildServices::default();
    let (part, palette) = (pid("probe"), palette([9, 9, 9]));
    let ctx = RecolourCtx { part: &part, palette: &palette, services: &services };
    let mut output = PartOutput::default();
    // An output the kind does not recognise (it has no material): declined, not guessed at.
    assert_eq!((reg.recolour)(&json!({}), &mut output, &ctx), Ok(Recoloured::NeedsRebuild));
}
