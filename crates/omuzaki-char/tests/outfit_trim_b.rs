//! Trim library II (#189): draped chains, medallion clasps, emblem decals and lace edges on a tube garment, through the
//! conformance suite on every template and shape; chain sag against the catenary at five slacks; decals offset from the
//! surface and never sunk into it; a store-image decal that needs an asset source; a deterministic lace texture.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss, clippy::cast_sign_loss)] // test arithmetic
#![allow(clippy::many_single_char_names, clippy::chunks_exact_to_as_chunks)] // geometry reads best as a, b, c

#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;

use std::collections::HashMap;
use std::sync::Arc;

use omuzaki_char::kit::grid::MeshBuilder;
use omuzaki_char::outfit::common::{Garment, GarmentCommon, GarmentOutput, LAYER_SHIRT};
use omuzaki_char::outfit::loft::Surface;
use omuzaki_char::outfit::trim::chain::{self, Chain, Hang};
use omuzaki_char::outfit::trim::clasp::{Clasp, ClaspStyle};
use omuzaki_char::outfit::trim::decal::{self, Decal, DecalAnchor, DecalSource, IMAGE_MAGIC};
use omuzaki_char::outfit::trim::lace::{self, Lace, LacePattern};
use omuzaki_char::outfit::trim::{Finish, RailSpec, TrimSpec, add_trim};
use omuzaki_char_api::{
    Anchor, AnchorsFact, AssetError, AssetSource, BodyFit, BodyRegion, BodyShape, BuildCtx, BuildError, BuildServices,
    JointRef, PartCategory, PartKind, PartMeta, PartOutput, Publishes,
};
use omuzaki_char_mock::TorsoFit;
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{DVec3, Quat, Vec2};
use omuzaki_face_api::{Check, ColourRef, DrawList, FaceLayerId, FaceLayerSpec, FaceRect, Paint, Raster, Validate};
use omuzaki_face_mock::MockRasterizer;
use omuzaki_store_api::ContentHash;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

// ------------------------------------------------------------------------------------------- a trimmed tube garment

/// A tube round the torso with trims on it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TrimmedParams {
    common: GarmentCommon,
    trims: Vec<TrimSpec>,
}

impl Validate for TrimmedParams {
    fn check(&self, c: &mut Check) {
        c.nested("common", &self.common);
        for (k, trim) in self.trims.iter().enumerate() {
            c.nested(&format!("trims/{k}"), trim);
        }
    }
}

/// Where the tube runs along the torso, clear of the arm roots on every template.
const SPAN: (f64, f64) = (0.2, 0.55);

struct Trimmed;

impl PartKind for Trimmed {
    type Params = TrimmedParams;

    fn meta() -> PartMeta {
        PartMeta { id: "test-trimmed", summary: "a trimmed tube", category: PartCategory::Garment, requires: &["body"] }
    }

    fn build(p: &TrimmedParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let fail = |e: omuzaki_char::outfit::common::GarmentError| ctx.fail(e.to_string());
        let surface = Surface::new(ctx.fit()?, BodyRegion::Torso, 2.0, p.common.ease).map_err(fail)?;
        let mut out = GarmentOutput::new(ctx, &p.common)?;
        let mut builder = MeshBuilder::new();
        surface.loft(SPAN.0, SPAN.1, 24, 48).map_err(fail)?.add_to(&mut builder, out.tile()).map_err(fail)?;
        out.surface(ctx, "tube", builder)?;
        for (k, trim) in p.trims.iter().enumerate() {
            add_trim(&mut out, ctx, &surface, &format!("trim{k}"), trim)?;
        }
        out.finish(ctx, &p.common, &[BodyRegion::Torso], 2.0)
    }
}

impl Garment for Trimmed {
    fn common(p: &TrimmedParams) -> &GarmentCommon {
        &p.common
    }

    fn regions(_: &TrimmedParams) -> Vec<BodyRegion> {
        vec![BodyRegion::Torso]
    }

    fn open_borders(p: &TrimmedParams) -> usize {
        2 + p.trims.iter().map(TrimSpec::open_borders).sum::<usize>()
    }
}

omuzaki_char_api::register_part_kind!(Trimmed);

/// Publishes an anchor in front of the chest, as an accessory's mount would.
struct Mount;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Nothing {}

