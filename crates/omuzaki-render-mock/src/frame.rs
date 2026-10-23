//! One frame, start to finish: pose instances, draw the passes in contract order, resolve to bytes.

use omuzaki_core::glam::{Mat3, Mat4, UVec2, Vec2, Vec3, Vec4};
use omuzaki_render_api::{
    AlphaEncoding, BackdropFit, Background, Camera, Colour, ColourSpace, Image, Instance, Material, Outline,
    OutlineWidthMode, Pass, Post, Projection, Quality, RenderError, Scene, TargetDesc, Tonemap, gradient_at,
    srgb_encode, to_byte,
};

use crate::TextureData;
use crate::arena::Arena;
use crate::environment::{EnvLight, EnvironmentData};
use crate::morph::morph_vertices;
use crate::particles::{self, SystemEntry};
use crate::raster::{Cull, Mode, Surface, Varying, Vertex, Write, draw_triangle};
use crate::resources::{MeshEntry, weld_normals};
use crate::shade::{FogLight, Lights, Lit, Textures, light_triangle, mask_keeps, shade_pixel};

/// The internal buffer may hold at most this many pixels (a 4096x4096 frame): the mock keeps 20 bytes per pixel.
const MAX_INTERNAL_PIXELS: u64 = 1 << 24;

/// The renderer's resource tables, borrowed for the duration of a frame.
pub(crate) struct Resources<'a> {
    pub(crate) meshes: &'a Arena<MeshEntry>,
    pub(crate) textures: &'a Arena<TextureData>,
    pub(crate) materials: &'a Arena<Material>,
    pub(crate) environments: &'a Arena<EnvironmentData>,
    pub(crate) particle_systems: &'a Arena<SystemEntry>,
}

/// What a frame produced.
pub(crate) struct Drawn {
    pub(crate) image: Image,
    pub(crate) render_size: UVec2,
    pub(crate) draw_calls: u32,
    pub(crate) triangles: u64,
    /// Particle slots submitted.
    pub(crate) particles: u32,
    /// Which passes had work, indexed like `Pass::ALL`.
    pub(crate) ran: [bool; Pass::COUNT],
}

/// An instance's vertices after skinning and the model transform.
struct Posed {
    world: Vec<Vec3>,
    /// Unit directions the outline pushes each vertex along.
    hull_dir: Vec<Vec3>,
}

/// A blended triangle waiting for the transparent pass.
struct BlendTriangle<'a> {
    order: i16,
    /// Distance along the view direction, for back-to-front sorting.
    depth: f32,
    verts: [Vertex; 3],
    mode: Mode,
    material: &'a Material,
    lit: Lit,
}

/// The smallest `g` with `g * g >= samples`: the mock supersamples on a `g x g` grid, so 4 samples are 2x2, 16 are 4x4.
fn grid_for(samples: u8) -> u32 {
    // `samples` is validated to at most 16, so the grid never exceeds 4.
    (1u32..=4).find(|g| g * g >= u32::from(samples)).unwrap_or(4)
}

/// Paints the environment's radiance into every pixel of `surface`, seen through `rays` (`Camera::sky_rays`), opaque.
fn fill_backdrop(surface: &mut Surface, environment: &EnvLight<'_>, rays: Mat3) {
    let (size, width) = (surface.size.as_vec2(), surface.size.x as usize);
    for (i, pixel) in surface.colour.iter_mut().enumerate() {
        #[allow(clippy::cast_precision_loss)] // the internal picture is at most 4096 on a side
        let (x, y) = ((i % width) as f32, (i / width) as f32);
        let ndc = Vec2::new((x + 0.5) / size.x * 2.0 - 1.0, 1.0 - (y + 0.5) / size.y * 2.0);
        *pixel = environment.radiance(rays * ndc.extend(1.0)).extend(1.0);
    }
}

