//! Benchmarks for the caching character builder against the reference one: a full build, an edit of one hair
//! parameter (only the hair may rebuild), an edit that does not reach the hair (only the vest rebuilds; the hair's
//! simulation is forked from the cache), the price of that fork against the hair's generator, an unchanged character,
//! a palette change through `rebuild_materials`, and how a wide character scales with threads. Run with
//! `cargo bench -p omuzaki-char --bench builder`.
//!
//! Building runs on every parameter edit, not every frame, so the targets are interactivity: a hair edit costs at most
//! 40% of a full build. A palette change never rebuilds geometry (decision Q6): a swap that reaches only material-only
//! parts costs at most 1 ms and a whole colourway swap, texture repaints included, at most 5 ms. Before measuring a
//! swap the bench checks that no generator ran for it (`GENERATORS` counts the bench kinds' builds) and which parts
//! were recoloured.
//!
//! Three characters are measured. `mock` is the mock character (five parts, 900 vertices, the hair is most of its
//! cost). `dressed` adds six props of 4 225 vertices each, the size of a garment, which is closer to a real wardrobe.
//! `wardrobe` adds, on top of `dressed`, two hair stand-ins that bake palette colours into 4 225 vertex colours each
//! and two cloth garments with a plaid painted into a 256 x 256 texture by the mock rasteriser (the production CPU
//! rasteriser will cost different amounts). The extra kinds are bench-only, registered below.

#![allow(clippy::expect_used)] // a character that fails to build should stop the run with its reason

