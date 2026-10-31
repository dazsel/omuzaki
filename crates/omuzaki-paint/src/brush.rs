//! Brushes: how a stroke's points become paint.
//!
//! A stroke is a line through points, each with a pen pressure. The line is smoothed (a centripetal Catmull-Rom
//! spline through the points, so an agent can give a few points and get a clean curve), walked along its length, and
//! stamped with round or flat dabs every `spacing` of a dab's width. Each dab's size and opacity follow the pressure,
//! the tapers at the ends, and the brush's jitter. The dabs build one coverage [`Mask`] for the stroke, either as a
//! wash (the strongest dab wins, so a stroke never darkens where it overlaps itself: ink, markers) or built up (each
//! dab adds over the last: airbrush, soft shading). Grain multiplies the mask by a paper texture (pencil, chalk). The
//! stroke's colour then goes down through the mask once.
//!
//! A brush with `smudge` above 0 paints no colour of its own: it picks up the colour under each dab and drags it along
//! (the blender).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::blend::{BlendMode, Keep};
use crate::canvas::{Canvas, Mask, Rect};
use crate::colour::Px;
use crate::error::PaintError;
use crate::noise::{Rng, grain};

/// Every setting of a brush. A [`BrushSettings`] overrides some of a preset's.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Brush {
    /// How much pen pressure shrinks the dab: 0 keeps the size, 1 makes it proportional to pressure.
    pub size_pressure: f64,
    /// How much pen pressure fades the dab: 0 keeps the opacity, 1 makes it proportional to pressure.
    pub opacity_pressure: f64,
    /// The smallest dab, as a share of the size, at zero pressure.
    pub min_size: f64,
    /// 1 is a crisp edge; 0 fades from the middle out.
    pub hardness: f64,
    /// The gap between dabs, as a share of a dab's width (0.01 to 2).
    pub spacing: f64,
    /// Each dab's opacity, 0 to 1.
    pub flow: f64,
    /// `false`: a wash (the strongest dab wins; ink and markers). `true`: dabs build up over each other (airbrush).
    pub buildup: bool,
    /// The dab's height over its width, 0.05 to 1 (1 is round, less is a flat nib).
    pub roundness: f64,
    /// The flat nib's angle in degrees, anticlockwise from the x axis.
    pub angle: f64,
    /// Turn the nib with the stroke's direction (`angle` is then relative to it).
    pub follow: bool,
    /// Paper grain strength, 0 to 1 (pencil, chalk).
    pub grain: f64,
    /// The grain's cell size in pixels.
    pub grain_scale: f64,
    /// How far dabs scatter off the line, as a share of the size.
    pub scatter: f64,
    /// Random shrink of each dab, 0 to 1.
    pub size_jitter: f64,
    /// Random fade of each dab, 0 to 1.
    pub opacity_jitter: f64,
    /// Smudging, 0 to 1: above 0 the brush drags the colour under it instead of painting its own (the blender).
    pub smudge: f64,
    /// The length in pixels over which a stroke thins in from its start (0: no taper).
    pub taper_start: f64,
    /// The length in pixels over which a stroke thins out to its end (0: no taper).
    pub taper_end: f64,
    /// How the stroke lands on its layer by default (a stroke's own `blend` overrides it); the eraser erases.
    pub erase: bool,
}

/// Some settings of a brush, each overriding the preset's when set.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[allow(missing_docs)] // each field is the `Brush` field of the same name
pub struct BrushSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_pressure: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity_pressure: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardness: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spacing: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buildup: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roundness: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grain: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grain_scale: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scatter: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_jitter: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity_jitter: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smudge: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taper_start: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taper_end: Option<f64>,
}

impl Brush {
    /// A plain hard round brush with no pressure, the base the presets start from.
    pub const BASE: Self = Self {
        size_pressure: 0.0,
        opacity_pressure: 0.0,
        min_size: 0.0,
        hardness: 1.0,
        spacing: 0.05,
        flow: 1.0,
        buildup: false,
        roundness: 1.0,
        angle: 0.0,
        follow: false,
        grain: 0.0,
        grain_scale: 3.0,
        scatter: 0.0,
        size_jitter: 0.0,
        opacity_jitter: 0.0,
        smudge: 0.0,
        taper_start: 0.0,
        taper_end: 0.0,
        erase: false,
    };

