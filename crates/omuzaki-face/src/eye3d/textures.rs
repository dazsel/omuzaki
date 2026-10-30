//! The eye's atlas: the iris on the left half and the white of the eye on the right, each a disc (see `eye.wgsl` in
//! `omuzaki-render`). The iris is drawn in polar terms, as the anatomy is laid out: the pupillary zone inside the
//! collarette and the ciliary zone outside it, thousands of radial fibres at three scales, crypts opening round the
//! collarette, contraction furrows, pigment specks and the limbal ring. The white is a polar map of the angle from the
//! gaze axis, with a fine vessel network and vessels reaching in from the corners.

use std::f32::consts::{PI, TAU};

use omuzaki_core::glam::{Vec2, Vec3};

use super::canvas::Canvas;
use super::design::{Anime, EyeDesign, Iris, Sclera};
use super::noise::{Pcg, fbm, smoothstep, value_wrapped, worley};

/// Each disc's radius as a share of its half of the atlas (the shader's `ATLAS_DISC`, doubled).
pub const DISC: f32 = 0.96;
/// The pupil radius the iris is drawn for, in iris radii (the shader's `DRAWN_PUPIL`).
pub const DRAWN_PUPIL: f32 = 0.3;
/// The white's polar map reaches this far from the gaze axis at its rim (the shader's `SCLERA_REACH`), radians.
pub const SCLERA_REACH: f32 = 1.919_862_2;

const FIBRE_STREAM: u32 = 0x1f1b;
const CRYPT_STREAM: u32 = 0x2c3d;
const SPECK_STREAM: u32 = 0x3e4f;
const VESSEL_STREAM: u32 = 0x4a5b;

/// The atlas, `2 * size` by `size` sRGB bytes (RGBA).
#[derive(Clone, Debug)]
pub struct Atlas {
    /// Pixels across: two halves.
    pub width: u32,
    /// Pixels down.
    pub height: u32,
    /// sRGB bytes, RGBA, row by row from the top.
    pub rgba: Vec<u8>,
}

/// Draws the eye's atlas at `size` pixels per half (a power of two keeps mipmaps exact).
pub fn atlas(design: &EyeDesign, size: u32) -> Atlas {
    let style = design.style.clamp(0.0, 1.0);
    let mut iris =
        styled(style, || iris(&design.iris, design.seed, size), || anime_iris(&design.anime, design.seed, size));
    let mut sclera = styled(style, || sclera(&design.sclera, design.seed, size), || anime_sclera(&design.sclera, size));
    super::paint::iris(&mut iris, design);
    super::paint::sclera(&mut sclera, design);
    let (w, h) = (size as usize * 2, size as usize);
    let mut rgba = vec![0u8; w * h * 4];
    let (left, right) = (iris.to_srgb(), sclera.to_srgb());
    for y in 0..h {
        let row = y * w * 4;
        let half = y * h * 4;
        rgba[row..row + h * 4].copy_from_slice(&left[half..half + h * 4]);
        rgba[row + h * 4..row + w * 4].copy_from_slice(&right[half..half + h * 4]);
    }
    Atlas { width: size * 2, height: size, rgba }
}

/// The photoreal canvas at `style` 0, the anime one at 1, and a blend of the two between; only the canvases the style
/// shows are drawn.
pub(super) fn styled(style: f32, real: impl FnOnce() -> Canvas, anime: impl FnOnce() -> Canvas) -> Canvas {
    if style <= 0.0 {
        return real();
    }
    if style >= 1.0 {
        return anime();
    }
    let (mut real, anime) = (real(), anime());
    for (p, q) in real.pixels.iter_mut().zip(&anime.pixels) {
        *p = p.lerp(*q, style);
    }
    real
}

