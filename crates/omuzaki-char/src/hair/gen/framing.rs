//! Framing-locks generator: `count` locks per side that run down beside the face, then tuck behind the ear or hang to
//! the chest (port of `parts/hair.js` section 3, lines 202 to 254).
//!
//! **A lock.** Lock `i` of a side runs along the `edge` outline (the inner edge of the framing hair, `[head-local y, x]`
//! rows, linear between rows and held at the first and last), pushed out by `spread * i` as it leaves the forehead
//! (over the first 0.14 m below the top), and lies on the head surface (the prototype's `surfAt`, using the head's own
//! half-width as the front projection's radius) `0.034 + 0.004 i` above it. After the outline the **left** side tucks
//! locks `1..` over and behind the ear, and its lock `0` frames the cheek; the **right** side hangs every lock in
//! S-waves. A path is resampled by [`spaced_points`] to [`TUCKED_POINTS`] (tucked) or [`HANGING_POINTS`] points.
//!
//! **Scale.** When [`FramingGen::outline_radius`] is set (the default) and no lock heights are given, the outline and
//! `spread` are scaled to the head the hair is built on (widths by its half-width, heights by its crown, over that
//! radius), and the default tail is made
//! of surface-anchored points, so the default locks frame the face on any head: down the temple and cheek in front of the
//! ear, then round the jaw to under the chin. A file without the field is taken in metres, as before.
//!
//! **The projection.** With [`FramingLocks::front_radius`] set the projection is the prototype's exactly: a point outside
//! the front disc gets no depth and lands on the side of the head at the height its direction gives. Without it the
//! head's half-width is the radius and such a point slides in along `x` to keep its authored height. The jaw and
//! chest depths are [`FramingLocks::jaw_z`] and [`FramingLocks::chest_z`].
//!
//! **Where the heights come from.** With [`FramingGen::locks`] set, the heights are the prototype's own: the lock is
//! built from its forehead, lower, cheek, tuck, ear, drop and end values. With `locks` unset, the outline's rows are
//! the forehead heights and `tail` (authored for the left side, mirrored for the right) supplies the free points after
//! it, each pushed out by `0.9 * spread * i` and back by `0.006 i`, so every existing file keeps its meaning.
//!
//! **Sway.** The `wave` sways the tail points sideways by `sin(2 pi frequency t + phase + wv) * amplitude` growing in
//! over `growth`. The amplitude is `wave.amplitude` scaled by the lock's `wa` over [`WAVE_UNIT`], the mean of `wa`'s
//! range, so `wave.amplitude = 0.016` is the prototype's own `wa`; the default (no sway) still draws every number. A
//! hanging lock also swings forward and back by the prototype's fixed 8 mm, whatever the amplitude. The tucked wave's
//! phase is on the bind-space height, as in the prototype. `wave.curl` curls the tail's tip outward (squared
//! smoothstep over the last 22 %; the prototype has none, so a zero curl is the prototype).
//!
//! **Randomness.** Two draws per lock, in this order and nothing between locks: `wv = rr(0, 6.28)`, then
//! `wa = rr(0.012, 0.02)`. Locks run left side first, then the right.
//!
//! **Width and thickness.** The group's `width` with its `start` raised by `0.004 i` (the prototype's
//! `taper(0.030 + 0.004 i, 0.050, 0.006, 0.80)` is exactly a [`Taper`]) and the group's
//! `thickness`. The prototype's `0.006 + 0.004 sin(pi min(1, 1.2 t))` peaks at `t = 0.42` and settles at the
//! base `0.006` past `t = 0.83`; a [`Bulge`](omuzaki_char_api::Bulge) is the closest shape (`base + swell sin(pi t)`, peak at the middle).
//! The shade is `1 - 0.025 i`. Stiffness and follow are the group's [`Dynamics`](omuzaki_char_api::Dynamics).

