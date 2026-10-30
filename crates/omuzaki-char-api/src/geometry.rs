//! What a part produces for the renderer: meshes, materials and procedural textures (spec §9, §21).
//!
//! The mesh type is `omuzaki_core::mesh::MeshData`, shared with the renderer and the I/O contract. Geometry is authored in
//! **bind space**: metres, +Y up, the character faces +Z, the character's left is +X, in the template's bind pose.
//!
//! **Materials are the renderer's, with names instead of handles.** [`MaterialSpec`] holds exactly what
//! `omuzaki_render_api::Material` holds, using the renderer's own parameter types ([`Shading`], [`Outline`],
//! [`AlphaMode`]), except that textures and face layers are *named* (a handle exists only once the stage has uploaded
//! something). [`MaterialSpec::to_material`] is the one conversion, so the stage writes no per-part glue. Colours are
//! resolved through the palette when the part builds (`BuildCtx::colour`), and again when only the palette changes
//! (`PartKind::recolour`, which re-resolves them without rebuilding the geometry).

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec3;
use omuzaki_core::mesh::{MeshData, MeshError};
use omuzaki_face_api::{FaceLayerId, FaceLayerSpec, FaceProjection, PlanarWindow};
use omuzaki_render_api::{
    AlphaEncoding, AlphaMode, ColourSpace, HairShadow, Mapping, Material, Outline, OutlineStyle, Overlay, Sampler,
    Shading, TextureDesc, TextureId, TextureUsage, UvTransform,
};

use crate::ids::{JointRef, MaterialName, TextureName};

/// A face layer composited over a material's base colour before lighting (the renderer's `Overlay`, by layer name).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceOverlay {
    /// Which painted layer. The stage maps it to the dynamic texture it created for that layer.
    pub layer: FaceLayerId,
    /// The planar mapping, in the mesh's local (bind) space.
    pub window: PlanarWindow,
    /// Overall opacity, `0..=1`.
    pub strength: f32,
}

impl FaceOverlay {
    /// The overlay for `layer`, given where the face window sits and where the head centre is in bind space.
    pub fn of(projection: &FaceProjection, head_centre: Vec3, layer: FaceLayerId) -> Self {
        Self { layer, window: projection.planar(layer, head_centre), strength: layer.composite().strength }
    }

    /// The renderer's overlay of this layer, once the stage has its texture.
    pub fn to_overlay(&self, texture: TextureId) -> Overlay {
        let w = &self.window;
        Overlay {
            texture,
            mapping: Mapping::Planar { min: w.min, max: w.max, fade_from: w.fade_from, fade_to: w.fade_to },
            strength: self.strength,
        }
    }
}

/// Everything about how one mesh's surface looks (the renderer's `Material`, with named textures).
#[derive(Clone, Debug, PartialEq)]
pub struct MaterialSpec {
    /// The surface colour (its alpha multiplies the texture's).
    pub base: Colour,
    /// A procedural texture multiplied into `base`, read with the mesh's UVs.
    pub base_texture: Option<TextureName>,
    /// Lighting model: toon, unlit or a named program (hair).
    pub shading: Shading,
    /// Face layers composited over the base, in order: the head carries `Base`, the hair carries `Brows` when its
    /// shading says brows show through, and both carry `Overlay`, which is in front of everything.
    pub overlays: Vec<FaceOverlay>,
    /// How alpha is used.
    pub alpha: AlphaMode,
    /// Draw both sides of every triangle (hair sheets, cloth).
    pub double_sided: bool,
    /// Multiply the mesh's vertex colours into `base`.
    pub vertex_colours: bool,
    /// The outline drawn around meshes using this material.
    pub outline: Option<Outline>,
    /// Whether shadow maps darken this surface. Faces turn it off.
    pub receives_shadows: bool,
    /// Whether the surface casts the hair shadow (hair) or receives it (a face): the renderer's `Material::hair_shadow`.
    pub hair_shadow: HairShadow,
    /// Scale and offset applied to the UV of every UV-addressed texture of the material (the renderer's
    /// `Material::uv_transform`). Identity for every built part; an imported material keeps its file's texture
    /// transform here, and an expression's UV bind moves it (`byom.md` D3).
    pub uv_transform: UvTransform,
}

