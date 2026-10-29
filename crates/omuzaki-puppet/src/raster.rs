//! The CPU renderer: draws a [`DrawList2D`] into a premultiplied canvas, with every blend mode, masks and composites.
//!
//! It is the reference the GPU pass is checked against, the renderer for offline video (`short.render`) and the
//! fallback where no GPU is. The canvas is split into horizontal bands drawn on separate threads; every band runs
//! the whole command list over its own rows, so masks and composites need no synchronisation, and the result is the
//! same bytes for any thread count.
//!
//! Triangles are filled at pixel centres with the top-left rule, so the triangles of one mesh never cover a pixel
//! twice; art carries its own soft edges in its alpha (meshes enclose it with a transparent margin), so no
//! multisampling is needed. Texture detail follows each triangle's scale on screen (trilinear mips).
#![allow(clippy::many_single_char_names, reason = "geometry and sampling read best in their formulas' own letters")]

use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_puppet_api::{DrawCmd2D, DrawList2D, DrawState, MaskMode};

use crate::blend::{Px, blend, shade};
use crate::texture::Texture;

/// A premultiplied RGBA canvas, `0..1` values, row-major from the top.
#[derive(Clone, Debug)]
pub struct Canvas {
    w: u32,
    h: u32,
    px: Vec<Px>,
}

impl Canvas {
    /// A transparent canvas.
    #[must_use]
    pub fn new(w: u32, h: u32) -> Self {
        Self { w, h, px: vec![[0.0; 4]; (w as usize) * (h as usize)] }
    }

    /// Its size.
    #[must_use]
    pub fn size(&self) -> [u32; 2] {
        [self.w, self.h]
    }

    /// Fills it with one premultiplied colour.
    pub fn fill(&mut self, px: Px) {
        self.px.iter_mut().for_each(|p| *p = px);
    }

    /// The pixel at `(x, y)`.
    #[must_use]
    pub fn get(&self, x: u32, y: u32) -> Option<Px> {
        (x < self.w && y < self.h).then(|| self.px[(y as usize) * (self.w as usize) + x as usize])
    }

    /// The pixels, row-major.
    #[must_use]
    pub fn pixels(&self) -> &[Px] {
        &self.px
    }

