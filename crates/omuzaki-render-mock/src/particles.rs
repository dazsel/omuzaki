//! Particles on the CPU (particles.md D3 to D5): the reference renderer for ST-13.
//!
//! Every particle is [`evaluate`]d at the frame's time, turned into a quad by its system's orientation, and rasterised
//! with the mock's exact rasteriser. Coverage stands in for blending, as on the GPU (D4): a pixel's alpha (the shape's
//! signed distance anti-aliased over one output pixel, the atlas texel, the system's `coverage`) is compared with a
//! fixed ordered (Bayer) threshold per sub-sample of the supersampling grid, and a covered sub-sample takes the opaque
//! colour and writes depth. The resolve then averages the sub-samples, so a particle half covering a pixel over a
//! transparent background resolves to `(c/2, 1/2)`, premultiplied by construction, in any draw order.
//!
//! The step allocates nothing: it reads the scene's batches and the systems in place.

use std::f32::consts::FRAC_1_SQRT_2;

use omuzaki_core::glam::{Mat4, Vec2, Vec3, Vec4};
use omuzaki_render_api::particles::{ParticleState, evaluate};
use omuzaki_render_api::{
    Camera, CellPick, Material, Orientation, ParticleShading, ParticleSystemDesc, Projection, Rim, Scene, Shading,
    Sprite, SpriteShape, Toon,
};

use crate::raster::{Cull, Mode, Surface, Varying, Vertex, Write, draw_triangle};
use crate::shade::{FogLight, Lights, Textures, light_triangle, lit_colour};

/// A live particle system: its description and the material its toon shading is evaluated with.
pub(crate) struct SystemEntry {
    pub(crate) desc: ParticleSystemDesc,
    /// Toon shading as the surfaces' toon model sees it: the system's shade, shift and softness, no rim, no skin.
    material: Material,
}

impl SystemEntry {
    pub(crate) fn new(desc: ParticleSystemDesc) -> Self {
        let material = shading_material(&desc.params.shading);
        Self { desc, material }
    }
}

/// The material a particle's toon shading runs through (D5): no rim, no skin treatment, no shadows.
pub(crate) fn shading_material(shading: &ParticleShading) -> Material {
    match *shading {
        ParticleShading::Toon { shade, shift, softness, .. } => {
            let toon = Toon { shade, shift, softness, rim: Rim { strength: 0.0, ..Rim::default() }, ..Toon::default() };
            Material {
                shading: Shading::Toon(toon),
                receives_shadows: false,
                ..Material::toon(omuzaki_render_api::Colour::WHITE)
            }
        }
        ParticleShading::Unlit => Material::unlit(omuzaki_render_api::Colour::WHITE),
    }
}

/// The ordered threshold of internal pixel `(x, y)` in a `grid` x `grid` supersampling cell: the cell's sub-samples
/// take thresholds spread evenly over `(0, 1)`, so alpha `a` covers about `a` of them. One sample is a 0.5 alpha test.
pub(crate) fn bayer(x: u32, y: u32, grid: u32) -> f32 {
    const B2: [u8; 4] = [0, 2, 3, 1];
    const B3: [u8; 9] = [0, 7, 3, 6, 5, 2, 4, 1, 8];
    const B4: [u8; 16] = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];
    let (i, j) = ((x % grid.max(1)) as usize, (y % grid.max(1)) as usize);
    let rank = match grid {
        2 => B2[j * 2 + i],
        3 => B3[j * 3 + i],
        4 => B4[j * 4 + i],
        _ => 0,
    };
    let side = f32::from(u8::try_from(grid.clamp(1, 4)).unwrap_or(1));
    let cells = side * side;
    (f32::from(rank) + 0.5) / cells
}