/// The anime iris: a vertical gradient from the dark top, under the lid's shadow, to the light bottom where the light
/// pools, a dark ring round the rim, a glowing crescent inside its lower edge, faint spokes, a darker rim round the
/// pupil, and the painted highlights (a large soft oval high on the light's side and a small dot below opposite).
pub fn anime_iris(anime: &Anime, seed: u32, size: u32) -> Canvas {
    let (top, mid, low, ring, pupil) =
        (lin(anime.top), lin(anime.mid), lin(anime.low), lin(anime.ring), lin(anime.pupil));
    let glow = low.lerp(Vec3::ONE, 0.45);
    let s = seed ^ FIBRE_STREAM;
    let mut canvas = Canvas::new(size, size, ring);
    canvas.fill(|u, v| {
        let (r, angle) = polar(u, v);
        if r < DRAWN_PUPIL {
            return pupil;
        }
        if r > 1.0 {
            return ring;
        }
        let y = r * angle.sin();
        // Down the iris in cel bands: the dark top under the lid's shadow held for a third, the middle, and the light
        // low, each edge soft enough not to alias.
        let k = 0.5 - 0.5 * y;
        let mut colour = top.lerp(mid, smoothstep(0.3, 0.42, k)).lerp(low, smoothstep(0.62, 0.86, k));
        // Faint spokes.
        let t = ((r - DRAWN_PUPIL) / (1.0 - DRAWN_PUPIL)).min(1.0);
        let a = (angle + PI) / TAU;
        let spoke = value_wrapped(a * 56.0, t * 1.5, 56, s ^ 0x5a);
        colour *= 0.94 + 0.12 * spoke;
        // The glowing crescent inside the lower rim.
        let crescent = smoothstep(0.5, 0.78, r) * (1.0 - smoothstep(0.8, 0.9, r)) * smoothstep(-0.25, -0.75, y);
        colour = colour.lerp(glow, 0.65 * crescent);
        // A darker rim round the pupil, and the ring round the iris.
        colour = colour.lerp(pupil, 0.55 * (1.0 - smoothstep(DRAWN_PUPIL, DRAWN_PUPIL + 0.1, r)));
        colour.lerp(ring, smoothstep(0.84, 0.93, r))
    });
    let h = anime.highlight.clamp(0.0, 1.0);
    if h > 0.0 {
        #[allow(clippy::cast_precision_loss)]
        let px = size as f32 * 0.5 * DISC;
        let at = |x: f32, y: f32| to_px(size, x.hypot(y), y.atan2(x));
        // High on the light's side (up and to the left, as the studio key is), and a dot below on the other side.
        canvas.disc(at(-0.36, 0.4), Vec2::new(0.24, 0.17) * px * h, Vec3::ONE, 1.0, 0.85);
        canvas.disc(at(0.34, -0.36), Vec2::splat(0.075) * px * h, Vec3::ONE, 0.95, 0.8);
    }
    canvas
}

/// The anime white: clean, a touch cooler and greyer toward its rim.
pub fn anime_sclera(sclera: &Sclera, size: u32) -> Canvas {
    let white = lin(sclera.white).lerp(Vec3::new(0.93, 0.94, 0.96), 0.7);
    let rim = white * Vec3::new(0.86, 0.88, 0.93);
    let mut canvas = Canvas::new(size, size, rim);
    canvas.fill(|u, v| {
        let (rho, _) = polar(u, v);
        white.lerp(rim, smoothstep(0.35, 0.95, rho))
    });
    canvas
}

fn lin(c: omuzaki_core::colour::Colour) -> Vec3 {
    c.to_linear()
}

/// Polar position of a disc pixel `(u, v)` (`0..1` across the half): radius in disc radii and angle.
fn polar(u: f32, v: f32) -> (f32, f32) {
    let q = Vec2::new((u - 0.5) / (0.5 * DISC), (0.5 - v) / (0.5 * DISC));
    (q.length(), q.y.atan2(q.x))
}

/// A disc pixel's position in the canvas from a radius (disc radii) and an angle.
fn to_px(size: u32, r: f32, angle: f32) -> Vec2 {
    #[allow(clippy::cast_precision_loss)]
    let s = size as f32;
    Vec2::new(s * (0.5 + 0.5 * DISC * r * angle.cos()), s * (0.5 - 0.5 * DISC * r * angle.sin()))
}

/// The radius (disc radii) of the iris at radial position `t` (0 the drawn pupil's edge, 1 the rim).
fn iris_r(t: f32) -> f32 {
    DRAWN_PUPIL + t * (1.0 - DRAWN_PUPIL)
}

/// The collarette's radial position round the iris: wavy at two scales.
fn collarette_at(iris: &Iris, a: f32, seed: u32) -> f32 {
    iris.collarette
        + 0.14 * (value_wrapped(a * 14.0, 0.5, 14, seed ^ 0x77) - 0.5)
        + 0.07 * (value_wrapped(a * 52.0, 0.5, 52, seed ^ 0x78) - 0.5)
}

