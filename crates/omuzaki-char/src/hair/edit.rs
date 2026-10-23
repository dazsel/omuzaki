//! Guide-curve editing (CB-32, #180): the pure edits behind the `hair.guide.*` ops, and freezing any generator's
//! clumps into explicit guides a person can drag point by point.
//!
//! **Freezing.** [`freeze`] builds the character up to the hair part and compiles the group as the build would, then
//! turns each clump's rest curve into a [`GuideClump`]: a few control points (evenly spread rest points, then the
//! ones the curve misses most) whose curve ([`guides::through`]) stays within [`FREEZE_TOLERANCE`] of the original, the root anchored to the head's surface and each other point where it lies near it ([`guides::anchored`]). A strip (the curtain) or a shell (the cap) becomes
//! tubes along the same curves: the guides keep the shape, not the way it was drawn.
//!
//! **Mirror and jitter.** [`mirrored`] reflects every point left to right ([`GuidePoint::mirrored`]), so twice is the
//! identity. [`jittered`] moves every point but the root by up to `amount` metres, the same for the same seed.

use std::cell::RefCell;
use std::sync::Arc;

use omuzaki_char_api::hair::MAX_GUIDE_POINTS;
use omuzaki_char_api::{
    Assembler, BuildCtx, BuildError, BuildOptions, BuildServices, CharId, CharacterDef, Generator, GroupId, GuideClump,
    GuidePoint, GuidesGen, HairClump, HairParams, HeadSurface, PartId, build_part, part_rng, plan,
};
use omuzaki_core::glam::Vec3;
use omuzaki_core::rng::Rng;

use super::compile::{CompiledGroup, Dispatch, GroupGenerator, GroupGeometry, HairGenCtx};
use super::r#gen::guides;

/// A frozen guide's curve stays this close to the clump it was made from, metres.
pub const FREEZE_TOLERANCE: f32 = 0.0005;
/// A point within this of the head's surface anchors to it, metres.
pub const NEAR: f32 = 0.03;
/// A frozen clump's root anchors to the head within this, metres: the farthest a surface point may stand off it.
const ROOT_NEAR: f32 = 0.3;
/// Fewest control points a frozen guide starts with.
const FREEZE_START: usize = 6;
/// A head's radius, for turning a jitter in metres into degrees on the surface.
const HEAD_RADIUS: f64 = 0.09;

/// Builds `def` up to its part `part` (the parts it needs, in build order) and calls `f` with that part's build context,
/// the one the part itself would get. Every texture is left blank: only geometry is read.
///
/// # Errors
/// What a part before `part` fails with, or what `f` returns.
pub fn at_part<R>(
    id: &CharId,
    def: &CharacterDef,
    part: &PartId,
    services: &BuildServices,
    f: impl FnOnce(&mut BuildCtx<'_>) -> Result<R, BuildError>,
) -> Result<R, BuildError> {
    let options = BuildOptions::default();
    let mut assembler = Assembler::new(id, def, &options);
    for next in plan(def)? {
        if next == *part {
            let part_def = def.parts.get(part).ok_or_else(|| BuildError::Part {
                part: part.clone(),
                message: "the character has no such part".to_owned(),
            })?;
            let mut ctx = BuildCtx::new(
                part,
                &def.template,
                &def.palette,
                &def.face,
                part_rng(def.seed, part),
                assembler.published(),
                options.quality,
                services,
            )
            .with_attach(part_def.attach.as_ref());
            return f(&mut ctx);
        }
        let output = build_part(def, &next, &options, services, assembler.published())?;
        assembler.add(&next, output)?;
    }
    Err(BuildError::Part { part: part.clone(), message: "the part is not in the build plan".to_owned() })
}

/// The head the hair part `part` of `def` grows from.
///
/// # Errors
/// As [`at_part`], or [`BuildError::Missing`] without a head.
pub fn head_of(
    id: &CharId,
    def: &CharacterDef,
    part: &PartId,
    services: &BuildServices,
) -> Result<Arc<dyn HeadSurface>, BuildError> {
    at_part(id, def, part, services, |ctx| ctx.head())
}

/// A group as its generator compiled it, before any mirroring, and the same clumps frozen as guides.
#[derive(Clone, Debug)]
pub struct Frozen {
    /// The compiled clumps.
    pub clumps: Vec<HairClump>,
    /// The clumps as guides ([`guide_of`]).
    pub guides: GuidesGen,
}

/// The group `group` of the hair part `part` (holding `params`), compiled as the build compiles it and frozen as
/// explicit guides; see the module docs.
///
/// # Errors
/// As [`at_part`], or what building the hair reports; [`BuildError::Part`] if the group doesn't build (it is turned
/// off) or is a shell with no clumps (the cap).
pub fn freeze(
    id: &CharId,
    def: &CharacterDef,
    part: &PartId,
    params: &HairParams,
    group: &GroupId,
    services: &BuildServices,
) -> Result<Frozen, BuildError> {
    let recorder = Recorder { group, frozen: RefCell::new(None) };
    at_part(id, def, part, services, |ctx| super::part::build_with(params, ctx, &recorder).map(drop))?;
    let fail = |message: String| BuildError::Part { part: part.clone(), message };
    match recorder.frozen.into_inner() {
        None => Err(fail(format!("the group `{group}` didn't build: enable it first"))),
        Some((_, true)) => Err(fail(format!("the group `{group}` is a shell over the scalp, with no locks to freeze"))),
        Some((frozen, false)) => Ok(frozen),
    }
}

/// Compiles with the real generators, freezing the one group it is after as it goes by (and whether it is a shell).
struct Recorder<'a> {
    group: &'a GroupId,
    frozen: RefCell<Option<(Frozen, bool)>>,
}

