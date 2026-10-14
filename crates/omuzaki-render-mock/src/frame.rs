//! One frame, start to finish: pose instances, draw the passes in contract order, resolve to bytes.

use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3, Vec4};
use omuzaki_render_api::{
    AlphaEncoding, Camera, Image, Instance, Material, Pass, Post, Projection, Quality, RenderError, Scene, TargetDesc,
    Tonemap, srgb_encode, to_byte,
};

use crate::TextureData;
use crate::arena::Arena;
use crate::environment::{EnvLight, EnvironmentData};
use crate::morph::morph_vertices;
use crate::raster::{Cull, Mode, Surface, Varying, Vertex, Write, draw_triangle};
use crate::resources::{MeshEntry, weld_normals};
use crate::shade::{FogLight, Lights, Lit, Textures, light_triangle, shade_pixel};

/// The internal buffer may hold at most this many pixels (a 4096x4096 frame): the mock keeps 20 bytes per pixel.
const MAX_INTERNAL_PIXELS: u64 = 1 << 24;

/// The renderer's resource tables, borrowed for the duration of a frame.
pub(crate) struct Resources<'a> {
    pub(crate) meshes: &'a Arena<MeshEntry>,
    pub(crate) textures: &'a Arena<TextureData>,
    pub(crate) materials: &'a Arena<Material>,
    pub(crate) environments: &'a Arena<EnvironmentData>,
}

/// What a frame produced.
pub(crate) struct Drawn {
    pub(crate) image: Image,
    pub(crate) render_size: UVec2,
    pub(crate) draw_calls: u32,
    pub(crate) triangles: u64,
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

/// Draws `scene` for a target of `desc`.
///
/// `painted` says a painter ran since the last frame, so the `PaintTextures` pass is reported.
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
    let lights = Lights::new(&scene.lights).with_environment(environment);
    let fog = FogLight::new(scene.fog);
    let textures = Textures(res.textures);

    let background = match scene.background {
        omuzaki_render_api::Background::Solid { colour } => {
            (colour.to_linear() * colour.alpha()).extend(colour.alpha())
        }
        omuzaki_render_api::Background::Transparent => Vec4::ZERO,
    };
    let mut surface = Surface::new(internal, background);
    let mut out = Drawn {
        image: Image { size: desc.size, alpha: desc.alpha, rgba8: Vec::new() },
        render_size: internal,
        draw_calls: 0,
        triangles: 0,
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

    for (inst, mesh, material, posed) in &outlined {
        out.ran[Pass::Outlines.index()] = true;
        out.draw_calls += 1;
        out.triangles += mesh.data.triangle_count() as u64;
        draw_hull(&mut surface, inst, mesh, material, posed, scene, fog, &view, &view_proj);
    }

    // Back to front, authored order first; a stable sort keeps submission order for exact ties.
    blended.sort_by(|a, b| a.order.cmp(&b.order).then(b.depth.total_cmp(&a.depth)));
    out.ran[Pass::Transparent.index()] = !blended.is_empty();
    for tri in &blended {
        draw_triangle(&mut surface, &tri.verts, tri.mode, &mut |v: Varying| {
            shade_pixel(tri.material, tri.lit, &textures, v).map(|c| fog.apply(c, v.depth))
        });
    }

    out.image = resolve(&surface, desc, factor, &scene.post);
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

/// Inverted hull: the mesh pushed out along its smooth normals, back faces only, in the outline colour.
#[allow(clippy::too_many_arguments)] // one cohesive draw: splitting it would only move the arguments around
fn draw_hull(
    surface: &mut Surface,
    inst: &Instance,
    mesh: &MeshEntry,
    material: &Material,
    posed: &Posed,
    scene: &Scene,
    fog: FogLight,
    view: &Mat4,
    view_proj: &Mat4,
) {
    let Some(outline) = material.outline else { return };
    let colour = (outline.colour.to_linear() * scene.outline_light).extend(1.0);
    let pushed: Vec<Vec3> = posed
        .world
        .iter()
        .zip(&posed.hull_dir)
        .map(|(&p, &dir)| p + dir * outline.world_width(-view.transform_point3(p).z))
        .collect();
    let mode = Mode { cull: Cull::Front, mirrored: inst.transform.determinant() < 0.0, write: Write::Opaque };
    for &ids in mesh.data.indices.as_chunks::<3>().0 {
        let verts = ids.map(|i| vertex(mesh, i as usize, pushed[i as usize], view, view_proj));
        draw_triangle(surface, &verts, mode, &mut |v: Varying| Some(fog.apply(colour, v.depth)));
    }
}

/// Averages the supersamples, applies exposure and tone mapping, and encodes to sRGB bytes.
fn resolve(surface: &Surface, desc: &TargetDesc, factor: u32, post: &Post) -> Image {
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
            let stored = if desc.alpha == AlphaEncoding::Premultiplied { encoded * alpha } else { encoded };
            rgba8.extend([to_byte(stored.x), to_byte(stored.y), to_byte(stored.z), to_byte(alpha)]);
        }
    }
    Image { size: desc.size, alpha: desc.alpha, rgba8 }
}

fn tonemap(c: Vec3, mode: Tonemap) -> Vec3 {
    match mode {
        Tonemap::None => c.clamp(Vec3::ZERO, Vec3::ONE),
        // Narkowicz's fit of the ACES filmic curve; the GPU renderer may use the full ACES transform, so goldens
        // compare the two with a tolerance.
        Tonemap::Aces => {
            let fit = |x: f32| (x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14);
            Vec3::new(fit(c.x), fit(c.y), fit(c.z)).clamp(Vec3::ZERO, Vec3::ONE)
        }
    }
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
}