/// Draws `scene` for a target of `desc`.
///
/// `painted` says a painter ran since the last frame, so the `PaintTextures` pass is reported.
// One frame in contract order, pass after pass, reads best as one function.
#[allow(clippy::too_many_lines)]
pub(crate) fn draw_frame(
    res: &Resources<'_>,
    desc: &TargetDesc,
    scene: &Scene,
    quality: &Quality,
    painted: bool,
) -> Result<Drawn, RenderError> {
    // Integer supersampling only: the mock rounds `render_scale` up to a whole factor and ignores scales below 1.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // validated to 0.25..=4
    let scale = quality.render_scale.round().max(1.0) as u32;
    let factor = grid_for(quality.samples) * scale;
    let internal = desc.size * factor;
    if u64::from(internal.x) * u64::from(internal.y) > MAX_INTERNAL_PIXELS {
        return Err(RenderError::LimitExceeded { what: "internal render size in pixels", limit: MAX_INTERNAL_PIXELS });
    }

    #[allow(clippy::cast_precision_loss)] // image sizes are far below 2^24
    let aspect = desc.size.x as f32 / desc.size.y as f32;
    let view = scene.camera.view();
    let view_proj = scene.camera.projection_matrix(aspect) * view;
    let environment = scene
        .environment
        .map(|using| {
            live(res.environments, using.id.index(), using.id.generation(), "environment")
                .map(|data| EnvLight::new(data, &using))
        })
        .transpose()?;
    let background = match scene.background {
        omuzaki_render_api::Background::Solid { colour } => {
            (colour.to_linear() * colour.alpha()).extend(colour.alpha())
        }
        // Pictures shown as authored are laid under the resolved frame, so the scene renders over nothing.
        omuzaki_render_api::Background::Transparent
        | omuzaki_render_api::Background::Gradient { .. }
        | omuzaki_render_api::Background::Texture { .. } => Vec4::ZERO,
        omuzaki_render_api::Background::Environment => Vec4::W,
    };
    let backdrop = Backdrop::new(scene.background, res.textures)?;
    // Every batch names a live system and cells inside its atlas, checked before anything is drawn (D2).
    for batch in &scene.particles {
        let system = live(res.particle_systems, batch.system.index(), batch.system.generation(), "particle system")?;
        if !particles::cells_fit(&system.desc, batch.cells) {
            return Err(RenderError::invalid("spawn batch", "a listed cell is outside its system's atlas"));
        }
    }
    let mut surface = Surface::new(internal, background);
    if let (omuzaki_render_api::Background::Environment, Some(environment)) = (scene.background, &environment) {
        fill_backdrop(&mut surface, environment, scene.camera.sky_rays(aspect));
    }
    let lights = Lights::new(&scene.lights).with_environment(environment);
    let fog = FogLight::new(scene.fog);
    let textures = Textures(res.textures);
    let mut out = Drawn {
        image: Image { size: desc.size, alpha: desc.alpha, rgba8: Vec::new() },
        render_size: internal,
        draw_calls: 0,
        triangles: 0,
        particles: 0,
        ran: [false; Pass::COUNT],
    };
    out.ran[Pass::PaintTextures.index()] = painted;
    out.ran[Pass::Opaque.index()] = true;
    out.ran[Pass::Resolve.index()] = true;

    let mut blended: Vec<BlendTriangle<'_>> = Vec::new();
    let mut outlined: Vec<(&Instance, &MeshEntry, &Material, Posed)> = Vec::new();

    for inst in &scene.instances {
        let mesh = live(res.meshes, inst.mesh.index(), inst.mesh.generation(), "mesh")?;
        let material = live(res.materials, inst.material.index(), inst.material.generation(), "material")?;
        let posed = pose(mesh, inst, scene)?;
        out.draw_calls += 1;
        out.triangles += mesh.data.triangle_count() as u64;

        let blend = matches!(material.alpha, omuzaki_render_api::AlphaMode::Blend);
        let mode = Mode {
            cull: if material.double_sided { Cull::None } else { Cull::Back },
            mirrored: inst.transform.determinant() < 0.0,
            write: if blend { Write::Blend } else { Write::Opaque },
        };
        for &ids in mesh.data.indices.as_chunks::<3>().0 {
            let ids = ids.map(|i| i as usize);
            let world = ids.map(|i| posed.world[i]);
            let centroid = (world[0] + world[1] + world[2]) / 3.0;
            let facing = (world[1] - world[0]).cross(world[2] - world[0]).normalize_or_zero();
            if facing == Vec3::ZERO {
                continue;
            }
            // Light the side the viewer sees: that also handles double-sided and mirrored geometry.
            let to_viewer = towards_viewer(&scene.camera, centroid);
            let n = if facing.dot(to_viewer) < 0.0 { -facing } else { facing };
            let lit = light_triangle(material, &lights, n, centroid, to_viewer);
            let verts = ids.map(|i| vertex(mesh, i, posed.world[i], &view, &view_proj));
            if blend {
                let depth = -view.transform_point3(centroid).z;
                blended.push(BlendTriangle { order: inst.order, depth, verts, mode, material, lit });
            } else {
                draw_triangle(&mut surface, &verts, mode, &mut |v: Varying| {
                    shade_pixel(material, lit, &textures, v).map(|c| fog.apply(c, v.depth))
                });
            }
        }
        if material.outline.is_some() {
            outlined.push((inst, mesh, material, posed));
        }
    }

    let hull = HullCx { scene, lights: &lights, textures: &textures, fog, view, view_proj, aspect };
    for (inst, mesh, material, posed) in &outlined {
        out.ran[Pass::Outlines.index()] = true;
        out.draw_calls += 1;
        out.triangles += mesh.data.triangle_count() as u64;
        draw_hull(&mut surface, &hull, inst, mesh, material, posed);
    }

    // Particles test and write depth after the outlines and before the blended instances (D4).
    if !scene.particles.is_empty() {
        out.ran[Pass::Particles.index()] = true;
        out.draw_calls += 1;
        #[allow(clippy::cast_precision_loss)] // image sizes are far below 2^24
        let height = desc.size.y as f32;
        out.particles = particles::draw(
            &mut surface,
            scene,
            res.particle_systems,
            &lights,
            fog,
            &textures,
            quality.particle_density,
            grid_for(quality.samples),
            view_proj,
            height,
            aspect,
        );
    }

    // Back to front, authored order first; a stable sort keeps submission order for exact ties.
    blended.sort_by(|a, b| a.order.cmp(&b.order).then(b.depth.total_cmp(&a.depth)));
    out.ran[Pass::Transparent.index()] = !blended.is_empty();
    for tri in &blended {
        draw_triangle(&mut surface, &tri.verts, tri.mode, &mut |v: Varying| {
            shade_pixel(tri.material, tri.lit, &textures, v).map(|c| fog.apply(c, v.depth))
        });
    }

    out.image = resolve(&surface, desc, factor, &scene.post, backdrop.as_ref());
    Ok(out)
}

