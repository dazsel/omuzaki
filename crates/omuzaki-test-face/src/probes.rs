//! The probe drawings of the face area's rasteriser tests, shared by the CPU probes against Chrome's canvas
//! (`tests/raster_probes.rs`) and the GPU probes against the CPU raster (`tests/face_gpu_fills.rs`).
//!
//! Each probe is a 256 x 160 px image of a 0.256 x 0.160 m face-plane rect at 1000 px/m. The drawings are the same as in
//! the Chromium probe script that saves `fixtures/raster/*.png`, command for command; change one and change the other,
//! then regenerate the PNGs. [`stroke_widths`] is the GPU stroke tests' own and has no Chromium twin.

use image::RgbaImage;
use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_face_api::{Cap, Cmd, Colour, DrawList, FaceLayerId, FaceLayerSpec, FaceRect, Join, Paint, PathRef, Stops};

/// The probe width in pixels.
pub const WIDTH: u32 = 256;
/// The probe height in pixels.
pub const HEIGHT: u32 = 160;
/// The density the probes are drawn at.
pub const PX_PER_M: f32 = 1000.0;

/// A probe drawing.
pub type Probe = fn(&mut DrawList);

/// The layer spec every probe is rasterised under.
pub fn spec() -> FaceLayerSpec {
    FaceLayerSpec::for_rect(FaceRect { x0: -0.128, x1: 0.128, y0: -0.08, y1: 0.08 }, FaceLayerId::Base, PX_PER_M)
}

/// A premultiplied RGBA8 raster as the straight-alpha image `omuzaki_test::compare` expects (rounded to nearest, so a
/// pixel survives `compare`'s own premultiply to within the rounding of 8-bit colour). `None` if the length is not
/// `width * height * 4`.
pub fn straight_image(width: u32, height: u32, premultiplied: &[u8]) -> Option<RgbaImage> {
    let straight = premultiplied
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[r, g, b, a]| {
            let un = |c: u8| match u32::from(a) {
                0 => 0,
                a => u8::try_from((u32::from(c) * 255 + a / 2) / a).unwrap_or(u8::MAX),
            };
            [un(r), un(g), un(b), a]
        })
        .collect();
    RgbaImage::from_raw(width, height, straight)
}

// ---------------------------------------------------------------------------------------------- drawing helpers

/// A face-plane point.
pub fn v(x: f32, y: f32) -> Vec2 {
    Vec2::new(x, y)
}

/// A closed ellipse centred on (`cx`, `cy`).
pub fn ellipse(d: &mut DrawList, cx: f32, cy: f32, rx: f32, ry: f32) -> PathRef {
    d.path().ellipse(v(cx, cy), rx, ry).finish()
}

/// A closed polygon.
pub fn polygon(d: &mut DrawList, points: &[[f32; 2]]) -> PathRef {
    let points: Vec<Vec2> = points.iter().map(|p| v(p[0], p[1])).collect();
    d.path().polyline(&points).close().finish()
}

/// An open polyline.
pub fn polyline(d: &mut DrawList, points: &[[f32; 2]]) -> PathRef {
    let points: Vec<Vec2> = points.iter().map(|p| v(p[0], p[1])).collect();
    d.path().polyline(&points).finish()
}

/// A solid paint from straight-alpha components.
pub fn rgba(r: u8, g: u8, b: u8, a: u8) -> Paint {
    Paint::Solid(Colour::rgba(r, g, b, a))
}

/// Pushes a non-zero fill.
pub fn fill(d: &mut DrawList, path: PathRef, paint: Paint, opacity: f32) {
    d.push(Cmd::Fill { path, paint, opacity });
}

/// Pushes a stroke.
pub fn stroke(d: &mut DrawList, path: PathRef, paint: Paint, width: f32, cap: Cap, join: Join, opacity: f32) {
    d.push(Cmd::Stroke { path, paint, width, cap, join, opacity });
}

/// Gradient stops from `(offset, [r, g, b, a])`.
pub fn stops(list: &[(f32, [u8; 4])]) -> Stops {
    let list: Vec<(f32, Colour)> = list.iter().map(|&(at, [r, g, b, a])| (at, Colour::rgba(r, g, b, a))).collect();
    Stops::new(&list)
}