/// The signed distance from `p` to `shape`, in units of the quad's half height, for a quad `aspect` wide per unit of
/// height: negative inside. The GPU's `shape_distance` (`shader/particles.wgsl`) is the same function.
pub(crate) fn shape_distance(shape: SpriteShape, p: Vec2, aspect: f32) -> f32 {
    match shape {
        SpriteShape::Square | SpriteShape::Rect { .. } => (p.x.abs() - aspect).max(p.y.abs() - 1.0),
        SpriteShape::Disc => p.length() - 1.0,
        SpriteShape::Petal => {
            // The lens where two discs meet, half as wide as it is tall.
            let (half_width, c) = (0.5, 0.75);
            let r = c + half_width;
            ((p - Vec2::new(c, 0.0)).length() - r).max((p + Vec2::new(c, 0.0)).length() - r)
        }
        SpriteShape::Star { points, .. } => star_distance(p, f32::from(points.max(2))),
        SpriteShape::Heart => heart_distance(p),
        SpriteShape::Streak => {
            // A capsule along the quad's height, as wide as the quad.
            let r = aspect.min(1.0);
            let reach = 1.0 - r;
            (p - Vec2::new(0.0, p.y.clamp(-reach, reach))).length() - r
        }
    }
}

/// A star with `n` points of radius 1 and inner vertices at 0.45: the distance to the edge from a tip to the next inner
/// vertex, after folding `p` into one half-sector.
fn star_distance(p: Vec2, n: f32) -> f32 {
    const INNER: f32 = 0.45;
    let sector = std::f32::consts::PI / n;
    // Angle from +Y, so a point is at the top.
    let angle = p.x.atan2(p.y);
    let folded = ((angle + sector).rem_euclid(2.0 * sector) - sector).abs();
    let q = Vec2::new(folded.cos(), folded.sin()) * p.length();
    let (tip, inner) = (Vec2::new(1.0, 0.0), Vec2::new(sector.cos(), sector.sin()) * INNER);
    let edge = inner - tip;
    let t = ((q - tip).dot(edge) / edge.length_squared()).clamp(0.0, 1.0);
    let distance = (q - (tip + edge * t)).length();
    // Inside is the side of the edge the centre is on.
    let side = edge.perp_dot(q - tip) * edge.perp_dot(-tip);
    if side > 0.0 { -distance } else { distance }
}

/// A heart: a square turned 45 degrees with a disc on each upper side, fitted in the quad.
fn heart_distance(p: Vec2) -> f32 {
    const SIDE: f32 = 1.17;
    let centre = Vec2::new(0.0, -1.0 + SIDE * FRAC_1_SQRT_2);
    let local = p - centre;
    let turned = Vec2::new(local.x + local.y, local.y - local.x) * FRAC_1_SQRT_2;
    let square = turned.x.abs().max(turned.y.abs()) - SIDE * 0.5;
    let lobe = Vec2::new(SIDE * 0.5 * FRAC_1_SQRT_2, -1.0 + 1.5 * SIDE * FRAC_1_SQRT_2);
    let discs = (p - lobe).length().min((p - Vec2::new(-lobe.x, lobe.y)).length()) - SIDE * 0.5;
    square.min(discs)
}

/// The camera facts the quads need.
struct View {
    eye_space: Mat4,
    world_to_clip: Mat4,
    camera: Camera,
    right: Vec3,
    up: Vec3,
    /// Projection scale: output pixels per metre at view depth 1 (and per metre for an orthographic camera).
    pixels_per_metre: f32,
    orthographic: bool,
}

impl View {
    fn new(scene: &Scene, view_proj: Mat4, height_px: f32, aspect: f32) -> Self {
        let projection = scene.camera.projection_matrix(aspect);
        Self {
            eye_space: scene.camera.view(),
            world_to_clip: view_proj,
            camera: scene.camera,
            right: scene.camera.transform.x_axis.truncate().normalize_or_zero(),
            up: scene.camera.transform.y_axis.truncate().normalize_or_zero(),
            pixels_per_metre: projection.y_axis.y * height_px * 0.5,
            orthographic: matches!(scene.camera.projection, Projection::Orthographic { .. }),
        }
    }

    fn to_viewer(&self, p: Vec3) -> Vec3 {
        if self.orthographic {
            self.camera.transform.z_axis.truncate().normalize_or_zero()
        } else {
            (self.camera.position() - p).normalize_or_zero()
        }
    }

    /// Output pixels a metre spans at `p`.
    fn pixels_at(&self, p: Vec3) -> f32 {
        let w = if self.orthographic { 1.0 } else { (self.world_to_clip * p.extend(1.0)).w.max(1e-6) };
        self.pixels_per_metre / w
    }
}

