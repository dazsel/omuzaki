//! Curtain generator: the long hair, `count` guides round the back with an S-wave and a tip curl (port of
//! `parts/hair.js` 291 to 326), drawn as a strip of columns per guide (`strip`, `shellFill`, 421 to 494). This file
//! makes the clumps and their position `u` across the back, and meshes them ([`group_mesh`]).
//!
//! **A guide.** Guide `i` of `count` stands at `u = i / (count - 1)` across the back, at azimuth
//! `lerp(-spread_deg, spread_deg, u)` from the back centre. Its root sits just under the braid line, which runs from
//! `root_height_front` at the sides to `root_height_back` behind (smoothstepped between 70 and 180 degrees from the
//! front, as [`part_sweep`](super::part_sweep) blends its braid line). From there it falls to a length of
//! `lerp(length_front, length_back, backness) (0.95 + 0.05 sin(23 u) + jitter)`, `backness` being `|cos(azimuth)|`. On
//! the way down it moves outward over the shoulders, from the root's own distance from the body axis to the
//! `envelope` (linear between its rows; less far out at the back), over the first half of its length. A sideways
//! S-wave and an outward wave a quarter-turn behind it grow in along `wave.growth`, and the last 22 % curls at the
//! tip. The path is resampled to [`GUIDE_POINTS`] points evenly by arc length.
//!
//! **The drape (#2486).** With `drape_deg` set, a guide first lies over the head: it starts that many degrees of polar
//! angle above its root, under the scalp shell at the tuck's height, comes out over the shell over the first
//! `DRAPE_EMERGE` of the way and follows the head's curve down to the root at the root's own height, so the hair reads
//! as one mass from the crown down instead of hanging from under the shell's edge. A bob or a nape
//! layer is a short draped curtain whose `envelope` holds it close to the head.
//!
//! **Randomness.** Six draws per guide, in this order and nothing else between guides: length jitter, wave amplitude,
//! wave frequency, wave phase, curl size, curl sign.
//!
//! **Axis.** Like the prototype, a guide is measured from the body axis at `x` of the head centre and
//! `z = ``BODY_AXIS_Z`, but its fall starts from the root's distance from the axis in `x` and from the origin in `z`,
//! so the first step runs a little outward. That is the prototype's look, kept for parity.
//!
//! **Folds (#841).** The prototype gives every guide its own wave, so neighbours can cross lower down and the strip
//! between them folds over itself. Builds smooth the wave's amplitude, frequency and phase jitter across `u` instead, so
//! neighbours stay in step ([`Folds::Coherent`], the art director's pick over the prototype's crossing and over clamping
//! each column to its guide); the draws are still all made, in the prototype's order. [`Folds::Prototype`] stays for
//! the parity tests.
//!
//! The group's stiffness and follow are its [`Dynamics`](omuzaki_char_api::Dynamics), applied by the context.

mod body;
mod strip;

pub use body::BodyKeepOut;
pub use strip::{
    MIN_GAP, Spacing, StripError, StripLook, column_count, curtain_order, strip_indices, strip_mesh, strip_normals,
    strip_positions, vertex_count, winding_flips,
};

use omuzaki_char_api::{CurtainGen, GuidePoint, HairClump, HeadSurface};
use omuzaki_core::glam::{DVec3, Vec3};
use omuzaki_core::mesh::MeshData;

use super::cap::smoothstep;
use crate::hair::compile::{CompiledGroup, GroupGeometry, HairGenCtx};
use crate::hair::strands::StrandMap;
use crate::hair::tube::{OutwardFrame, vertex_colour};
use crate::kit::catmull::spaced_points;