fn live<'a, T>(arena: &'a Arena<T>, index: u32, generation: u32, what: &'static str) -> Result<&'a T, RenderError> {
    arena.get(index, generation).ok_or(RenderError::UnknownHandle { what })
}

/// The direction from `point` toward the viewer. An orthographic camera sees every point along its own axis, as the GPU
/// renderer's `camera_back` does, so rims and facing do not shift with the surface's place in the view.
fn towards_viewer(camera: &Camera, point: Vec3) -> Vec3 {
    match camera.projection {
        Projection::Perspective { .. } => camera.position() - point,
        Projection::Orthographic { .. } => camera.transform.z_axis.truncate(),
    }
}

fn vertex(mesh: &MeshEntry, i: usize, world: Vec3, view: &Mat4, view_proj: &Mat4) -> Vertex {
    let d = &mesh.data;
    Vertex {
        clip: *view_proj * world.extend(1.0),
        uv: d.uvs.get(i).copied().unwrap_or(Vec2::ZERO),
        local: d.positions[i],
        colour: d.colours.get(i).map_or(Vec4::ONE, |&c| Vec4::from_array(c)),
        depth: -view.transform_point3(world).z,
    }
}

/// The vertices a pose starts from: the mesh's own, or the morphed ones, with the outline direction each moves along.
struct Rest {
    positions: Vec<Vec3>,
    hull_dir: Vec<Vec3>,
}