// ---------------------------------------------------------------------------------------------- the fill probes

/// Three opaque overlapping shapes, a pentagram (non-zero fills its middle) and a square ring wound the opposite way inside.
pub fn overlapping_fills(d: &mut DrawList) {
    let p = ellipse(d, -0.075, 0.015, 0.045, 0.035);
    fill(d, p, rgba(217, 68, 82, 255), 1.0);
    let p = ellipse(d, -0.035, -0.01, 0.04, 0.04);
    fill(d, p, rgba(61, 143, 217, 255), 1.0);
    let points: Vec<Vec2> = [0_u8, 2, 4, 1, 3]
        .iter()
        .map(|&k| {
            let angle = std::f64::consts::FRAC_PI_2 + f64::from(k) * 2.0 * std::f64::consts::PI / 5.0;
            #[allow(clippy::cast_possible_truncation)] // metres, far inside f32 range
            v((0.04 + 0.05 * angle.cos()) as f32, (0.005 + 0.05 * angle.sin()) as f32)
        })
        .collect();
    let star = d.path().polyline(&points).close().finish();
    fill(d, star, rgba(242, 177, 52, 255), 1.0);
    let ring = d
        .path()
        .polyline(&[v(0.0803, -0.0702), v(0.1197, -0.0702), v(0.1197, -0.0248), v(0.0803, -0.0248)])
        .close()
        .polyline(&[v(0.0903, -0.0597), v(0.0903, -0.0351), v(0.1097, -0.0351), v(0.1097, -0.0597)])
        .close()
        .finish();
    fill(d, ring, rgba(76, 175, 122, 255), 1.0);
}

/// A four-stop radial gradient (the last stop transparent) clipped by an ellipse and a diagonal half-plane, and a linear bar.
pub fn clipped_radial_gradient(d: &mut DrawList) {
    let clip = ellipse(d, 0.0, 0.0, 0.1, 0.06);
    d.push(Cmd::PushClip(clip));
    let half_plane = polygon(d, &[[-0.13, 0.045], [0.13, -0.02], [0.13, -0.09], [-0.13, -0.09]]);
    d.push(Cmd::PushClip(half_plane));
    let sheet = polygon(d, &[[-0.13, -0.09], [0.13, -0.09], [0.13, 0.09], [-0.13, 0.09]]);
    let radial = Paint::Radial {
        centre: v(-0.02, 0.01),
        radius: 0.085,
        stops: stops(&[
            (0.0, [255, 247, 224, 255]),
            (0.3, [255, 194, 60, 255]),
            (0.65, [224, 64, 46, 176]),
            (1.0, [42, 16, 112, 0]),
        ]),
    };
    fill(d, sheet, radial, 1.0);
    d.push(Cmd::PopClip);
    d.push(Cmd::PopClip);
    let bar = polygon(d, &[[-0.1, 0.065], [0.1, 0.065], [0.1, 0.075], [-0.1, 0.075]]);
    let linear = Paint::Linear {
        from: v(-0.1, 0.0),
        to: v(0.1, 0.0),
        stops: stops(&[(0.0, [32, 96, 224, 255]), (0.5, [224, 224, 224, 128]), (1.0, [224, 32, 96, 255])]),
    };
    fill(d, bar, linear, 1.0);
}

/// Five overlapping translucent ellipses (paint alpha and command opacity together) and a translucent ring and band.
pub fn translucent_stack(d: &mut DrawList) {
    let p = ellipse(d, -0.05, 0.02, 0.05, 0.04);
    fill(d, p, rgba(220, 40, 60, 200), 1.0);
    let p = ellipse(d, 0.0, 0.0, 0.05, 0.05);
    fill(d, p, rgba(40, 120, 230, 160), 0.8);
    let p = ellipse(d, 0.05, 0.02, 0.045, 0.045);
    fill(d, p, rgba(250, 200, 20, 120), 0.7);
    let p = ellipse(d, 0.0, -0.03, 0.06, 0.03);
    fill(d, p, rgba(30, 200, 120, 90), 1.0);
    let ring = ellipse(d, 0.0, 0.0, 0.07, 0.06);
    stroke(d, ring, rgba(20, 20, 20, 255), 0.006, Cap::Round, Join::Round, 0.35);
    let band = polygon(d, &[[-0.12, -0.07], [0.12, -0.07], [0.12, -0.05], [-0.12, -0.05]]);
    fill(d, band, rgba(0, 0, 0, 64), 1.0);
}