/// Points of a guide (the prototype's `catmull(pts, 26)`).
pub const GUIDE_POINTS: usize = 26;
/// Segments of the hand-built path before it is resampled.
const SEGMENTS: usize = 12;
/// The body axis's `z`, where the guides hang from (the prototype's `new Vector3(0, y, -0.03)`).
const BODY_AXIS_Z: f64 = -0.03;
/// The root sits this far under the braid line, metres.
const ROOT_BELOW_BRAID: f64 = 0.022;
/// How far the root stands off the head surface, metres.
const ROOT_OFFSET: f64 = 0.032;
/// Points of the drape over the head before the root ([`CurtainGen::drape_deg`]).
const DRAPE_POINTS: usize = 6;
/// The share of the drape over which it rises from under the scalp shell to the root's height over it.
const DRAPE_EMERGE: f64 = 0.4;
/// The fall outward finishes over this fraction of the length.
const FALL_END: f64 = 0.5;
/// The envelope's outward factor (`0.92 + 0.06`), and how much of it holds at the back.
const ENVELOPE_SCALE: f64 = 0.98;
/// At the back the hair hangs this fraction as far out.
const BACK_SCALE: f64 = 0.80;
/// The outward wave runs a quarter turn behind the sideways one at this strength.
const BULGE_STRENGTH: f64 = 0.7;
/// The outward wave grows in this much later than the sideways one.
const BULGE_LAG: f64 = 0.05;
/// The tip curl starts here, as a fraction of the length.
const CURL_FROM: f64 = 0.78;
/// The curl size the prototype's pull and lift go with: they scale with `wave.curl` over this.
const CURL_UNIT: f64 = 0.045;
/// The tip's pull toward the body at the prototype's curl, metres.
const CURL_PULL: f64 = 0.015;
/// The tip's lift at the prototype's curl, metres.
const CURL_LIFT: f64 = 0.045;
/// Jitter on the length, as a fraction.
const LENGTH_JITTER: f64 = 0.03;
/// Jitter on the wave amplitude, as a fraction of it: the prototype's `+- 0.006` on its `0.036`. Relative, so a curtain
/// whose wave is off (amplitude 0) is straight, and the default is the prototype's.
const AMPLITUDE_JITTER: f64 = 0.006 / 0.036;
/// Where the braid line blends from the sides to the back, degrees from the front.
const BRAID_BLEND_DEG: (f64, f64) = (70.0, 180.0);

/// How the curtain treats guides that cross (#841).
#[doc(hidden)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Folds {
    /// The prototype's: independent waves per guide, columns follow their neighbours wherever they go. The parity
    /// target only.
    Prototype,
    /// The waves' amplitude, frequency and phase jitter are smoothed across `u`, so neighbours stay in step: what builds
    /// use.
    #[default]
    Coherent,
}

/// [`Folds::Coherent`]'s smoothing width across the curtain, as a fraction of `u` (one standard deviation).
const COHERENCE_SPAN: f64 = 0.12;

/// Compiles the group: one clump per guide, with `u` across the back, the waves kept in step ([`Folds::Coherent`]).
pub fn compile(g: &CurtainGen, ctx: &mut HairGenCtx<'_>) -> CompiledGroup {
    compile_folds(g, ctx, Folds::Coherent)
}

/// [`compile`] with a fold treatment. Only the tests choose one.
#[doc(hidden)]
pub fn compile_folds(g: &CurtainGen, ctx: &mut HairGenCtx<'_>, folds: Folds) -> CompiledGroup {
    let count = usize::from(g.count);
    let centre = ctx.head.centre();
    // Every guide's draws are taken up front, in order: the stream is the prototype's whatever the treatment.
    let mut draws: Vec<Draws> = (0..count).map(|_| Draws::take(ctx)).collect();
    if folds == Folds::Coherent {
        smooth_jitter(&mut draws);
    }
    let keep_out = ctx.body.map(BodyKeepOut::new);
    let mut clumps = Vec::with_capacity(count);
    let mut across = Vec::with_capacity(count);
    for (i, draws) in draws.iter().enumerate() {
        let u = across_back(i, count);
        let rest = guide(g, ctx, keep_out.as_ref(), centre, u, draws);
        clumps.push(ctx.clump(rest, g.width, g.thickness, 1.0));
        across.push(narrow_f32(u));
    }
    CompiledGroup { clumps, geometry: GroupGeometry::Strip { columns: g.columns, u: across } }
}