/// Morphs the mesh by the instance's weights (the base mesh if the instance names none). The outline directions are
/// welded again from the morphed positions and normals by the same function creation uses, so a morphed mesh outlines
/// exactly like a freshly created mesh of the blended geometry, hard-edge seams included.
fn morph_rest(mesh: &MeshEntry, inst: &Instance, weights: &[f32]) -> Result<Rest, RenderError> {
    let d = &mesh.data;
    let (Some(range), Some(set)) = (inst.morph, mesh.morphs.as_ref()) else {
        if inst.morph.is_some() {
            return Err(RenderError::invalid("instance", "it has morph weights but its mesh has no morph targets"));
        }
        return Ok(Rest { positions: d.positions.clone(), hull_dir: mesh.smooth_normals.clone() });
    };
    if range.len as usize != set.len() {
        let reason = format!("{} morph weights for a mesh with {} morph targets", range.len, set.len());
        return Err(RenderError::invalid("instance", reason));
    }
    let used = weights
        .get(range.start as usize..(range.start + range.len) as usize)
        .ok_or_else(|| RenderError::invalid("instance", "its morph range is past the scene's weights"))?;
    let (positions, normals) = morph_vertices(&d.positions, &d.normals, set, used)?;
    let hull_dir = weld_normals(&positions, &normals);
    Ok(Rest { positions, hull_dir })
}

/// Morphs, then skins (if the instance and mesh both are), then transforms every vertex to world space.
fn pose(mesh: &MeshEntry, inst: &Instance, scene: &Scene) -> Result<Posed, RenderError> {
    let d = &mesh.data;
    let rest = morph_rest(mesh, inst, &scene.morph_weights)?;
    let palette =
        inst.skin.filter(|_| d.is_skinned()).map(|r| &scene.joints[r.start as usize..(r.start + r.len) as usize]);
    let mut posed =
        Posed { world: Vec::with_capacity(d.positions.len()), hull_dir: Vec::with_capacity(d.positions.len()) };
    for (i, (&position, &normal)) in rest.positions.iter().zip(&rest.hull_dir).enumerate() {
        let model = match palette {
            None => inst.transform,
            Some(palette) => inst.transform * blend_joints(palette, d.joints[i], d.weights[i])?,
        };
        posed.world.push(model.transform_point3(position));
        posed.hull_dir.push(model.transform_vector3(normal).normalize_or_zero());
    }
    Ok(posed)
}

/// The weighted sum of up to four joint matrices. Joints with zero weight are not looked at, so their index may be
/// anything (exporters often leave 0 there).
fn blend_joints(palette: &[Mat4], joints: [u16; 4], weights: [f32; 4]) -> Result<Mat4, RenderError> {
    let mut sum = Mat4::ZERO;
    for (joint, weight) in joints.into_iter().zip(weights) {
        if weight == 0.0 {
            continue;
        }
        let matrix = palette.get(usize::from(joint)).ok_or_else(|| {
            RenderError::invalid(
                "skin",
                format!("a vertex uses joint {joint} but the instance has {} joints", palette.len()),
            )
        })?;
        sum += *matrix * weight;
    }
    Ok(sum)
}

/// What every hull of a frame is drawn with.
struct HullCx<'a> {
    scene: &'a Scene,
    lights: &'a Lights<'a>,
    textures: &'a Textures<'a>,
    fog: FogLight,
    view: Mat4,
    view_proj: Mat4,
    /// The target's width over its height.
    aspect: f32,
}