impl Validate for Nothing {
    fn check(&self, _: &mut Check) {}
}

impl PartKind for Mount {
    type Params = Nothing;

    fn meta() -> PartMeta {
        PartMeta { id: "test-mount", summary: "an anchor", category: PartCategory::Accessory, requires: &["body"] }
    }

    fn build(_: &Nothing, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let ring = omuzaki_char::outfit::loft::ring_at(ctx.fit()?.as_ref(), BodyRegion::Torso, 0.42, 2.0, 0.02)
            .map_err(|e| ctx.fail(e.to_string()))?;
        let anchor = Anchor { bone: JointRef::HIPS, position: ring.point(0.3).as_vec3(), rotation: Quat::IDENTITY };
        Ok(PartOutput::new()
            .with_publishes(Publishes::default().with::<AnchorsFact>(("crest".parse().unwrap(), anchor))))
    }
}

omuzaki_char_api::register_part_kind!(Mount);

fn services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer))
}

fn rail(points: &[[f64; 2]], closed: bool) -> RailSpec {
    RailSpec::new(points, closed)
}

/// A star, in a 4 cm square.
fn star() -> DecalSource {
    let mut list = DrawList::new();
    let points: Vec<Vec2> = (0..10)
        .map(|k| {
            let a = std::f32::consts::TAU * k as f32 / 10.0;
            let r = if k % 2 == 0 { 0.019 } else { 0.008 };
            Vec2::new(0.02 + r * a.sin(), 0.02 + r * a.cos())
        })
        .collect();
    let path = list.path().polyline(&points).close().finish();
    list.fill(path, Paint::Solid(Colour::rgb(0xe0, 0xc0, 0x40)));
    DecalSource::Drawing { list, extent: FaceRect { x0: 0.0, x1: 0.04, y0: 0.0, y1: 0.04 } }
}

/// Every trim of this library, between them every option.
fn trims() -> Vec<TrimSpec> {
    let lace = |pattern, closed| {
        TrimSpec::Lace(Lace {
            rail: if closed {
                rail(&[[0.25, 0.0], [0.25, 2.1], [0.25, 4.2]], true)
            } else {
                rail(&[[0.4, 1.9], [0.3, 2.3]], false)
            },
            scallop_radius: 0.004,
            scallop_count: if closed { 60 } else { 8 },
            pattern,
            colour: ColourRef::rgb(0xfa, 0xf6, 0xee),
            flip: false,
        })
    };
    vec![
        TrimSpec::Chain(Chain {
            from: [0.45, -0.5],
            to: [0.45, 0.5],
            sag: 0.012,
            bow: chain::SAG.1,
            link_size: chain::LINK.0,
            link_count: None,
            pendant: true,
            finish: Finish::default(),
        }),
        TrimSpec::Clasp(Clasp {
            at: [0.48, 0.6],
            style: ClaspStyle::Medallion,
            size: 0.02,
            jewel: Some(ColourRef::rgb(0x30, 0x60, 0xd0)),
            finish: Finish::default(),
        }),
        TrimSpec::Clasp(Clasp {
            at: [0.48, -0.6],
            style: ClaspStyle::Cross,
            size: 0.02,
            jewel: None,
            finish: Finish::default(),
        }),
        TrimSpec::Decal(Decal {
            source: star(),
            anchor: DecalAnchor::Surface { at: [0.35, std::f64::consts::PI] },
            size: [0.03, 0.03],
            rotation: 15.0,
            opacity: 0.9,
        }),
        lace(LacePattern::Net, true),
        lace(LacePattern::Eyelet, false),
    ]
}

// ------------------------------------------------------------------------------------------------------- the suite

#[test]
fn every_trim_passes_the_garment_suite_on_every_template_and_shape() {
    let params = TrimmedParams { trims: trims(), ..TrimmedParams::default() };
    conformance::assert_conforms::<Trimmed>(&params, &services());
}

// --------------------------------------------------------------------------------------------------------- chains

