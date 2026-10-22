//! `MToon` 1.0's own lighting (`VRMC_materials_mtoon`, LK-07): what an imported model's material carries beyond the
//! kit's toon ramp, so it draws the way its maker saw it in `UniVRM` and three-vrm.
//!
//! A [`Material`](crate::Material) with [`Material::mtoon`](crate::Material::mtoon) set and toon shading is lit by the
//! spec's model in place of the kit's ramp. Its `base` and `base_texture` are `MToon`'s lit colour and texture, its
//! toon `emissive` the emissive factor, its `outline` and `outline_style` the outline; everything else is here. Per
//! fragment, after the UV animation moves the texture coordinates:
//!
//! - per light, `shading = linearstep(-1 + toony, 1 - toony, N·L + shift) × shadow` and the light adds
//!   `colour × mix(shade, lit, shading) / π`;
//! - the ambient light (hemisphere and environment) adds `irradiance × lit / π`, with the irradiance flattened toward
//!   the mean of straight up and straight down by `gi_equalisation` (`UniVRM`'s definition);
//! - the rim, `rim × (1 - N·V + lift)^fresnel_power` plus the matcap, times the rim texture, is added scaled by a mix
//!   from 1 to the light reaching the surface by `rim_lighting_mix`; then the emissive.
//!
//! Every texture is read through the material's `uv_transform` after the animation; their channels are the spec's
//! (the shading shift texture's red, the animation mask's blue).

use omuzaki_core::glam::Vec2;

use crate::{Colour, RenderError, TextureId};

/// `MToon` 1.0's parameters beyond the base colour, the emissive factor and the outline, with textures of type `T`. See
/// the module docs.
#[derive(Clone, Debug, PartialEq)]
pub struct Mtoon<T = TextureId> {
    /// The shade colour (`shadeColorFactor`).
    pub shade: Colour,
    /// Multiplied into `shade` (`shadeMultiplyTexture`, sRGB colour).
    pub shade_texture: Option<T>,
    /// Moves the terminator (`shadingShiftFactor`): negative lights more of the surface.
    pub shading_shift: f32,
    /// Added to the shift per texel: its red channel times `shading_shift_scale` (`shadingShiftTexture`, linear data).
    pub shading_shift_texture: Option<T>,
    /// The shading shift texture's scale.
    pub shading_shift_scale: f32,
    /// How hard the terminator is (`shadingToonyFactor`), `0..=1`; 1 is a step.
    pub shading_toony: f32,
    /// How much the ambient light is flattened (`giEqualizationFactor`), `0..=1`.
    pub gi_equalisation: f32,
    /// The matcap's colour (`matcapFactor`).
    pub matcap: Colour,
    /// The matcap (`matcapTexture`, sRGB colour), read by the view-space normal.
    pub matcap_texture: Option<T>,
    /// The parametric rim's colour (`parametricRimColorFactor`).
    pub rim: Colour,
    /// Multiplied into the rim and the matcap (`rimMultiplyTexture`, sRGB colour).
    pub rim_texture: Option<T>,
    /// The rim's fresnel exponent (`parametricRimFresnelPowerFactor`), `>= 0`.
    pub rim_fresnel_power: f32,
    /// Lifts the rim inward from the silhouette (`parametricRimLiftFactor`).
    pub rim_lift: f32,
    /// How much the light reaching the surface scales the rim (`rimLightingMixFactor`), `0..=1`.
    pub rim_lighting_mix: f32,
    /// Multiplied into the toon `emissive` (glTF's `emissiveTexture`, sRGB colour).
    pub emissive_texture: Option<T>,
    /// A tangent-space normal map (glTF's `normalTexture`, linear data), tangents taken from the screen-space
    /// derivatives of the position and UV, as three.js does when a mesh has none.
    pub normal_texture: Option<T>,
    /// Scales the normal map's X and Y (glTF's `normalTexture.scale`).
    pub normal_scale: f32,
    /// How far the texture coordinates scroll per second (`uvAnimationScrollXSpeedFactor`, `…YSpeedFactor`).
    pub uv_scroll: Vec2,
    /// How far they turn about (0.5, 0.5) per second, in radians (`uvAnimationRotationSpeedFactor`).
    pub uv_rotation: f32,
    /// Scales the animation per texel by its blue channel (`uvAnimationMaskTexture`, linear data).
    pub uv_animation_mask: Option<T>,
    /// A blended surface also writes depth (`transparentWithZWrite`), so it hides its own far side; ignored unless the
    /// material's alpha mode is blend.
    pub blend_writes_depth: bool,
}

impl<T> Default for Mtoon<T> {
    /// The spec's defaults: a black shade, a soft terminator (toony 0.9), GI flattened by 0.9, no rim, no matcap.
    fn default() -> Self {
        Self {
            shade: Colour::BLACK,
            shade_texture: None,
            shading_shift: 0.0,
            shading_shift_texture: None,
            shading_shift_scale: 1.0,
            shading_toony: 0.9,
            gi_equalisation: 0.9,
            matcap: Colour::WHITE,
            matcap_texture: None,
            rim: Colour::BLACK,
            rim_texture: None,
            rim_fresnel_power: 5.0,
            rim_lift: 0.0,
            rim_lighting_mix: 1.0,
            emissive_texture: None,
            normal_texture: None,
            normal_scale: 1.0,
            uv_scroll: Vec2::ZERO,
            uv_rotation: 0.0,
            uv_animation_mask: None,
            blend_writes_depth: false,
        }
    }
}

