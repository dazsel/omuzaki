//! Toon shading and material evaluation for the CPU renderer.
//!
//! Lighting is computed once per triangle from the face normal (flat shading); only the albedo (textures, overlays)
//! varies per pixel. That is enough to see the toon ramp, the tinted shade, overlays and skin treatment in a golden,
//! and cheap enough to run in every test.

use std::f32::consts::PI;

use omuzaki_core::glam::{Vec2, Vec3, Vec4};
use omuzaki_render_api::{
    AlphaEncoding, AlphaMode, ColourSpace, Fog, Light, Mapping, Material, Shading, Skin, TextureId, Toon, Wrap,
    srgb_decode,
};

use crate::TextureData;
use crate::arena::Arena;
use crate::environment::EnvLight;
use crate::raster::Varying;

/// A spot light in linear light, with the cone's two edges as cosines (three.js `coneCos` and `penumbraCos`).
struct Spot {
    position: Vec3,
    /// Unit vector from the lamp along the cone's axis.
    axis: Vec3,
    colour: Vec3,
    range: f32,
    cone_cos: f32,
    penumbra_cos: f32,
}

/// A scene's lights in linear light, intensity folded into the colour.
#[derive(Default)]
pub(crate) struct Lights<'a> {
    directional: Vec<(Vec3, Vec3)>,
    hemisphere: Vec<(Vec3, Vec3)>,
    point: Vec<(Vec3, Vec3, f32)>,
    spot: Vec<Spot>,
    environment: Option<EnvLight<'a>>,
}

impl<'a> Lights<'a> {
    pub(crate) fn new(lights: &[Light]) -> Self {
        let mut out = Self::default();
        for light in lights {
            match *light {
                Light::Directional { to_light, colour, intensity, .. } => {
                    out.directional.push((to_light.normalize_or_zero(), colour.to_linear() * intensity));
                }
                Light::Hemisphere { sky, ground, intensity } => {
                    out.hemisphere.push((sky.to_linear() * intensity, ground.to_linear() * intensity));
                }
                Light::Point { position, colour, intensity, range } => {
                    out.point.push((position, colour.to_linear() * intensity, range));
                }
                Light::Spot { position, direction, colour, intensity, range, cone_outer_deg, penumbra, .. } => {
                    let angle = cone_outer_deg.to_radians();
                    out.spot.push(Spot {
                        position,
                        axis: direction.normalize_or_zero(),
                        colour: colour.to_linear() * intensity,
                        range,
                        cone_cos: angle.cos(),
                        penumbra_cos: (angle * (1.0 - penumbra)).cos(),
                    });
                }
            }
        }
        out
    }

    /// The same lights, lit by an environment too.
    pub(crate) fn with_environment(mut self, environment: Option<EnvLight<'a>>) -> Self {
        self.environment = environment;
        self
    }
}

/// Distance fog resolved to linear light.
#[derive(Copy, Clone)]
pub(crate) struct FogLight(Option<(Fog, Vec3)>);

impl FogLight {
    pub(crate) fn new(fog: Option<Fog>) -> Self {
        Self(fog.map(|f| (f, f.colour.to_linear())))
    }

    /// Fades straight linear `rgba` toward the fog colour for a surface `depth` metres along the view direction. Alpha
    /// is untouched: a faded blended surface is still as see-through as before.
    pub(crate) fn apply(self, rgba: Vec4, depth: f32) -> Vec4 {
        match self.0 {
            Some((fog, colour)) => rgba.truncate().lerp(colour, fog.amount(depth)).extend(rgba.w),
            None => rgba,
        }
    }
}

/// What a triangle's lighting contributes to every pixel of it.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Lit {
    /// Multiplies the albedo.
    radiance: Vec3,
    /// Added after.
    rim: Vec3,
}