use omuzaki_char_api::hair::FramingLocks;
use omuzaki_char_api::{FramingGen, HairSide, HeadSurface, Taper};
use omuzaki_core::glam::{DVec3, Vec3};

use super::cap::smoothstep;
use crate::hair::compile::{CompiledGroup, HairGenCtx};
use crate::kit::catmull::spaced_points;

/// Points of a tucked lock (the prototype's `catmull(pts, 22)`).
pub const TUCKED_POINTS: usize = 22;
/// Points of a hanging lock (the prototype's `catmull(pts, 24)`).
pub const HANGING_POINTS: usize = 24;
/// The mean of the amplitude draw `rr(0.012, 0.02)`.
pub const WAVE_UNIT: f64 = 0.016;
/// The prototype's literal for the phase draw's range (not exactly a turn; kept for stream parity).
#[allow(clippy::approx_constant)]
const PHASE_RANGE: f64 = 6.28;
/// The forehead part of a lock moves outward over this much height below the top, metres.
const FOREHEAD_RAMP: f64 = 0.14;
/// A point is kept inside this fraction of the head's half-width in `x`, so `surfAt` always has a front to find.
const SURFACE_X_LIMIT: f64 = 0.985;
/// Width of one lock's `start` grows by this per lock index.
const START_STEP: f64 = 0.004;
/// Shade falls by this per lock index.
const SHADE_STEP: f64 = 0.025;
/// The most a taper's width may reach (the contract's limit).
const WIDTH_MAX: f64 = 0.3;
/// The forward-back swing of a hanging lock, metres (the prototype's `0.008`, not drawn).
const DEPTH_AMPLITUDE: f64 = 0.008;
/// The tip curl starts here along the tail.
const CURL_FROM: f64 = 0.78;
/// Stand-in length of a lock whose path cannot be resampled.
const FALLBACK_LENGTH: f64 = 0.3;

/// Compiles the group: `count` locks for each side asked for, left first.
pub fn compile(g: &FramingGen, ctx: &mut HairGenCtx<'_>) -> CompiledGroup {
    let scaled = scaled_to_head(g, f64::from(ctx.head.radius(Vec3::X)), f64::from(ctx.head.radius(Vec3::Y)));
    let g = &scaled;
    let rh =
        g.locks.as_ref().and_then(|l| l.front_radius).unwrap_or_else(|| f64::from(ctx.head.radius(Vec3::X))).max(1e-3);
    let exact = g.locks.as_ref().is_some_and(|l| l.front_radius.is_some());
    let mut clumps = Vec::with_capacity(clump_count(g));
    for &side in sides(g) {
        for i in 0..usize::from(g.count) {
            let lock = Lock { g, side, i: index(i), rh, exact, tucked: side > 0.0 && i >= 1 };
            let rest = lock.rest(ctx);
            let width = Taper { start: (g.width.start + START_STEP * lock.i).min(WIDTH_MAX), ..g.width };
            clumps.push(ctx.clump(rest, width, g.thickness, 1.0 - SHADE_STEP * lock.i));
        }
    }
    CompiledGroup::tubes(clumps)
}

/// `g` with its outline and spread scaled to a head of half-width `half_width` and crown height `crown`, when it says
/// what size they were drawn for ([`FramingGen::outline_radius`], a round head's radius) and gives no lock heights of
/// its own (those are the prototype's metres). The outline's widths and the spread follow the half-width and its
/// heights the crown, so a head widened alone keeps the locks at their heights and carries them out with its sides
/// (#2262); on a round head, as every template's is, both are the one scale. A non-finite or zero size leaves `g` as
/// it is.
fn scaled_to_head(g: &FramingGen, half_width: f64, crown: f64) -> FramingGen {
    let mut out = g.clone();
    let Some(drawn_for) = g.outline_radius.filter(|r| r.is_finite() && *r > 0.0) else { return out };
    let sane = |r: f64| r.is_finite() && r > 0.0;
    if g.locks.is_some() || !sane(half_width) || !sane(crown) {
        return out;
    }
    let (across, up) = (half_width / drawn_for, crown / drawn_for);
    for row in &mut out.edge {
        *row = [row[0] * up, row[1] * across];
    }
    out.spread *= across;
    out
}

