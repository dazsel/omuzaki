//! The skin texture round the eye: the tone, its mottling, a darker band under the eye, fine lines (the upper lid's
//! creases, arcs under the eye and a fan at the outer corner) and freckles of many sizes, denser on the cheek than on
//! the lids. Pores are the `skin` shader's, not the texture's.

use omuzaki_core::glam::{Vec2, Vec3};

use super::canvas::Canvas;
use super::design::{EyeDesign, Skin};
use super::geometry::{SKIN_HALF, SKIN_TOP, lid_outline};
use super::noise::{Pcg, fbm, smoothstep};

/// The skin texture of `design`: `width` pixels across the patch (its height keeps the patch's proportions).
pub fn texture(design: &EyeDesign, width: u32) -> Canvas {
    let (skin, seed) = (&design.skin, design.seed);
    let tone = skin.tone.to_linear();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
    let height = (width as f32 * SKIN_HALF.y / SKIN_HALF.x) as u32;
    let mut canvas = Canvas::new(width, height, tone);
    let s = seed ^ 0x5c1d;
    canvas.fill(|u, v| {
        let (x, y) = (u * 2.0 * SKIN_HALF.x - SKIN_HALF.x, SKIN_TOP - v * 2.0 * SKIN_HALF.y);
        // Mottling: warm and cool patches a few millimetres across.
        let m1 = fbm(x * 260.0, y * 260.0, 4, s ^ 1) - 0.5;
        let m2 = fbm(x * 900.0, y * 900.0, 2, s ^ 2) - 0.5;
        // Grain: the speckle of pores and fine texture, a few tenths of a millimetre across.
        let grain = fbm(x * 3600.0, y * 3600.0, 2, s ^ 3) - 0.5;
        let mut c = tone * (1.0 + 0.16 * m1 + 0.06 * m2 + 0.12 * grain);
        c = c.lerp(c * Vec3::new(1.04, 0.88, 0.86), (0.5 + m1).clamp(0.0, 1.0) * 0.35);
        // A faint darker band under the eye.
        let under = smoothstep(-0.004, -0.009, y)
            * (1.0 - smoothstep(-0.012, -0.02, y))
            * (1.0 - smoothstep(0.012, 0.02, x.abs()));
        c *= 1.0 - 0.1 * under;
        c
    });
    lines(&mut canvas, design);
    freckles(&mut canvas, skin, seed);
    canvas
}