/// A name the stage could not turn into a handle.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MaterialError {
    /// The material names a texture the stage didn't upload.
    #[error("the material uses texture '{0}', which wasn't uploaded")]
    MissingTexture(TextureName),
    /// The material composites a face layer the stage has no texture for.
    #[error("the material composites the {0:?} face layer, which has no texture")]
    MissingLayer(FaceLayerId),
}

impl MaterialSpec {
    /// An opaque toon material of `base` with the renderer's default shading and no overlays or outline.
    pub fn toon(base: Colour) -> Self {
        let m = Material::toon(base);
        Self {
            base,
            base_texture: None,
            shading: m.shading,
            overlays: Vec::new(),
            alpha: m.alpha,
            double_sided: m.double_sided,
            vertex_colours: m.vertex_colours,
            outline: m.outline,
            receives_shadows: m.receives_shadows,
            hair_shadow: m.hair_shadow,
            uv_transform: m.uv_transform,
        }
    }

    /// The renderer's own material rules ([`Material::validate`]: ranges, finite numbers, counts) on this spec, so a
    /// spec that would be refused at upload is caught while it is built. Texture handles are not part of those rules,
    /// so placeholders stand in for them.
    ///
    /// # Errors
    /// [`omuzaki_render_api::RenderError`] naming the first broken rule.
    pub fn validate(&self) -> Result<(), omuzaki_render_api::RenderError> {
        let placeholder = TextureId::from_parts(0, 0);
        self.to_material(&|_| Some(placeholder), &|_| Some(placeholder))
            .map_err(|e| omuzaki_render_api::RenderError::invalid("material", e.to_string()))?
            .validate()
    }

    /// The renderer's material, given the stage's handles: `textures` for named procedural textures, `layers` for the
    /// face layer textures.
    ///
    /// # Errors
    /// [`MaterialError`] naming the first texture or layer without a handle.
    pub fn to_material(
        &self,
        textures: &dyn Fn(&TextureName) -> Option<TextureId>,
        layers: &dyn Fn(FaceLayerId) -> Option<TextureId>,
    ) -> Result<Material, MaterialError> {
        let base_texture = match &self.base_texture {
            Some(name) => Some(textures(name).ok_or_else(|| MaterialError::MissingTexture(name.clone()))?),
            None => None,
        };
        let overlays = self
            .overlays
            .iter()
            .map(|o| layers(o.layer).map(|t| o.to_overlay(t)).ok_or(MaterialError::MissingLayer(o.layer)))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Material {
            base: self.base,
            base_texture,
            shading: self.shading.clone(),
            overlays,
            alpha: self.alpha,
            order_independent: false,
            double_sided: self.double_sided,
            vertex_colours: self.vertex_colours,
            outline: self.outline,
            outline_style: OutlineStyle::KIT,
            receives_shadows: self.receives_shadows,
            hair_shadow: self.hair_shadow,
            uv_transform: self.uv_transform,
            mtoon: None,
        })
    }
}

/// One drawable mesh of a part.
#[derive(Clone, Debug, PartialEq)]
pub struct PartMesh {
    /// A readable name for tools and reports (`hair-curtain`).
    pub name: String,
    /// Geometry. If skinned, its `joints` index `joint_bones`.
    pub geometry: MeshData,
    /// What each joint index of `geometry` means, so the mesh doesn't depend on the order of any skeleton's bone
    /// array. The stage maps these to the instance's joint run. Empty for rigid and dynamic meshes.
    pub joint_bones: Vec<JointRef>,
    /// The material, by name (`BuiltCharacter::materials`).
    pub material: MaterialName,
    /// Whether it casts shadows (the renderer's `Instance.casts_shadow`).
    pub casts_shadow: bool,
    /// Draw order within the character; higher draws later.
    pub render_order: i32,
    /// True if a [`PartRuntime`](crate::PartRuntime) rewrites this mesh's positions and normals every step.
    pub dynamic: bool,
}