/// Ellipses a few pixels across and one under a pixel, at fractional positions.
pub fn tiny_ellipse(d: &mut DrawList) {
    for (cx, cy, rx, ry, colour) in [
        (0.0123, 0.0071, 0.0012, 0.0009, [32, 32, 32]),
        (-0.05, 0.02, 0.0025, 0.0025, [200, 40, 40]),
        (0.05, -0.02, 0.0006, 0.0006, [40, 40, 200]),
        (-0.0307, -0.0413, 0.0018, 0.0031, [20, 140, 60]),
        (0.0911, 0.0507, 0.0035, 0.0011, [140, 60, 160]),
    ] {
        let p = ellipse(d, cx, cy, rx, ry);
        fill(d, p, rgba(colour[0], colour[1], colour[2], 255), 1.0);
    }
}

/// A rotated fill and stroke, and a radial gradient and a stroke under a further non-uniform scale (paint coordinates are
/// local), then a fill back at identity.
pub fn nested_transforms(d: &mut DrawList) {
    let turn = Affine2::from_angle(25.0_f32.to_radians()) * Affine2::IDENTITY;
    d.push(Cmd::PushTransform(Affine2::from_translation(v(-0.05, 0.0)) * turn));
    let slab = polygon(d, &[[-0.04, -0.02], [0.04, -0.02], [0.04, 0.02], [-0.04, 0.02]]);
    fill(d, slab, rgba(60, 140, 200, 255), 1.0);
    let ring = ellipse(d, 0.0, 0.0, 0.05, 0.03);
    stroke(d, ring, rgba(20, 60, 120, 255), 0.004, Cap::Round, Join::Round, 1.0);
    d.push(Cmd::PushTransform(Affine2::from_translation(v(0.05, 0.0)) * Affine2::from_scale(v(0.6, 1.4))));
    let disc = ellipse(d, 0.0, 0.0, 0.02, 0.02);
    let radial = Paint::Radial {
        centre: v(0.0, 0.0),
        radius: 0.02,
        stops: stops(&[(0.0, [255, 255, 255, 255]), (0.6, [250, 170, 40, 255]), (1.0, [200, 40, 40, 255])]),
    };
    fill(d, disc, radial, 1.0);
    let corner = polyline(d, &[[-0.03, -0.03], [0.03, -0.03], [0.03, 0.03]]);
    stroke(d, corner, rgba(30, 30, 30, 255), 0.008, Cap::Butt, Join::Miter, 1.0);
    d.push(Cmd::PopTransform);
    d.push(Cmd::PopTransform);
    let after = ellipse(d, 0.08, -0.05, 0.02, 0.012);
    fill(d, after, rgba(120, 200, 80, 255), 1.0);
}

// ---------------------------------------------------------------------------------------------- the stroke probes

/// A wide round-capped stroke along a curve and a corner, a translucent second stroke over it, and a butt-capped bar.
pub fn round_capped_stroke(d: &mut DrawList) {
    let curve = d
        .path()
        .move_to(v(-0.1, -0.04))
        .cubic_to(v(-0.06, 0.08), v(0.0, -0.08), v(0.04, 0.03))
        .line_to(v(0.1, 0.05))
        .finish();
    stroke(d, curve, rgba(90, 63, 192, 255), 0.022, Cap::Round, Join::Round, 1.0);
    let zig = polyline(d, &[[-0.09, 0.02], [0.0, -0.03], [0.09, -0.005]]);
    stroke(d, zig, rgba(255, 255, 255, 255), 0.008, Cap::Round, Join::Round, 0.6);
    let bar = polyline(d, &[[-0.1, 0.065], [0.1, 0.065]]);
    stroke(d, bar, rgba(224, 122, 31, 255), 0.012, Cap::Butt, Join::Miter, 1.0);
}