    /// The canvas as straight (not premultiplied) RGBA8, rounded.
    #[must_use]
    pub fn to_straight_rgba8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.px.len() * 4);
        for p in &self.px {
            let a = p[3].clamp(0.0, 1.0);
            let un = |c: f32| if a > 0.0 { (c / a).clamp(0.0, 1.0) } else { 0.0 };
            out.extend_from_slice(&[to_byte(un(p[0])), to_byte(un(p[1])), to_byte(un(p[2])), to_byte(a)]);
        }
        out
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to 0..255")]
fn to_byte(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Scratch memory for one band.
#[derive(Clone, Debug, Default)]
struct Band {
    /// The layer stack of open composites (each a band-sized buffer), reused frame to frame.
    layers: Vec<Vec<Px>>,
    depth: usize,
    mask_in: Vec<f32>,
    mask_out: Vec<f32>,
    has_in: bool,
}

/// The CPU renderer and its scratch memory.
#[derive(Clone, Debug)]
pub struct CpuRenderer {
    threads: usize,
    bands: Vec<Band>,
    screen: Vec<Vec2>,
}

impl Default for CpuRenderer {
    fn default() -> Self {
        Self::new(std::thread::available_parallelism().map_or(1, std::num::NonZero::get).min(8))
    }
}

/// What one triangle needs to be filled.
struct Tri {
    p: [Vec2; 3],
    uv: [[f32; 2]; 3],
    lod: f32,
}

impl CpuRenderer {
    /// A renderer drawing on `threads` threads (at least one).
    #[must_use]
    pub fn new(threads: usize) -> Self {
        Self { threads: threads.max(1), bands: Vec::new(), screen: Vec::new() }
    }

    /// Draws `list` over `canvas` (blending onto what is there), with `view` taking model space to pixels (pixel
    /// `(x, y)` covers `x..x+1`, `y..y+1`) and `textures` indexed as the list names them.
    pub fn draw(&mut self, list: &DrawList2D, textures: &[Texture], view: Affine2, canvas: &mut Canvas) {
        self.screen.clear();
        self.screen.extend(list.vertices.iter().map(|v| view.transform_point2(Vec2::from(v.pos))));
        let (w, h) = (canvas.w as usize, canvas.h as usize);
        if w == 0 || h == 0 {
            return;
        }
        let rows_per = h.div_ceil(self.threads);
        let bands = h.div_ceil(rows_per);
        self.bands.resize_with(bands, Band::default);
        let screen = &self.screen;
        std::thread::scope(|scope| {
            for (i, (rows, band)) in canvas.px.chunks_mut(rows_per * w).zip(self.bands.iter_mut()).enumerate() {
                let y0 = i * rows_per;
                scope.spawn(move || draw_band(list, textures, screen, rows, band, w, y0));
            }
        });
    }
}

fn draw_band(
    list: &DrawList2D,
    textures: &[Texture],
    screen: &[Vec2],
    target: &mut [Px],
    band: &mut Band,
    w: usize,
    y0: usize,
) {
    let n = target.len();
    band.mask_in.resize(n, 0.0);
    band.mask_out.resize(n, 0.0);
    band.depth = 0;
    band.has_in = false;
    for cmd in &list.cmds {
        match *cmd {
            DrawCmd2D::MaskBegin => {
                band.mask_in.fill(0.0);
                band.mask_out.fill(0.0);
                band.has_in = false;
            }
            DrawCmd2D::MaskShape { texture, first, count, mode, threshold } => {
                let tex = texture.and_then(|t| textures.get(t.get()));
                debug_assert!(texture.is_none() || tex.is_some(), "a mask names a texture that isn't loaded");
                if mode == MaskMode::Mask {
                    band.has_in = true;
                }
                let buf = if mode == MaskMode::Mask { &mut band.mask_in } else { &mut band.mask_out };
                for_each_tri(list, screen, tex, first, count, |tri| {
                    fill(tri, w, y0, n / w.max(1), |i, uv, lod| {
                        let cover = tex.map_or(1.0, |t| if t.sample(uv, lod)[3] > threshold { 1.0 } else { 0.0 });
                        if cover > buf[i] {
                            buf[i] = cover;
                        }
                    });
                });
            }
            DrawCmd2D::Mesh { texture, first, count, state } => {
                // A hidden part (an expression or a pose that is off) costs nothing: zero opacity changes no pixel.
                if state.opacity <= 0.0 {
                    continue;
                }
                let Some(tex) = textures.get(texture.get()) else {
                    debug_assert!(false, "a mesh names a texture that isn't loaded");
                    continue;
                };
                let (has_in, depth) = (band.has_in, band.depth);
                let Band { layers, mask_in, mask_out, .. } = band;
                let dst: &mut [Px] = if depth == 0 { &mut *target } else { &mut layers[depth - 1] };
                for_each_tri(list, screen, Some(tex), first, count, |tri| {
                    fill(tri, w, y0, n / w.max(1), |i, uv, lod| {
                        let k = state.opacity * mask_factor(&state, has_in, mask_in, mask_out, i);
                        if k <= 0.0 {
                            return;
                        }
                        let s = shade(tex.sample(uv, lod), state.tint, state.screen_tint, k);
                        if s[3] > 0.0 || matches!(state.blend, omuzaki_puppet_api::BlendMode::ClipToLower) {
                            dst[i] = blend(state.blend, s, dst[i]);
                        }
                    });
                });
            }
            DrawCmd2D::CompositeBegin => {
                if band.layers.len() <= band.depth {
                    band.layers.push(vec![[0.0; 4]; n]);
                }
                let layer = &mut band.layers[band.depth];
                layer.resize(n, [0.0; 4]);
                layer.fill([0.0; 4]);
                band.depth += 1;
            }
            DrawCmd2D::CompositeEnd { state } => {
                if band.depth == 0 {
                    debug_assert!(false, "a composite ends that never began");
                    continue;
                }
                band.depth -= 1;
                let (has_in, depth) = (band.has_in, band.depth);
                let Band { layers, mask_in, mask_out, .. } = band;
                let (below, above) = layers.split_at_mut(depth);
                let src = &above[0];
                let dst: &mut [Px] = if depth == 0 { &mut *target } else { &mut below[depth - 1] };
                for i in 0..n {
                    if src[i][3] <= 0.0 {
                        continue;
                    }
                    let k = state.opacity * mask_factor(&state, has_in, mask_in, mask_out, i);
                    dst[i] = blend(state.blend, shade(src[i], state.tint, state.screen_tint, k), dst[i]);
                }
            }
        }
    }
}

fn mask_factor(state: &DrawState, has_in: bool, mask_in: &[f32], mask_out: &[f32], i: usize) -> f32 {
    if !state.masked {
        return 1.0;
    }
    let inside = if has_in { mask_in[i] } else { 1.0 };
    inside * (1.0 - mask_out[i])
}

/// Calls `f` with each triangle of an index range, in screen space, with its texture detail level.
fn for_each_tri(
    list: &DrawList2D,
    screen: &[Vec2],
    tex: Option<&Texture>,
    first: u32,
    count: u32,
    mut f: impl FnMut(&Tri),
) {
    let (a, b) = (first as usize, first as usize + count as usize);
    let Some(indices) = list.indices.get(a..b) else {
        debug_assert!(false, "a command past the index buffer");
        return;
    };
    #[allow(clippy::cast_precision_loss, reason = "texture sides far below 2^24")]
    let size = tex.map_or(Vec2::ONE, |t| Vec2::new(t.size()[0] as f32, t.size()[1] as f32));
    for t in indices.as_chunks::<3>().0 {
        let (Some(va), Some(vb), Some(vc)) =
            (list.vertices.get(t[0] as usize), list.vertices.get(t[1] as usize), list.vertices.get(t[2] as usize))
        else {
            continue;
        };
        let p = [screen[t[0] as usize], screen[t[1] as usize], screen[t[2] as usize]];
        let uv = [va.uv, vb.uv, vc.uv];
        // The affine map from pixels to texels: its larger column is the texels per pixel step.
        let (e1, e2) = (p[1] - p[0], p[2] - p[0]);
        let det = e1.perp_dot(e2);
        if det.abs() < 1e-9 {
            continue;
        }
        let (u1, u2) = ((Vec2::from(uv[1]) - Vec2::from(uv[0])) * size, (Vec2::from(uv[2]) - Vec2::from(uv[0])) * size);
        // d(uv)/dx and d(uv)/dy, from [e1 e2] * J = [u1 u2].
        let ddx = (u1 * e2.y - u2 * e1.y) / det;
        let ddy = (u2 * e1.x - u1 * e2.x) / det;
        let rho = ddx.length().max(ddy.length());
        let lod = if rho > 1.0 { rho.log2() } else { 0.0 };
        f(&Tri { p, uv, lod });
    }
}

/// Fills `tri` over the band starting at row `y0` (`rows` rows of `w`), calling `px` with each covered pixel's index
/// in the band, its texture coordinate and the detail level.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixel maths")]
fn fill(tri: &Tri, w: usize, y0: usize, rows: usize, mut px: impl FnMut(usize, [f32; 2], f32)) {
    let [a, mut b, mut c] = tri.p;
    let [ua, mut ub, mut uc] = tri.uv;
    let mut area = (b - a).perp_dot(c - a);
    if area < 0.0 {
        std::mem::swap(&mut b, &mut c);
        std::mem::swap(&mut ub, &mut uc);
        area = -area;
    }
    if area <= 0.0 {
        return;
    }
    let lo = a.min(b).min(c);
    let hi = a.max(b).max(c);
    let x_lo = (lo.x - 0.5).ceil().max(0.0) as usize;
    let x_hi = ((hi.x - 0.5).floor().min(w as f32 - 1.0)).max(-1.0);
    let y_band_lo = y0 as f32;
    let y_band_hi = (y0 + rows) as f32 - 1.0;
    let y_lo = (lo.y - 0.5).ceil().max(y_band_lo);
    let y_hi = (hi.y - 0.5).floor().min(y_band_hi);
    if x_hi < 0.0 || y_hi < y_lo || (x_lo as f32) > x_hi {
        return;
    }
    let (x_hi, y_lo, y_hi) = (x_hi as usize, y_lo as usize, y_hi as usize);
    // Edge functions, positive inside (counter-clockwise in y-down space after the swap above).
    let edge = |p: Vec2, q: Vec2| {
        // Top-left rule: a pixel exactly on an edge belongs to it only if the edge is a top or left edge.
        let d = q - p;
        let top_left = (d.y == 0.0 && d.x < 0.0) || d.y > 0.0;
        (p, d, if top_left { 0.0 } else { f32::EPSILON })
    };
    let edges = [edge(b, c), edge(c, a), edge(a, b)];
    let inv = 1.0 / area;
    for y in y_lo..=y_hi {
        let py = y as f32 + 0.5;
        let row = (y - y0) * w;
        for x in x_lo..=x_hi {
            let p = Vec2::new(x as f32 + 0.5, py);
            let mut wts = [0.0; 3];
            let mut inside = true;
            for (k, (o, d, bias)) in edges.iter().enumerate() {
                let v = d.perp_dot(p - *o);
                if v < *bias {
                    inside = false;
                    break;
                }
                wts[k] = v;
            }
            if !inside {
                continue;
            }
            let (l0, l1, l2) = (wts[0] * inv, wts[1] * inv, wts[2] * inv);
            let uv = [ua[0] * l0 + ub[0] * l1 + uc[0] * l2, ua[1] * l0 + ub[1] * l1 + uc[1] * l2];
            px(row + x, uv, tri.lod);
        }
    }
}

/// The view that fits `canvas` (model units, centred on the origin) into a `w` x `h` image: the puppet's whole canvas
/// in frame, centred.
#[must_use]
#[allow(clippy::cast_precision_loss, reason = "image sides far below 2^24")]
pub fn fit_view(canvas: [f32; 2], w: u32, h: u32) -> Affine2 {
    let scale = (w as f32 / canvas[0]).min(h as f32 / canvas[1]);
    Affine2::from_scale_angle_translation(Vec2::splat(scale), 0.0, Vec2::new(w as f32, h as f32) * 0.5)
}