    /// This brush with `settings` laid over it.
    #[must_use]
    pub fn with(&self, s: &BrushSettings) -> Self {
        let mut b = self.clone();
        macro_rules! set {
            ($($f:ident),*) => { $( if let Some(v) = s.$f { b.$f = v; } )* };
        }
        set!(
            size_pressure,
            opacity_pressure,
            min_size,
            hardness,
            spacing,
            flow,
            buildup,
            roundness,
            angle,
            follow,
            grain,
            grain_scale,
            scatter,
            size_jitter,
            opacity_jitter,
            smudge,
            taper_start,
            taper_end
        );
        b
    }

    /// Checks every setting is in its range.
    ///
    /// # Errors
    /// [`PaintError::Invalid`] naming the first setting out of range, under `field`.
    pub fn check(&self, field: &str) -> Result<(), PaintError> {
        let unit = [
            ("size_pressure", self.size_pressure),
            ("opacity_pressure", self.opacity_pressure),
            ("min_size", self.min_size),
            ("hardness", self.hardness),
            ("flow", self.flow),
            ("grain", self.grain),
            ("size_jitter", self.size_jitter),
            ("opacity_jitter", self.opacity_jitter),
            ("smudge", self.smudge),
        ];
        for (name, v) in unit {
            if !(0.0..=1.0).contains(&v) {
                return Err(PaintError::invalid(format!("{field}/{name}"), format!("{v} is outside 0 to 1")));
            }
        }
        let ranged = [
            ("spacing", self.spacing, 0.01, 2.0),
            ("roundness", self.roundness, 0.05, 1.0),
            ("angle", self.angle, -360.0, 360.0),
            ("grain_scale", self.grain_scale, 0.5, 256.0),
            ("scatter", self.scatter, 0.0, 4.0),
            ("taper_start", self.taper_start, 0.0, 8192.0),
            ("taper_end", self.taper_end, 0.0, 8192.0),
        ];
        for (name, v, lo, hi) in ranged {
            if !(lo..=hi).contains(&v) {
                return Err(PaintError::invalid(format!("{field}/{name}"), format!("{v} is outside {lo} to {hi}")));
            }
        }
        Ok(())
    }
}

/// A named brush and what it is for.
#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    /// Its name in `Stroke.brush`.
    pub name: &'static str,
    /// What it is for, in a line.
    pub use_for: &'static str,
    /// Its settings.
    pub brush: Brush,
}