/// How many clumps [`compile`] makes, from the parameters alone: `count` per side asked for.
pub fn clump_count(g: &FramingGen) -> usize {
    usize::from(g.count) * sides(g).len()
}

/// The sides to build as `x` signs, in build order.
fn sides(g: &FramingGen) -> &'static [f64] {
    match g.side {
        HairSide::Left => &[1.0],
        HairSide::Right => &[-1.0],
        HairSide::Both => &[1.0, -1.0],
    }
}

#[allow(clippy::cast_precision_loss)] // a lock index
fn index(i: usize) -> f64 {
    i as f64
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// The outline's `x` at head-local height `y`: linear between rows, held at the first and last row (rows may run
/// either way).
fn frame_x(edge: &[[f64; 2]], y: f64) -> f64 {
    let (Some(first), Some(last)) = (edge.first(), edge.last()) else { return 0.0 };
    let descending = first[0] >= last[0];
    if if descending { y >= first[0] } else { y <= first[0] } {
        return first[1];
    }
    for pair in edge.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let inside = if descending { y <= a[0] && y >= b[0] } else { y >= a[0] && y <= b[0] };
        if inside {
            let span = b[0] - a[0];
            return if span.abs() < f64::EPSILON { a[1] } else { lerp(a[1], b[1], (y - a[0]) / span) };
        }
    }
    last[1]
}

/// One lock being built.
struct Lock<'g> {
    g: &'g FramingGen,
    /// +1 for the left, -1 for the right.
    side: f64,
    /// The lock's index on its side, as a number.
    i: f64,
    /// The front projection's radius: the locks' `front_radius`, else the head's half-width.
    rh: f64,
    /// Whether `surf_at` is the prototype's exactly (`front_radius` was given).
    exact: bool,
    tucked: bool,
}