use std::collections::BTreeSet;
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use omuzaki_char::builder::{BuildTimings, CachingBuilder};
use omuzaki_char_api::{
    Assembler, BuildCtx, BuildError, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder,
    CharacterDef, MaterialName, MaterialSpec, MeshData, PartCategory, PartDef, PartId, PartKind, PartKindId, PartMesh,
    PartMeta, PartOutput, RecolourCtx, Recoloured, RegistryBuilder, TextureName, build_part, plan,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{UVec2, Vec2, Vec3};
use omuzaki_face_api::{Check, ColourRef, DrawList, FaceRect, Paint, PaletteSlot, Validate};
use omuzaki_face_mock::MockRasterizer;
use omuzaki_render_api::TextureDesc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct PropParams {
    cells: u32,
    tint: ColourRef,
}

impl Default for PropParams {
    fn default() -> Self {
        Self { cells: 64, tint: ColourRef::slot_or("prop", ColourRef::rgb(200, 200, 200)) }
    }
}

impl Validate for PropParams {
    fn check(&self, c: &mut Check) {
        c.range("cells", f64::from(self.cells), 2.0, 256.0);
    }
}

/// A silent part of garment weight: a rippled grid with `cells`² quads.
struct Prop;

impl PartKind for Prop {
    type Params = PropParams;

    fn meta() -> PartMeta {
        PartMeta { id: "bench-prop", summary: "A rippled grid.", category: PartCategory::Garment, requires: &[] }
    }

    #[allow(clippy::cast_precision_loss)] // grid coordinates are tiny
    fn build(params: &PropParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        GENERATORS.fetch_add(1, Ordering::Relaxed);
        let side = (params.cells + 1) as usize;
        let height = relaxed_ripple(side, &mut ctx.rng);
        let mut positions = Vec::with_capacity(side * side);
        let mut normals = Vec::with_capacity(side * side);
        for j in 0..side {
            for i in 0..side {
                positions.push(Vec3::new(i as f32 * 0.005, j as f32 * 0.005, height[j * side + i]));
                // Central differences, clamped at the border.
                let at = |i: usize, j: usize| height[j.min(side - 1) * side + i.min(side - 1)];
                let (dx, dy) = (at(i + 1, j) - at(i.saturating_sub(1), j), at(i, j + 1) - at(i, j.saturating_sub(1)));
                normals.push(Vec3::new(-dx, -dy, 0.01).normalize());
            }
        }
        let cells = params.cells as usize;
        let mut indices = Vec::with_capacity(cells * cells * 6);
        for j in 0..cells {
            for i in 0..cells {
                let a = u32::try_from(j * side + i).map_err(|_| ctx.fail("too many vertices"))?;
                let s = u32::try_from(side).map_err(|_| ctx.fail("too many vertices"))?;
                indices.extend([a, a + 1, a + s, a + 1, a + s + 1, a + s]);
            }
        }
        let id = MaterialName::new(format!("bench-prop-{}", ctx.part)).map_err(|e| ctx.fail(e.to_string()))?;
        let mesh = PartMesh {
            name: "prop".to_owned(),
            geometry: MeshData { positions, normals, indices, ..MeshData::default() },
            joint_bones: Vec::new(),
            material: id.clone(),
            casts_shadow: true,
            render_order: 0,
            dynamic: false,
        };
        let material = MaterialSpec::toon(ctx.colour(&params.tint)?);
        Ok(PartOutput { meshes: vec![mesh], materials: vec![(id, material)], ..PartOutput::default() })
    }

    fn recolour(params: &PropParams, output: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        let [(_, material)] = output.materials.as_mut_slice() else { return Ok(Recoloured::NeedsRebuild) };
        *material = MaterialSpec::toon(ctx.colour(&params.tint)?);
        Ok(Recoloured::Done)
    }
}

/// How many bench-kind generators have run: a palette swap must leave it where it was.
static GENERATORS: AtomicUsize = AtomicUsize::new(0);

/// Smoothing passes over the grid: stands in for the lofting, relaxation and normal work of a real garment, so a
/// prop costs about a millisecond like the kit's own surfaces do (`benches/kit.rs`).
const RELAX_PASSES: usize = 80;

/// A ripple with jitter from the part's stream, relaxed by repeated neighbour averaging.
#[allow(clippy::cast_precision_loss)] // grid coordinates are tiny
fn relaxed_ripple(side: usize, rng: &mut omuzaki_core::rng::Rng) -> Vec<f32> {
    let mut height: Vec<f32> = (0..side * side)
        .map(|k| {
            let (x, y) = ((k % side) as f32 * 0.005, (k / side) as f32 * 0.005);
            0.004 * (x * 40.0).sin() * (y * 30.0).cos() + rng.range_f32(0.0, 1e-3)
        })
        .collect();
    let mut previous = height.clone();
    for _ in 0..RELAX_PASSES {
        previous.copy_from_slice(&height);
        for j in 1..side - 1 {
            for i in 1..side - 1 {
                let k = j * side + i;
                height[k] = 0.5 * previous[k]
                    + 0.125 * (previous[k - 1] + previous[k + 1] + previous[k - side] + previous[k + side]);
            }
        }
    }
    height
}
omuzaki_char_api::register_part_kind!(Prop);

/// A prop's geometry with a fixed colour, so the kinds built on it name only the colours of their own parameters (a
/// kind must not read the palette behind its parameters' back).
fn white_prop(cells: u32) -> PropParams {
    PropParams { cells, tint: ColourRef::rgb(255, 255, 255) }
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct DyedParams {
    cells: u32,
    root: ColourRef,
    tip: ColourRef,
}

impl Default for DyedParams {
    fn default() -> Self {
        Self {
            cells: 64,
            root: ColourRef::slot_or("hair_root", ColourRef::rgb(90, 60, 40)),
            tip: ColourRef::slot_or("trim", ColourRef::rgb(230, 230, 230)),
        }
    }
}

impl Validate for DyedParams {
    fn check(&self, c: &mut Check) {
        c.range("cells", f64::from(self.cells), 2.0, 256.0);
    }
}

/// A gradient from `root` at the bottom of the mesh to `tip` at the top, in linear light, as vertex colours.
fn gradient(positions: &[Vec3], root: Colour, tip: Colour) -> Vec<[f32; 4]> {
    let top = positions.iter().map(|p| p.y).fold(f32::EPSILON, f32::max);
    let (root, tip) = (root.to_linear(), tip.to_linear());
    positions
        .iter()
        .map(|p| {
            let c = root.lerp(tip, (p.y / top).clamp(0.0, 1.0));
            [c.x, c.y, c.z, 1.0]
        })
        .collect()
}

/// A silent stand-in for hair: a grid whose palette colours are baked into its vertex colours. It recolours by
/// rewriting them.
struct Dyed;

impl PartKind for Dyed {
    type Params = DyedParams;

    fn meta() -> PartMeta {
        PartMeta { id: "bench-dyed", summary: "A grid with baked tints.", category: PartCategory::Hair, requires: &[] }
    }

    fn build(params: &DyedParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        // The geometry is the prop's: same grid, same relaxation.
        let mut out = Prop::build(&white_prop(params.cells), ctx)?;
        let [mesh] = out.meshes.as_mut_slice() else { return Err(ctx.fail("a prop has one mesh")) };
        mesh.geometry.colours = gradient(&mesh.geometry.positions, ctx.colour(&params.root)?, ctx.colour(&params.tip)?);
        if let Some((_, material)) = out.materials.first_mut() {
            material.vertex_colours = true;
        }
        Ok(out)
    }

    fn recolour(params: &DyedParams, output: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        let [mesh] = output.meshes.as_mut_slice() else { return Ok(Recoloured::NeedsRebuild) };
        mesh.geometry.colours = gradient(&mesh.geometry.positions, ctx.colour(&params.root)?, ctx.colour(&params.tip)?);
        Ok(Recoloured::Done)
    }
}
omuzaki_char_api::register_part_kind!(Dyed);

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct PlaidParams {
    cells: u32,
    cloth: ColourRef,
    stripe: ColourRef,
}

impl Default for PlaidParams {
    fn default() -> Self {
        Self {
            cells: 64,
            cloth: ColourRef::slot_or("cloth", ColourRef::rgb(200, 190, 170)),
            stripe: ColourRef::slot_or("trim", ColourRef::rgb(120, 40, 40)),
        }
    }
}

impl Validate for PlaidParams {
    fn check(&self, c: &mut Check) {
        c.range("cells", f64::from(self.cells), 2.0, 256.0);
    }
}

/// Texture side in pixels: a garment pattern at live quality.
const PLAID_PX: u32 = 256;
const PLAID_RECT: FaceRect = FaceRect { x0: 0.0, x1: 0.08, y0: 0.0, y1: 0.08 };

/// A cloth ground with a grid of stripes, as a procedural plaid would draw: 1 ground and 8 stripes.
fn plaid_list(cloth: Colour, stripe: Colour) -> DrawList {
    let mut list = DrawList::new();
    let mut fill = |x0: f32, x1: f32, y0: f32, y1: f32, colour: Colour| {
        let pts = [Vec2::new(x0, y0), Vec2::new(x1, y0), Vec2::new(x1, y1), Vec2::new(x0, y1)];
        let path = list.path().polyline(&pts).close().finish();
        list.fill(path, Paint::Solid(colour));
    };
    fill(0.0, 0.08, 0.0, 0.08, cloth);
    for k in 0..4_u8 {
        let at = 0.005 + 0.02 * f32::from(k);
        fill(at, at + 0.008, 0.0, 0.08, stripe);
        fill(0.0, 0.08, at, at + 0.008, stripe);
    }
    list
}

/// A silent stand-in for a cloth garment: a prop whose pattern is painted through the rasteriser. It recolours by
/// repainting the texture and re-resolving the base colour.
struct Plaid;

impl PartKind for Plaid {
    type Params = PlaidParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "bench-plaid",
            summary: "A grid with a painted plaid.",
            category: PartCategory::Garment,
            requires: &[],
        }
    }

    fn build(params: &PlaidParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let mut out = Prop::build(&white_prop(params.cells), ctx)?;
        let (cloth, stripe) = (ctx.colour(&params.cloth)?, ctx.colour(&params.stripe)?);
        let list = plaid_list(cloth, stripe);
        let texture = ctx.paint_texture(UVec2::splat(PLAID_PX), PLAID_RECT, &list, TextureDesc::colour(UVec2::ONE))?;
        let name = TextureName::new(format!("bench-plaid-{}", ctx.part)).map_err(|e| ctx.fail(e.to_string()))?;
        if let Some((_, material)) = out.materials.first_mut() {
            material.base = cloth;
            material.base_texture = Some(name.clone());
        }
        out.textures.push((name, texture));
        Ok(out)
    }

    fn recolour(
        params: &PlaidParams,
        output: &mut PartOutput,
        ctx: &RecolourCtx<'_>,
    ) -> Result<Recoloured, BuildError> {
        let ([(_, material)], [(_, texture)]) = (output.materials.as_mut_slice(), output.textures.as_mut_slice())
        else {
            return Ok(Recoloured::NeedsRebuild);
        };
        let (cloth, stripe) = (ctx.colour(&params.cloth)?, ctx.colour(&params.stripe)?);
        *texture = ctx.paint_texture(texture.desc.size, PLAID_RECT, &plaid_list(cloth, stripe), texture.desc)?;
        material.base = cloth;
        Ok(Recoloured::Done)
    }
}
omuzaki_char_api::register_part_kind!(Plaid);