/// The brush presets, the kit a painter reaches for: line art, colour, shading, texture, blending and erasing.
pub fn presets() -> Vec<Preset> {
    let b = Brush::BASE;
    vec![
        Preset {
            name: "ink",
            use_for: "clean line art: crisp, thins with pressure, tapers at both ends",
            brush: Brush { size_pressure: 1.0, min_size: 0.05, taper_start: 12.0, taper_end: 18.0, ..b.clone() },
        },
        Preset {
            name: "g_pen",
            use_for: "manga line art with strong thick-thin: crisp, pressure to size, long tapers",
            brush: Brush { size_pressure: 1.0, min_size: 0.0, taper_start: 20.0, taper_end: 30.0, ..b.clone() },
        },
        Preset {
            name: "maru_pen",
            use_for: "fine even lines: lashes, hair strands, small details",
            brush: Brush { size_pressure: 0.5, min_size: 0.3, taper_start: 4.0, taper_end: 6.0, ..b.clone() },
        },
        Preset {
            name: "hard_round",
            use_for: "flat colour and cel shadow shapes: crisp edge, no pressure",
            brush: b.clone(),
        },
        Preset {
            name: "marker",
            use_for: "even flat strokes that never darken where they cross: fills, cel shading",
            brush: Brush { hardness: 0.85, ..b.clone() },
        },
        Preset {
            name: "soft_round",
            use_for: "soft shading and blush: soft edge, opacity follows pressure, builds up",
            brush: Brush {
                hardness: 0.0,
                flow: 0.35,
                buildup: true,
                opacity_pressure: 1.0,
                spacing: 0.08,
                ..b.clone()
            },
        },
        Preset {
            name: "airbrush",
            use_for: "gradual soft gradients and glows: very soft, low flow, builds up",
            brush: Brush { hardness: 0.0, flow: 0.08, buildup: true, spacing: 0.06, ..b.clone() },
        },
        Preset {
            name: "pencil",
            use_for: "sketching and textured lines: grainy, thins and fades with pressure",
            brush: Brush {
                size_pressure: 0.4,
                opacity_pressure: 0.8,
                min_size: 0.4,
                hardness: 0.8,
                grain: 0.75,
                grain_scale: 1.6,
                ..b.clone()
            },
        },
        Preset {
            name: "chalk",
            use_for: "rough painted texture: soft, grainy, builds up",
            brush: Brush { hardness: 0.4, flow: 0.5, buildup: true, grain: 0.9, grain_scale: 4.0, ..b.clone() },
        },
        Preset {
            name: "flat_brush",
            use_for: "painterly strokes and hair clumps: a flat nib that turns with the stroke",
            brush: Brush { roundness: 0.3, follow: true, angle: 90.0, hardness: 0.7, size_pressure: 0.5, ..b.clone() },
        },
        Preset {
            name: "watercolor",
            use_for: "transparent washes: soft, light flow that builds up, slight scatter",
            brush: Brush { hardness: 0.2, flow: 0.15, buildup: true, scatter: 0.05, opacity_jitter: 0.3, ..b.clone() },
        },
        Preset {
            name: "spray",
            use_for: "speckles, stars and glitter: scattered small dabs",
            brush: Brush { scatter: 1.5, size_jitter: 0.8, spacing: 0.6, buildup: true, ..b.clone() },
        },
        Preset {
            name: "blender",
            use_for: "smoothing paint already down: drags the colour under it",
            brush: Brush { hardness: 0.3, smudge: 0.6, flow: 0.6, spacing: 0.1, ..b.clone() },
        },
        Preset {
            name: "eraser",
            use_for: "erasing: a hard round that removes paint",
            brush: Brush { erase: true, ..b.clone() },
        },
        Preset {
            name: "soft_eraser",
            use_for: "fading paint out: a soft round that removes paint",
            brush: Brush { erase: true, hardness: 0.0, flow: 0.4, buildup: true, ..b },
        },
    ]
}

/// The preset called `name`.
///
/// # Errors
/// [`PaintError::Invalid`] listing the presets when there is none of that name.
pub fn preset(name: &str) -> Result<Brush, PaintError> {
    let all = presets();
    all.iter().find(|p| p.name == name).map(|p| p.brush.clone()).ok_or_else(|| {
        let names: Vec<&str> = all.iter().map(|p| p.name).collect();
        PaintError::invalid("brush", format!("there is no brush {name:?}; the presets are {}", names.join(", ")))
    })
}

/// A stroke ready to stamp.
#[derive(Clone, Debug)]
pub struct StrokeInput<'a> {
    /// The points, `[x, y, pressure]` in canvas pixels.
    pub points: &'a [[f64; 3]],
    /// The brush.
    pub brush: &'a Brush,
    /// The dab width in pixels at full pressure.
    pub size: f64,
    /// Smooth the line through the points (a spline) instead of joining them straight.
    pub smooth: bool,
    /// Join the last point back to the first.
    pub closed: bool,
    /// The seed of the brush's jitter and grain.
    pub seed: u64,
}

/// One stamp of the brush.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Dab {
    /// Centre x, pixels.
    pub x: f64,
    /// Centre y, pixels.
    pub y: f64,
    /// Radius, pixels.
    pub r: f64,
    /// Opacity, 0 to 1.
    pub a: f64,
    /// Nib angle, radians.
    pub angle: f64,
}

/// A point of the smoothed line: position, pressure, and the length along the line to it.
#[derive(Copy, Clone, Debug)]
struct Sample {
    x: f64,
    y: f64,
    p: f64,
    s: f64,
}