/// Where a hull vertex goes in clip space: the posed vertex `p` pushed along its outline direction `dir` by the
/// material's `width` (already multiplied by its width texture) as its `OutlineStyle` measures it (#647). `Screen` pushes
/// in clip space along the projected direction, normalised in pixels and scaled by clip w, as `outline.wgsl` does.
fn hull_clip(cx: &HullCx<'_>, outline: Outline, mode: OutlineWidthMode, p: Vec3, dir: Vec3) -> Vec4 {
    let metres = match mode {
        OutlineWidthMode::WorldByDistance => outline.world_width(-cx.view.transform_point3(p).z),
        OutlineWidthMode::World => outline.width,
        OutlineWidthMode::Screen => {
            let clip = cx.view_proj * p.extend(1.0);
            let projected = (cx.view_proj * dir.extend(0.0)).truncate().truncate();
            let in_pixels = Vec2::new(projected.x * cx.aspect, projected.y);
            let along = in_pixels.length();
            if along < 1e-6 {
                return clip;
            }
            let offset = Vec2::new(in_pixels.x / cx.aspect, in_pixels.y) / along * 2.0 * outline.width * clip.w;
            return clip + offset.extend(0.0).extend(0.0);
        }
    };
    cx.view_proj * (p + dir * metres).extend(1.0)
}

/// Inverted hull: the mesh pushed out along its smooth normals, back faces only, in the outline colour, tinted toward
/// the lit surface by the style's lighting mix.
fn draw_hull(
    surface: &mut Surface,
    cx: &HullCx<'_>,
    inst: &Instance,
    mesh: &MeshEntry,
    material: &Material,
    posed: &Posed,
) {
    let Some(outline) = material.outline else { return };
    let style = material.outline_style;
    let uv = |i: usize| mesh.data.uvs.get(i).copied().unwrap_or(Vec2::ZERO);
    let clips: Vec<Vec4> = (0..posed.world.len())
        .map(|i| {
            let multiplier =
                style.width_texture.map_or(1.0, |id| cx.textures.sample(id, material.uv_transform.apply(uv(i))).y);
            let outline = Outline { width: outline.width * multiplier, ..outline };
            hull_clip(cx, outline, style.width_mode, posed.world[i], posed.hull_dir[i])
        })
        .collect();
    let mirrored = inst.transform.determinant() < 0.0;
    let mode = Mode { cull: Cull::Front, mirrored, write: Write::Opaque };
    let line = outline.colour.to_linear();
    for &ids in mesh.data.indices.as_chunks::<3>().0 {
        let verts = ids.map(|i| {
            let i = i as usize;
            // The fog reads the pushed vertex's depth, except a screen-space push, which leaves the depth alone.
            let mut v = vertex(mesh, i, posed.world[i], &cx.view, &cx.view_proj);
            v.clip = clips[i];
            v
        });
        // The surface's light for the lighting mix: the triangle lit from outside, as the surface it grows from.
        let lit = (style.lighting_mix > 0.0).then(|| {
            let world = ids.map(|i| posed.world[i as usize]);
            let facing = (world[1] - world[0]).cross(world[2] - world[0]).normalize_or_zero();
            let outward = if mirrored { -facing } else { facing };
            let centroid = (world[0] + world[1] + world[2]) / 3.0;
            light_triangle(material, cx.lights, outward, centroid, towards_viewer(&cx.scene.camera, centroid))
        });
        // A masked material's hull is cut like its surface: the line follows what is visible (#649).
        draw_triangle(surface, &verts, mode, &mut |v: Varying| {
            if !mask_keeps(material, cx.textures, v) {
                return None;
            }
            let base = Vec3::splat(cx.scene.outline_light);
            let light = match lit {
                Some(lit) => {
                    let surface = shade_pixel(material, lit, cx.textures, v).map_or(base, Vec4::truncate);
                    base.lerp(surface, style.lighting_mix)
                }
                None => base,
            };
            Some(cx.fog.apply((line * light).extend(1.0), v.depth))
        });
    }
}

