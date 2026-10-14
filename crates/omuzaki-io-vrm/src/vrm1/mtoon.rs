//! `VRMC_materials_mtoon` 1.0 (`VRMC_materials_mtoon-1.0/README.md`) to and from the scene's `Toon`, whose fields mirror
//! the extension one for one.
//!
//! Every value is written explicitly, so a file we write never depends on a reader's idea of a default (the README and
//! the JSON schema disagree on `shadeColorFactor`). On read, a missing field takes the README default. Texture
//! references inside the body are ordinary glTF texture infos: `omuzaki-gltf` maps them (UV transform, UV set) in both
//! directions.

use omuzaki_core::glam::Vec3;
use omuzaki_gltf::schema::TextureInfo;
use omuzaki_gltf::{MaterialCx, Tally, TextureMap, texture_info, to_json};
use omuzaki_io_api::{AlphaMode, Finding, OutlineMode, Report, ShiftTexture, TexRef, Toon};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The extension's name.
pub(crate) const MTOON: &str = "VRMC_materials_mtoon";

/// `VRMC_materials_mtoon`, every field optional so a file may omit what it likes and a writer can set all of them.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct WireMtoon {
    spec_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    transparent_with_z_write: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    render_queue_offset_number: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shade_color_factor: Option<[f32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shade_multiply_texture: Option<TextureInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shading_shift_factor: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shading_shift_texture: Option<TextureInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shading_toony_factor: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gi_equalization_factor: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    matcap_factor: Option<[f32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    matcap_texture: Option<TextureInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parametric_rim_color_factor: Option<[f32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parametric_rim_fresnel_power_factor: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parametric_rim_lift_factor: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rim_multiply_texture: Option<TextureInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rim_lighting_mix_factor: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outline_width_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outline_width_factor: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outline_width_multiply_texture: Option<TextureInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outline_color_factor: Option<[f32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outline_lighting_mix_factor: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    uv_animation_mask_texture: Option<TextureInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    uv_animation_scroll_x_speed_factor: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    uv_animation_scroll_y_speed_factor: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    uv_animation_rotation_speed_factor: Option<f32>,
}

/// Maps the texture references of one `MToon` body: `at` locates them in findings.
struct Textures<'a, 'b> {
    map: &'a TextureMap,
    tally: &'b mut Tally,
    at: String,
}

impl Textures<'_, '_> {
    fn get(&mut self, info: Option<&TextureInfo>, name: &str) -> Option<TexRef> {
        info.and_then(|i| self.map.tex_ref(i, &format!("{}/{name}", self.at), self.tally))
    }
}

/// Turns `cx.material` into a toon material from the extension `body`. A body that doesn't parse is reported in
/// `report` and replaced by the defaults, so one bad material doesn't cost the whole model.
#[allow(clippy::field_reassign_with_default)] // `Toon` is `#[non_exhaustive]`: only assignment can fill it from outside
pub(crate) fn read(cx: &mut MaterialCx<'_>, body: &Value, report: &mut Report) {
    let at = format!("/materials/{}/extensions/{MTOON}", cx.index);
    let wire = match WireMtoon::deserialize(body) {
        Ok(w) => w,
        Err(e) => {
            report.push(
                Finding::warning("vrm.mtoon_invalid", format!("VRMC_materials_mtoon couldn't be read: {e}"))
                    .at(&at)
                    .hint("the material falls back to MToon defaults"),
            );
            WireMtoon::default()
        }
    };
    let mut tex = Textures { map: cx.textures, tally: cx.tally, at };
    let d = Toon::default();
    let mut t = Toon::default();
    let color = |c: Option<[f32; 3]>, default: Vec3| c.map_or(default, Vec3::from_array);
    t.shade_color = color(wire.shade_color_factor, d.shade_color);
    t.shade_multiply_texture = tex.get(wire.shade_multiply_texture.as_ref(), "shadeMultiplyTexture");
    t.shading_shift = wire.shading_shift_factor.unwrap_or(d.shading_shift);
    t.shading_shift_texture = wire.shading_shift_texture.as_ref().and_then(|info| {
        let scale = info.scale.unwrap_or(1.0);
        tex.get(Some(info), "shadingShiftTexture").map(|texture| ShiftTexture { texture, scale })
    });
    t.shading_toony = wire.shading_toony_factor.unwrap_or(d.shading_toony);
    t.gi_equalization = wire.gi_equalization_factor.unwrap_or(d.gi_equalization);
    t.matcap_color = color(wire.matcap_factor, d.matcap_color);
    t.matcap_texture = tex.get(wire.matcap_texture.as_ref(), "matcapTexture");
    t.rim_color = color(wire.parametric_rim_color_factor, d.rim_color);
    t.rim_fresnel_power = wire.parametric_rim_fresnel_power_factor.unwrap_or(d.rim_fresnel_power);
    t.rim_lift = wire.parametric_rim_lift_factor.unwrap_or(d.rim_lift);
    t.rim_multiply_texture = tex.get(wire.rim_multiply_texture.as_ref(), "rimMultiplyTexture");
    t.rim_lighting_mix = wire.rim_lighting_mix_factor.unwrap_or(d.rim_lighting_mix);
    t.outline_mode = match wire.outline_width_mode.as_deref() {
        Some("worldCoordinates") => OutlineMode::WorldCoordinates,
        Some("screenCoordinates") => OutlineMode::ScreenCoordinates,
        _ => OutlineMode::None,
    };
    t.outline_width = wire.outline_width_factor.unwrap_or(d.outline_width);
    t.outline_width_texture = tex.get(wire.outline_width_multiply_texture.as_ref(), "outlineWidthMultiplyTexture");
    t.outline_color = color(wire.outline_color_factor, d.outline_color);
    t.outline_lighting_mix = wire.outline_lighting_mix_factor.unwrap_or(d.outline_lighting_mix);
    t.uv_animation_mask = tex.get(wire.uv_animation_mask_texture.as_ref(), "uvAnimationMaskTexture");
    t.uv_scroll_x = wire.uv_animation_scroll_x_speed_factor.unwrap_or(d.uv_scroll_x);
    t.uv_scroll_y = wire.uv_animation_scroll_y_speed_factor.unwrap_or(d.uv_scroll_y);
    t.uv_rotation = wire.uv_animation_rotation_speed_factor.unwrap_or(d.uv_rotation);
    t.transparent_with_zwrite = wire.transparent_with_z_write.unwrap_or(d.transparent_with_zwrite);
    t.render_queue_offset = wire.render_queue_offset_number.unwrap_or(d.render_queue_offset);
    cx.material.shading = omuzaki_io_api::Shading::Toon(Box::new(t));
}

/// The most `renderQueueOffsetNumber` allows for a material, by alpha mode and `transparentWithZWrite`
/// (`VRMC_materials_mtoon-1.0/README.md`, "Render Queue").
fn queue_range(alpha: AlphaMode, zwrite: bool) -> (i32, i32) {
    match (alpha, zwrite) {
        (AlphaMode::Blend, true) => (0, 9),
        (AlphaMode::Blend, false) => (-9, 0),
        _ => (0, 0),
    }
}

/// The extension body for toon material `t` of a material with alpha mode `alpha`, as JSON. `at` locates findings.
pub(crate) fn write(t: &Toon, alpha: AlphaMode, at: &str, tally: &mut Tally) -> Value {
    let (lo, hi) = queue_range(alpha, t.transparent_with_zwrite);
    let queue = t.render_queue_offset.clamp(lo, hi);
    if queue != t.render_queue_offset {
        let msg = format!("renderQueueOffsetNumber {} isn't allowed here; {queue} was written", t.render_queue_offset);
        tally.warn("vrm.mtoon_queue_clamped", msg, at, Some("only blended materials may offset the render queue"));
    }
    let tex = |r: &Option<TexRef>| r.as_ref().map(|r| texture_info(r, None));
    let wire = WireMtoon {
        spec_version: "1.0".to_owned(),
        transparent_with_z_write: Some(t.transparent_with_zwrite),
        render_queue_offset_number: Some(queue),
        shade_color_factor: Some(t.shade_color.to_array()),
        shade_multiply_texture: tex(&t.shade_multiply_texture),
        shading_shift_factor: Some(t.shading_shift),
        shading_shift_texture: t.shading_shift_texture.as_ref().map(|s| texture_info(&s.texture, Some(s.scale))),
        shading_toony_factor: Some(t.shading_toony),
        gi_equalization_factor: Some(t.gi_equalization),
        matcap_factor: Some(t.matcap_color.to_array()),
        matcap_texture: tex(&t.matcap_texture),
        parametric_rim_color_factor: Some(t.rim_color.to_array()),
        parametric_rim_fresnel_power_factor: Some(t.rim_fresnel_power),
        parametric_rim_lift_factor: Some(t.rim_lift),
        rim_multiply_texture: tex(&t.rim_multiply_texture),
        rim_lighting_mix_factor: Some(t.rim_lighting_mix),
        outline_width_mode: Some(
            match t.outline_mode {
                OutlineMode::None => "none",
                OutlineMode::WorldCoordinates => "worldCoordinates",
                OutlineMode::ScreenCoordinates => "screenCoordinates",
            }
            .to_owned(),
        ),
        outline_width_factor: Some(t.outline_width),
        outline_width_multiply_texture: tex(&t.outline_width_texture),
        outline_color_factor: Some(t.outline_color.to_array()),
        outline_lighting_mix_factor: Some(t.outline_lighting_mix),
        uv_animation_mask_texture: tex(&t.uv_animation_mask),
        uv_animation_scroll_x_speed_factor: Some(t.uv_scroll_x),
        uv_animation_scroll_y_speed_factor: Some(t.uv_scroll_y),
        uv_animation_rotation_speed_factor: Some(t.uv_rotation),
    };
    to_json(&wire)
}