/// The centripetal Catmull-Rom point between `p1` and `p2` at `t` (0..1), with `p0` and `p3` beside them.
fn catmull(p0: [f64; 3], p1: [f64; 3], p2: [f64; 3], p3: [f64; 3], t: f64) -> [f64; 3] {
    let knot = |a: [f64; 3], b: [f64; 3]| ((b[0] - a[0]).hypot(b[1] - a[1])).sqrt().max(1e-6);
    let (t0, t1) = (0.0, knot(p0, p1));
    let t2 = t1 + knot(p1, p2);
    let t3 = t2 + knot(p2, p3);
    let u = t1 + (t2 - t1) * t;
    let lerp = |a: [f64; 3], b: [f64; 3], ta: f64, tb: f64| {
        let k = (u - ta) / (tb - ta);
        [a[0] + (b[0] - a[0]) * k, a[1] + (b[1] - a[1]) * k, a[2] + (b[2] - a[2]) * k]
    };
    let a1 = lerp(p0, p1, t0, t1);
    let a2 = lerp(p1, p2, t1, t2);
    let a3 = lerp(p2, p3, t2, t3);
    let b1 = lerp(a1, a2, t0, t2);
    let b2 = lerp(a2, a3, t1, t3);
    let c = lerp(b1, b2, t1, t2);
    // Pressure follows the segment linearly: a spline can overshoot it below 0.
    [c[0], c[1], p1[2] + (p2[2] - p1[2]) * t]
}

/// The smoothed (or straight) line through the points, densely enough to place dabs on.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap
)] // point counts are capped far below isize::MAX
fn samples(input: &StrokeInput<'_>) -> Vec<Sample> {
    let mut pts: Vec<[f64; 3]> = input.points.to_vec();
    if input.closed && pts.len() > 2 {
        pts.push(pts[0]);
    }
    let n = pts.len();
    let mut out: Vec<Sample> = Vec::new();
    let push = |q: [f64; 3], out: &mut Vec<Sample>| {
        let s = out.last().map_or(0.0, |l: &Sample| l.s + (q[0] - l.x).hypot(q[1] - l.y));
        out.push(Sample { x: q[0], y: q[1], p: q[2], s });
    };
    if n == 1 {
        push(pts[0], &mut out);
        return out;
    }
    for i in 0..n - 1 {
        let (p1, p2) = (pts[i], pts[i + 1]);
        let len = (p2[0] - p1[0]).hypot(p2[1] - p1[1]);
        let steps = ((len / 0.5).ceil() as usize).clamp(1, 1 << 16);
        let ends = |j: isize| -> [f64; 3] {
            if input.closed && pts.len() > 3 {
                // A closed loop wraps round (the repeated first point is skipped).
                let m = (n - 1) as isize;
                pts[(((i as isize + j) % m + m) % m) as usize]
            } else {
                let k = (i as isize + j).clamp(0, n as isize - 1) as usize;
                pts[k]
            }
        };
        let (p0, p3) = if input.smooth && n > 2 {
            let p0 = if i == 0 && !input.closed { mirror(p2, p1) } else { ends(-1) };
            let p3 = if i + 2 >= n && !input.closed { mirror(p1, p2) } else { ends(2) };
            (p0, p3)
        } else {
            (p1, p2)
        };
        for k in usize::from(i > 0)..=steps {
            let t = k as f64 / steps as f64;
            let q = if input.smooth && n > 2 {
                catmull(p0, p1, p2, p3, t)
            } else {
                [p1[0] + (p2[0] - p1[0]) * t, p1[1] + (p2[1] - p1[1]) * t, p1[2] + (p2[2] - p1[2]) * t]
            };
            push(q, &mut out);
        }
    }
    out
}

/// `b` reflected through `a` (a phantom point past the end of an open spline).
fn mirror(b: [f64; 3], a: [f64; 3]) -> [f64; 3] {
    [2.0 * a[0] - b[0], 2.0 * a[1] - b[1], a[2]]
}

/// How much of the full width is left at `d` pixels from an end of length `taper` (an ease-out to a point).
fn taper(d: f64, taper: f64) -> f64 {
    if taper <= 0.0 {
        return 1.0;
    }
    let t = (d / taper).clamp(0.0, 1.0);
    1.0 - (1.0 - t) * (1.0 - t)
}