/// A quad: its centre, its half edges (width along `u`, height along `v`, `v` down the picture), and the share of
/// coverage left after the 1.5-pixel minimum enlarged it.
struct Quad {
    centre: Vec3,
    half: [Vec3; 2],
    /// Height in output pixels, after the minimum.
    height_px: f32,
    /// Width over height of the drawn quad.
    aspect: f32,
    coverage: f32,
}

/// The smallest a quad side is drawn, in output pixels (D5): smaller ones are enlarged and their coverage scaled down
/// by the area ratio, so far rain doesn't crawl as it crosses sample positions.
const MIN_PIXELS: f32 = 1.5;

fn quad(state: &ParticleState, orientation: Orientation, view: &View) -> Option<Quad> {
    let to_viewer = view.to_viewer(state.position);
    let (along_u, along_v) = match orientation {
        Orientation::Billboard { .. } => {
            let (s, c) = state.roll.sin_cos();
            (view.right * c + view.up * s, view.up * c - view.right * s)
        }
        Orientation::Velocity { .. } => {
            let across = state.velocity - to_viewer * state.velocity.dot(to_viewer);
            let long = across.try_normalize().unwrap_or(view.up);
            (to_viewer.cross(long).normalize_or_zero(), long)
        }
        Orientation::Tumble { .. } => (state.edges[0].normalize_or_zero(), state.edges[1].normalize_or_zero()),
    };
    let per_metre = view.pixels_at(state.position);
    let (mut width, mut height) = (state.extent.x, state.extent.y);
    if width <= 0.0 || height <= 0.0 || !per_metre.is_finite() {
        return None;
    }
    let area = width * height;
    width = width.max(MIN_PIXELS / per_metre);
    height = height.max(MIN_PIXELS / per_metre);
    Some(Quad {
        centre: state.position,
        half: [along_u * (width * 0.5), along_v * (height * 0.5)],
        height_px: height * per_metre,
        aspect: width / height,
        coverage: area / (width * height),
    })
}

/// The particle step: draws every batch's particles into `surface`, between the outlines and the blended instances.
/// Returns the slots submitted (the sum of the batches' `count`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw(
    surface: &mut Surface,
    scene: &Scene,
    systems: &crate::arena::Arena<SystemEntry>,
    lights: &Lights<'_>,
    fog: FogLight,
    textures: &Textures<'_>,
    density: f32,
    grid: u32,
    view_proj: Mat4,
    height_px: f32,
    aspect: f32,
) -> u32 {
    let view = View::new(scene, view_proj, height_px, aspect);
    let mut slots = 0u32;
    for batch in &scene.particles {
        let Some(system) = systems.get(batch.system.index(), batch.system.generation()) else { continue };
        slots = slots.saturating_add(batch.count);
        let p = &system.desc.params;
        for g in batch.first..batch.first.saturating_add(batch.count) {
            let Some(state) = evaluate(&system.desc, batch, g, scene.frame.time, density) else { continue };
            let Some(quad) = quad(&state, p.orientation, &view) else { continue };
            draw_particle(surface, &view, system, &state, &quad, lights, fog, textures, scene.outline_light, grid);
        }
    }
    slots
}