/// Replaces each guide's amplitude, frequency and phase jitter by a Gaussian-weighted average over its neighbours
/// ([`COHERENCE_SPAN`] wide), so the jitter varies smoothly across the back. Length and curl are left alone.
#[allow(clippy::cast_precision_loss)] // up to a few hundred guides
fn smooth_jitter(draws: &mut [Draws]) {
    let n = draws.len();
    if n < 2 {
        return;
    }
    let sigma = COHERENCE_SPAN * (n - 1) as f64;
    let weight = |i: usize, j: usize| (-0.5 * ((i as f64 - j as f64) / sigma).powi(2)).exp();
    let smooth = |pick: fn(&Draws) -> f64| -> Vec<f64> {
        (0..n)
            .map(|i| {
                let total: f64 = (0..n).map(|j| weight(i, j)).sum();
                (0..n).map(|j| weight(i, j) * pick(&draws[j])).sum::<f64>() / total
            })
            .collect()
    };
    let (amplitude, frequency, phase) = (smooth(|d| d.amplitude), smooth(|d| d.frequency), smooth(|d| d.phase));
    for (i, d) in draws.iter_mut().enumerate() {
        (d.amplitude, d.frequency, d.phase) = (amplitude[i], frequency[i], phase[i]);
    }
}

/// The curtain as one mesh at rest, from the compiled `clumps` and their `u` (a strip's position across the back): each
/// guide's strip of `columns` columns, in order of `u`, every strip with vertices of its own. Each strip takes its
/// clump's shade and streak tint, and `map` lays it on the strand texture. Vertices inside the body (`keep_out`) are moved out of it. Every treatment spaces the
/// columns as the prototype does; they differ in the guides [`compile_folds`] makes.
///
/// # Errors
/// [`StripError`] if `u` and `clumps` differ in length or the guides cannot make a strip (fewer than two points, or not
/// all the same number).
#[doc(hidden)]
pub fn group_mesh(
    clumps: &[HairClump],
    u: &[f32],
    columns: u8,
    frame: &OutwardFrame,
    keep_out: Option<&BodyKeepOut>,
    map: &StrandMap,
) -> Result<MeshData, StripError> {
    if u.len() != clumps.len() {
        return Err(StripError::GuideCount { clumps: clumps.len(), u: u.len() });
    }
    let order = curtain_order(u);
    let guides: Vec<&[Vec3]> = order.iter().map(|&i| clumps[i].rest.as_slice()).collect();
    let looks: Vec<StripLook> =
        order.iter().map(|&i| StripLook { shade: clumps[i].shade, colour: vertex_colour(clumps[i].tint) }).collect();
    strip_mesh(
        &guides,
        &looks,
        columns,
        Spacing::Prototype,
        map,
        frame.head,
        |p| frame.rest(p),
        |p| keep_out.map_or(p, |k| k.push_out(p, STRIP_BODY_MARGIN)),
    )
}

/// How many clumps [`compile`] makes, from the parameters alone: one per guide.
pub fn clump_count(g: &CurtainGen) -> usize {
    usize::from(g.count)
}

/// `u` of guide `i` of `count`: 0 at one side to 1 at the other; a lone guide stands at the back centre.
#[allow(clippy::cast_precision_loss)] // up to a few hundred guides
fn across_back(i: usize, count: usize) -> f64 {
    if count < 2 { 0.5 } else { i as f64 / (count - 1) as f64 }
}

#[allow(clippy::cast_possible_truncation)] // u is 0..1
fn narrow_f32(x: f64) -> f32 {
    x as f32
}

