//! The skin texture round the eye: the tone, its mottling, a darker band under the eye and freckles of many sizes,
//! denser on the cheek than on the lids. Pores and fine lines are the `skin` shader's, not the texture's.

use omuzaki_core::glam::{Vec2, Vec3};

use super::canvas::Canvas;
use super::design::Skin;
use super::geometry::{SKIN_HALF, SKIN_TOP};
use super::noise::{Pcg, fbm, smoothstep};

/// The skin texture: `width` pixels across the patch (its height keeps the patch's proportions).
pub fn texture(skin: &Skin, seed: u32, width: u32) -> Canvas {
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
        let mut c = tone * (1.0 + 0.16 * m1 + 0.06 * m2);
        c = c.lerp(c * Vec3::new(1.04, 0.88, 0.86), (0.5 + m1).clamp(0.0, 1.0) * 0.35);
        // A faint darker band under the eye.
        let under = smoothstep(-0.004, -0.009, y)
            * (1.0 - smoothstep(-0.012, -0.02, y))
            * (1.0 - smoothstep(0.012, 0.02, x.abs()));
        c *= 1.0 - 0.1 * under;
        c
    });
    freckles(&mut canvas, skin, seed);
    canvas
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
        canvas.disc(p, Vec2::new(r, r * rng.range(0.7, 1.0)), shade, rng.range(0.25, 0.6), 0.2);
    }
}
