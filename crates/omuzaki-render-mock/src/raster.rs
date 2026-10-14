//! A small exact triangle rasteriser: the only place pixels are decided.
//!
//! Exactness is the point. Vertices snap to a 1/256-pixel grid and the inside test uses 64-bit integers, so two
//! triangles sharing an edge cover every pixel on it exactly once (no gaps, no double hits) and the same input gives
//! the same pixels on every machine. Only the per-pixel interpolation uses floats, and it uses nothing but
//! `+ - * /`, which IEEE 754 makes identical everywhere.

use omuzaki_core::glam::{UVec2, Vec2, Vec3, Vec4};

/// Sub-pixel precision: vertices snap to 1/256 pixel.
const SUBPIXEL_BITS: u32 = 8;
const SUBPIXEL: i64 = 1 << SUBPIXEL_BITS;
/// `SUBPIXEL` as a float (a test pins the two together).
const SUBPIXEL_F32: f32 = 256.0;
/// Screen coordinates beyond this are clamped, which keeps the 64-bit edge products from overflowing. Triangles that
/// reach it are far off screen anyway.
const COORD_LIMIT: f32 = 1.0e6;

/// The colour and depth buffers one frame draws into.
pub(crate) struct Surface {
    pub(crate) size: UVec2,
    /// Premultiplied linear RGBA, row-major, top row first.
    pub(crate) colour: Vec<Vec4>,
    /// NDC depth `0..=1`; 1 is the far plane.
    pub(crate) depth: Vec<f32>,
}

impl Surface {
    pub(crate) fn new(size: UVec2, clear: Vec4) -> Self {
        let n = size.x as usize * size.y as usize;
        Self { size, colour: vec![clear; n], depth: vec![1.0; n] }
    }
}

/// A vertex after projection, with the attributes the pixel shader needs.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Vertex {
    pub(crate) clip: Vec4,
    pub(crate) uv: Vec2,
    /// The vertex's position in the mesh's own space (bind pose), for planar overlays.
    pub(crate) local: Vec3,
    pub(crate) colour: Vec4,
    /// Distance along the camera's view direction in metres, for fog.
    pub(crate) depth: f32,
}

impl Vertex {
    fn lerp(self, other: Self, t: f32) -> Self {
        Self {
            clip: self.clip.lerp(other.clip, t),
            uv: self.uv.lerp(other.uv, t),
            local: self.local.lerp(other.local, t),
            colour: self.colour.lerp(other.colour, t),
            depth: self.depth + (other.depth - self.depth) * t,
        }
    }
}

/// What the pixel shader receives, interpolated perspective-correctly.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Varying {
    pub(crate) uv: Vec2,
    pub(crate) local: Vec3,
    pub(crate) colour: Vec4,
    /// The pixel's distance along the view direction in metres.
    pub(crate) depth: f32,
}

/// Which screen-space facing is thrown away.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Cull {
    /// Keep front faces (the normal case).
    Back,
    /// Keep back faces (the inverted-hull outline).
    Front,
    /// Keep both (double-sided materials).
    None,
}

/// How covered pixels are written.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Write {
    /// Replace colour and depth.
    Opaque,
    /// Blend over the colour with the shader's alpha; depth is tested but not written.
    Blend,
}

/// Per-draw state.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Mode {
    pub(crate) cull: Cull,
    /// True when the model matrix mirrors the mesh, which reverses its winding.
    pub(crate) mirrored: bool,
    pub(crate) write: Write,
}

/// Draws one triangle. `shade` returns the pixel's straight linear RGBA, or `None` to discard it.
pub(crate) fn draw_triangle(
    surface: &mut Surface,
    tri: &[Vertex; 3],
    mode: Mode,
    shade: &mut dyn FnMut(Varying) -> Option<Vec4>,
) {
    let (poly, n) = clip_near(tri);
    for k in 1..n.saturating_sub(1) {
        raster(surface, &[poly[0], poly[k], poly[k + 1]], mode, shade);
    }
}

/// Clips against the near plane (clip-space `z >= 0`); the other planes are handled per pixel. A triangle becomes a
/// polygon of up to four vertices.
fn clip_near(tri: &[Vertex; 3]) -> ([Vertex; 6], usize) {
    let mut out = [tri[0]; 6];
    let mut n = 0;
    for i in 0..3 {
        let (a, b) = (tri[i], tri[(i + 1) % 3]);
        let (da, db) = (a.clip.z, b.clip.z);
        if da >= 0.0 {
            out[n] = a;
            n += 1;
        }
        if (da >= 0.0) != (db >= 0.0) {
            out[n] = a.lerp(b, da / (da - db));
            n += 1;
        }
    }
    (out, n)
}