/// The dabs of a stroke, in order.
pub fn dabs(input: &StrokeInput<'_>) -> Vec<Dab> {
    let b = input.brush;
    let line = samples(input);
    let Some(last) = line.last() else { return Vec::new() };
    let total = last.s;
    let mut rng = Rng::new(input.seed);
    let mut out = Vec::new();
    let mut s = 0.0;
    let mut at = 0;
    loop {
        while at + 1 < line.len() && line[at + 1].s < s {
            at += 1;
        }
        let (a, c) = (line[at], line[(at + 1).min(line.len() - 1)]);
        let k = if c.s > a.s { ((s - a.s) / (c.s - a.s)).clamp(0.0, 1.0) } else { 0.0 };
        let (x, y, p) = (a.x + (c.x - a.x) * k, a.y + (c.y - a.y) * k, (a.p + (c.p - a.p) * k).clamp(0.0, 1.0));
        let tap = if input.closed { 1.0 } else { taper(s, b.taper_start).min(taper(total - s, b.taper_end)) };
        let size_k = (1.0 - b.size_pressure * (1.0 - p)).max(b.min_size);
        let mut diameter = input.size * size_k * tap;
        diameter *= 1.0 - b.size_jitter * rng.next();
        let mut alpha = b.flow * (1.0 - b.opacity_pressure * (1.0 - p));
        alpha *= 1.0 - b.opacity_jitter * rng.next();
        let (dx, dy) = (c.x - a.x, c.y - a.y);
        let heading = if dx == 0.0 && dy == 0.0 { 0.0 } else { libm::atan2(dy, dx) };
        let mut angle = b.angle.to_radians();
        if b.follow {
            angle += heading;
        }
        let (sx, sy) = (rng.signed(), rng.signed());
        let spread = b.scatter * input.size;
        if diameter > 0.0 && alpha > 0.0 {
            out.push(Dab { x: x + sx * spread, y: y + sy * spread, r: diameter / 2.0, a: alpha, angle });
        }
        if s >= total {
            break;
        }
        let step = (b.spacing * (input.size * size_k * tap).max(1.0)).max(0.25);
        s = (s + step).min(total);
        if out.len() > MAX_DABS {
            break;
        }
    }
    out
}

/// The most dabs one stroke stamps, so a pathological stroke can't run for minutes.
pub const MAX_DABS: usize = 2_000_000;

/// A dab's coverage of pixel `(px, py)` (its centre at `+0.5`).
fn dab_cover(d: &Dab, hardness: f64, roundness: f64, px: i32, py: i32) -> f32 {
    let (cx, cy) = (f64::from(px) + 0.5 - d.x, f64::from(py) + 0.5 - d.y);
    let (sin, cos) = (libm::sin(-d.angle), libm::cos(-d.angle));
    let (u, v) = (cx * cos - cy * sin, cx * sin + cy * cos);
    let dist = u.hypot(v / roundness);
    let r = d.r;
    let edge = (r - dist + 0.5).clamp(0.0, 1.0);
    if edge <= 0.0 {
        return 0.0;
    }
    let core = r * hardness;
    let fall = if dist <= core || r - core <= 1e-9 {
        1.0
    } else {
        let t = ((dist - core) / (r - core)).clamp(0.0, 1.0);
        1.0 - t * t * (3.0 - 2.0 * t)
    };
    #[allow(clippy::cast_possible_truncation)] // 0..1
    let c = (edge * fall) as f32;
    c
}

/// A dab smaller than this radius is drawn at it, fainter by its lost area, so hairlines stay continuous.
const MIN_RADIUS: f64 = 0.7;

/// The stroke's coverage, before its colour, over the canvas `clip`.
pub fn stroke_mask(input: &StrokeInput<'_>, clip: Rect) -> Mask {
    let b = input.brush;
    let all = dabs(input);
    let mut bounds = Rect::default();
    for d in &all {
        let r = d.r.max(MIN_RADIUS) + 1.0;
        bounds = bounds.union(Rect::covering(d.x - r, d.y - r, d.x + r, d.y + r));
    }
    let rect = bounds.clip(clip);
    let mut mask = Mask::new(rect);
    for d in &all {
        let mut d = *d;
        if d.r < MIN_RADIUS {
            d.a *= (d.r / MIN_RADIUS) * (d.r / MIN_RADIUS);
            d.r = MIN_RADIUS;
        }
        stamp(&mut mask, &d, b, rect);
    }
    if b.grain > 0.0 {
        apply_grain(&mut mask, b, input.seed);
    }
    mask
}