/// `smoothstep(e0, e1, x)`, degenerating to a hard step when the edges meet (zero softness).
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    if e1 <= e0 {
        return f32::from(x >= e0);
    }
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The toon parameters a material is drawn with, or `None` for unlit. The mock knows no shading programs, so it draws
/// them with the default toon look over the material's base, textures and overlays: the contract's fallback.
fn toon_of(shading: &Shading) -> Option<Toon> {
    match shading {
        Shading::Toon(toon) => Some(*toon),
        Shading::Program { .. } => Some(Toon::default()),
        Shading::Unlit => None,
    }
}

/// How much of a point light's colour reaches a surface `distance` metres away: three.js `getDistanceAttenuation` with
/// decay 2 and cutoff `range` (D10 in `docs/design/render-api.md`). It is the GPU renderer's `point_falloff`, term for
/// term, so both renderers agree on the same scene. The inverse square is clamped at 0.01 m² so a surface at the light
/// does not blow up, and the smooth window reaches zero at `range`.
fn point_falloff(distance: f32, range: f32) -> f32 {
    let window = (1.0 - (distance / range).powi(4)).clamp(0.0, 1.0);
    window * window / (distance * distance).max(0.01)
}

/// Lights one triangle. `n` faces the viewer; `centroid` is in world space and `to_viewer` points from it toward the
/// eye (for an orthographic camera, the constant view axis).
pub(crate) fn light_triangle(
    material: &Material,
    lights: &Lights<'_>,
    n: Vec3,
    centroid: Vec3,
    to_viewer: Vec3,
) -> Lit {
    let Some(toon) = toon_of(&material.shading) else {
        return Lit { radiance: Vec3::ONE, rim: Vec3::ZERO };
    };
    let shade = toon.shade.to_linear();
    // The prototype's ramp: the shadow side is a tinted multiplier, never a grey.
    let ramp =
        |ndl: f32| shade.lerp(Vec3::ONE, smoothstep(toon.shift - toon.softness, toon.shift + toon.softness, ndl));

    let mut irradiance = Vec3::ZERO;
    for &(to_light, colour) in &lights.directional {
        irradiance += colour * ramp(n.dot(to_light));
    }
    for &(position, colour, range) in &lights.point {
        let offset = position - centroid;
        irradiance += colour * point_falloff(offset.length(), range) * ramp(n.dot(offset.normalize_or_zero()));
    }
    for spot in &lights.spot {
        let offset = spot.position - centroid;
        let to_light = offset.normalize_or_zero();
        // three.js: smoothstep(coneCos, penumbraCos, cos of the angle off the axis); a zero penumbra is a hard edge.
        let cone = smoothstep(spot.cone_cos, spot.penumbra_cos, (-to_light).dot(spot.axis));
        irradiance += spot.colour * point_falloff(offset.length(), spot.range) * cone * ramp(n.dot(to_light));
    }
    for &(sky, ground) in &lights.hemisphere {
        irradiance += ground.lerp(sky, 0.5 + 0.5 * n.y);
    }
    if let Some(environment) = &lights.environment {
        irradiance += environment
            .irradiance(n, |along| smoothstep(toon.shift - toon.softness, toon.shift + toon.softness, along));
    }

    let view = to_viewer.normalize_or_zero();
    let fresnel = (1.0 - n.dot(view).clamp(0.0, 1.0)).powf(toon.rim.power);
    Lit { radiance: irradiance / PI, rim: toon.rim.colour.to_linear() * fresnel * toon.rim.strength }
}

/// Texture lookups for the pixel shader. Nearest-texel, no mips: the mock is about correctness of the pipeline, not
/// filtering quality.
pub(crate) struct Textures<'a>(pub(crate) &'a Arena<TextureData>);