/// A picture shown as authored behind the frame (`Background::Gradient` and `Background::Texture`): what it shows at
/// each output pixel, as premultiplied sRGB-encoded colour.
struct Backdrop<'a> {
    top: Colour,
    bottom: Colour,
    texture: Option<(&'a TextureData, BackdropFit)>,
}

impl<'a> Backdrop<'a> {
    /// The backdrop of `background`, if it is a picture.
    ///
    /// # Errors
    /// [`RenderError::UnknownHandle`] for a texture that was released.
    fn new(background: Background, textures: &'a Arena<TextureData>) -> Result<Option<Self>, RenderError> {
        Ok(match background {
            Background::Gradient { top, bottom } => Some(Self { top, bottom, texture: None }),
            Background::Texture { texture, fit, top, bottom } => {
                let data = live(textures, texture.index(), texture.generation(), "texture")?;
                Some(Self { top, bottom, texture: Some((data, fit)) })
            }
            _ => None,
        })
    }

    /// The colour at the centre `at` of an output pixel (`0..1` across a frame of aspect `frame`).
    fn at(&self, at: Vec2, frame: f32) -> Vec4 {
        let behind = gradient_at(self.top, self.bottom, at.y);
        let Some((texture, fit)) = self.texture else { return behind };
        let size = texture.desc.size.as_vec2();
        let uv = fit.uv(at, frame, size.x / size.y);
        if !(0.0..=1.0).contains(&uv.x) || !(0.0..=1.0).contains(&uv.y) {
            return behind;
        }
        let picture = texel_bilinear(texture, uv * size - 0.5);
        picture + behind * (1.0 - picture.w)
    }
}

/// The texture's colour at texel coordinates `at` (texel centres at whole numbers), filtered bilinearly with the edges
/// clamped, as premultiplied sRGB-encoded colour.
fn texel_bilinear(texture: &TextureData, at: Vec2) -> Vec4 {
    let size = texture.desc.size;
    let texel = |column: f32, row: f32| {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
        // clamped into the texture first; textures are far smaller than 2^24 texels a side
        let (column, row) =
            (column.clamp(0.0, (size.x - 1) as f32) as usize, row.clamp(0.0, (size.y - 1) as f32) as usize);
        let start = (row * size.x as usize + column) * 4;
        let [red, green, blue, alpha] = [0, 1, 2, 3].map(|i| f32::from(texture.rgba[start + i]) / 255.0);
        let mut rgb = Vec3::new(red, green, blue);
        if texture.desc.alpha == AlphaEncoding::Premultiplied && alpha > 0.0 {
            rgb = (rgb / alpha).min(Vec3::ONE);
        }
        if texture.desc.colour_space == ColourSpace::Linear {
            rgb = Vec3::new(srgb_encode(rgb.x), srgb_encode(rgb.y), srgb_encode(rgb.z));
        }
        (rgb * alpha).extend(alpha)
    };
    let (left, top) = (at.x.floor(), at.y.floor());
    let (along, below) = (at.x - left, at.y - top);
    let upper = texel(left, top).lerp(texel(left + 1.0, top), along);
    let lower = texel(left, top + 1.0).lerp(texel(left + 1.0, top + 1.0), along);
    upper.lerp(lower, below)
}