#[test]
fn a_chain_follows_its_parabola_within_3_percent_at_five_sags() {
    let (a, b) = (DVec3::new(-0.1, 1.0, 0.2), DVec3::new(0.1, 1.0, 0.2));
    for sag in [0.005, 0.015, 0.026, 0.05, 0.1] {
        let hang = Hang { from: a, to: b, up: DVec3::Y, out: DVec3::Z, sag, bow: chain::SAG.1 };
        let links = chain::links(&hang, chain::PITCH, None, 0.002).unwrap();
        // Each link's centre lies on the parabola: its distance to the densely sampled curve.
        let curve: Vec<DVec3> = (0..=4000).map(|i| hang.at(f64::from(i) / 4000.0)).collect();
        let off = links
            .iter()
            .map(|l| curve.iter().map(|c| c.distance(l.centre)).fold(f64::INFINITY, f64::min))
            .fold(0.0, f64::max);
        // And the chain sags as deep as the parabola: the lowest link against the curve's lowest point.
        let lowest = links.iter().map(|l| a.y - l.centre.y).fold(0.0, f64::max);
        println!("sag {sag}: {} links, {:.3} mm off the curve, lowest {:.2} mm", links.len(), off * 1e3, lowest * 1e3);
        assert!(off <= 0.03 * sag, "sag {sag}: a link is {off} m off its curve");
        assert!((lowest - sag).abs() <= 0.03 * sag, "sag {sag}: the chain sags {lowest}, the curve {sag}");
        assert!(links.len() >= chain::MIN_LINKS as usize);
        let total: f64 = curve.windows(2).map(|w| w[0].distance(w[1])).sum();
        let expected = (total / chain::PITCH).round().max(f64::from(chain::MIN_LINKS)) as usize;
        assert_eq!(links.len(), expected, "one link per pitch");
    }
    let still = Hang { from: a, to: a, up: DVec3::Y, out: DVec3::Z, sag: 0.0, bow: 0.0 };
    assert!(chain::links(&still, chain::PITCH, None, 0.002).is_err(), "no length");
}

#[test]
fn a_draped_chain_rests_on_the_garment_and_carries_its_cross() {
    let fit: Arc<dyn BodyFit> = Arc::new(TorsoFit::new(&omuzaki_char_api::BodyParams::default()));
    let surface = Surface::new(fit, BodyRegion::Torso, 2.0, 0.004).unwrap();
    let spec = Chain {
        from: [0.6, -0.6],
        to: [0.6, 0.6],
        sag: chain::SAG.0,
        bow: chain::SAG.1,
        link_size: chain::LINK.0,
        link_count: Some(20),
        pendant: false,
        finish: Finish::default(),
    };
    let plain = chain::build(&surface, &spec).unwrap();
    for p in &plain.mesh().positions {
        let at = surface.project(p.as_dvec3()).unwrap().expect("over the torso");
        assert!(at.outside >= 0.0, "a link inside the garment: {at:?}");
    }
    let with_cross = chain::build(&surface, &Chain { pendant: true, ..spec.clone() }).unwrap();
    assert!(with_cross.vertex_count() > plain.vertex_count(), "the cross is added");
    assert!(chain::build(&surface, &Chain { sag: 3.0, ..spec }).is_err());
}

// --------------------------------------------------------------------------------------------------------- decals

fn closest_on_triangle(p: DVec3, a: DVec3, b: DVec3, c: DVec3) -> DVec3 {
    // Clamp the barycentric solution by brute force over the triangle's edges and interior: small meshes only.
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d00, d01, d11) = (ab.dot(ab), ab.dot(ac), ac.dot(ac));
    let (d20, d21) = (ap.dot(ab), ap.dot(ac));
    let denom = d00 * d11 - d01 * d01;
    let v = (d11 * d20 - d01 * d21) / denom;
    let w = (d00 * d21 - d01 * d20) / denom;
    if v >= 0.0 && w >= 0.0 && v + w <= 1.0 {
        return a + ab * v + ac * w;
    }
    let on = |s: DVec3, e: DVec3| s + (e - s) * ((p - s).dot(e - s) / (e - s).length_squared()).clamp(0.0, 1.0);
    [on(a, b), on(b, c), on(c, a)].into_iter().min_by(|x, y| x.distance(p).total_cmp(&y.distance(p))).unwrap_or(a)
}

