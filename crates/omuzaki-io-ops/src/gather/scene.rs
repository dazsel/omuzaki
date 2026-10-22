//! The scene half of the gather: nodes in parents-first order, one skinned primitive per built mesh, materials,
//! textures and the uniform scale to the target height.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use omuzaki_char_api::{
    BuiltCharacter, BuiltMesh, CharacterRig, MaterialClass, MaterialName, MaterialSpec, PartMesh, TextureName,
};
use omuzaki_core::glam::Vec3;
use omuzaki_io_api::{self as io, Finding, IoScene, MaterialIx, Mesh, Node, Primitive, Report, Skin, SkinIx};
use omuzaki_render_api as render;

use super::{GatherError, GatherOptions, GatherPurpose, Gathered, skeleton};
use crate::convert::material::{SceneTexture, export_material};
use crate::convert::texture::{encode_png, sampler_to_io};

/// Hands out names no node has: a taken name gets `.2`, `.3`, ... after it.
struct Names(HashSet<String>);

impl Names {
    fn unique(&mut self, wanted: &str) -> String {
        let mut name = wanted.to_owned();
        let mut n = 2;
        while !self.0.insert(name.clone()) {
            name = format!("{wanted}.{n}");
            n += 1;
        }
        name
    }
}

pub(super) fn gather(built: &BuiltCharacter, options: &GatherOptions) -> Result<Gathered, GatherError> {
    let rig = &built.rig;
    if options.purpose == GatherPurpose::Vrm {
        rig.skeleton.require_humanoid()?;
    }
    let mut report = Report::new();
    let meshes = drawable_meshes(built, &mut report);
    let scale = scale_factor(options.target_height, &meshes, &mut report)?;

    let mut scene = IoScene::new();
    for node in skeleton::bone_nodes(&rig.skeleton, scale)? {
        scene.add_node(node);
    }
    scene.roots = skeleton::root_nodes(&rig.skeleton);
    let materials = add_materials(&mut scene, built, &meshes, &mut report)?;
    let skin = meshes.iter().any(|m| m.mesh.geometry.is_skinned()).then(|| add_skin(&mut scene, built, scale));

    let mut names = Names(scene.nodes.iter().map(|n| n.name.clone()).collect());
    for (i, built_mesh) in meshes.iter().enumerate() {
        let part_mesh = &built_mesh.mesh;
        let name = names.unique(&format!("{}.{}", built_mesh.part, part_mesh.name));
        let material = *materials.get(&part_mesh.material).ok_or_else(|| GatherError::UnknownMaterial {
            mesh: part_mesh.name.clone(),
            material: part_mesh.material.to_string(),
        })?;
        let primitive = primitive_of(rig, part_mesh, material, scale)?;
        let skinned = primitive.mesh.is_skinned();
        if part_mesh.dynamic && !skinned {
            let msg = format!("mesh '{name}' is simulated at runtime; it is exported at its rest shape");
            let hint = "build with rig_export to export hair and cloth as skinned meshes on spring bones";
            report.push(Finding::info("gather.dynamic_at_rest", msg).at(format!("meshes/{i}")).hint(hint));
        }
        let mut mesh = Mesh::new(name.clone());
        mesh.primitives.push(primitive);
        let mut node = Node::new(name);
        node.mesh = Some(scene.add_mesh(mesh));
        node.skin = skin.filter(|_| skinned);
        let ix = scene.add_node(node);
        scene.roots.push(ix);
    }
    Ok(Gathered { scene, report, scale })
}

/// The character's one skin: joint `i` is bone `i`, so every mesh's own joint list is remapped onto it. The `skeleton`
/// hint names the root only when there is exactly one, since glTF wants the closest common root of the joints.
fn add_skin(scene: &mut IoScene, built: &BuiltCharacter, scale: f32) -> SkinIx {
    let skeleton = &built.rig.skeleton;
    let joints = skeleton::joint_nodes(skeleton);
    let mut skin = Skin::new(built.id.as_str(), joints, skeleton::inverse_bind(&built.rig, scale));
    if let [root] = scene.roots.as_slice() {
        skin.skeleton = Some(*root);
    }
    scene.add_skin(skin)
}

/// The primitive for `mesh`: its geometry scaled, and if skinned its joints moved onto skeleton indices.
fn primitive_of(
    rig: &CharacterRig,
    mesh: &PartMesh,
    material: MaterialIx,
    scale: f32,
) -> Result<Primitive, GatherError> {
    let mut geometry = mesh.geometry.clone();
    geometry.positions.iter_mut().for_each(|p| *p *= scale);
    if geometry.is_skinned() {
        skeleton::remap_joints(rig, mesh, &mut geometry)?;
    }
    let mut primitive = Primitive::new(geometry);
    primitive.material = Some(material);
    Ok(primitive)
}