/// Fine lines: the upper lid's creases following its edge, shallow arcs under the eye following the lower lid, and a
/// fan at the outer corner. Each is a run of short, broken, wavering strokes, darker in its fold and lighter on the
/// skin just above it.
fn lines(canvas: &mut Canvas, design: &EyeDesign) {
    let mut rng = Pcg::new(design.seed, 0x11e5);
    #[allow(clippy::cast_precision_loss)]
    let px_per_m = canvas.width as f32 / (2.0 * SKIN_HALF.x);
    let to_px = |x: f32, y: f32| Vec2::new((x + SKIN_HALF.x) * px_per_m, (SKIN_TOP - y) * px_per_m);
    let fold = Vec3::new(0.22, 0.1, 0.07);
    let lift = Vec3::new(0.95, 0.75, 0.65);
    let draw = |canvas: &mut Canvas, points: &[Vec2], rng: &mut Pcg, strength: f32| {
        // Broken into short runs, each fading in and out (a run that stops at full strength reads as a cut), and the
        // whole line fading toward its ends.
        let mut i = 0;
        while i + 3 < points.len() {
            // In range: 0..8.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let run = (3 + (rng.unit() * 8.0) as usize).min(points.len() - i);
            if rng.unit() < 0.8 {
                #[allow(clippy::cast_precision_loss)]
                let along = (i as f32 + run as f32 * 0.5) / points.len() as f32;
                let strength = strength * (std::f32::consts::PI * along).sin().sqrt();
                let seg = &points[i..i + run];
                let middle = run / 2;
                for (half, rising) in [(&seg[..=middle], true), (&seg[middle..], false)] {
                    let ramp = |a: f32| if rising { (0.0, a) } else { (a, 0.0) };
                    let up: Vec<Vec2> = half.iter().map(|p| *p - Vec2::new(0.0, 1.6)).collect();
                    canvas.stroke(&up, ramp(2.4), lift, ramp(0.02 * strength));
                    canvas.stroke(half, ramp(1.6), fold, ramp(0.2 * strength));
                }
            }
            i += run;
        }
    };
    // The upper lid: crinkled creases between the lash line and the crease, then two on the fold above it, each
    // following the lid's edge and fading toward the corners.
    let crease = design.skin.crease;
    let offsets = [0.0016, 0.0024, 0.0032, 0.0041, 0.005, crease + 0.0009, crease + 0.002];
    for (k, offset) in offsets.iter().enumerate() {
        let points: Vec<Vec2> = (0..=60)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let u = 0.06 + 0.86 * i as f32 / 60.0;
                let edge = lid_outline(&design.aperture, u, true);
                let rise = offset * (0.55 + 0.45 * (std::f32::consts::PI * u).sin());
                let wave = 0.00008 * (u * 90.0 + rng.range(0.0, 0.5)).sin();
                to_px(edge.x, edge.y + rise + wave)
            })
            .collect();
        draw(canvas, &points, &mut rng, if k < 5 { 0.45 } else { 0.6 });
    }
    // Under the eye: arcs a few millimetres below the lid, the lowest the faintest.
    for (k, depth) in [5.8f32, 7.2, 8.9, 10.8].iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let strength = 1.0 - 0.18 * k as f32;
        let points: Vec<Vec2> = (0..=60)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let x = -0.016 + 0.027 * i as f32 / 60.0;
                let wave = 0.00012 * (x * 3100.0 + rng.range(0.0, 0.4)).sin();
                let y = -depth * 0.001 + 0.0042 * ((x + 0.002) / 0.015).powi(2) + wave;
                to_px(x, y)
            })
            .collect();
        draw(canvas, &points, &mut rng, strength);
    }
    // The outer corner: a fan of lines running out toward the temple.
    for k in 0..6u8 {
        let side = f32::from(k) - 2.5;
        let angle = std::f32::consts::PI + side * 0.2 + rng.range(-0.06, 0.06);
        let start = Vec2::new(-0.0165, -0.0005) + Vec2::new(angle.cos(), angle.sin()) * rng.range(0.0005, 0.0015);
        let length = rng.range(0.005, 0.009);
        let points: Vec<Vec2> = (0..=30)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let t = i as f32 / 30.0;
                let bend = angle + 0.25 * t * side.signum();
                let p = start + Vec2::new(bend.cos(), bend.sin()) * length * t;
                to_px(p.x, p.y)
            })
            .collect();
        draw(canvas, &points, &mut rng, 0.8);
    }
}

fn freckles(canvas: &mut Canvas, skin: &Skin, seed: u32) {
    let mut rng = Pcg::new(seed, 0xf1ec);
    let colour = skin.freckle.to_linear();
    // Per square centimetre over the patch.
    let area_cm2 = (2.0 * SKIN_HALF.x * 100.0) * (2.0 * SKIN_HALF.y * 100.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let count = (skin.freckles.max(0.0) * area_cm2 * 1.8) as u32;
    #[allow(clippy::cast_precision_loss)]
    let px_per_m = canvas.width as f32 / (2.0 * SKIN_HALF.x);
    for _ in 0..count {
        let (x, y) = (rng.range(-SKIN_HALF.x, SKIN_HALF.x), rng.range(SKIN_TOP - 2.0 * SKIN_HALF.y, SKIN_TOP));
        // Denser below the eye and on the nose's side; sparse on the lids.
        let density = 0.25 + 0.75 * smoothstep(-0.004, -0.014, y) + 0.4 * smoothstep(0.012, 0.02, x);
        if rng.unit() > density.min(1.0) * 0.55 {
            continue;
        }
        let size = rng.range(0.12, 0.45) * 0.001 * if rng.unit() < 0.12 { 1.8 } else { 1.0 };
        let p = Vec2::new((x + SKIN_HALF.x) * px_per_m, (SKIN_TOP - y) * px_per_m);
        let r = size * px_per_m;
        let shade = colour * rng.range(0.8, 1.2);
        let alpha = rng.range(0.3, 0.7);
        // Irregular and soft-edged: a few overlapping blots.
        for _ in 0..3 {
            let at = p + Vec2::new(rng.normal(), rng.normal()) * 0.35 * r;
            let size = Vec2::new(r, r * rng.range(0.6, 1.0)) * rng.range(0.6, 1.0);
            canvas.disc(at, size, shade, alpha * 0.6, 0.0);
        }
    }
}