/// Averages the supersamples, applies exposure and tone mapping, encodes to sRGB and lays the result over `backdrop`
/// (in encoded values, as a compositor would), then writes bytes in the target's alpha encoding.
fn resolve(surface: &Surface, desc: &TargetDesc, factor: u32, post: &Post, backdrop: Option<&Backdrop<'_>>) -> Image {
    let (w, h) = (desc.size.x as usize, desc.size.y as usize);
    let f = factor as usize;
    #[allow(clippy::cast_precision_loss)] // factor is at most 16
    let inv_samples = 1.0 / (f * f) as f32;
    let mut rgba8 = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let mut sum = Vec4::ZERO;
            for sy in 0..f {
                for sx in 0..f {
                    sum += surface.colour[(y * f + sy) * w * f + x * f + sx];
                }
            }
            let premultiplied = sum * inv_samples;
            let alpha = premultiplied.w;
            let straight = if alpha > 0.0 { premultiplied.truncate() / alpha } else { Vec3::ZERO };
            let display = tonemap(straight * post.exposure, post.tonemap);
            let encoded = Vec3::new(srgb_encode(display.x), srgb_encode(display.y), srgb_encode(display.z));
            let (stored, alpha) = match backdrop {
                None if desc.alpha == AlphaEncoding::Premultiplied => (encoded * alpha, alpha),
                None => (encoded, alpha),
                Some(backdrop) => {
                    #[allow(clippy::cast_precision_loss)] // image sizes are far below 2^24
                    let (at, frame) =
                        (Vec2::new((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32), w as f32 / h as f32);
                    let over = (encoded * alpha).extend(alpha) + backdrop.at(at, frame) * (1.0 - alpha);
                    let straight = if over.w > 0.0 { over.truncate() / over.w } else { Vec3::ZERO };
                    let premultiplied = desc.alpha == AlphaEncoding::Premultiplied;
                    (if premultiplied { over.truncate() } else { straight }, over.w)
                }
            };
            rgba8.extend([to_byte(stored.x), to_byte(stored.y), to_byte(stored.z), to_byte(alpha)]);
        }
    }
    Image { size: desc.size, alpha: desc.alpha, rgba8 }
}

fn tonemap(c: Vec3, mode: Tonemap) -> Vec3 {
    match mode {
        Tonemap::None => c.clamp(Vec3::ZERO, Vec3::ONE),
        Tonemap::Aces => aces(c),
    }
}

