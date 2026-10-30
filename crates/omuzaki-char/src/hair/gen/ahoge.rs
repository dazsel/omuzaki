//! Ahoge generator: one to three springy locks standing up from the crown and curling over (a new design in the
//! prototype's clump language; the prototype has none).
//!
//! **The curve.** A lock leaves the head at [`AhogeGen::root`] along the surface normal, tipped [`LIFT_DEG`] toward the
//! direction it curls in (`toward`: the horizontal direction [`AhogeGen::toward_deg`] from the front, laid on the
//! head's tangent plane). Its direction at fraction `s` of the length turns by `curl * s^CURL_POWER` from the normal
//! toward `toward`, so it stands up first and hooks over at the end, and a little sideways in the middle
//! ([`TWIST`]) so it reads as a lock rather than a drawn arc. The curve is integrated in [`POINTS`] equal steps.
//!
//! **More locks.** The second and third locks fan [`AhogeGen::fan_deg`] to either side of the first, shorter and a
//! touch less curled ([`LOCKS`]).
//!
//! **Spring.** With [`AhogeGen::spring`] the group publishes a spring chain of [`CHAIN_BONES`] bones along the first
//! lock (stiff and light, so it bobs and settles quickly) and the part skins every lock to it; without, the locks are
//! rigid on the head.
//!
//! The generator draws no random numbers.

use omuzaki_anim_api::SpringParams;
use omuzaki_char_api::{AhogeGen, BoneName, HeadSurface};
use omuzaki_core::glam::{DVec3, Vec3};
use omuzaki_core::id::SlugError;

use super::root::{Root, head_scale, sane};
use super::tail::Chain;
use crate::hair::compile::{CompiledGroup, HairGenCtx};

/// Points per lock.
pub const POINTS: usize = 24;
/// Bones in the spring chain along the first lock.
pub const CHAIN_BONES: usize = 4;
/// How far the lock is tipped toward its curl as it leaves the head, degrees.
pub const LIFT_DEG: f64 = 14.0;
/// How the curl is spread along the lock: above 1 it gathers toward the tip.
pub const CURL_POWER: f64 = 1.7;
/// The sideways bow in the middle of a lock, as a fraction of its length.
pub const TWIST: f64 = 0.08;
/// Each lock's `(length, curl, side)`: a fraction of the first's length and curl, and which way it fans (`-1`, `0`,
/// `1` times [`AhogeGen::fan_deg`]).
pub const LOCKS: [(f64, f64, f64); 3] = [(1.0, 1.0, 0.0), (0.78, 0.86, 1.0), (0.64, 0.9, -1.0)];
/// The root used for a coordinate of [`AhogeGen::root`] that is not finite (the default's).
const DEFAULT_ROOT: [f64; 3] = [14.0, 0.0, 0.022];
/// Length from the first bone over which the locks blend from the head to the chain, metres.
const ROOT_BAND: f32 = 0.012;
/// The chain's springs: stiff and light at the root, a little looser at the tip, so the lock bobs and settles.
const CHAIN_ROOT: SpringParams =
    SpringParams { hit_radius: 0.008, stiffness: 1.0, gravity_power: 0.02, gravity_dir: Vec3::NEG_Y, drag_force: 0.5 };
const CHAIN_TIP: SpringParams = SpringParams {
    hit_radius: 0.008,
    stiffness: 0.55,
    gravity_power: 0.06,
    gravity_dir: Vec3::NEG_Y,
    drag_force: 0.35,
};

/// The single-hook preset: one long lock that stands up from the crown and hooks forward over the fringe.
pub fn hook() -> AhogeGen {
    AhogeGen { length: 0.14, ..AhogeGen::default() }
}

/// The antenna preset: two locks, the longer curling forward and a shorter one fanned beside it.
pub fn antenna() -> AhogeGen {
    AhogeGen { locks: 2, length: 0.14, curl_deg: 230.0, fan_deg: 28.0, ..AhogeGen::default() }
}

/// How many locks the generator makes: [`AhogeGen::locks`], held to `1..=3`.
pub fn clump_count(g: &AhogeGen) -> usize {
    usize::from(g.locks.clamp(1, 3))
}

/// Where the locks spring from, bind space (on the authored side of a mirrored group).
pub fn root(g: &AhogeGen, head: &dyn HeadSurface) -> DVec3 {
    Root::new(g.root, DEFAULT_ROOT, head).point
}