impl Textures<'_> {
    /// The texel at `uv` as straight, linear RGBA.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
    fn sample(&self, id: TextureId, uv: Vec2) -> Vec4 {
        let Some(tex) = self.0.get(id.index(), id.generation()) else {
            return Vec4::ZERO;
        };
        let texel = |coord: f32, n: u32| -> usize {
            let i = (coord * n as f32).floor() as i64;
            let n = i64::from(n);
            (match tex.desc.sampler.wrap {
                Wrap::Clamp => i.clamp(0, n - 1),
                Wrap::Repeat => i.rem_euclid(n),
            }) as usize
        };
        let (x, y) = (texel(uv.x, tex.desc.size.x), texel(uv.y, tex.desc.size.y));
        let at = (y * tex.desc.size.x as usize + x) * 4;
        let p = &tex.rgba[at..at + 4];
        let a = f32::from(p[3]) / 255.0;
        let mut rgb = Vec3::new(f32::from(p[0]), f32::from(p[1]), f32::from(p[2])) / 255.0;
        if tex.desc.alpha == AlphaEncoding::Premultiplied && a > 0.0 {
            rgb = (rgb / a).min(Vec3::ONE);
        }
        if tex.desc.colour_space == ColourSpace::Srgb {
            rgb = Vec3::new(srgb_decode(rgb.x), srgb_decode(rgb.y), srgb_decode(rgb.z));
        }
        rgb.extend(a)
    }
}

/// How much of an overlay shows at local depth `z`: a smooth fade, or a hard cut when the two ends meet.
fn fade(from: f32, to: f32, z: f32) -> f32 {
    if to <= from { f32::from(z > from) } else { smoothstep(from, to, z) }
}

fn skin_adjust(lit: Vec3, albedo: Vec3, skin: Skin) -> Vec3 {
    const LUMA: Vec3 = Vec3::new(0.299, 0.587, 0.114);
    // Keep the albedo's hue under coloured light, then never fall below a fraction of the albedo.
    let keep_hue = albedo * (lit.dot(LUMA) / albedo.dot(LUMA).max(1e-3));
    lit.lerp(keep_hue, skin.hue_keep).max(albedo * skin.floor)
}

/// The pixel's final straight linear RGBA, or `None` if the material discards it.
pub(crate) fn shade_pixel(material: &Material, lit: Lit, textures: &Textures<'_>, v: Varying) -> Option<Vec4> {
    let mut albedo = material.base.to_linear().extend(material.base.alpha());
    if material.vertex_colours {
        albedo *= v.colour;
    }
    if let Some(texture) = material.base_texture {
        albedo *= textures.sample(texture, material.uv_transform.apply(v.uv));
    }
    for overlay in &material.overlays {
        let (uv, weight) = match overlay.mapping {
            Mapping::Uv => (material.uv_transform.apply(v.uv), 1.0),
            Mapping::Planar { min, max, fade_from, fade_to } => {
                let size = max - min;
                // The top row of the texture is at max.y.
                let uv = Vec2::new((v.local.x - min.x) / size.x, (max.y - v.local.y) / size.y);
                let inside = (0.0..=1.0).contains(&uv.x) && (0.0..=1.0).contains(&uv.y);
                (uv, if inside { fade(fade_from, fade_to, v.local.z) } else { 0.0 })
            }
        };
        if weight > 0.0 {
            let texel = textures.sample(overlay.texture, uv);
            albedo = albedo.truncate().lerp(texel.truncate(), texel.w * overlay.strength * weight).extend(albedo.w);
        }
    }
    let alpha = match material.alpha {
        AlphaMode::Mask { cutoff } if albedo.w < cutoff => return None,
        AlphaMode::Opaque | AlphaMode::Mask { .. } => 1.0,
        AlphaMode::Blend => albedo.w,
    };
    let colour =
        toon_of(&material.shading).map_or(albedo.truncate(), |toon| toon_colour(&toon, albedo.truncate(), lit));
    Some(colour.extend(alpha))
}

fn toon_colour(toon: &Toon, albedo: Vec3, lit: Lit) -> Vec3 {
    let colour = albedo * lit.radiance + toon.emissive.to_linear() + lit.rim;
    toon.skin.map_or(colour, |skin| skin_adjust(colour, albedo, skin))
}

#[cfg(test)]
mod tests {
    use omuzaki_render_api::Colour;

    use super::*;