/// The iris half of the atlas.
pub fn iris(iris: &Iris, seed: u32, size: u32) -> Canvas {
    let (outer, inner, deep, fibre, limbal) =
        (lin(iris.outer), lin(iris.inner), lin(iris.deep), lin(iris.fibre_colour), lin(iris.limbal_colour));
    let fibres = iris.fibres.clamp(0.0, 1.0);
    let ring = iris.limbal_ring.clamp(0.0, 1.0);
    let s = seed ^ FIBRE_STREAM;
    let mut canvas = Canvas::new(size, size, limbal);
    canvas.fill(|u, v| {
        let (r, angle) = polar(u, v);
        if r < DRAWN_PUPIL {
            return Vec3::splat(0.004);
        }
        let a = (angle + PI) / TAU;
        let t = ((r - DRAWN_PUPIL) / (1.0 - DRAWN_PUPIL)).min(1.0);
        let c = collarette_at(iris, a, seed);
        // The two zones, meeting at the collarette.
        let mut colour = inner.lerp(outer, smoothstep(c - 0.08, c + 0.1, t));
        // Large, soft variation in the stroma.
        colour *= 0.82 + 0.36 * fbm(u * 6.0, v * 6.0, 3, s ^ 0x11);
        // Mottling: darker and lighter blotches of pigment a few fibres across.
        colour *= 0.86 + 0.28 * fbm(u * 22.0, v * 22.0, 3, s ^ 0x12);
        // Radial fibres at three scales, wandering as they run out and breaking every short run: the stroma is a mesh
        // of short, wavy strands, never spokes running the whole way from the pupil to the rim.
        let warp = 0.45 * (fbm(a * 18.0, t * 5.0, 2, s ^ 0x22) - 0.5);
        let f1 = value_wrapped(a * 240.0 + warp * 4.0, t * 4.0, 240, s ^ 0x33);
        let f2 = value_wrapped(a * 620.0 + warp * 9.0, t * 8.0, 620, s ^ 0x44);
        let f3 = value_wrapped(a * 1400.0 + warp * 18.0, t * 16.0, 1400, s ^ 0x55);
        let f = 0.3 * f1 + 0.35 * f2 + 0.35 * f3;
        let light = smoothstep(0.52, 0.8, f);
        let dark = smoothstep(0.48, 0.18, f);
        // The pupillary zone's fibres are coarser and darker (the ruff's spokes).
        let inside = 1.0 - smoothstep(c - 0.02, c + 0.02, t);
        colour *= 1.0 - fibres * dark * (0.4 + 0.2 * inside);
        colour = colour.lerp(fibre, fibres * light * (0.42 - 0.14 * inside));
        // The collarette: a jagged ring where the two zones meet, darker just inside it (the crypts' shadow) and
        // lighter on its ridge.
        let jag = c + 0.04 * (value_wrapped(a * 90.0, t * 4.0, 90, s ^ 0x66) - 0.5);
        let inside_band = smoothstep(jag - 0.14, jag - 0.03, t) * (1.0 - smoothstep(jag - 0.01, jag + 0.01, t));
        colour *= 1.0 - 0.5 * inside_band;
        let ridge = (-((t - jag - 0.012) / 0.012).powi(2)).exp();
        colour = colour.lerp(fibre, 0.18 * ridge);
        // The pupillary ruff: a thin dark rim round the pupil.
        colour = colour.lerp(deep * 0.5, smoothstep(0.045, 0.0, t));
        // The limbal ring, and the rim beyond the iris.
        colour = colour.lerp(limbal, ring * smoothstep(0.7, 1.0, t).powf(1.4));
        if r > 1.0 {
            return limbal.lerp(Vec3::splat(0.5), smoothstep(1.0, 1.04, r));
        }
        colour
    });
    crypts(&mut canvas, iris, seed, deep, fibre);
    furrows(&mut canvas, iris, seed, deep);
    specks(&mut canvas, iris, seed, deep);
    canvas
}