#[allow(clippy::too_many_arguments)]
fn draw_particle(
    surface: &mut Surface,
    view: &View,
    system: &SystemEntry,
    state: &ParticleState,
    quad: &Quad,
    lights: &Lights<'_>,
    fog: FogLight,
    textures: &Textures<'_>,
    outline_light: f32,
    grid: u32,
) {
    let p = &system.desc.params;
    let to_viewer = view.to_viewer(state.position);
    // Lit once, at the centre (D5): a camera-facing quad faces the viewer, a tumbling one turns its own side to it.
    let normal = if state.normal == Vec3::ZERO {
        to_viewer
    } else if state.normal.dot(to_viewer) < 0.0 {
        -state.normal
    } else {
        state.normal
    };
    let lit = light_triangle(&system.material, lights, normal, state.position, to_viewer);
    let depth = -view.eye_space.transform_point3(state.position).z;
    let glow = 1.0 + p.glow;
    let corner = |u: f32, v: f32| {
        let world = quad.centre + quad.half[0] * (2.0 * u - 1.0) + quad.half[1] * (1.0 - 2.0 * v);
        Vertex {
            clip: view.world_to_clip * world.extend(1.0),
            uv: Vec2::new(u, v),
            local: Vec3::ZERO,
            colour: Vec4::ONE,
            depth,
        }
    };
    let [top_left, top_right, bottom_right, bottom_left] =
        [corner(0.0, 0.0), corner(1.0, 0.0), corner(1.0, 1.0), corner(0.0, 1.0)];
    let mode = Mode { cull: Cull::None, mirrored: false, write: Write::Coverage { grid } };
    // One output pixel in the shape's units (half heights).
    let pixel = 2.0 / quad.height_px.max(1e-3);
    let mut shade = |v: Varying| -> Option<Vec4> {
        let q = Vec2::new((2.0 * v.uv.x - 1.0) * quad.aspect, 1.0 - 2.0 * v.uv.y);
        let (albedo, alpha, edge) = match system.desc.sprite {
            Sprite::Shape(shape) => {
                let d = shape_distance(shape, q, quad.aspect);
                let alpha = (0.5 - d / pixel).clamp(0.0, 1.0);
                let edge = p.edge.is_some_and(|e| d > -e.width_px * pixel);
                (state.colour, alpha, edge)
            }
            Sprite::Atlas { texture, columns, rows, .. } => {
                let (cols, rows) = (u32::from(columns.max(1)), u32::from(rows.max(1)));
                #[allow(clippy::cast_precision_loss)] // at most 4,096 cells
                let cell = Vec2::new((state.cell % cols) as f32, (state.cell / cols) as f32);
                #[allow(clippy::cast_precision_loss)]
                let at = (cell + v.uv.clamp(Vec2::ZERO, Vec2::ONE)) / Vec2::new(cols as f32, rows as f32);
                let texel = textures.sample(texture, at);
                (state.colour * texel.truncate(), texel.w, false)
            }
        };
        let mut colour = lit_colour(&system.material, lit, albedo) * glow;
        if edge && let Some(e) = p.edge {
            colour *= e.shade * outline_light;
        }
        Some(fog.apply(colour.extend(alpha * p.coverage * quad.coverage), depth))
    };
    draw_triangle(surface, &[top_left, top_right, bottom_right], mode, &mut shade);
    draw_triangle(surface, &[top_left, bottom_right, bottom_left], mode, &mut shade);
}

/// Whether every listed cell of a batch is inside its system's atlas (D2: a cell outside is `Invalid` at render).
pub(crate) fn cells_fit(desc: &ParticleSystemDesc, cells: CellPick) -> bool {
    match desc.sprite {
        Sprite::Shape(_) => true,
        Sprite::Atlas { .. } => cells.as_slice().iter().all(|&c| u32::from(c) < desc.cells()),
    }
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_split_alpha_evenly_on_every_grid() {
        for grid in 1..=4 {
            let mut t: Vec<f32> = (0..grid).flat_map(|y| (0..grid).map(move |x| bayer(x, y, grid))).collect();
            t.sort_by(f32::total_cmp);
            let n = (grid * grid) as f32;
            for (i, v) in t.iter().enumerate() {
                assert!((v - (i as f32 + 0.5) / n).abs() < 1e-6, "grid {grid}: {t:?}");
            }
        }
    }

    #[test]
    fn every_shape_is_inside_at_its_centre_and_outside_at_the_corners() {
        let shapes = [
            SpriteShape::Square,
            SpriteShape::rect(0.6),
            SpriteShape::Disc,
            SpriteShape::Petal,
            SpriteShape::star(5),
            SpriteShape::Heart,
            SpriteShape::Streak,
        ];
        for shape in shapes {
            let aspect = if matches!(shape, SpriteShape::Streak) { 0.3 } else { 1.0 };
            assert!(shape_distance(shape, Vec2::new(0.0, -0.1), aspect) < 0.0, "{shape:?} centre");
            assert!(shape_distance(shape, Vec2::new(1.4, 1.4), aspect) > 0.0, "{shape:?} corner");
        }
        assert!(shape_distance(SpriteShape::star(5), Vec2::new(0.0, 0.95), 1.0) < 0.0, "a point at the top");
        assert!(shape_distance(SpriteShape::star(5), Vec2::new(0.0, -0.9), 1.0) > 0.0, "no point at the bottom");
        assert!(shape_distance(SpriteShape::Heart, Vec2::new(0.0, 0.8), 1.0) > 0.0, "the notch");
    }
}