type Fixed = (i64, i64);

fn edge(a: Fixed, b: Fixed, p: Fixed) -> i64 {
    (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0)
}

/// A pixel centre exactly on an edge belongs to exactly one of the two triangles sharing it: the one that walks the
/// edge "downward" (or rightward when it is horizontal). Any consistent rule works; this one needs no case analysis.
fn owns_edge(a: Fixed, b: Fixed) -> bool {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    dy > 0 || (dy == 0 && dx > 0)
}

#[allow(clippy::cast_possible_truncation)] // clamped to ±COORD_LIMIT before the cast
fn to_fixed(v: f32) -> i64 {
    (v.clamp(-COORD_LIMIT, COORD_LIMIT) * SUBPIXEL_F32).round() as i64
}

#[allow(
    clippy::many_single_char_names,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn raster(surface: &mut Surface, v: &[Vertex; 3], mode: Mode, shade: &mut dyn FnMut(Varying) -> Option<Vec4>) {
    let (w, h) = (surface.size.x, surface.size.y);
    let mut pts = [(0, 0); 3];
    let mut inv_w = [0.0f32; 3];
    let mut z = [0.0f32; 3];
    for k in 0..3 {
        let c = v[k].clip;
        if c.w <= 0.0 || !c.is_finite() {
            return;
        }
        let iw = 1.0 / c.w;
        pts[k] = (to_fixed((c.x * iw * 0.5 + 0.5) * w as f32), to_fixed((0.5 - c.y * iw * 0.5) * h as f32));
        inv_w[k] = iw;
        z[k] = c.z * iw;
    }

    // In y-down screen space a counter-clockwise (front) triangle has a negative area.
    let mut area = edge(pts[0], pts[1], pts[2]);
    if area == 0 {
        return;
    }
    let front = (area < 0) != mode.mirrored;
    match mode.cull {
        Cull::Back if !front => return,
        Cull::Front if front => return,
        _ => {}
    }
    let mut order = [0usize, 1, 2];
    if area < 0 {
        order.swap(1, 2);
        area = -area;
    }
    let [p0, p1, p2] = order.map(|i| pts[i]);
    let [i0, i1, i2] = order.map(|i| inv_w[i]);
    let [z0, z1, z2] = order.map(|i| z[i]);
    let [v0, v1, v2] = order.map(|i| &v[i]);

    let min_x = p0.0.min(p1.0).min(p2.0) >> SUBPIXEL_BITS;
    let max_x = p0.0.max(p1.0).max(p2.0) >> SUBPIXEL_BITS;
    let min_y = p0.1.min(p1.1).min(p2.1) >> SUBPIXEL_BITS;
    let max_y = p0.1.max(p1.1).max(p2.1) >> SUBPIXEL_BITS;
    let (x_lo, x_hi) = ((min_x - 1).max(0), (max_x + 1).min(i64::from(w) - 1));
    let (y_lo, y_hi) = ((min_y - 1).max(0), (max_y + 1).min(i64::from(h) - 1));
    let (own0, own1, own2) = (owns_edge(p1, p2), owns_edge(p2, p0), owns_edge(p0, p1));
    let area_f = area as f64;

    for py in y_lo..=y_hi {
        for px in x_lo..=x_hi {
            let centre = ((px << SUBPIXEL_BITS) + SUBPIXEL / 2, (py << SUBPIXEL_BITS) + SUBPIXEL / 2);
            let (e0, e1, e2) = (edge(p1, p2, centre), edge(p2, p0, centre), edge(p0, p1, centre));
            if e0 < 0 || e1 < 0 || e2 < 0 || (e0 == 0 && !own0) || (e1 == 0 && !own1) || (e2 == 0 && !own2) {
                continue;
            }
            let (l0, l1, l2) = ((e0 as f64 / area_f) as f32, (e1 as f64 / area_f) as f32, (e2 as f64 / area_f) as f32);
            let depth = l0 * z0 + l1 * z1 + l2 * z2;
            let idx = py as usize * w as usize + px as usize;
            if !(0.0..=1.0).contains(&depth) || depth >= surface.depth[idx] {
                continue;
            }
            // Perspective-correct weights: attributes are linear in 1/w, not in screen space.
            let (b0, b1, b2) = (l0 * i0, l1 * i1, l2 * i2);
            let sum = b0 + b1 + b2;
            let (b0, b1, b2) = (b0 / sum, b1 / sum, b2 / sum);
            let varying = Varying {
                uv: v0.uv * b0 + v1.uv * b1 + v2.uv * b2,
                local: v0.local * b0 + v1.local * b1 + v2.local * b2,
                colour: v0.colour * b0 + v1.colour * b1 + v2.colour * b2,
                depth: v0.depth * b0 + v1.depth * b1 + v2.depth * b2,
            };
            let Some(rgba) = shade(varying) else { continue };
            match mode.write {
                Write::Opaque => {
                    surface.colour[idx] = (rgba.truncate() * rgba.w).extend(rgba.w);
                    surface.depth[idx] = depth;
                }
                Write::Blend => {
                    let behind = surface.colour[idx];
                    surface.colour[idx] = (rgba.truncate() * rgba.w).extend(rgba.w) + behind * (1.0 - rgba.w);
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::many_single_char_names,
    clippy::unnecessary_wraps
)] // tests use tiny images and shaders that must return `Option` to fit the callback type
mod tests {
    use std::collections::HashMap;

    use omuzaki_core::rng::Rng;

    use super::*;

    const SIZE: UVec2 = UVec2::new(24, 24);

    fn vertex(x: f32, y: f32, z: f32, w: f32) -> Vertex {
        // Clip-space xy are NDC * w; uv carries the NDC position so tests can read it back.
        Vertex {
            clip: Vec4::new(x * w, y * w, z * w, w),
            uv: Vec2::new(x, y),
            local: Vec3::ZERO,
            colour: Vec4::ONE,
            depth: w,
        }
    }

    fn solid(_: Varying) -> Option<Vec4> {
        Some(Vec4::ONE)
    }

    const FRONT_OPAQUE: Mode = Mode { cull: Cull::Back, mirrored: false, write: Write::Opaque };

    fn covered(s: &Surface) -> usize {
        s.colour.iter().filter(|c| c.w > 0.0).count()
    }

    /// Screen position (in pixels, y down) of an NDC point of the test surface.
    fn to_pixels(p: Vec2) -> Vec2 {
        Vec2::new((p.x * 0.5 + 0.5) * SIZE.x as f32, (0.5 - p.y * 0.5) * SIZE.y as f32)
    }

    fn strictly_inside(tri: [Vec2; 3], p: Vec2, margin: f32) -> bool {
        let e = |a: Vec2, b: Vec2| (b - a).perp_dot(p - a);
        let s = [e(tri[0], tri[1]), e(tri[1], tri[2]), e(tri[2], tri[0])];
        s.iter().all(|&v| v > margin) || s.iter().all(|&v| v < -margin)
    }

    #[test]
    fn the_float_and_integer_subpixel_scales_agree() {
        assert_eq!(SUBPIXEL_F32 as i64, SUBPIXEL);
    }

    #[test]
    fn two_triangles_sharing_an_edge_cover_every_pixel_exactly_once() {
        let mut rng = Rng::new(7);
        for _ in 0..300 {
            let pt = |r: &mut Rng| Vec2::new(r.range_f32(-1.2, 1.2), r.range_f32(-1.2, 1.2));
            let (a, c) = (pt(&mut rng), pt(&mut rng));
            let (b, d) = (pt(&mut rng), pt(&mut rng));
            // B and D must lie on opposite sides of the shared edge A-C, or the triangles overlap.
            if (c - a).perp_dot(b - a) * (c - a).perp_dot(d - a) >= 0.0 || (c - a).length() < 0.2 {
                continue;
            }
            let hits = std::cell::RefCell::new(HashMap::<(u32, u32), u32>::new());
            let mut surface = Surface::new(SIZE, Vec4::ZERO);
            for tri in [[a, b, c], [a, c, d]] {
                let verts = tri.map(|p| vertex(p.x, p.y, 0.5, 1.0));
                // Blend never writes depth, so both triangles are tested against the same empty depth buffer.
                let mode = Mode { cull: Cull::None, mirrored: false, write: Write::Blend };
                let mut count = |v: Varying| {
                    let p = to_pixels(v.uv);
                    *hits.borrow_mut().entry((p.x.floor() as u32, p.y.floor() as u32)).or_default() += 1;
                    Some(Vec4::new(1.0, 1.0, 1.0, 0.0))
                };
                draw_triangle(&mut surface, &verts, mode, &mut count);
            }
            let hits = hits.into_inner();
            assert!(hits.values().all(|&n| n == 1), "a pixel was drawn twice: {a:?} {b:?} {c:?} {d:?}");
            for y in 0..SIZE.y {
                for x in 0..SIZE.x {
                    let centre = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                    let inside = |t: [Vec2; 3]| strictly_inside(t.map(to_pixels), centre, 0.05);
                    if inside([a, b, c]) || inside([a, c, d]) {
                        assert_eq!(hits.get(&(x, y)), Some(&1), "gap at ({x},{y}) in {a:?} {b:?} {c:?} {d:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn faces_are_culled_by_winding_and_mirroring_flips_it() {
        let ccw = [vertex(-0.8, -0.8, 0.5, 1.0), vertex(0.8, -0.8, 0.5, 1.0), vertex(0.0, 0.8, 0.5, 1.0)];
        let cw = [ccw[0], ccw[2], ccw[1]];
        let drawn = |tri: &[Vertex; 3], mode: Mode| {
            let mut s = Surface::new(SIZE, Vec4::ZERO);
            draw_triangle(&mut s, tri, mode, &mut solid);
            covered(&s) > 0
        };
        assert!(drawn(&ccw, FRONT_OPAQUE) && !drawn(&cw, FRONT_OPAQUE));
        let hull = Mode { cull: Cull::Front, ..FRONT_OPAQUE };
        assert!(!drawn(&ccw, hull) && drawn(&cw, hull), "the hull keeps back faces");
        assert!(drawn(&cw, Mode { cull: Cull::None, ..FRONT_OPAQUE }));
        assert!(
            drawn(&cw, Mode { mirrored: true, ..FRONT_OPAQUE }),
            "a mirrored model's clockwise triangle is its front"
        );
    }

    #[test]
    fn nearer_surfaces_win_whatever_the_draw_order() {
        let tri = |z: f32| [vertex(-0.9, -0.9, z, 1.0), vertex(0.9, -0.9, z, 1.0), vertex(0.0, 0.9, z, 1.0)];
        for order in [[0.3, 0.7], [0.7, 0.3]] {
            let mut s = Surface::new(SIZE, Vec4::ZERO);
            for z in order {
                let mut paint = |_: Varying| Some(Vec4::new(z, 0.0, 0.0, 1.0));
                draw_triangle(&mut s, &tri(z), FRONT_OPAQUE, &mut paint);
            }
            let centre = s.colour[(SIZE.y as usize / 2) * SIZE.x as usize + SIZE.x as usize / 2];
            assert!((centre.x - 0.3).abs() < 1e-6, "the z=0.3 triangle is in front: {centre:?}");
        }
    }

    #[test]
    fn a_triangle_crossing_the_near_plane_is_clipped_not_dropped() {
        // One vertex is behind the camera (negative clip z and w would make it vanish if clipping were missing).
        let behind = Vertex { clip: Vec4::new(0.0, 0.0, -0.5, 0.5), ..vertex(0.0, 0.0, 0.0, 1.0) };
        let tri = [vertex(-0.9, -0.9, 0.5, 1.0), vertex(0.9, -0.9, 0.5, 1.0), behind];
        let mut s = Surface::new(SIZE, Vec4::ZERO);
        draw_triangle(&mut s, &tri, Mode { cull: Cull::None, ..FRONT_OPAQUE }, &mut solid);
        assert!(covered(&s) > 0);
        assert!(s.colour.iter().all(|c| c.is_finite()));
    }

    #[test]
    fn attributes_are_perspective_correct() {
        // A quad receding in depth: w is 1 along the bottom edge and 3 along the top. The attribute `a` runs 0 to 1
        // up the quad. At a given screen height, perspective-correct interpolation weights each end by 1/w, so the
        // far (top) end counts for less than linear interpolation in screen space would say.
        let with_a = |mut v: Vertex, a: f32| {
            v.uv.y = a;
            v
        };
        let (bl, br) = (with_a(vertex(-0.9, -0.9, 0.5, 1.0), 0.0), with_a(vertex(0.9, -0.9, 0.5, 1.0), 0.0));
        let (tl, tr) = (with_a(vertex(-0.9, 0.9, 0.5, 3.0), 1.0), with_a(vertex(0.9, 0.9, 0.5, 3.0), 1.0));
        let mut s = Surface::new(SIZE, Vec4::ZERO);
        for tri in [[bl, br, tr], [bl, tr, tl]] {
            let mode = Mode { cull: Cull::None, ..FRONT_OPAQUE };
            draw_triangle(&mut s, &tri, mode, &mut |v: Varying| Some(Vec4::new(v.uv.y, 0.0, 0.0, 1.0)));
        }
        let row = SIZE.y as usize / 2;
        let got = s.colour[row * SIZE.x as usize + SIZE.x as usize / 2].x;
        let ndc_y = (0.5 - (row as f32 + 0.5) / SIZE.y as f32) * 2.0;
        let t = (ndc_y + 0.9) / 1.8; // how far up the quad this row is on screen
        let expected = (t / 3.0) / ((1.0 - t) + t / 3.0);
        assert!(
            (got - expected).abs() < 0.01,
            "got {got}, perspective-correct is {expected}, screen-linear would be {t}"
        );
    }
}