#[test]
fn decals_sit_just_off_the_surface_and_in_front_of_the_garment_on_every_template() {
    let decal =
        |anchor| TrimSpec::Decal(Decal { source: star(), anchor, size: [0.03, 0.02], rotation: 30.0, opacity: 1.0 });
    let params = TrimmedParams {
        trims: vec![
            decal(DecalAnchor::Surface { at: [0.4, 0.2] }),
            decal(DecalAnchor::Anchor { part: "mount".parse().unwrap(), anchor: "crest".parse().unwrap() }),
        ],
        ..TrimmedParams::default()
    };
    let value = serde_json::to_value(&params).unwrap();
    for template in conformance::TEMPLATES {
        for (label, shape) in conformance::shapes() {
            let def = conformance::character(
                template,
                &shape,
                &[("mount", "test-mount", 5, json!({})), ("coat", "test-trimmed", LAYER_SHIRT, value.clone())],
            );
            let stack = conformance::build(def, &services()).unwrap();
            let output = stack.output("coat");
            let fit = stack.before("coat").outer_fit().unwrap().clone();
            let surface = Surface::new(fit, BodyRegion::Torso, 2.0, params.common.ease).unwrap();
            let tube = output.meshes.iter().find(|m| m.name == "coat-tube").unwrap();
            let g = &tube.geometry;
            let triangles: Vec<[DVec3; 3]> =
                g.indices.chunks_exact(3).map(|t| [0, 1, 2].map(|k| g.positions[t[k] as usize].as_dvec3())).collect();
            for name in ["coat-trim0", "coat-trim1"] {
                let mesh = output.meshes.iter().find(|m| m.name == name).unwrap();
                for p in &mesh.geometry.positions {
                    let p = p.as_dvec3();
                    let off = surface.project(p).unwrap().expect("over the torso").outside;
                    assert!(
                        (0.0002..=0.0005).contains(&off),
                        "{template}/{label}: {name} is {:.3} mm off the surface",
                        off * 1e3
                    );
                    let normal = (p - surface.ring(surface.project(p).unwrap().unwrap().t).unwrap().centre).normalize();
                    let front = triangles
                        .iter()
                        .map(|[a, b, c]| closest_on_triangle(p, *a, *b, *c))
                        .min_by(|x, y| x.distance(p).total_cmp(&y.distance(p)))
                        .unwrap();
                    assert!(
                        (p - front).dot(normal) > 0.0001,
                        "{template}/{label}: {name} is not in front of the garment"
                    );
                }
            }
        }
    }
}

/// Assets from memory.
struct Memory(HashMap<ContentHash, Arc<[u8]>>);

impl AssetSource for Memory {
    fn get(&self, hash: &ContentHash) -> Result<Arc<[u8]>, AssetError> {
        self.0.get(hash).cloned().ok_or(AssetError::NotFound(*hash))
    }
}

fn raw_image(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = IMAGE_MAGIC.to_vec();
    bytes.extend(width.to_le_bytes());
    bytes.extend(height.to_le_bytes());
    bytes.extend((0..width * height * 4).map(|k| (k % 251) as u8));
    bytes
}

fn image_decal(hash: ContentHash) -> TrimmedParams {
    TrimmedParams {
        trims: vec![TrimSpec::Decal(Decal {
            source: DecalSource::Image { hash },
            anchor: DecalAnchor::Surface { at: [0.4, 0.0] },
            size: [0.04, 0.03],
            rotation: 0.0,
            opacity: 1.0,
        })],
        ..TrimmedParams::default()
    }
}

fn build_with(params: &TrimmedParams, services: &BuildServices) -> Result<conformance::Stack, BuildError> {
    let def = conformance::character(
        "standard",
        &BodyShape::default(),
        &[("coat", "test-trimmed", LAYER_SHIRT, serde_json::to_value(params).unwrap())],
    );
    conformance::build(def, services)
}