    fn key(to_light: Vec3) -> Vec<Light> {
        vec![Light::Directional { to_light, colour: Colour::WHITE, intensity: 2.0, casts_shadow: false }]
    }

    fn lit_by(material: &Material, lights: &[Light], n: Vec3) -> Lit {
        light_triangle(material, &Lights::new(lights), n, Vec3::ZERO, Vec3::Z)
    }

    #[test]
    fn point_falloff_is_threejs_decay_two_with_a_cutoff() {
        // D10, evaluated in f64 by hand: 1 / max(d², 0.01) × saturate(1 − (d / range)⁴)².
        let cases = [
            (1.6, 7.0, 0.388_495),
            (3.0, 7.0, 0.103_741),
            (6.3, 7.0, 0.002_979_773), // 0.9 × range: the window is nearly closed
            (0.2, 7.0, 24.999_967),
            (0.1, 7.0, 100.0), // exactly the 0.01 m² floor
            (0.0, 7.0, 100.0), // at the light itself
            (7.0, 7.0, 0.0),   // exactly the range
            (8.0, 7.0, 0.0),   // beyond it
        ];
        for (distance, range, want) in cases {
            let got = point_falloff(distance, range);
            assert!((got - want).abs() <= 1e-5 * (1.0 + want), "falloff({distance}, {range}) = {got}, expected {want}");
        }
    }

    #[test]
    fn a_point_light_shines_with_its_falloff_and_stops_at_its_range() {
        let mut m = Material::toon(Colour::WHITE);
        m.shading = Shading::Toon(Toon {
            shade: Colour::WHITE,
            rim: omuzaki_render_api::Rim { strength: 0.0, ..Toon::default().rim },
            ..Toon::default()
        });
        let radiance_at = |distance: f32| {
            let light = Light::Point {
                position: Vec3::new(0.0, 0.0, distance),
                colour: Colour::WHITE,
                intensity: 2.0,
                range: 7.0,
            };
            lit_by(&m, &[light], Vec3::Z).radiance.x
        };
        // Radiance is irradiance / π: intensity 2 at 1.6 m of a 7 m range is 2 × 0.388 / π.
        assert!((radiance_at(1.6) - 2.0 * 0.388_495 / PI).abs() < 1e-5, "{}", radiance_at(1.6));
        assert_eq!(radiance_at(7.5), 0.0, "no light past the range");
    }