fn mock() -> CharacterDef {
    mock_character().expect("the mock character parses")
}

/// The mock character with `props` props of `cells`² quads, each tinted from the `prop` palette slot.
fn dressed(props: usize, cells: u32) -> CharacterDef {
    let mut def = mock();
    def.palette.0.insert(PaletteSlot::new("prop").expect("a slug"), Colour::rgb(90, 60, 50));
    for i in 0..props {
        let part = PartDef {
            kind: PartKindId::new("bench-prop").expect("a slug"),
            parent: None,
            enabled: true,
            layer: 0,
            attach: None,
            params: json!({ "cells": cells }),
        };
        def.parts.insert(PartId::new(format!("prop-{i}")).expect("a slug"), part);
    }
    def
}

/// `dressed` plus two hair stand-ins and two plaid garments, which name the `hair_root`, `cloth` and `trim` slots.
fn wardrobe() -> CharacterDef {
    let mut def = dressed(6, 64);
    for (name, colour) in [("hair_root", [90, 60, 40]), ("cloth", [200, 190, 170]), ("trim", [120, 40, 40])] {
        def.palette.0.insert(PaletteSlot::new(name).expect("a slug"), Colour::rgb(colour[0], colour[1], colour[2]));
    }
    for (name, kind) in
        [("dyed-0", "bench-dyed"), ("dyed-1", "bench-dyed"), ("plaid-0", "bench-plaid"), ("plaid-1", "bench-plaid")]
    {
        let part = PartDef {
            kind: PartKindId::new(kind).expect("a slug"),
            parent: None,
            enabled: true,
            layer: 0,
            attach: None,
            params: json!({}),
        };
        def.parts.insert(PartId::new(name).expect("a slug"), part);
    }
    def
}