impl PartMesh {
    /// Checks the geometry, that skin weights sum to 1, and that every joint index it uses names a bone in
    /// `joint_bones`.
    ///
    /// # Errors
    /// The first violated invariant.
    pub fn validate(&self) -> Result<(), MeshError> {
        self.geometry.validate()?;
        let bad = |m: String| Err(MeshError(m));
        if self.geometry.is_skinned() {
            for (i, (joints, weights)) in self.geometry.joints.iter().zip(&self.geometry.weights).enumerate() {
                let sum: f32 = weights.iter().sum();
                if weights.iter().any(|w| w.is_nan() || *w < 0.0) || (sum - 1.0).abs() > 1e-3 {
                    return bad(format!("vertex {i} weights {weights:?} don't sum to 1"));
                }
                if let Some(j) =
                    joints.iter().zip(weights).find(|(j, w)| **w > 0.0 && usize::from(**j) >= self.joint_bones.len())
                {
                    return bad(format!(
                        "vertex {i} uses joint {} outside the {}-bone joint list",
                        j.0,
                        self.joint_bones.len()
                    ));
                }
            }
        } else if !self.joint_bones.is_empty() {
            return bad("a rigid mesh has no joint list".to_owned());
        }
        Ok(())
    }
}

/// A procedural texture, drawn in code by a part (cloth weaves, trim bands): the renderer's description plus pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextureData {
    /// Size, colour space, alpha encoding, mips and sampler (including `Wrap::Repeat` for cloth).
    pub desc: TextureDesc,
    /// `desc.byte_len()` bytes, RGBA8, rows top to bottom.
    pub rgba8: Vec<u8>,
}

impl TextureData {
    /// Checks the description and the pixel count.
    ///
    /// # Errors
    /// A message naming what is wrong.
    pub fn validate(&self) -> Result<(), String> {
        self.desc.validate().map_err(|e| e.to_string())?;
        if self.rgba8.len() != self.desc.byte_len() {
            return Err(format!(
                "{} bytes for a {}x{} RGBA texture (needs {})",
                self.rgba8.len(),
                self.desc.size.x,
                self.desc.size.y,
                self.desc.byte_len()
            ));
        }
        Ok(())
    }
}

