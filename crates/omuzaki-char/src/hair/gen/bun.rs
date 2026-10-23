//! Bun generator: strands wound round a ball that sits on the head. A new design in the prototype's clump language (the
//! prototype has no bun). The bun is rigid to the head: it draws no random numbers and has no bones.
//!
//! **The ball.** Its centre is `radius` out from the surface point [`BunGen::centre`] along the head's surface normal,
//! so the ball rests on the point (its lowest pole is the point itself, `offset` off the head).
//!
//! **The strands.** Strand `k` of [`BunGen::strands`] winds from the lowest pole to the top pole in [`REST_POINTS`]
//! points: at fraction `t` it is at polar angle `pi (1 - t)` from the ball's axis and azimuth `k * 2 pi / strands +
//! 2 pi * wraps * t` round it, at the ball's radius plus [`LAYER_STEP`] times `k mod` [`LAYERS`] (neighbouring strands
//! stand a little apart, so they do not share a surface). Every strand therefore starts and ends in the same two
//! poles, like the hair gathered into a tie.
//!
//! **Gather locks.** [`BunGen::gather`] broad flat locks (after the strands) sweep from the back and side hairline along the
//! scalp up into the bun's base, ending [`INSET`] of the radius inside the ball ([`gather_locks`]); the presets have them, and `0` turns them off.
//!
//! **Clearance.** Every rest point stands at least `CLEARANCE` off the head ([`Root::keep_outside`]).
//!
//! **Anchors.** [`anchors`] gives `bun_centre` (the ball's centre) and `bun_tie` (the lowest pole, +Y along the ball's
//! axis) for a hair tie accessory.

use std::f64::consts::{PI, TAU};

use omuzaki_char_api::{Anchor, BunGen, HeadSurface, JointRef};
use omuzaki_core::glam::{DQuat, DVec3, Quat, Vec3};

use super::root::{GATHER_SHADE, GATHER_THICKNESS, Root, gather_locks, gather_width, head_scale, sane};
use crate::hair::compile::{CompiledGroup, HairGenCtx};

/// Points per strand.
pub const REST_POINTS: usize = 32;
/// How far each neighbouring layer of strands stands off the last, metres.
pub const LAYER_STEP: f64 = 0.0015;
/// Layers of strands (strand `k` is on layer `k mod LAYERS`).
pub const LAYERS: u8 = 3;
/// The root used for a coordinate of [`BunGen::centre`] that is not finite (the default's).
const DEFAULT_CENTRE: [f64; 3] = [30.0, 180.0, 0.02];
/// How far into the ball, as a fraction of its radius, the gather locks end from its lowest pole.
pub const INSET: f64 = 0.4;
/// Shade of the first strand; each later strand is [`SHADE_STEP`] darker.
const SHADE: f64 = 1.0;
/// Shade lost per strand.
const SHADE_STEP: f64 = 0.02;

/// The low bun preset: a bun at the nape.
pub fn low_bun() -> BunGen {
    BunGen { centre: [105.0, 180.0, 0.015], radius: 0.055, gather: 6, ..BunGen::default() }
}

/// The high bun preset: a bun on the crown, behind the top of the head.
pub fn high_bun() -> BunGen {
    BunGen { centre: [38.0, 180.0, 0.015], radius: 0.05, gather: 6, ..BunGen::default() }
}

/// The ball a bun is wound round.
struct Ball {
    root: Root,
    radius: f64,
    wraps: f64,
    centre: DVec3,
}

impl Ball {
    fn new(g: &BunGen, head: &dyn HeadSurface) -> Self {
        let root = Root::new(g.centre, DEFAULT_CENTRE, head);
        let radius = sane(g.radius, 0.01, 0.2, 0.05);
        let wraps = sane(g.wraps, 0.0, 6.0, 1.5);
        Self { root, radius, wraps, centre: root.point + root.outward * radius }
    }
}

/// Compiles the group: [`BunGen::strands`] strands.
pub fn compile(g: &BunGen, ctx: &mut HairGenCtx<'_>) -> CompiledGroup {
    let ball = Ball::new(g, ctx.head);
    let frame = DQuat::from_rotation_arc(DVec3::Y, ball.root.outward);
    let mut clumps: Vec<_> = (0..g.strands)
        .map(|k| {
            let rest = strand(g, &ball, ctx.head, frame, k);
            ctx.clump(rest, g.width, g.thickness, SHADE - SHADE_STEP * f64::from(k))
        })
        .collect();
    // The gather locks end at the bun's base, inside the ball.
    let width = gather_width(head_scale(ctx.head));
    let end = ball.root.point + ball.root.outward * (INSET * ball.radius);
    for rest in gather_locks(&ball.root, ctx.head, g.gather, end) {
        clumps.push(ctx.clump(rest, width, GATHER_THICKNESS, GATHER_SHADE));
    }
    CompiledGroup::tubes(clumps)
}

/// How many clumps [`compile`] makes: [`BunGen::strands`], then [`BunGen::gather`] gather locks.
pub fn clump_count(g: &BunGen) -> usize {
    usize::from(g.strands) + usize::from(g.gather)
}

/// Strand `k`: [`REST_POINTS`] points from the lowest pole to the top one.
#[allow(clippy::cast_precision_loss)] // point indices are small
fn strand(g: &BunGen, ball: &Ball, head: &dyn HeadSurface, frame: DQuat, k: u8) -> Vec<Vec3> {
    let start = f64::from(k) * TAU / f64::from(g.strands.max(1));
    let radius = ball.radius + LAYER_STEP * f64::from(k % LAYERS);
    (0..REST_POINTS)
        .map(|i| {
            let t = i as f64 / (REST_POINTS - 1) as f64;
            let (polar, azimuth) = (PI * (1.0 - t), start + TAU * ball.wraps * t);
            let local = DVec3::new(polar.sin() * azimuth.cos(), polar.cos(), polar.sin() * azimuth.sin());
            // The ball's lowest pole is `centre - outward * radius`; layers grow the ball about its centre.
            let p = ball.centre + frame * local * radius;
            ball.root.keep_outside(head, p).as_vec3()
        })
        .collect()
}

/// The anchors a bun publishes: `bun_centre` and `bun_tie`.
pub fn anchors(g: &BunGen, head: &dyn HeadSurface) -> Vec<(&'static str, Anchor)> {
    let ball = Ball::new(g, head);
    let rotation = Quat::from_rotation_arc(Vec3::Y, ball.root.outward.as_vec3());
    let at = |position: DVec3| Anchor { bone: JointRef::HEAD, position: position.as_vec3(), rotation };
    vec![("bun_centre", at(ball.centre)), ("bun_tie", at(ball.root.point))]
}