impl Lock<'_> {
    /// The lock's rest points, resampled.
    fn rest(&self, ctx: &mut HairGenCtx<'_>) -> Vec<Vec3> {
        let head = ctx.head;
        let centre = head.centre();
        let mut pts: Vec<DVec3> = Vec::new();
        self.forehead(head, &mut pts);
        // The draws come between the forehead and the rest, as in the prototype: `wv`, then `wa`.
        let phase = ctx.rr(0.0, PHASE_RANGE);
        let amp = ctx.rr(0.012, 0.02);
        let sway = Sway::new(self.g, phase, amp);
        match &self.g.locks {
            Some(locks) => self.along_locks(locks, head, centre, &sway, &mut pts),
            None => self.along_tail(head, &sway, &mut pts),
        }
        let n = if self.tucked { TUCKED_POINTS } else { HANGING_POINTS };
        spaced_points(&pts, n).map_or_else(|_| straight_fall(head, n), |p| p.into_iter().map(DVec3::as_vec3).collect())
    }

    /// `surfAt`: the point on the head at head-local `(x, y)` in front projection, pushed out by `off`.
    fn surf_at(&self, head: &dyn HeadSurface, x: f64, y: f64, off: f64) -> DVec3 {
        let mut dx = x.abs().min(self.rh * SURFACE_X_LIMIT);
        // Where the point lies outside the front disc, slide it in along `x` so the authored height `y` is kept. The
        // prototype does not: it gives such a point no depth.
        if !self.exact && dx * dx + y * y > self.rh * self.rh {
            dx = (self.rh * self.rh - y * y).max(0.0).sqrt();
        }
        let dx = if x < 0.0 { -dx } else { dx };
        let dz = (self.rh * self.rh - dx * dx - y * y).max(0.0).sqrt();
        let dir = DVec3::new(dx, y, dz).normalize_or(DVec3::Z).as_vec3();
        head.point(dir, narrow(off)).as_dvec3()
    }

    /// The part down the forehead, along the outline.
    fn forehead(&self, head: &dyn HeadSurface, pts: &mut Vec<DVec3>) {
        let edge = &self.g.edge;
        let top = self.g.locks.as_ref().map_or_else(|| edge.first().map_or(0.0, |r| r[0]), |l| l.top);
        let heights: Vec<f64> = match &self.g.locks {
            Some(l) => l.upper.clone(),
            None => edge.iter().map(|r| r[0]).collect(),
        };
        let gap = self.g.spread * self.i;
        for y in heights {
            let fy = smoothstep(top - y, 0.0, FOREHEAD_RAMP);
            let x = frame_x(edge, y) + gap * fy + 0.020 * fy + 0.004;
            pts.push(self.surf_at(head, self.side * x, y, 0.034 + 0.004 * self.i));
        }
    }

    /// The rest of a lock from the prototype's heights.
    fn along_locks(
        &self,
        locks: &FramingLocks,
        head: &dyn HeadSurface,
        centre: Vec3,
        sway: &Sway,
        pts: &mut Vec<DVec3>,
    ) {
        let (sign, lock, edge) = (self.side, self.i, &self.g.edge);
        let gap = self.g.spread * lock;
        let at = |x: f64, y: f64, z: f64| centre.as_dvec3() + DVec3::new(x, y, z);
        if self.tucked {
            let reach = locks.tuck_max_x.min(frame_x(edge, locks.tuck_a) + gap + 0.012);
            pts.push(self.surf_at(head, sign * reach, locks.tuck_b, 0.044 + 0.004 * lock));
            pts.push(at(sign * locks.ear_top[0], locks.ear_top[1], locks.ear_top[2] - 0.004 * lock));
            pts.push(at(sign * locks.ear_back[0], locks.ear_back[1], locks.ear_back[2] - 0.006 * lock));
            let last = locks.drop.len().saturating_sub(1);
            for (k, [px, py, pz]) in locks.drop.iter().copied().enumerate() {
                let y = if k == last { py + 0.02 * lock } else { py };
                // The prototype's phase is on the bind-space height.
                let wave =
                    sway.side((f64::from(centre.y) + y) * 40.0) + sway.tip(index(k + 1) / index(locks.drop.len()));
                pts.push(at(sign * (px + 0.012 * lock + wave), y, pz - 0.01 * lock));
            }
            return;
        }
        for &y in &locks.lower {
            pts.push(self.surf_at(head, sign * (frame_x(edge, y) + gap * 0.9 + 0.022), y, 0.030 + 0.005 * lock));
        }
        let fx = frame_x(edge, locks.cheek);
        if sign > 0.0 {
            // The cheek lock takes the wave's tip curl at its two points, so `wave.curl` moves it too.
            pts.push(at(sign * (fx + 0.006 + sway.tip(0.5)), locks.cheek - 0.005, locks.jaw_z[0]));
            pts.push(at(sign * (fx - 0.004 + sway.tip(1.0)), locks.cheek - 0.035, locks.jaw_z[1]));
        } else {
            let end =
                locks.end.get(self.i_usize().min(locks.end.len().saturating_sub(1))).copied().unwrap_or(locks.cheek);
            let x0 = fx + gap * 0.9 + 0.008;
            for step in 1..=6_u8 {
                let t = f64::from(step) / 6.0;
                let sideways =
                    x0 + 0.03 * smoothstep(t, 0.0, 0.5) + sway.side(t * sway.turns) * sway.grow(t) + sway.tip(t);
                let depth = lerp(locks.chest_z[0], locks.chest_z[1] - 0.006 * lock, smoothstep(t, 0.0, 0.4))
                    + sway.depth(t * sway.turns);
                pts.push(at(sign * sideways, lerp(locks.cheek, end, t), depth));
            }
        }
    }

    /// The rest of a lock from `tail`, swayed by `wave`.
    fn along_tail(&self, head: &dyn HeadSurface, sway: &Sway, pts: &mut Vec<DVec3>) {
        let tail = &self.g.tail;
        for (j, p) in tail.iter().enumerate() {
            let t = index(j + 1) / index(tail.len());
            let p = if self.side < 0.0 { p.mirrored() } else { *p };
            let shift = DVec3::new(
                self.side * self.g.spread * self.i * 0.9
                    + self.side * (sway.side(t * sway.turns) * sway.grow(t) + sway.tip(t)),
                0.0,
                -0.006 * self.i,
            );
            pts.push(p.resolve(head).as_dvec3() + shift);
        }
    }

    fn i_usize(&self) -> usize {
        // A lock index is small and never negative.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let i = self.i as usize;
        i
    }
}