/// Compiles the group: [`clump_count`] locks, the first the longest.
#[allow(clippy::cast_precision_loss)] // at most three locks
pub fn compile(g: &AhogeGen, ctx: &mut HairGenCtx<'_>) -> CompiledGroup {
    let scale = head_scale(ctx.head);
    let width = omuzaki_char_api::Taper {
        start: g.width.start * scale,
        mid: g.width.mid * scale,
        tip: g.width.tip * scale,
        ..g.width
    };
    let clumps = (0..clump_count(g))
        .map(|i| {
            let shade = 1.0 - 0.05 * i as f64;
            ctx.clump(lock(g, ctx.head, i), width, g.thickness, shade)
        })
        .collect();
    CompiledGroup::tubes(clumps)
}

/// Lock `i`'s rest points, root first.
#[allow(clippy::cast_precision_loss)] // point indices are small
pub fn lock(g: &AhogeGen, head: &dyn HeadSurface, i: usize) -> Vec<Vec3> {
    let root = Root::new(g.root, DEFAULT_ROOT, head);
    let (length_k, curl_k, side) = LOCKS[i.min(LOCKS.len() - 1)];
    let length = sane(g.length, 0.02, 0.4, 0.11) * head_scale(head) * length_k;
    let curl = sane(g.curl_deg, 0.0, 320.0, 200.0).to_radians() * curl_k;
    let azimuth = (sane(g.toward_deg, -360.0, 360.0, 0.0) + side * sane(g.fan_deg, 0.0, 90.0, 24.0)).to_radians();
    let normal = root.outward;
    let toward = tangent(normal, DVec3::new(azimuth.sin(), 0.0, azimuth.cos()));
    let sideways = normal.cross(toward).normalize_or(DVec3::X);
    let lift = LIFT_DEG.to_radians();
    let step = length / (POINTS - 1) as f64;
    let mut p = root.point;
    let mut out = Vec::with_capacity(POINTS);
    for k in 0..POINTS {
        out.push(p.as_vec3());
        let s = (k as f64 + 0.5) / (POINTS - 1) as f64;
        let angle = lift + curl * s.powf(CURL_POWER);
        let bow = TWIST * (std::f64::consts::PI * s).cos() * if side < 0.0 { -1.0 } else { 1.0 };
        let dir = (normal * angle.cos() + toward * angle.sin() + sideways * bow).normalize_or(normal);
        p = root.keep_outside(head, p + dir * step);
    }
    out
}

/// `along` laid on the plane square to `normal`, as a unit vector (any square direction if it is parallel to it).
fn tangent(normal: DVec3, along: DVec3) -> DVec3 {
    (along - normal * along.dot(normal)).try_normalize().unwrap_or_else(|| normal.any_orthonormal_vector())
}

/// The group's spring chains: none without [`AhogeGen::spring`]; else one along the first lock (`_l` and `_r` for a
/// mirrored group, the second reflected in the plane `x = head centre x`). `prefix` starts every bone and chain name.
///
/// # Errors
/// [`SlugError`] if `prefix` makes a bone name that is not a valid name.
pub fn chains(g: &AhogeGen, head: &dyn HeadSurface, prefix: &str, mirror: bool) -> Result<Vec<Chain>, SlugError> {
    if !g.spring {
        return Ok(Vec::new());
    }
    let points = lock(g, head, 0);
    let sides: &[(&str, bool)] = if mirror { &[("_l", false), ("_r", true)] } else { &[("", false)] };
    let mid_x = head.centre().x;
    let mut out = Vec::with_capacity(sides.len());
    for (suffix, flip) in sides {
        let mut bones = Vec::with_capacity(CHAIN_BONES);
        let mut rest = Vec::with_capacity(CHAIN_BONES);
        for b in 0..CHAIN_BONES {
            let mut at = points[b * (POINTS - 1) / (CHAIN_BONES - 1)];
            if *flip {
                at.x = 2.0 * mid_x - at.x;
            }
            bones.push(BoneName::new(format!("{prefix}{suffix}_{b}"))?);
            rest.push(at);
        }
        out.push(Chain {
            name: format!("{prefix}{suffix}"),
            bones,
            rest,
            root_band: ROOT_BAND,
            springs: [CHAIN_ROOT, CHAIN_TIP],
        });
    }
    Ok(out)
}