fn id() -> CharId {
    CharId::new("bench").expect("a slug")
}

fn names(t: &BuildTimings) -> BTreeSet<String> {
    t.built().map(ToString::to_string).collect()
}

/// Edits the hair so the part's key is new every time: the cache can never answer it.
fn nudge_hair(def: &mut CharacterDef, n: u32) {
    let hair = def.parts.get_mut(&PartId::new("hair").expect("a slug")).expect("the mock has hair");
    hair.params = json!({
        "groups": {
            "a-cap": { "generator": { "type": "cap" } },
            "c-curtain": { "generator": { "type": "curtain" }, "dynamics": { "simulate": true, "follow": "body" } }
        },
        "shading": { "ring_strength": 0.1 + f64::from(n % 1000) * 0.0005 }
    });
}

fn edit_benches(c: &mut Criterion) {
    let options = BuildOptions::default();
    let services = BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer));
    for (label, def) in [("mock", mock()), ("dressed", dressed(6, 64)), ("wardrobe", wardrobe())] {
        let mut group = c.benchmark_group(format!("builder/{label}"));
        group.bench_function("full/registry", |b| {
            b.iter(|| black_box(RegistryBuilder.build(&id(), black_box(&def), &options, &services).expect("builds")));
        });
        let builder = CachingBuilder::new();
        builder.build(&id(), &def, &options, &services).expect("builds"); // learns which kinds are silent
        group.bench_function("full/caching-cold", |b| {
            b.iter_batched(
                || builder.clear(),
                |()| black_box(builder.build(&id(), black_box(&def), &options, &services).expect("builds")),
                BatchSize::SmallInput,
            );
        });

        // One hair parameter: only the hair may be built (the timings say so before anything is measured).
        let (mut edited, mut n) = (def.clone(), 0_u32);
        builder.build(&id(), &edited, &options, &services).expect("builds");
        nudge_hair(&mut edited, 1);
        let (_, timings) = builder.build_timed(&id(), &edited, &options, &services).expect("builds");
        assert_eq!(names(&timings), BTreeSet::from(["hair".to_owned()]), "a hair edit rebuilds only the hair");
        group.bench_function("edit/hair", |b| {
            b.iter(|| {
                n += 1;
                nudge_hair(&mut edited, n);
                black_box(builder.build(&id(), black_box(&edited), &options, &services).expect("builds"))
            });
        });

        // An edit that does not reach the hair (the outermost garment's fit): only the vest may be built, and the
        // hair's simulation comes from the cache by forking. Before `PartRuntime::fork` the hair was rebuilt as well.
        let (mut edited, mut n) = (def.clone(), 0_u32);
        let nudge_vest = |def: &mut CharacterDef, n: u32| {
            let vest = def.parts.get_mut(&PartId::new("vest").expect("a slug")).expect("the mock has a vest");
            vest.params = json!({ "ease": 0.005 + f64::from(n % 1000) * 0.00001, "colour": "@outfit" });
        };
        builder.build(&id(), &edited, &options, &services).expect("builds");
        nudge_vest(&mut edited, 1);
        let (_, timings) = builder.build_timed(&id(), &edited, &options, &services).expect("builds");
        assert_eq!(names(&timings), BTreeSet::from(["vest".to_owned()]), "a vest edit rebuilds only the vest");
        group.bench_function("edit/vest", |b| {
            b.iter(|| {
                n += 1;
                nudge_vest(&mut edited, n);
                black_box(builder.build(&id(), black_box(&edited), &options, &services).expect("builds"))
            });
        });

        // Nothing changed: every part, the hair included, is served from the cache.
        group.bench_function("rebuild/unchanged", |b| {
            b.iter(|| black_box(builder.build(&id(), black_box(&def), &options, &services).expect("builds")));
        });

        // The body is first in build order and everything follows a publisher: the worst edit.
        let (mut edited, mut n) = (def.clone(), 0_u32);
        group.bench_function("edit/body", |b| {
            b.iter(|| {
                n += 1;
                let body = edited.parts.get_mut(&PartId::new("body").expect("a slug")).expect("the mock has a body");
                body.params = json!({ "proportions": { "torso_length": 0.9 + f64::from(n % 1000) * 0.0002 } });
                black_box(builder.build(&id(), black_box(&edited), &options, &services).expect("builds"))
            });
        });

        // Palette changes through `rebuild_materials`; none may build any geometry. Only the vest names `@outfit`, so one
        // slot is cheap. A colourway swap of the skin and outfit recolours the body, head and vest. The props name
        // `@prop`, the hair stand-ins `@hair_root` and `@trim`, the plaids `@cloth` and `@trim` (a texture repaint
        // each); a swap of all slots reaches everything. The simulated mock hair is forked from the cache like any
        // part and names no slot, so it is served untouched.
        let mut character: BuiltCharacter = builder.build(&id(), &def, &options, &services).expect("builds");
        let swaps: [(&str, &[&str]); 6] = [
            ("one-slot", &["outfit"]),
            ("colourway", &["skin", "skin_shade", "skin_line", "outfit"]),
            ("props", &["prop"]),
            ("textures", &["cloth", "trim"]),
            ("hair-tints", &["hair_root"]),
            ("all-slots", &["skin", "skin_shade", "skin_line", "outfit", "prop", "hair_root", "cloth", "trim"]),
        ];
        for (name, slots) in swaps {
            // Every swap is a palette the cache has never seen (24 bits of colour), or the bench would measure hits.
            let mut n = 0_u32;
            let mut swap = |character: &mut BuiltCharacter| {
                n += 1;
                let [red, green, blue, _] = n.to_le_bytes();
                let mut palette = character.palette.clone();
                for slot in slots {
                    palette.0.insert(PaletteSlot::new(*slot).expect("a slug"), Colour::rgb(red, green, blue));
                }
                builder.rebuild_materials(character, &palette, &options, &services).expect("rebuilds")
            };
            let generators = GENERATORS.load(Ordering::Relaxed);
            let timings = swap(&mut character);
            assert_eq!(GENERATORS.load(Ordering::Relaxed), generators, "{label}/{name}: a swap ran a generator");
            assert!(names(&timings).is_empty(), "{label}/{name}: built {:?}", names(&timings));
            group.bench_function(format!("palette/{name}"), |b| b.iter(|| black_box(swap(&mut character))));
        }
        group.finish();
    }
}