/// The sway of one lock: the group's wave, with this lock's two draws.
struct Sway {
    /// `wv` plus the wave's own phase.
    phase: f64,
    /// Radians per unit of `t`.
    turns: f64,
    /// Amplitude, metres.
    amplitude: f64,
    /// The wave's tip curl, metres.
    curl: f64,
    growth: [f64; 2],
}

impl Sway {
    fn new(g: &FramingGen, wv: f64, wa: f64) -> Self {
        Self {
            phase: wv + g.wave.phase,
            turns: std::f64::consts::TAU * g.wave.frequency,
            amplitude: g.wave.amplitude * wa / WAVE_UNIT,
            curl: g.wave.curl,
            growth: g.wave.growth,
        }
    }

    /// The sideways offset at wave argument `arg` (radians before the phase).
    fn side(&self, arg: f64) -> f64 {
        (arg + self.phase).sin() * self.amplitude
    }

    /// The forward-back offset, a quarter turn behind [`side`](Self::side), of the prototype's fixed size.
    fn depth(&self, arg: f64) -> f64 {
        (arg + self.phase).cos() * DEPTH_AMPLITUDE
    }

    /// The outward curl of the last fifth of the tail, `wave.curl` at the tip (none in the prototype).
    fn tip(&self, t: f64) -> f64 {
        let curling = smoothstep(t, CURL_FROM, 1.0);
        self.curl * curling * curling
    }

    /// How far the sway has grown in at `t`.
    fn grow(&self, t: f64) -> f64 {
        smoothstep(t, self.growth[0], self.growth[1])
    }
}

/// A straight drop for a path that cannot be resampled (a non-finite parameter), so the group still has the right
/// number of locks.
fn straight_fall(head: &dyn HeadSurface, n: usize) -> Vec<Vec3> {
    let root = head.point(Vec3::Z, 0.03);
    (0..n).map(|k| root - Vec3::Y * narrow(FALLBACK_LENGTH * index(k) / index(n.saturating_sub(1).max(1)))).collect()
}

#[allow(clippy::cast_possible_truncation)] // offsets and lengths are metres
fn narrow(x: f64) -> f32 {
    x as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sphere of radius 0.2 at the origin.
    #[derive(Debug)]
    struct Ball;

    impl HeadSurface for Ball {
        fn centre(&self) -> Vec3 {
            Vec3::ZERO
        }
        fn radius(&self, _dir: Vec3) -> f32 {
            0.2
        }
    }

    #[test]
    fn surface_projection_keeps_the_authored_height() {
        let g = FramingGen::default();
        let lock = Lock { g: &g, side: 1.0, i: 0.0, rh: 0.2, exact: false, tucked: false };
        // Outside the front disc (0.18² + y² > 0.2²), and the default lower point y = -0.2 on the rim.
        for y in [-0.15, -0.1, 0.0, 0.1, -0.2] {
            let p = lock.surf_at(&Ball, 0.18, y, 0.0);
            let dir = p.normalize();
            assert!((dir.y - y / 0.2).abs() < 1e-5, "y {y}: {dir}");
        }
    }
}