/// three.js's `ACESFilmicToneMapping`, the curve the GPU renderer and the prototype use (#630): the `1 / 0.6`
/// pre-scale, `ACESInputMat`, the RRT and ODT fit, `ACESOutputMat`, clamped. `c` already carries the exposure.
fn aces(c: Vec3) -> Vec3 {
    // three.js declares the matrices by column; these are its columns, so `INPUT * v` is its `ACESInputMat * v`.
    const INPUT: Mat3 = Mat3::from_cols(
        Vec3::new(0.59719, 0.07600, 0.02840),
        Vec3::new(0.35458, 0.90834, 0.13383),
        Vec3::new(0.04823, 0.01566, 0.83777),
    );
    const OUTPUT: Mat3 = Mat3::from_cols(
        Vec3::new(1.60475, -0.10208, -0.00327),
        Vec3::new(-0.53108, 1.10813, -0.07276),
        Vec3::new(-0.07367, -0.00605, 1.07602),
    );
    let v = INPUT * (c / 0.6);
    let a = v * (v + 0.024_578_6) - 0.000_090_537;
    let b = v * (0.983_729 * v + 0.432_951) + 0.238_081;
    (OUTPUT * (a / b)).clamp(Vec3::ZERO, Vec3::ONE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two coincident vertices with hard-edge normals X and Y (a seam), and a target that turns them to Y and -X.
    fn seam() -> (MeshEntry, MeshEntry, omuzaki_render_api::MorphSet) {
        use omuzaki_core::id::Slug;
        use omuzaki_render_api::{MeshData, MeshUsage, MorphSet};
        let base = MeshData {
            positions: vec![Vec3::ZERO; 2],
            normals: vec![Vec3::X, Vec3::Y],
            indices: vec![0, 1, 0],
            ..MeshData::default()
        };
        let deltas = [Vec3::ZERO; 2];
        let normal_deltas = [Vec3::new(-1.0, 1.0, 0.0), Vec3::new(-1.0, -1.0, 0.0)];
        let slug = Slug::new("turn").unwrap();
        let set = MorphSet::from_dense(2, &[(slug, &deltas, &normal_deltas)], 0.0).unwrap();
        let entry = MeshEntry::new(base.clone(), MeshUsage::Static, Some(set.clone()));
        (entry, MeshEntry::new(base, MeshUsage::Static, None), set)
    }

    #[test]
    fn a_morphed_seam_outlines_like_a_freshly_created_mesh() {
        let (morphed, _, set) = seam();
        let mut inst = Instance::new(
            omuzaki_render_api::MeshId::from_parts(0, 1),
            omuzaki_render_api::MaterialId::from_parts(0, 1),
            Mat4::IDENTITY,
        );
        inst.morph = Some(omuzaki_render_api::MorphRange { start: 0, len: 1 });
        for weight in [0.0, 0.5, 1.0] {
            let rest = morph_rest(&morphed, &inst, &[weight]).unwrap();
            let (p, n) = morph_vertices(&morphed.data.positions, &morphed.data.normals, &set, &[weight]).unwrap();
            let fresh = weld_normals(&p, &n);
            assert_eq!(rest.hull_dir, fresh, "weight {weight}");
            assert_eq!(rest.hull_dir[0], rest.hull_dir[1], "welded vertices share one outline direction");
        }
    }

    #[test]
    fn sample_counts_map_to_square_grids() {
        let grids: Vec<u32> = [1u8, 2, 4, 8, 16].into_iter().map(grid_for).collect();
        assert_eq!(grids, vec![1, 2, 2, 3, 4]);
    }

    #[test]
    fn aces_maps_black_to_black_and_compresses_highlights() {
        assert_eq!(tonemap(Vec3::ZERO, Tonemap::Aces), Vec3::ZERO);
        let bright = tonemap(Vec3::splat(8.0), Tonemap::Aces);
        assert!(bright.x > 0.95 && bright.x <= 1.0);
        assert_eq!(tonemap(Vec3::splat(2.0), Tonemap::None), Vec3::ONE);
    }

    /// three.js's own values (its shader chunk evaluated outside a renderer; the same table pins the GPU renderer in
    /// `omuzaki-render`'s `render.rs`): `(sRGB colour, exposure, levels per channel)`.
    #[test]
    fn aces_matches_three_js_within_a_level() {
        const THREE_JS: &[([u8; 3], f32, [f32; 3])] = &[
            ([128, 128, 128], 1.0, [140.7; 3]),
            ([128, 128, 128], 2.0, [188.4; 3]),
            ([201, 184, 240], 1.0, [205.2, 195.3, 220.8]),
            ([255, 255, 255], 1.0, [226.4; 3]),
            ([60, 200, 120], 1.0, [108.7, 204.7, 142.4]),
            ([60, 200, 120], 2.0, [173.7, 231.4, 190.8]),
            ([240, 120, 40], 1.0, [234.0, 140.3, 50.0]),
            // Pure cyan, where the old per-channel fit kept red at 0 and missed by 204 levels.
            ([0, 255, 255], 1.0, [153.4, 227.4, 225.7]),
            ([0, 255, 255], 2.0, [204.2, 242.7, 241.6]),
        ];
        for &(rgb, exposure, want) in THREE_JS {
            let linear = Vec3::from(rgb.map(|c| omuzaki_core::colour::srgb_decode(f32::from(c) / 255.0)));
            let display = tonemap(linear * exposure, Tonemap::Aces);
            let got = [display.x, display.y, display.z].map(|c| srgb_encode(c) * 255.0);
            for (g, w) in got.iter().zip(want) {
                assert!((g - w).abs() <= 1.0, "{rgb:?} at {exposure}: {got:?} vs three.js {want:?}");
            }
        }
    }
}