/// The meshes that become primitives: every one with a triangle. A mesh without any (a hair style with no strands) has
/// nothing to draw and would fail the scene check, so it's left out and said so.
fn drawable_meshes<'a>(built: &'a BuiltCharacter, report: &mut Report) -> Vec<&'a BuiltMesh> {
    let (drawable, empty): (Vec<_>, Vec<_>) = built.meshes.iter().partition(|m| m.mesh.geometry.triangle_count() > 0);
    for m in empty {
        let msg = format!("mesh '{}' of part '{}' has no triangles, so it was left out", m.mesh.name, m.part);
        report.push(Finding::info("gather.mesh_empty", msg).at(format!("parts/{}", m.part)));
    }
    drawable
}

/// The factor that makes the meshes `target` metres tall, or 1 with no target. Height is the extent along Y of every
/// vertex gathered: feet to the top of the hair, which is what a viewer measures.
fn scale_factor(target: Option<f32>, meshes: &[&BuiltMesh], report: &mut Report) -> Result<f32, GatherError> {
    let Some(target) = target else { return Ok(1.0) };
    if !target.is_finite() || target <= 0.0 {
        return Err(GatherError::BadHeight(target));
    }
    let heights = meshes.iter().flat_map(|m| m.mesh.geometry.positions.iter().map(|p| p.y));
    let (lo, hi) = heights.fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), y| (lo.min(y), hi.max(y)));
    let height = hi - lo;
    if !height.is_finite() || height <= 0.0 {
        return Err(GatherError::NoHeight(target));
    }
    let scale = target / height;
    let msg = format!("scaled by {scale:.4}, from {height:.3} m to {target:.3} m");
    report.push(Finding::info("gather.scaled", msg).at("scene"));
    Ok(scale)
}

/// Adds the materials the meshes use, and the textures those need, in name order; returns each material's index.
fn add_materials(
    scene: &mut IoScene,
    built: &BuiltCharacter,
    meshes: &[&BuiltMesh],
    report: &mut Report,
) -> Result<BTreeMap<MaterialName, MaterialIx>, GatherError> {
    let used: BTreeSet<&MaterialName> = meshes.iter().map(|m| &m.mesh.material).collect();
    let used_textures: BTreeSet<&TextureName> =
        used.iter().filter_map(|name| built.materials.get(*name)?.base_texture.as_ref()).collect();

    let mut textures: BTreeMap<&TextureName, SceneTexture> = BTreeMap::new();
    for name in used_textures {
        // A texture the character lacks stays out of the map, and `export_material` names the material that asked.
        let Some(data) = built.textures.get(name) else { continue };
        let image =
            scene.add_image(encode_png(name.as_str(), data, &format!("images/{}", scene.images.len()), report)?);
        let texture = scene.add_texture(io::Texture { image, sampler: sampler_to_io(&data.desc) });
        textures.insert(name, SceneTexture { tex: io::TexRef::of(texture), colour_space: data.desc.colour_space });
    }

    let mut indices = BTreeMap::new();
    for name in used {
        let Some(spec) = built.materials.get(name) else { continue };
        let at = format!("materials/{}", scene.materials.len());
        let mut material =
            export_material(name.as_str(), spec, &|texture| textures.get(texture).copied(), &at, report)?;
        draw_program_in_look(built, name, spec, &mut material);
        indices.insert(name.clone(), scene.add_material(material));
    }
    Ok(indices)
}

/// A shading program (the kit's hair) exports as its toon approximation, which the look's softness and rim scales
/// never reached: the look draws a program by its parameters. Scales the exported terminator and rim as the look scales
/// a toon material's, so the file carries the look on every material (#713).
fn draw_program_in_look(built: &BuiltCharacter, name: &MaterialName, spec: &MaterialSpec, material: &mut io::Material) {
    let (render::Shading::Program { .. }, io::Shading::Toon(toon)) = (&spec.shading, &mut material.shading) else {
        return;
    };
    let class = built.material_classes.get(name).copied().unwrap_or(MaterialClass::Cloth);
    let look = built.look().class(class);
    if let Some(scale) = look.softness_scale {
        toon.shading_toony = 1.0 - ((1.0 - toon.shading_toony) * scale).clamp(0.0, 1.0);
    }
    if let Some(scale) = look.rim_scale {
        toon.rim_color = (toon.rim_color * scale.max(0.0)).min(Vec3::ONE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_taken_name_gets_a_counter() {
        let mut names = Names(["body".to_owned()].into());
        assert_eq!(names.unique("hair"), "hair");
        assert_eq!(names.unique("hair"), "hair.2");
        assert_eq!(names.unique("hair"), "hair.3");
        assert_eq!(names.unique("body"), "body.2");
    }

    #[test]
    fn scaling_needs_a_positive_target() {
        for bad in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(matches!(scale_factor(Some(bad), &[], &mut Report::new()), Err(GatherError::BadHeight(_))));
        }
        assert!(matches!(scale_factor(Some(1.5), &[], &mut Report::new()), Err(GatherError::NoHeight(_))));
        assert_eq!(scale_factor(None, &[], &mut Report::new()), Ok(1.0));
    }
}