/// The six random numbers of one guide, in the order the prototype draws them.
struct Draws {
    /// Length jitter, a fraction.
    length: f64,
    /// Wave amplitude jitter, as a fraction of the amplitude in `-AMPLITUDE_JITTER..AMPLITUDE_JITTER`.
    amplitude: f64,
    /// Wave frequency jitter, radians along the guide.
    frequency: f64,
    /// Wave phase jitter, radians.
    phase: f64,
    /// Curl size, `0.5..1`.
    curl: f64,
    /// Curl direction, -1 or 1.
    curl_sign: f64,
}

impl Draws {
    fn take(ctx: &mut HairGenCtx<'_>) -> Self {
        let length = ctx.rr(-LENGTH_JITTER, LENGTH_JITTER);
        let amplitude = AMPLITUDE_JITTER * ctx.rr(-1.0, 1.0);
        let frequency = ctx.rr(-1.0, 1.0);
        let phase = ctx.rr(-0.6, 0.6);
        let curl = ctx.rr(0.5, 1.0);
        let curl_sign = if ctx.rnd() < 0.5 { -1.0 } else { 1.0 };
        Self { length, amplitude, frequency, phase, curl, curl_sign }
    }
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// The guide's angle round the head from the front, in `-pi..=pi` (`pi` is the back centre).
fn angle_of(g: &CurtainGen, u: f64) -> f64 {
    let ph = lerp(-g.spread_deg, g.spread_deg, u).to_radians() + std::f64::consts::PI;
    ph.sin().atan2(ph.cos())
}

/// The guide's root: on the head just under the braid line, which is found as `part_sweep` finds its own.
fn root_point(g: &CurtainGen, head: &dyn HeadSurface, phi: f64) -> (Vec3, f64) {
    let blend = smoothstep(phi.abs().to_degrees(), BRAID_BLEND_DEG.0, BRAID_BLEND_DEG.1);
    let height = lerp(g.root_height_front, g.root_height_back, blend) - ROOT_BELOW_BRAID;
    let crown = f64::from(head.radius(Vec3::Y)).max(1e-6);
    let theta = (height / crown).clamp(-1.0, 1.0).acos();
    let theta = theta.to_degrees();
    (GuidePoint::Surface([theta, phi.to_degrees(), ROOT_OFFSET]).resolve(head), theta)
}

/// The distance the hair stands from the body axis at height `y`: linear between the table's rows, held at the first
/// and last. A table with no rows has no say, so the hair keeps `fallback`.
fn envelope_at(table: &[[f64; 2]], y: f64, fallback: f64) -> f64 {
    let (Some(first), Some(last)) = (table.first(), table.last()) else { return fallback };
    // Rows run from the shoulders down, so `y` falls along the table; accept either order.
    let descending = first[0] > last[0];
    let before = |row: &[f64; 2]| if descending { row[0] >= y } else { row[0] <= y };
    let Some(at) = table.iter().rposition(before) else { return first[1] };
    let (Some(a), Some(b)) = (table.get(at), table.get(at + 1)) else { return last[1] };
    let span = b[0] - a[0];
    if span.abs() < f64::EPSILON { a[1] } else { lerp(a[1], b[1], (y - a[0]) / span) }
}

/// One guide's rest shape: the hand-built path, resampled evenly by arc length.
fn guide(
    params: &CurtainGen,
    ctx: &HairGenCtx<'_>,
    keep_out: Option<&BodyKeepOut>,
    centre: Vec3,
    u: f64,
    draws: &Draws,
) -> Vec<Vec3> {
    let phi = angle_of(params, u);
    let (root, root_theta) = root_point(params, ctx.head, phi);
    let root = root.as_dvec3();
    let backness = phi.cos().abs();
    let length =
        lerp(params.length_front, params.length_back, backness) * (0.95 + 0.05 * (u * 23.0).sin() + draws.length);
    let axis = DVec3::new(f64::from(centre.x), 0.0, BODY_AXIS_Z);
    let (sin, cos) = phi.sin_cos();
    let root_reach = (root.x - axis.x).hypot(root.z);
    let amplitude = params.wave.amplitude * (1.0 + draws.amplitude);
    let frequency = std::f64::consts::TAU * params.wave.frequency + draws.frequency;
    let phase = params.wave.phase + u * 10.0 + draws.phase;
    let curl = draws.curl * draws.curl_sign * params.wave.curl;
    let curl_scale = params.wave.curl.abs() / CURL_UNIT;
    let [grow_from, grow_to] = params.wave.growth;
    let end_y = root.y - length;
    let at = |t: f64| {
        let height = lerp(root.y, end_y, t);
        let outer =
            envelope_at(&params.envelope, height, root_reach) * ENVELOPE_SCALE * lerp(1.0, BACK_SCALE, backness);
        let reach = lerp(root_reach, outer, smoothstep(t, 0.0, FALL_END));
        let swing = t * frequency + phase;
        let wave = swing.sin() * amplitude * smoothstep(t, grow_from, grow_to);
        let bulge =
            swing.cos() * amplitude * BULGE_STRENGTH * smoothstep(t, grow_from + BULGE_LAG, grow_to + BULGE_LAG);
        // `tangent` is sideways in the floor plane, perpendicular to the heading `(sin, cos)`.
        let (tx, tz) = (cos, -sin);
        let mut point =
            DVec3::new(axis.x + sin * (reach + bulge) + tx * wave, height, axis.z + cos * (reach + bulge) + tz * wave);
        if t > CURL_FROM {
            let curling = smoothstep(t, CURL_FROM, 1.0);
            let pull = CURL_PULL * curl_scale * curling * curling;
            point.x += tx * curl * curling - sin * pull;
            point.z += tz * curl * curling - cos * pull;
            point.y += CURL_LIFT * curl_scale * curling * curling;
        }
        point
    };
    // The tuck: a first point carried up the head, just under the cap's shell, so the curtain starts hidden under the
    // cap's edge instead of standing off the head as a band. It is zero in the prototype's own parameters.
    // The drape, where there is one, takes its place: the guide lies over the head from `drape_deg` above the root down
    // to it, at the root's own height over the scalp, so it covers the cap instead of starting under it.
    let start: Vec<DVec3> = if params.drape_deg > 0.0 {
        (0..DRAPE_POINTS)
            .map(|k| {
                let s = along(k, DRAPE_POINTS);
                let theta = (root_theta - params.drape_deg * (1.0 - s)).max(0.0);
                // It comes out from under the shell: at the tuck's height where it starts, at the root's over the rest.
                let offset = lerp(params.root_tuck_offset, ROOT_OFFSET, smoothstep(s, 0.0, DRAPE_EMERGE));
                GuidePoint::Surface([theta, phi.to_degrees(), offset]).resolve(ctx.head).as_dvec3()
            })
            .collect()
    } else if params.root_tuck_deg > 0.0 {
        let theta = (root_theta - params.root_tuck_deg).max(0.0);
        vec![GuidePoint::Surface([theta, phi.to_degrees(), params.root_tuck_offset]).resolve(ctx.head).as_dvec3()]
    } else {
        Vec::new()
    };
    let hang = (!params.in_front).then_some(keep_out).flatten().map(|k| {
        let head = ctx.head;
        (k, head.centre().y - head.radius(Vec3::NEG_Y), HANG_OVER * head.radius(Vec3::X))
    });
    let head = ctx.head;
    let fall = drape((1..=SEGMENTS).map(|k| clear_of_head(head, at(along(k, SEGMENTS)))), keep_out, hang);
    let path: Vec<DVec3> = start.into_iter().chain(std::iter::once(root)).chain(fall).collect();
    spaced_points(&path, GUIDE_POINTS).map_or_else(
        |_| straight_fall(root, length),
        |pts| {
            pts.into_iter()
                .map(|p| {
                    // The fall only: the tuck above the root stands under the cap on purpose, the drape over it.
                    let p = if p.y < root.y { clear_of_head(head, p) } else { p };
                    clear_of_body(keep_out, p).as_vec3()
                })
                .collect()
        },
    )
}

/// How far the fall keeps off the head, metres: a wide or deep head would otherwise have the curtain, which narrows
/// toward the body as it falls, cut into its sides (#2262).
const HEAD_MARGIN: f64 = 0.012;

/// `p` pushed straight out from the head's centre until it stands [`HEAD_MARGIN`] off the head, if it is closer.
fn clear_of_head(head: &dyn HeadSurface, p: DVec3) -> DVec3 {
    let centre = head.centre().as_dvec3();
    let from = p - centre;
    let distance = from.length();
    if !(distance.is_finite() && distance > 0.0) {
        return p;
    }
    let dir = from / distance;
    let need = f64::from(head.radius(dir.as_vec3())) + HEAD_MARGIN;
    if need.is_finite() && distance < need { centre + dir * need } else { p }
}

/// How far below the head's lowest point (in head half-widths) the hair has finished moving behind the arms.
const HANG_OVER: f32 = 0.6;
/// The most a strand's push out of the body is carried down it, metres.
const CARRY_MAX: f32 = 0.08;

/// The fall's points kept off the body without folding: each is drawn behind the arms (`hang`: the keep-out, the head's
/// lowest point and the distance over which the hair settles behind them), then pushed out of the body, and the push is
/// carried on to the points below it, so a strand that is pushed out over the bust drapes over it instead of folding back
/// under it where the body curves away.
fn drape(
    points: impl Iterator<Item = DVec3>,
    keep_out: Option<&BodyKeepOut>,
    hang: Option<(&BodyKeepOut, f32, f32)>,
) -> Vec<DVec3> {
    let mut carry = Vec3::ZERO;
    points
        .map(|raw| {
            let drawn = hang.map_or(raw, |(k, from_y, over)| k.hang_behind(raw.as_vec3(), from_y, over).as_dvec3());
            let Some(k) = keep_out else { return drawn };
            let wanted = drawn.as_vec3() + carry;
            let pushed = k.push_out(wanted, GUIDE_BODY_MARGIN);
            carry = (pushed - drawn.as_vec3()).clamp_length_max(CARRY_MAX);
            if !carry.is_finite() {
                carry = Vec3::ZERO;
            }
            pushed.as_dvec3()
        })
        .collect()
}

/// How far a guide's points keep from the body, metres. The strips take a smaller margin ([`STRIP_BODY_MARGIN`]):
/// they stand a little sideways of their guide.
const GUIDE_BODY_MARGIN: f32 = 0.010;
/// How far the strips' vertices keep from the body, metres.
pub const STRIP_BODY_MARGIN: f32 = 0.006;

/// `p` moved out of the body, if there is one and `p` is inside it. Points that start the guide (on the head, above the
/// shoulders) are in no region, so only the fall is touched.
fn clear_of_body(keep_out: Option<&BodyKeepOut>, p: DVec3) -> DVec3 {
    keep_out.map_or(p, |k| k.push_out(p.as_vec3(), GUIDE_BODY_MARGIN).as_dvec3())
}

/// The guide as a straight drop, for a path that cannot be resampled (a non-finite parameter): the group still builds
/// the right number of guides of the right length.
fn straight_fall(root: DVec3, length: f64) -> Vec<Vec3> {
    (0..GUIDE_POINTS).map(|k| (root - DVec3::Y * (length * along(k, GUIDE_POINTS - 1))).as_vec3()).collect()
}

#[allow(clippy::cast_precision_loss)] // a few dozen points
fn along(i: usize, last: usize) -> f64 {
    i as f64 / last as f64
}