impl<T> Mtoon<T> {
    /// Every texture these parameters read.
    pub fn textures(&self) -> impl Iterator<Item = &T> + '_ {
        [
            &self.shade_texture,
            &self.shading_shift_texture,
            &self.matcap_texture,
            &self.rim_texture,
            &self.emissive_texture,
            &self.normal_texture,
            &self.uv_animation_mask,
        ]
        .into_iter()
        .flatten()
    }

    /// The same parameters with every texture passed through `f`, or `f`'s first error.
    ///
    /// # Errors
    /// What `f` returns for the first texture it refuses.
    pub fn try_map<U, E>(&self, mut f: impl FnMut(&T) -> Result<U, E>) -> Result<Mtoon<U>, E> {
        let mut texture = |t: &Option<T>| t.as_ref().map(&mut f).transpose();
        Ok(Mtoon {
            shade: self.shade,
            shade_texture: texture(&self.shade_texture)?,
            shading_shift: self.shading_shift,
            shading_shift_texture: texture(&self.shading_shift_texture)?,
            shading_shift_scale: self.shading_shift_scale,
            shading_toony: self.shading_toony,
            gi_equalisation: self.gi_equalisation,
            matcap: self.matcap,
            matcap_texture: texture(&self.matcap_texture)?,
            rim: self.rim,
            rim_texture: texture(&self.rim_texture)?,
            rim_fresnel_power: self.rim_fresnel_power,
            rim_lift: self.rim_lift,
            rim_lighting_mix: self.rim_lighting_mix,
            emissive_texture: texture(&self.emissive_texture)?,
            normal_texture: texture(&self.normal_texture)?,
            normal_scale: self.normal_scale,
            uv_scroll: self.uv_scroll,
            uv_rotation: self.uv_rotation,
            uv_animation_mask: texture(&self.uv_animation_mask)?,
            blend_writes_depth: self.blend_writes_depth,
        })
    }

    /// Checks the ranges every renderer shares.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] naming the first broken rule.
    pub fn validate(&self) -> Result<(), RenderError> {
        let bad = |reason: &str| Err(RenderError::invalid("material", reason));
        let unit = |v: f32| (0.0..=1.0).contains(&v);
        if !(unit(self.shading_toony) && unit(self.gi_equalisation) && unit(self.rim_lighting_mix)) {
            return bad("mtoon shading_toony, gi_equalisation and rim_lighting_mix must be in 0..=1");
        }
        if !(self.rim_fresnel_power >= 0.0 && self.rim_fresnel_power.is_finite()) {
            return bad("mtoon rim_fresnel_power must be >= 0");
        }
        let finite = [self.shading_shift, self.shading_shift_scale, self.rim_lift, self.normal_scale, self.uv_rotation];
        if !(finite.iter().all(|v| v.is_finite()) && self.uv_scroll.is_finite()) {
            return bad("mtoon shifts, scales, the rim lift and the UV animation must be finite");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_specs_and_valid() {
        let m: Mtoon = Mtoon::default();
        assert_eq!((m.shading_toony, m.gi_equalisation, m.rim_fresnel_power, m.rim_lighting_mix), (0.9, 0.9, 5.0, 1.0));
        assert_eq!(m.validate(), Ok(()));
        assert_eq!(m.textures().count(), 0);
    }

    #[test]
    fn out_of_range_factors_are_invalid() {
        let breaks: [fn(&mut Mtoon); 5] = [
            |m| m.shading_toony = 1.5,
            |m| m.gi_equalisation = -0.1,
            |m| m.rim_lighting_mix = 2.0,
            |m| m.rim_fresnel_power = -1.0,
            |m| m.uv_scroll.x = f32::NAN,
        ];
        for (i, broken) in breaks.iter().enumerate() {
            let mut m = Mtoon::default();
            broken(&mut m);
            assert!(matches!(m.validate(), Err(RenderError::Invalid { .. })), "break {i}");
        }
    }

    #[test]
    fn every_texture_is_listed() {
        let t = |i| Some(TextureId::from_parts(i, 1));
        let m = Mtoon {
            shade_texture: t(1),
            shading_shift_texture: t(2),
            matcap_texture: t(3),
            rim_texture: t(4),
            emissive_texture: t(5),
            normal_texture: t(6),
            uv_animation_mask: t(7),
            ..Mtoon::default()
        };
        assert_eq!(m.textures().count(), 7);
        let named = m.try_map(|t| Ok::<_, ()>(format!("t{}", t.index()))).unwrap();
        assert_eq!(named.textures().cloned().collect::<Vec<_>>(), ["t1", "t2", "t3", "t4", "t5", "t6", "t7"]);
        assert_eq!(m.try_map(|t| if t.index() == 4 { Err(4) } else { Ok(()) }), Err(4));
    }
}