fn stamp(mask: &mut Mask, d: &Dab, b: &Brush, rect: Rect) {
    let r = d.r + 1.0;
    let area = Rect::covering(d.x - r, d.y - r, d.x + r, d.y + r).clip(rect);
    #[allow(clippy::cast_possible_truncation)] // 0..1
    let a = d.a as f32;
    for y in area.y0..area.y1 {
        for x in area.x0..area.x1 {
            let c = dab_cover(d, b.hardness, b.roundness, x, y) * a;
            if c <= 0.0 {
                continue;
            }
            if let Some(v) = mask.at_mut(x, y) {
                *v = if b.buildup { *v + c * (1.0 - *v) } else { v.max(c) };
            }
        }
    }
}

fn apply_grain(mask: &mut Mask, b: &Brush, seed: u64) {
    let r = mask.rect;
    #[allow(clippy::cast_possible_truncation)] // 0..1
    let strength = b.grain as f32;
    for y in r.y0..r.y1 {
        for x in r.x0..r.x1 {
            let g = grain(seed, f64::from(x), f64::from(y), b.grain_scale);
            let g = ((g - 0.3) / 0.4).clamp(0.0, 1.0);
            if let Some(v) = mask.at_mut(x, y) {
                *v *= 1.0 - strength + strength * g;
            }
        }
    }
}

/// Drags the colour under the stroke along it (a brush with `smudge` above 0), on `canvas` itself.
pub fn smudge(canvas: &mut Canvas, input: &StrokeInput<'_>, opacity: f64, keep: Keep) {
    let b = input.brush;
    let mut carry: Option<Px> = None;
    for d in dabs(input) {
        let r = d.r.max(MIN_RADIUS);
        let area = Rect::covering(d.x - r - 1.0, d.y - r - 1.0, d.x + r + 1.0, d.y + r + 1.0).clip(canvas.bounds());
        let mut under = [0.0f32; 4];
        let mut weight = 0.0f32;
        for y in area.y0..area.y1 {
            for x in area.x0..area.x1 {
                let c = dab_cover(&Dab { r, ..d }, b.hardness, b.roundness, x, y);
                let p = canvas.get(x, y);
                for i in 0..4 {
                    under[i] += p[i] * c;
                }
                weight += c;
            }
        }
        if weight <= 0.0 {
            continue;
        }
        let under = under.map(|v| v / weight);
        let held = *carry.get_or_insert(under);
        #[allow(clippy::cast_possible_truncation)] // 0..1
        let a = (d.a * opacity * b.smudge) as f32;
        for y in area.y0..area.y1 {
            for x in area.x0..area.x1 {
                let c = dab_cover(&Dab { r, ..d }, b.hardness, b.roundness, x, y) * a;
                if c > 0.0
                    && let Some(px) = canvas.at_mut(x, y)
                {
                    // Smudging moves colour and coverage together: mix the pixel towards what the brush holds.
                    let alpha = px[3];
                    for i in 0..4 {
                        px[i] += (held[i] - px[i]) * c;
                    }
                    if keep == Keep::Atop {
                        // Under alpha lock the coverage stays; only the colour moves.
                        let k = if px[3] > 0.0 { alpha / px[3] } else { 0.0 };
                        *px = [px[0] * k, px[1] * k, px[2] * k, alpha];
                    }
                }
            }
        }
        #[allow(clippy::cast_possible_truncation)] // 0..1
        let pickup = (1.0 - b.smudge * 0.9) as f32;
        carry = Some(std::array::from_fn(|i| held[i] + (under[i] - held[i]) * pickup));
    }
}