/// Crypts: radially long openings round the collarette, dark with the deep layer and edged lighter.
fn crypts(canvas: &mut Canvas, iris: &Iris, seed: u32, deep: Vec3, fibre: Vec3) {
    let mut rng = Pcg::new(seed, CRYPT_STREAM);
    #[allow(clippy::cast_precision_loss)]
    let px_per_r = canvas.width as f32 * 0.5 * DISC;
    for _ in 0..iris.crypts {
        let a = rng.unit();
        let angle = a * TAU - PI;
        let c = collarette_at(iris, a, seed);
        // Most open just outside the collarette, wide at its edge and narrowing outward; a few sit inside it.
        let t = if rng.unit() < 0.75 { c + rng.range(0.0, 0.08) } else { c + rng.range(0.1, 0.3) };
        let half = rng.range(0.02, 0.06);
        let width = rng.range(0.01, 0.03) * px_per_r;
        let bend = rng.range(-0.008, 0.008);
        let points: Vec<Vec2> = (0..=6)
            .map(|k| {
                #[allow(clippy::cast_precision_loss)]
                let s = k as f32 / 6.0 - 0.5;
                let tt = (t + 2.0 * half * s).clamp(0.02, 0.97);
                to_px(canvas.width, iris_r(tt), angle + bend * (1.0 - 4.0 * s * s))
            })
            .collect();
        canvas.stroke(&points, (width * 1.6, width * 0.6), fibre * 0.9, (0.1, 0.04));
        canvas.stroke(&points, (width, width * 0.25), deep, (0.45, 0.2));
    }
}

/// Contraction furrows: broken dark arcs ringing the outer zone.
fn furrows(canvas: &mut Canvas, iris: &Iris, seed: u32, deep: Vec3) {
    let mut rng = Pcg::new(seed, CRYPT_STREAM ^ 0xf0);
    for k in 0..iris.furrows {
        #[allow(clippy::cast_precision_loss)]
        let t = 0.62 + 0.3 * (k as f32 + rng.unit()) / iris.furrows.max(1) as f32;
        let mut a = rng.unit() * TAU;
        while a < TAU * 1.9 {
            let span = rng.range(0.4, 1.4);
            let points: Vec<Vec2> = (0..=24)
                .map(|i| {
                    #[allow(clippy::cast_precision_loss)]
                    let ang = a + span * i as f32 / 24.0;
                    to_px(canvas.width, iris_r(t + 0.01 * (ang * 7.0).sin()), ang)
                })
                .collect();
            canvas.stroke(&points[2..points.len() - 2], (1.6, 1.6), deep, (0.18, 0.18));
            a += span + rng.range(0.3, 1.2);
        }
    }
}

/// Pigment specks: small dark freckles of the iris, most in the outer zone.
fn specks(canvas: &mut Canvas, iris: &Iris, seed: u32, deep: Vec3) {
    let mut rng = Pcg::new(seed, SPECK_STREAM);
    #[allow(clippy::cast_precision_loss)]
    let px_per_r = canvas.width as f32 * 0.5 * DISC;
    for _ in 0..iris.specks {
        let angle = rng.unit() * TAU;
        let t = rng.range(0.2, 0.92);
        let r = rng.range(0.004, 0.014) * px_per_r;
        let squash = rng.range(0.5, 1.0);
        canvas.disc(
            to_px(canvas.width, iris_r(t), angle),
            Vec2::new(r, r * squash),
            deep * 0.35,
            rng.range(0.5, 0.9),
            0.4,
        );
    }
}

/// The white half of the atlas.
pub fn sclera(sclera: &Sclera, seed: u32, size: u32) -> Canvas {
    let (white, edge) = (lin(sclera.white), lin(sclera.edge));
    let pink = Vec3::new(0.82, 0.42, 0.38);
    let net = sclera.network.clamp(0.0, 1.0);
    let s = seed ^ VESSEL_STREAM;
    let mut canvas = Canvas::new(size, size, edge);
    canvas.fill(|u, v| {
        let (rho, phi) = polar(u, v);
        let theta = rho.min(1.0) * SCLERA_REACH;
        // Whitest round the iris, warming toward the edges seen in the opening (about half the map's radius).
        let mut colour = white.lerp(edge, smoothstep(0.22, 0.65, rho));
        // The corners of the eye are pinker: the conjunctiva is thicker and the vessels denser there.
        let corner = smoothstep(0.3, 0.62, rho) * phi.cos().abs().powi(3);
        colour = colour.lerp(pink, 0.28 * corner);
        colour *= 0.95 + 0.08 * fbm(u * 9.0, v * 9.0, 3, s ^ 0x91);
        // The fine vessel network, on the sphere's surface so it doesn't crowd at the pole of the map.
        let (x, y) = (theta.sin() * phi.cos(), theta.sin() * phi.sin());
        let (f1, f2) = worley(x * 26.0, y * 26.0, s ^ 0x92);
        let line = 1.0 - smoothstep(0.0, 0.14, f2 - f1);
        let faint = fbm(x * 40.0, y * 40.0, 2, s ^ 0x93);
        let reach = smoothstep(0.24, 0.48, rho);
        colour = colour.lerp(Vec3::new(0.66, 0.46, 0.48), net * 0.3 * line * faint * reach);
        colour
    });
    vessels(&mut canvas, sclera, seed);
    canvas
}