/// A 25 degree corner (miter ratio 4.6: mitred under Canvas's limit of 10, bevelled under Skia's default of 4), a closed
/// diamond and a round-joined V.
pub fn miter_join(d: &mut DrawList) {
    let sharp = polyline(d, &[[-0.1, -0.04], [0.09, 0.02], [-0.07, 0.0411]]);
    stroke(d, sharp, rgba(200, 60, 40, 255), 0.01, Cap::Butt, Join::Miter, 1.0);
    let diamond = polygon(d, &[[0.025, -0.05], [0.0, -0.025], [-0.025, -0.05], [0.0, -0.075]]);
    stroke(d, diamond, rgba(43, 122, 93, 255), 0.008, Cap::Butt, Join::Miter, 1.0);
    let vee = polyline(d, &[[0.06, -0.03], [0.09, -0.07], [0.12, -0.03]]);
    stroke(d, vee, rgba(50, 70, 160, 255), 0.012, Cap::Butt, Join::Round, 1.0);
}

/// Strokes from 0.05 to 1.2 px across, diagonal, axis-aligned and curved: Skia draws them as modulated hairlines.
pub fn hairline(d: &mut DrawList) {
    let ink = rgba(20, 20, 20, 255);
    for (points, width, cap, join) in [
        ([[-0.11, 0.06], [0.11, 0.045]], 0.0003, Cap::Round, Join::Round),
        ([[-0.11, 0.035], [0.11, 0.0]], 0.0008, Cap::Round, Join::Round),
        ([[-0.11, -0.005], [0.11, -0.04]], 0.0012, Cap::Round, Join::Round),
        ([[-0.11, -0.0625], [0.11, -0.0625]], 0.0005, Cap::Butt, Join::Miter),
        ([[-0.0951, -0.07], [-0.0951, 0.07]], 0.0005, Cap::Butt, Join::Miter),
    ] {
        let p = polyline(d, &points);
        stroke(d, p, ink, width, cap, join, 1.0);
    }
    let curve = d.path().move_to(v(-0.08, -0.06)).cubic_to(v(-0.04, 0.05), v(0.04, -0.05), v(0.08, 0.06)).finish();
    stroke(d, curve, rgba(200, 30, 30, 255), 0.0004, Cap::Round, Join::Round, 1.0);
    let faint = polyline(d, &[[-0.11, -0.03], [0.11, -0.05]]);
    stroke(d, faint, ink, 0.00005, Cap::Round, Join::Round, 1.0);
}

/// Every cap and join at widths from 1 to 40 px: a zigzag per width with a sharp and a shallow corner, butt-capped and
/// mitred on the left, round on the right, and a curve. One plane millimetre is a pixel here.
pub fn stroke_widths(d: &mut DrawList) {
    let ink = rgba(30, 40, 90, 255);
    for (i, width_px) in [1.0_f32, 2.5, 6.0, 14.0, 24.0, 40.0].into_iter().enumerate() {
        #[allow(clippy::cast_precision_loss)] // six probes
        let y = 0.06 - 0.024 * i as f32;
        let width = width_px / PX_PER_M;
        let left = polyline(d, &[[-0.115, y], [-0.085, y - 0.008], [-0.06, y + 0.004], [-0.035, y]]);
        stroke(d, left, ink, width, Cap::Butt, Join::Miter, 1.0);
        let right = polyline(d, &[[0.0, y], [0.025, y - 0.008], [0.05, y + 0.004], [0.075, y]]);
        stroke(d, right, ink, width, Cap::Round, Join::Round, 0.8);
        let curve = d
            .path()
            .move_to(v(0.09, y - 0.006))
            .cubic_to(v(0.1, y + 0.01), v(0.11, y - 0.01), v(0.12, y + 0.004))
            .finish();
        stroke(d, curve, rgba(150, 30, 60, 255), width.min(0.006), Cap::Round, Join::Round, 1.0);
    }
}