/// The blend a stroke with this brush uses when it names none: erase for the erasers, else normal.
pub fn default_blend(b: &Brush) -> Option<BlendMode> {
    if b.erase { None } else { Some(BlendMode::Normal) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIP: Rect = Rect { x0: 0, y0: 0, x1: 256, y1: 256 };

    fn line(brush: &Brush, size: f64, pts: &[[f64; 3]]) -> Mask {
        stroke_mask(&StrokeInput { points: pts, brush, size, smooth: true, closed: false, seed: 1 }, CLIP)
    }

    #[test]
    fn every_preset_is_in_range() {
        for p in presets() {
            p.brush.check(p.name).unwrap();
        }
        assert!(preset("nope").is_err());
    }

    #[test]
    fn a_hard_line_covers_its_width_times_its_length() {
        let m = line(&Brush::BASE, 6.0, &[[20.0, 100.0, 1.0], [220.0, 100.0, 1.0]]);
        // 200 long and 6 wide, plus two round caps of radius 3.
        let want = 200.0 * 6.0 + std::f64::consts::PI * 9.0;
        assert!((m.area() - want).abs() / want < 0.02, "{} vs {want}", m.area());
        assert!((m.get(120, 100) - 1.0).abs() < 1e-6);
        assert_eq!(m.get(120, 105), 0.0);
    }

    #[test]
    fn a_wash_never_darkens_where_it_crosses_itself() {
        let m = line(
            &Brush { flow: 0.5, ..Brush::BASE },
            8.0,
            &[[20.0, 20.0, 1.0], [200.0, 200.0, 1.0], [200.0, 20.0, 1.0], [20.0, 200.0, 1.0]],
        );
        assert!(m.values.iter().all(|&v| v <= 0.5 + 1e-6));
    }

    #[test]
    fn tapers_thin_the_ends() {
        let b = preset("ink").unwrap();
        let m = line(&b, 10.0, &[[20.0, 100.0, 1.0], [230.0, 100.0, 1.0]]);
        let column = |x: i32| (90..110).map(|y| m.get(x, y)).sum::<f32>();
        assert!(column(21) < column(120) * 0.6, "{} {}", column(21), column(120));
        assert!(column(229) < column(120) * 0.6);
    }

    #[test]
    fn pressure_shrinks_the_ink() {
        let b = Brush { size_pressure: 1.0, ..Brush::BASE };
        let m = line(&b, 12.0, &[[20.0, 100.0, 0.2], [230.0, 100.0, 1.0]]);
        let column = |x: i32| (80..120).map(|y| m.get(x, y)).sum::<f32>();
        assert!(column(30) < column(220) * 0.5);
    }

    #[test]
    fn the_spline_passes_through_every_point() {
        let pts = [[20.0, 200.0, 1.0], [80.0, 40.0, 1.0], [160.0, 180.0, 1.0], [230.0, 60.0, 1.0]];
        let m = line(&Brush::BASE, 4.0, &pts);
        for p in pts {
            #[allow(clippy::cast_possible_truncation)]
            let v = m.get(p[0] as i32, p[1] as i32);
            assert!(v > 0.9, "{p:?} {v}");
        }
    }

    #[test]
    fn grain_breaks_up_the_pencil_and_is_seeded() {
        let b = preset("pencil").unwrap();
        let a = line(&b, 10.0, &[[20.0, 100.0, 1.0], [230.0, 100.0, 1.0]]);
        let again = line(&b, 10.0, &[[20.0, 100.0, 1.0], [230.0, 100.0, 1.0]]);
        assert_eq!(a, again);
        let row: Vec<f32> = (40..200).map(|x| a.get(x, 100)).collect();
        let (lo, hi) = row.iter().fold((1.0f32, 0.0f32), |(l, h), &v| (l.min(v), h.max(v)));
        assert!(hi - lo > 0.2, "{lo} {hi}");
    }

    #[test]
    fn the_blender_mixes_two_colours_where_it_passes() {
        let mut c = Canvas::new(64, 16).unwrap();
        for y in 0..16 {
            for x in 0..64 {
                if let Some(p) = c.at_mut(x, y) {
                    *p = if x < 32 { [1.0, 0.0, 0.0, 1.0] } else { [0.0, 0.0, 1.0, 1.0] };
                }
            }
        }
        let b = preset("blender").unwrap();
        let pts = [[10.0, 8.0, 1.0], [54.0, 8.0, 1.0]];
        smudge(
            &mut c,
            &StrokeInput { points: &pts, brush: &b, size: 10.0, smooth: false, closed: false, seed: 0 },
            1.0,
            Keep::Over,
        );
        let p = c.get(36, 8);
        assert!(p[0] > 0.1 && p[2] > 0.1, "{p:?}");
    }
}