/// Vessels: branching red lines from the corners toward the iris, thinning and fading as they go.
fn vessels(canvas: &mut Canvas, sclera: &Sclera, seed: u32) {
    let mut rng = Pcg::new(seed, VESSEL_STREAM);
    let red = Vec3::new(0.45, 0.16, 0.2);
    let mut todo: Vec<(f32, f32, f32, f32, u32)> = Vec::new();
    for _ in 0..sclera.vessels {
        // From the inner or the outer corner mostly, a few from above and below.
        let side = if rng.unit() < 0.5 { 0.0 } else { PI };
        let spread = if rng.unit() < 0.8 { 0.55 } else { 1.4 };
        // Rooted where the opening shows the white, so most of each vessel's length is seen.
        todo.push((
            rng.range(0.52, 0.8),
            side + spread * rng.normal() * 0.6,
            rng.range(3.0, 5.0),
            rng.range(0.5, 0.75),
            0,
        ));
    }
    while let Some((r0, a0, width, alpha, depth)) = todo.pop() {
        let mut points = Vec::new();
        let (mut r, mut a) = (r0, a0);
        // `heading`: the vessel's sideways drift per step inward, as a share of the step.
        let mut heading = rng.normal() * 0.3;
        let end = rng.range(0.27, 0.38);
        let step = 0.008;
        while r > end {
            points.push(to_px(canvas.width, r, a));
            heading = (heading + rng.normal() * 0.25).clamp(-0.9, 0.9);
            a += heading * step / r.max(0.3);
            r -= step;
            if depth < 3 && rng.unit() < 0.07 {
                todo.push((r, a, width * 0.6, alpha * 0.8, depth + 1));
            }
        }
        // A soft halo (the vessel seen through the conjunctiva), then its core.
        canvas.stroke(&points, (width * 3.5, width), red, (alpha * 0.22, 0.0));
        canvas.stroke(&points, (width * 0.8, 0.35), red, (alpha * 0.8, 0.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(canvas: &Canvas, r: f32, angle: f32) -> Vec3 {
        let p = to_px(canvas.width, r, angle);
        // In range: r < 1.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (x, y) = (p.x as usize, p.y as usize);
        canvas.pixels[y * canvas.width as usize + x]
    }

    #[test]
    fn the_iris_has_a_dark_pupil_a_coloured_body_and_a_darker_rim() {
        let design = EyeDesign::default();
        let c = iris(&design.iris, 1, 256);
        let pupil = at(&c, 0.1, 0.3);
        let body: Vec3 = (0..32u8).map(|i| at(&c, 0.62, f32::from(i) * 0.2)).sum::<Vec3>() / 32.0;
        let rim: Vec3 = (0..32u8).map(|i| at(&c, 0.985, f32::from(i) * 0.2)).sum::<Vec3>() / 32.0;
        assert!(pupil.max_element() < 0.02, "{pupil}");
        assert!(body.x > body.z * 3.0, "amber reads warm: {body}");
        assert!(rim.length() < body.length() * 0.8, "the limbal ring darkens the rim: {rim} vs {body}");
    }

    #[test]
    fn the_seed_moves_the_detail_and_repeats_it() {
        let d = EyeDesign::default();
        let (a, b, c) = (iris(&d.iris, 1, 128), iris(&d.iris, 1, 128), iris(&d.iris, 2, 128));
        assert_eq!(a.pixels, b.pixels);
        assert_ne!(a.pixels, c.pixels);
    }

    #[test]
    fn the_atlas_is_two_squares_side_by_side() {
        let a = atlas(&EyeDesign::default(), 64);
        assert_eq!((a.width, a.height, a.rgba.len()), (128, 64, 128 * 64 * 4));
    }

    #[test]
    fn the_white_is_light_with_vessels_toward_the_corners() {
        let c = sclera(&Sclera::default(), 1, 256);
        let centre = at(&c, 0.3, 1.57);
        assert!(centre.min_element() > 0.4, "{centre}");
        let corners: f32 = (0..200u8)
            .map(|i| at(&c, 0.6 + f32::from(i) * 0.0015, 0.0).x - at(&c, 0.6 + f32::from(i) * 0.0015, 0.0).y)
            .sum();
        assert!(corners > 0.0, "red vessels and pink corners: {corners}");
    }
}