#[test]
fn a_store_image_decal_needs_an_asset_source_and_says_so() {
    let bytes = raw_image(8, 6);
    let hash = ContentHash::of(&bytes);
    let params = image_decal(hash);
    let error = build_with(&params, &services()).err().expect("no asset source");
    assert!(matches!(&error, BuildError::Asset { error: AssetError::Unavailable, .. }), "{error}");
    assert!(error.to_string().contains("no asset reader"), "{error}");

    let store = Memory(HashMap::from([(hash, Arc::from(bytes.clone()))]));
    let stack = build_with(&params, &services().with_assets(store)).unwrap();
    let (_, texture) = stack.output("coat").textures.iter().find(|(n, _)| n.as_str() == "coat-trim0").unwrap();
    assert_eq!((texture.desc.size.x, texture.desc.size.y), (8, 6));
    assert_eq!(texture.rgba8, bytes[16..]);

    let wrong = ContentHash::of(b"not stored");
    let store = Memory(HashMap::from([(wrong, Arc::from(b"PNG....".to_vec()))]));
    let error = build_with(&image_decal(wrong), &services().with_assets(store)).err().expect("not a raw image");
    assert!(error.to_string().contains("OMZRGBA8"), "{error}");
}

#[test]
fn raw_images_are_checked_before_they_are_used() {
    assert!(decal::read_image(&raw_image(3, 2)).is_ok());
    let mut short = raw_image(3, 2);
    short.pop();
    assert!(decal::read_image(&short).is_err(), "a byte short");
    assert!(decal::read_image(&raw_image(0, 2)).is_err(), "no width");
    let mut huge = IMAGE_MAGIC.to_vec();
    huge.extend(100_000_u32.to_le_bytes());
    huge.extend(1_u32.to_le_bytes());
    assert!(decal::read_image(&huge).is_err(), "too wide");
    assert!(decal::read_image(b"OMZ").is_err());
}

// ----------------------------------------------------------------------------------------------------------- lace

fn paint_lace(pattern: LacePattern) -> Vec<u8> {
    let (list, rect) = lace::scallop(pattern, Colour::rgb(250, 246, 238));
    let desc = lace::texture_desc();
    let px_per_m = desc.size.x as f32 / rect.width();
    let spec = FaceLayerSpec::for_rect(rect, FaceLayerId::Base, px_per_m);
    assert_eq!(spec.size_px(), (desc.size.x, desc.size.y), "square pixels");
    let mut raster = Raster::new(desc.size.x, desc.size.y);
    let factory = services().rasterizer.unwrap();
    factory().rasterize(&spec, &list, px_per_m, &mut raster).unwrap();
    raster.pixels
}

#[test]
fn the_lace_texture_is_deterministic_and_cut_by_its_alpha() {
    for pattern in [LacePattern::Net, LacePattern::Eyelet, LacePattern::Picot] {
        let first = paint_lace(pattern);
        assert_eq!(paint_lace(pattern), first, "{pattern:?}: deterministic");
        let alpha: Vec<u8> = first.chunks_exact(4).map(|p| p[3]).collect();
        let clear = alpha.iter().filter(|a| **a < 128).count();
        assert!(clear > alpha.len() / 10 && clear < alpha.len() * 9 / 10, "{pattern:?}: cut, {clear} clear pixels");
        let width = lace::TILE_PX as usize;
        assert!(alpha[..width].iter().all(|a| *a > 128), "{pattern:?}: the heading band along the rail is solid");
    }
}

#[test]
fn trim_specs_round_trip_and_validate() {
    for trim in trims() {
        let value = serde_json::to_value(&trim).unwrap();
        assert_eq!(serde_json::from_value::<TrimSpec>(value).unwrap(), trim);
        trim.validate().unwrap();
    }
    let bad = [
        TrimSpec::Chain(Chain {
            from: [0.4, 0.0],
            to: [0.4, 1.0],
            sag: -0.1,
            bow: 0.0,
            link_size: 0.005,
            link_count: None,
            pendant: false,
            finish: Finish::default(),
        }),
        TrimSpec::Lace(Lace {
            rail: rail(&[[0.4, 0.0], [0.4, 1.0]], false),
            scallop_radius: 0.004,
            scallop_count: 0,
            pattern: LacePattern::Net,
            colour: ColourRef::rgb(255, 255, 255),
            flip: false,
        }),
        TrimSpec::Decal(Decal {
            source: star(),
            anchor: DecalAnchor::Surface { at: [0.4, 0.0] },
            size: [0.0, 0.02],
            rotation: 0.0,
            opacity: 1.0,
        }),
    ];
    for trim in bad {
        assert!(trim.validate().is_err(), "{trim:?}");
    }
}