/// The mock character with `strands` strands in its simulated curtain.
fn with_curtain(strands: u32) -> CharacterDef {
    let mut def = mock();
    let hair = def.parts.get_mut(&PartId::new("hair").expect("a slug")).expect("the mock has hair");
    hair.params = json!({
        "groups": {
            "a-cap": { "generator": { "type": "cap" } },
            "c-curtain": {
                "generator": { "type": "curtain", "count": strands },
                "dynamics": { "simulate": true, "follow": "body" }
            }
        }
    });
    def
}

/// What serving a simulated part from the cache costs against building it: `PartRuntime::fork` of the cache's template
/// (the price of a hit) and the hair's generator (the price of a miss), the same hair, back to back. The hair builds
/// against what the body and head published, so that view is made first. `strands` is the curtain's weight.
fn fork_benches(c: &mut Criterion) {
    let (options, services) = (BuildOptions::default(), BuildServices::default());
    let mut group = c.benchmark_group("builder/hair");
    for strands in [26, 60] {
        let def = with_curtain(strands);
        let hair = PartId::new("hair").expect("a slug");
        let id = id();
        let mut assembler = Assembler::new(&id, &def, &options);
        for part in plan(&def).expect("plans").into_iter().take_while(|part| *part != hair) {
            let output = build_part(&def, &part, &options, &services, assembler.published()).expect("builds");
            assembler.add(&part, output).expect("merges");
        }
        let view = assembler.published().clone();
        let template = build_part(&def, &hair, &options, &services, &view)
            .expect("builds")
            .runtime
            .expect("the mock hair simulates");
        group.bench_function(BenchmarkId::new("generate", strands), |b| {
            b.iter(|| black_box(build_part(black_box(&def), &hair, &options, &services, &view).expect("builds")));
        });
        group.bench_function(BenchmarkId::new("fork", strands), |b| {
            b.iter(|| black_box(black_box(&template).fork().expect("the mock hair forks")));
        });
    }
    group.finish();
}

fn thread_benches(c: &mut Criterion) {
    let (options, services) = (BuildOptions::default(), BuildServices::default());
    let def = dressed(8, 96);
    let mut group = c.benchmark_group("builder/wide-cold");
    for threads in [1, 2, 4, 8] {
        let builder = CachingBuilder::new().with_threads(threads);
        builder.build(&id(), &def, &options, &services).expect("builds"); // learns which kinds are silent
        group.bench_with_input(BenchmarkId::new("threads", threads), &threads, |b, _| {
            b.iter_batched(
                || builder.clear(),
                |()| black_box(builder.build(&id(), black_box(&def), &options, &services).expect("builds")),
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group! {
    name = builder;
    config = Criterion::default().warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(3)).sample_size(40);
    targets = edit_benches, fork_benches, thread_benches
}
criterion_main!(builder);