/// The renderer texture a face layer needs: a repaintable sRGB texture holding premultiplied pixels, mip-mapped and
/// filtered anisotropically (the face is often seen at a slant). The face system paints it through the renderer's
/// `Painter`, writing `Raster::pixels` (premultiplied sRGB, the same layout) or drawing on the GPU.
pub fn face_texture_desc(spec: &FaceLayerSpec) -> TextureDesc {
    let (w, h) = spec.size_px();
    TextureDesc {
        size: omuzaki_core::glam::UVec2::new(w, h),
        colour_space: ColourSpace::Srgb,
        alpha: AlphaEncoding::Premultiplied,
        mips: true,
        usage: TextureUsage::Dynamic,
        sampler: Sampler { anisotropy: 8, ..Sampler::default() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omuzaki_core::glam::Vec2;
    use omuzaki_face_api::FaceRect;

    fn skinned() -> PartMesh {
        PartMesh {
            name: "m".to_owned(),
            geometry: MeshData {
                positions: vec![Vec3::ZERO, Vec3::X, Vec3::Y],
                normals: vec![Vec3::Z; 3],
                indices: vec![0, 1, 2],
                joints: vec![[0, 1, 0, 0]; 3],
                weights: vec![[0.5, 0.5, 0.0, 0.0]; 3],
                ..MeshData::default()
            },
            joint_bones: vec![JointRef::HEAD, JointRef::NECK],
            material: MaterialName::new("skin").unwrap(),
            casts_shadow: true,
            render_order: 0,
            dynamic: false,
        }
    }

    fn projection() -> FaceProjection {
        FaceProjection { rect: FaceRect { x0: -0.2, x1: 0.2, y0: -0.3, y1: 0.1 }, front_fade: [0.0, 0.07] }
    }

    #[test]
    fn the_uv_transform_reaches_the_renderer_material() {
        let mut spec = MaterialSpec::toon(Colour::WHITE);
        assert_eq!(spec.uv_transform, UvTransform::IDENTITY, "identity by default");
        spec.uv_transform = UvTransform { scale: Vec2::new(0.5, 1.0), offset: Vec2::new(0.5, 0.0) };
        let material = spec.to_material(&|_| None, &|_| None).unwrap();
        assert_eq!(material.uv_transform, spec.uv_transform);
        assert_eq!(spec.validate(), Ok(()));
    }

    #[test]
    fn joints_must_name_bones_and_weights_sum_to_one() {
        let mut m = skinned();
        m.validate().unwrap();
        m.joint_bones.pop();
        assert!(m.validate().unwrap_err().0.contains("joint 1"), "{:?}", m.validate());
        let mut bad = skinned();
        bad.geometry.weights[1] = [0.5, 0.0, 0.0, 0.0];
        assert!(bad.validate().unwrap_err().0.contains("sum to 1"));
        let mut rigid = skinned();
        rigid.geometry.joints.clear();
        rigid.geometry.weights.clear();
        assert!(rigid.validate().unwrap_err().0.contains("rigid"), "a rigid mesh with a joint list is a mistake");
        rigid.joint_bones.clear();
        rigid.validate().unwrap();
    }

    #[test]
    fn unused_joint_slots_may_point_anywhere() {
        let mut m = skinned();
        m.geometry.joints = vec![[0, 9, 9, 9]; 3];
        m.geometry.weights = vec![[1.0, 0.0, 0.0, 0.0]; 3];
        m.validate().unwrap();
    }

    #[test]
    fn overlays_carry_the_layers_composite_settings_into_the_renderers_mapping() {
        let centre = Vec3::new(0.0, 0.8, -0.02);
        let brows = FaceOverlay::of(&projection(), centre, FaceLayerId::Brows);
        assert_eq!(brows.strength, 0.85);
        assert_eq!(brows.window.fade_from, brows.window.fade_to);
        let base = FaceOverlay::of(&projection(), centre, FaceLayerId::Base);
        assert_eq!((base.strength, base.window.min.y), (1.0, 0.5));
        let overlay = base.to_overlay(TextureId::from_parts(3, 1));
        let Mapping::Planar { min, max, fade_from, fade_to } = overlay.mapping else { panic!("planar") };
        assert!(min.abs_diff_eq(Vec2::new(-0.2, 0.5), 1e-6) && max.abs_diff_eq(Vec2::new(0.2, 0.9), 1e-6));
        assert!((fade_from - -0.02).abs() < 1e-6 && (fade_to - 0.05).abs() < 1e-6);
        assert_eq!((overlay.texture, overlay.strength), (TextureId::from_parts(3, 1), 1.0));
    }

    #[test]
    fn a_spec_becomes_a_validated_renderer_material_once_the_stage_has_handles() {
        let mut spec = MaterialSpec::toon(Colour::hex(0x00f9_d3c5));
        spec.base_texture = Some(TextureName::new("weave").unwrap());
        spec.overlays.push(FaceOverlay::of(&projection(), Vec3::ZERO, FaceLayerId::Base));
        spec.receives_shadows = false;
        let weave = TextureId::from_parts(1, 1);
        let face = TextureId::from_parts(2, 1);
        let m = spec
            .to_material(&|n| (n.as_str() == "weave").then_some(weave), &|l| (l == FaceLayerId::Base).then_some(face))
            .unwrap();
        m.validate().unwrap();
        assert_eq!((m.base_texture, m.overlays[0].texture, m.receives_shadows), (Some(weave), face, false));
        assert_eq!(m.textures().collect::<Vec<_>>(), [weave, face]);
        assert_eq!(
            spec.to_material(&|_| None, &|_| Some(face)),
            Err(MaterialError::MissingTexture(TextureName::new("weave").unwrap()))
        );
        assert_eq!(spec.to_material(&|_| Some(weave), &|_| None), Err(MaterialError::MissingLayer(FaceLayerId::Base)));
    }

    #[test]
    fn face_layers_become_dynamic_premultiplied_srgb_textures() {
        let spec = FaceLayerSpec::for_layer(FaceLayerId::Brows, &projection(), 3600.0);
        let desc = face_texture_desc(&spec);
        desc.validate().unwrap();
        assert_eq!((desc.size.x, desc.size.y), spec.size_px());
        assert_eq!(
            (desc.alpha, desc.usage, desc.colour_space, desc.mips, desc.sampler.anisotropy),
            (AlphaEncoding::Premultiplied, TextureUsage::Dynamic, ColourSpace::Srgb, true, 8)
        );
    }

    #[test]
    fn texture_data_checks_its_pixel_count() {
        let desc = TextureDesc::colour(omuzaki_core::glam::UVec2::new(2, 2));
        assert!(TextureData { desc, rgba8: vec![0; 16] }.validate().is_ok());
        assert!(TextureData { desc, rgba8: vec![0; 15] }.validate().unwrap_err().contains("15 bytes"));
    }
}