impl GroupGenerator for Recorder<'_> {
    fn compile(&self, generator: &Generator, ctx: &mut HairGenCtx<'_>) -> CompiledGroup {
        let compiled = Dispatch.compile(generator, ctx);
        if ctx.group() == self.group {
            let guides = compiled.clumps.iter().map(|clump| guide_of(clump, ctx.head)).collect();
            let shell = matches!(compiled.geometry, GroupGeometry::Shell(_));
            let frozen = Frozen { clumps: compiled.clumps.clone(), guides: GuidesGen { clumps: guides } };
            *self.frozen.borrow_mut() = Some((frozen, shell));
        }
        compiled
    }

    fn clump_count(&self, generator: &Generator) -> usize {
        Dispatch.clump_count(generator)
    }
}

/// `clump` as a guide over `head`: the fewest control points within [`FREEZE_TOLERANCE`] (module docs).
pub fn guide_of(clump: &HairClump, head: &dyn HeadSurface) -> GuideClump {
    let rest = &clump.rest;
    let most = rest.len().min(MAX_GUIDE_POINTS);
    // Start from a few evenly spread points, then add the rest point the curve misses most until it misses none.
    let last = rest.len().saturating_sub(1);
    let start = FREEZE_START.min(most).max(2);
    let mut picked: Vec<usize> = (0..start).map(|i| i * last / (start - 1)).collect();
    picked.dedup();
    let control = loop {
        let points: Vec<Vec3> = picked.iter().map(|&i| rest[i]).collect();
        if picked.len() >= most {
            break points;
        }
        let curve = guides::through(&points);
        let (worst, miss) = rest
            .iter()
            .enumerate()
            .map(|(i, p)| (i, distance_to(&curve, *p)))
            .fold((0, 0.0f32), |best, next| if next.1 > best.1 { next } else { best });
        if miss <= FREEZE_TOLERANCE || picked.contains(&worst) {
            break points;
        }
        let at = picked.partition_point(|&i| i < worst);
        picked.insert(at, worst);
    };
    GuideClump {
        // The root always sits on the head, so a reshaped head carries the lock; the rest anchor when near it.
        points: control
            .iter()
            .enumerate()
            .map(|(k, p)| guides::anchored(head, *p, if k == 0 { ROOT_NEAR } else { NEAR }))
            .collect(),
        width: clump.width,
        thickness: clump.thickness,
        shade: f64::from(clump.shade).clamp(0.5, 1.0),
    }
}

/// The farthest any point of `original` lies from the polyline `curve`.
pub fn deviation(curve: &[Vec3], original: &[Vec3]) -> f32 {
    original.iter().map(|p| distance_to(curve, *p)).fold(0.0, f32::max)
}

/// How far `p` lies from the polyline `curve`.
fn distance_to(curve: &[Vec3], p: Vec3) -> f32 {
    curve
        .windows(2)
        .map(|w| {
            let d = w[1] - w[0];
            let t =
                if d.length_squared() > 0.0 { ((p - w[0]).dot(d) / d.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
            p.distance(w[0] + d * t)
        })
        .fold(f32::INFINITY, f32::min)
}

/// `g` reflected left to right.
pub fn mirrored(g: &GuidesGen) -> GuidesGen {
    let clumps = g
        .clumps
        .iter()
        .map(|c| GuideClump { points: c.points.iter().map(|p| p.mirrored()).collect(), ..c.clone() })
        .collect();
    GuidesGen { clumps }
}

/// `g` with every point but each clump's root moved by up to `amount` metres, drawn from `seed`.
pub fn jittered(g: &GuidesGen, amount: f64, seed: u64) -> GuidesGen {
    let mut rng = Rng::new(seed);
    let mut draw = |scale: f64| f64::from(rng.range_f32(-1.0, 1.0)) * scale;
    let clumps = g
        .clumps
        .iter()
        .map(|c| {
            let points = c
                .points
                .iter()
                .enumerate()
                .map(|(k, p)| {
                    if k == 0 {
                        return *p;
                    }
                    match *p {
                        GuidePoint::Surface([theta, phi, offset]) => {
                            let degrees = (amount / HEAD_RADIUS).to_degrees();
                            GuidePoint::Surface([
                                (theta + draw(degrees)).clamp(0.0, 180.0),
                                phi + draw(degrees),
                                (offset + draw(amount)).clamp(-0.05, 0.3),
                            ])
                        }
                        GuidePoint::Head([x, y, z]) => {
                            GuidePoint::Head([x + draw(amount), y + draw(amount), z + draw(amount)])
                        }
                        GuidePoint::Body([x, y, z]) => {
                            GuidePoint::Body([x + draw(amount), y + draw(amount), z + draw(amount)])
                        }
                    }
                })
                .collect();
            GuideClump { points, ..c.clone() }
        })
        .collect();
    GuidesGen { clumps }
}