    #[test]
    fn the_rim_uses_the_view_direction_it_is_given() {
        // An orthographic camera gives every triangle one view axis, so a rim depends on the normal alone: a surface
        // 60° off the axis gets the same rim wherever its centroid is.
        let mut m = Material::toon(Colour::WHITE);
        m.shading = Shading::Toon(Toon {
            rim: omuzaki_render_api::Rim { colour: Colour::WHITE, power: 1.0, strength: 1.0 },
            ..Toon::default()
        });
        let normal = Vec3::new(0.866_025_4, 0.0, 0.5);
        let rim_at = |centroid: Vec3| light_triangle(&m, &Lights::default(), normal, centroid, Vec3::Z).rim.x;
        assert!((rim_at(Vec3::ZERO) - 0.5).abs() < 1e-6);
        assert!((rim_at(Vec3::new(0.8, 0.0, 0.0)) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn intensity_follows_the_threejs_convention() {
        // Key intensity 2 on albedo 1 gives 2 / pi = 0.64 (the number the prototype's rigs rely on).
        let mut m = Material::toon(Colour::WHITE);
        m.shading = Shading::Toon(Toon {
            rim: omuzaki_render_api::Rim { strength: 0.0, ..Toon::default().rim },
            ..Toon::default()
        });
        let lit = lit_by(&m, &key(Vec3::Z), Vec3::Z);
        assert!((lit.radiance.x - 2.0 / PI).abs() < 1e-5, "{lit:?}");
    }

    #[test]
    fn the_shadow_side_is_the_tinted_shade_not_grey() {
        let m = Material::toon(Colour::WHITE);
        let Shading::Toon(toon) = &m.shading else { unreachable!() };
        let dark = lit_by(&m, &key(Vec3::Z), -Vec3::Z).radiance * PI / 2.0;
        assert!(
            (dark - toon.shade.to_linear()).abs().max_element() < 1e-5,
            "facing away shows exactly the shade colour"
        );
        assert!((dark.x - dark.z).abs() > 0.01, "a tinted shade is not a grey: {dark:?}");
    }

    #[test]
    fn softness_widens_the_terminator_and_zero_makes_it_hard() {
        let ramp_at = |softness: f32, ndl: f32| {
            let mut m = Material::toon(Colour::WHITE);
            m.shading = Shading::Toon(Toon { softness, ..Toon::default() });
            // A normal whose dot with +Z is `ndl`.
            let n = Vec3::new((1.0 - ndl * ndl).sqrt(), 0.0, ndl);
            lit_by(&m, &key(Vec3::Z), n).radiance.x
        };
        let (soft_mid, hard_before, hard_after) = (ramp_at(0.3, 0.1), ramp_at(0.0, -0.01), ramp_at(0.0, 0.01));
        assert!(hard_after > hard_before * 1.2, "a hard edge jumps across ndl = 0");
        assert!(soft_mid > hard_before && soft_mid < hard_after, "a soft edge is in between");
    }

    #[test]
    fn skin_keeps_a_floor_in_shadow() {
        let albedo = Vec3::new(0.9, 0.6, 0.5);
        let out = skin_adjust(Vec3::splat(0.01), albedo, Skin::default());
        assert!(out.x >= albedo.x * 0.42 - 1e-6 && out.y >= albedo.y * 0.42 - 1e-6, "{out:?}");
    }

    #[test]
    fn unlit_materials_ignore_lights() {
        let m = Material::unlit(Colour::hex(0x0080_4020));
        let lit = lit_by(&m, &key(Vec3::Z), -Vec3::Z);
        let textures = Arena::default();
        let px = shade_pixel(
            &m,
            lit,
            &Textures(&textures),
            Varying { uv: Vec2::ZERO, local: Vec3::ZERO, colour: Vec4::ONE, depth: 0.0 },
        );
        assert_eq!(px, Some(Colour::hex(0x0080_4020).to_linear().extend(1.0)));
    }

    #[test]
    fn mask_discards_below_the_cutoff_and_blend_keeps_alpha() {
        let textures = Arena::default();
        let v = Varying { uv: Vec2::ZERO, local: Vec3::ZERO, colour: Vec4::ONE, depth: 0.0 };
        let half = Colour::rgba(255, 255, 255, 128);
        let mut m = Material::unlit(half);
        m.alpha = AlphaMode::Mask { cutoff: 0.6 };
        assert_eq!(shade_pixel(&m, lit_by(&m, &[], Vec3::Z), &Textures(&textures), v), None);
        m.alpha = AlphaMode::Mask { cutoff: 0.4 };
        assert_eq!(shade_pixel(&m, lit_by(&m, &[], Vec3::Z), &Textures(&textures), v).map(|c| c.w), Some(1.0));
        m.alpha = AlphaMode::Blend;
        let alpha = shade_pixel(&m, lit_by(&m, &[], Vec3::Z), &Textures(&textures), v).map(|c| c.w);
        assert!((alpha.unwrap_or_default() - half.alpha()).abs() < 1e-6);
    }

    #[test]
    fn overlay_fade_is_smooth_or_a_hard_cut() {
        assert_eq!(fade(0.0, 0.0, 0.001), 1.0);
        assert_eq!(fade(0.0, 0.0, 0.0), 0.0, "strictly in front");
        assert_eq!(fade(0.0, 0.07, -0.1), 0.0);
        assert_eq!(fade(0.0, 0.07, 0.2), 1.0);
        assert!(fade(0.0, 0.07, 0.035) > 0.4 && fade(0.0, 0.07, 0.035) < 0.6);
    }
}
