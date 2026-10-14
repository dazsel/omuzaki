//! VRM 0.x `materialProperties` (`MToon` 0.x and the unlit shaders) to and from the scene's `Toon` and `Unlit`
//! materials.
//!
//! A 0.x file keeps a plain glTF material for each Unity material (so the file still opens as a GLB) and puts what the
//! Unity shader needs in `VRM.materialProperties`, index for index. The conversion follows `UniVRM`'s migrator
//! (`Packages/VRM10/Runtime/Migration/Materials/*`, `MToon10Migrator.cs`, MIT; read for behaviour, nothing copied) so a
//! model looks the same here as in the incumbent:
//!
//! | 0.x property | Scene (`Toon`) |
//! |---|---|
//! | `_Color`, `_ShadeColor`, `_RimColor`, `_OutlineColor` (sRGB) | linear colours; `_EmissionColor` is linear already |
//! | `_ShadeShift`, `_ShadeToony` | `shading_shift`, `shading_toony` (see [`shading_from_0x`]) |
//! | `_IndirectLightIntensity` | `gi_equalization` = 1 − intensity |
//! | `_SphereAdd` | `matcap_texture`, factor 1 (no texture: factor 0) |
//! | `_OutlineWidth` (cm, or % of half the screen height) | metres (`× 0.01`) or screen fraction (`× 0.005`) |
//! | `_OutlineColorMode`, `_OutlineLightingMix` | `outline_lighting_mix` (fixed colour = 0) |
//! | `_UvAnimScrollY`, `_UvAnimRotation` (turns/s) | scroll Y negated (UV space is flipped), radians/s |
//! | `_BlendMode`, `_Cutoff`, `_CullMode`, `renderQueue` | alpha mode, cutoff, double sided, queue offset |
//! | `_MainTex` ST vector | one `KHR_texture_transform` on every texture except the matcap |
//!
//! Not representable in 0.x or 1.0: `_ReceiveShadowRate`, `_ShadingGradeRate` and their textures, and `_LightColorAttenuation`
//! (1.0 dropped them); the rim lighting mix becomes 1.0, because 1.0 merged rim and matcap. A lit texture with no shade
//! texture is used as the shade texture too, the same destructive-but-friendly choice `UniVRM` makes.

use std::collections::BTreeMap;
use std::f32::consts::TAU;

use omuzaki_core::colour::{srgb_decode, srgb_encode};
use omuzaki_core::glam::{Vec2, Vec3, Vec4};
use omuzaki_io_api::{
    AlphaMode, Finding, Material, NormalTexture, OutlineMode, Report, Shading, TexRef, TextureIx, Toon, UvTransform,
};

use super::wire::WireMaterial0;
use crate::ix32;

/// The shader name of `MToon` 0.x.
pub(crate) const MTOON_SHADER: &str = "VRM/MToon";
/// What `UniVRM` writes for a material that is plain glTF (no VRM shader).
pub(crate) const GLTF_SHADER: &str = "VRM_USE_GLTFSHADER";

/// `renderQueue` of the three `MToon` render modes (`Packages/VRM10/MToon10/Runtime/MToon0X/MToon0XUtils.cs`).
const QUEUE_CUTOUT: i64 = 2450;
const QUEUE_TRANSPARENT: i64 = 3000;
const QUEUE_ZWRITE: i64 = 2501;

/// 0.x stores its enums as floats (`_BlendMode`, `_CullMode`, `_OutlineWidthMode`); this reads one as the nearest
/// small non-negative integer, so no code compares floats for equality.
fn choice(v: f32) -> u8 {
    if v < 0.5 {
        0
    } else if v < 1.5 {
        1
    } else if v < 2.5 {
        2
    } else {
        3
    }
}

/// 0.x `_BlendMode`.
#[derive(Copy, Clone, PartialEq, Eq)]
enum BlendMode {
    Opaque,
    Cutout,
    Transparent,
    TransparentZWrite,
}

impl BlendMode {
    fn from_unity(v: f32) -> Self {
        match choice(v) {
            0 => Self::Opaque,
            1 => Self::Cutout,
            2 => Self::Transparent,
            _ => Self::TransparentZWrite,
        }
    }

    fn unity(self) -> f32 {
        match self {
            Self::Opaque => 0.0,
            Self::Cutout => 1.0,
            Self::Transparent => 2.0,
            Self::TransparentZWrite => 3.0,
        }
    }

    /// The mode of a scene material; `zwrite` is `MToon`'s `transparentWithZWrite`.
    fn of(m: &Material, zwrite: bool) -> Self {
        match m.alpha {
            AlphaMode::Opaque => Self::Opaque,
            AlphaMode::Mask { .. } => Self::Cutout,
            AlphaMode::Blend if zwrite => Self::TransparentZWrite,
            AlphaMode::Blend => Self::Transparent,
        }
    }

    fn is_blended(self) -> bool {
        matches!(self, Self::Transparent | Self::TransparentZWrite)
    }

    /// The Unity blend state that goes with the mode (`_SrcBlend`, `_DstBlend`, `_ZWrite`).
    fn blend_state(self) -> [f32; 3] {
        match self {
            Self::Opaque | Self::Cutout => [1.0, 0.0, 1.0],
            Self::Transparent => [5.0, 10.0, 0.0],
            Self::TransparentZWrite => [5.0, 10.0, 1.0],
        }
    }

    /// The shader's `RenderType` tag.
    fn render_type(self) -> &'static str {
        match self {
            Self::Opaque => "Opaque",
            Self::Cutout => "TransparentCutout",
            Self::Transparent | Self::TransparentZWrite => "Transparent",
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The shading range

/// 1.0 shading shift and toony from the 0.x pair. 0.x blends lit and shade over the range
/// `[shift, lerp(1, shift, toony)]` of the light-facing dot product; 1.0 takes the *centre* (negated) and the *margin*
/// of that range (`MToon10Migrator.cs`). Returns `(toony, shift)` in 1.0 terms.
pub(crate) fn shading_from_0x(toony: f32, shift: f32) -> (f32, f32) {
    let (min, max) = (shift, 1.0 + (shift - 1.0) * toony);
    let toony10 = ((2.0 - (max - min)) * 0.5).clamp(0.0, 1.0);
    let shift10 = (-(max + min) * 0.5).clamp(-1.0, 1.0);
    (toony10, shift10)
}

/// The inverse of [`shading_from_0x`]: the 0.x `(toony, shift)` that produces this range. Ranges reaching past the
/// 0.x limits (`shift` is −1..1) are clamped, so a 1.0 model with a very soft edge loses a little going to 0.x.
pub(crate) fn shading_to_0x(toony10: f32, shift10: f32) -> (f32, f32) {
    let (centre, half) = (-shift10, 1.0 - toony10);
    let min = (centre - half).clamp(-1.0, 1.0);
    let max = (centre + half).clamp(-1.0, 1.0);
    let toony = if (1.0 - min).abs() < 1e-6 { 1.0 } else { ((1.0 - max) / (1.0 - min)).clamp(0.0, 1.0) };
    (toony, min)
}

// ---------------------------------------------------------------------------------------------------------------
// Reading

/// The properties of one 0.x material, with the lookups the converter needs.
struct Props<'a>(&'a WireMaterial0);

impl Props<'_> {
    fn float(&self, name: &str, default: f32) -> f32 {
        self.0.float_properties.get(name).copied().unwrap_or(default)
    }

    fn vector(&self, name: &str) -> Option<&[f32]> {
        self.0.vector_properties.get(name).map(Vec::as_slice).filter(|v| v.len() >= 3)
    }

    /// An sRGB colour property decoded to linear; alpha stays as is.
    fn srgb(&self, name: &str, default: [f32; 4]) -> Vec4 {
        let c = self.vector(name).map_or(default, |v| [v[0], v[1], v[2], v.get(3).copied().unwrap_or(1.0)]);
        Vec4::new(srgb_decode(c[0]), srgb_decode(c[1]), srgb_decode(c[2]), c[3])
    }

    fn linear(&self, name: &str) -> Vec3 {
        self.vector(name).map_or(Vec3::ZERO, |v| Vec3::new(v[0], v[1], v[2]))
    }

    fn texture(&self, name: &str) -> Option<TextureIx> {
        self.0.texture_properties.get(name).and_then(|&t| u32::try_from(t).ok()).map(TextureIx)
    }

    fn blend_mode(&self) -> BlendMode {
        BlendMode::from_unity(self.float("_BlendMode", 0.0))
    }

    /// The `_MainTex` offset and scale (`[ox, oy, sx, sy]`, Unity's V axis points up) as a glTF UV transform.
    fn main_transform(&self) -> Option<UvTransform> {
        let st = self.0.vector_properties.get("_MainTex").filter(|v| v.len() >= 4)?;
        let t = UvTransform { offset: Vec2::new(st[0], 1.0 - st[1] - st[3]), scale: Vec2::new(st[2], st[3]) };
        (t != UvTransform::default()).then_some(t)
    }
}

/// How the distinct `renderQueue` values of the transparent materials map to 1.0 offsets: the order is kept, the
/// highest transparent queue becomes 0 and the rest go down, the lowest z-writing queue becomes 0 and the rest go up.
struct Queues {
    transparent: Vec<i64>,
    zwrite: Vec<i64>,
}

impl Queues {
    fn new(props: &[WireMaterial0]) -> Self {
        let mut transparent = Vec::new();
        let mut zwrite = Vec::new();
        for p in props.iter().filter(|p| p.shader == MTOON_SHADER) {
            match Props(p).blend_mode() {
                BlendMode::Transparent => transparent.push(queue_offset(p, QUEUE_TRANSPARENT)),
                BlendMode::TransparentZWrite => zwrite.push(queue_offset(p, QUEUE_ZWRITE)),
                BlendMode::Opaque | BlendMode::Cutout => {}
            }
        }
        transparent.sort_unstable_by(|a, b| b.cmp(a));
        transparent.dedup();
        zwrite.sort_unstable();
        zwrite.dedup();
        Self { transparent, zwrite }
    }

    fn rank(list: &[i64], offset: i64) -> i32 {
        list.iter().position(|&o| o == offset).map_or(0, |i| i32::try_from(i).unwrap_or(9).min(9))
    }

    /// The 1.0 `renderQueueOffsetNumber` of a material in `mode` with raw offset `offset`.
    fn offset(&self, mode: BlendMode, offset: i64) -> i32 {
        match mode {
            BlendMode::Transparent => -Self::rank(&self.transparent, offset),
            BlendMode::TransparentZWrite => Self::rank(&self.zwrite, offset),
            BlendMode::Opaque | BlendMode::Cutout => 0,
        }
    }
}

fn queue_offset(p: &WireMaterial0, default: i64) -> i64 {
    p.render_queue.map_or(0, |q| q - default)
}

/// Replaces the materials that 0.x describes with `MToon` or unlit shaders by their converted versions. A material the
/// properties don't cover, or a shader this crate doesn't know (a standard-shader glTF material), is left as the glTF
/// layer read it.
pub(crate) fn read(props: &[WireMaterial0], materials: &mut [Material], report: &mut Report) {
    let queues = Queues::new(props);
    for (i, (p, m)) in props.iter().zip(materials.iter_mut()).enumerate() {
        if p.shader == MTOON_SHADER {
            *m = mtoon(p, m, &queues, i, report);
        } else if p.shader.contains("Unlit") {
            unlit(p, m);
        }
    }
}

/// 0.x unlit shaders: the glTF material keeps its colour and texture; the shader name says how alpha works.
fn unlit(p: &WireMaterial0, m: &mut Material) {
    m.shading = Shading::Unlit;
    m.alpha = if p.shader.contains("Cutout") {
        AlphaMode::Mask { cutoff: Props(p).float("_Cutoff", 0.5) }
    } else if p.shader.contains("Transparent") {
        AlphaMode::Blend
    } else {
        AlphaMode::Opaque
    };
}

fn mtoon(p: &WireMaterial0, glb: &Material, queues: &Queues, index: usize, report: &mut Report) -> Material {
    let q = Props(p);
    let st = q.main_transform();
    let tex = |name: &str| q.texture(name).map(|texture| TexRef { texture, uv_set: 0, transform: st });
    let mode = q.blend_mode();
    let mut toon = toon(&q, &tex);
    toon.transparent_with_zwrite = mode == BlendMode::TransparentZWrite;
    toon.render_queue_offset = queues
        .offset(mode, queue_offset(p, if toon.transparent_with_zwrite { QUEUE_ZWRITE } else { QUEUE_TRANSPARENT }));
    let mut out = Material::new(glb.name.clone(), Shading::Toon(Box::new(toon)));
    out.passthrough = glb.passthrough.clone();
    out.base_color = q.srgb("_Color", [1.0; 4]);
    out.base_color_texture = tex("_MainTex");
    out.normal_texture = tex("_BumpMap").map(|texture| NormalTexture { texture, scale: q.float("_BumpScale", 1.0) });
    out.emissive = q.linear("_EmissionColor");
    out.emissive_texture = tex("_EmissionMap");
    out.alpha = match mode {
        BlendMode::Cutout => AlphaMode::Mask { cutoff: q.float("_Cutoff", 0.5) },
        BlendMode::Transparent | BlendMode::TransparentZWrite => AlphaMode::Blend,
        BlendMode::Opaque => AlphaMode::Opaque,
    };
    // `_CullMode`: 0 off, 1 front, 2 back. glTF can't cull front faces, so only back-face culling stays single sided.
    let cull = choice(q.float("_CullMode", 2.0));
    out.double_sided = cull < 2;
    if cull == 1 {
        report.push(
            Finding::info("vrm0.cull_front", "front-face culling can't be expressed; the material is double sided")
                .at(format!("/extensions/VRM/materialProperties/{index}")),
        );
    }
    out
}

/// The `Toon` fields of an `MToon` 0.x material, using the shader's own defaults for properties the file omits.
#[allow(clippy::field_reassign_with_default)] // `Toon` is `#[non_exhaustive]`: only assignment can fill it from outside
fn toon(q: &Props<'_>, tex: &dyn Fn(&str) -> Option<TexRef>) -> Toon {
    let mut t = Toon::default();
    t.shade_color = q.srgb("_ShadeColor", [0.97, 0.81, 0.86, 1.0]).truncate();
    // A lit texture with no shade texture stands in for it (UniVRM's choice; 0.x models rely on it).
    t.shade_multiply_texture = tex("_ShadeTexture").or_else(|| tex("_MainTex"));
    (t.shading_toony, t.shading_shift) = shading_from_0x(q.float("_ShadeToony", 0.9), q.float("_ShadeShift", 0.0));
    t.gi_equalization = (1.0 - q.float("_IndirectLightIntensity", 0.1)).clamp(0.0, 1.0);
    t.matcap_texture = q.texture("_SphereAdd").map(TexRef::of);
    t.matcap_color = if t.matcap_texture.is_some() { Vec3::ONE } else { Vec3::ZERO };
    t.rim_color = q.srgb("_RimColor", [0.0, 0.0, 0.0, 1.0]).truncate();
    t.rim_fresnel_power = q.float("_RimFresnelPower", 1.0);
    t.rim_lift = q.float("_RimLift", 0.0);
    t.rim_multiply_texture = tex("_RimTexture");
    t.rim_lighting_mix = 1.0;
    let width = q.float("_OutlineWidth", 0.5);
    (t.outline_mode, t.outline_width) = match choice(q.float("_OutlineWidthMode", 0.0)) {
        1 => (OutlineMode::WorldCoordinates, width * 0.01),
        // 0.x measured the screen width as a percentage of half the screen height; 1.0 as a fraction of the height.
        2 => (OutlineMode::ScreenCoordinates, width * 0.01 * 0.5),
        _ => (OutlineMode::None, 0.0),
    };
    t.outline_width_texture = tex("_OutlineWidthTexture");
    t.outline_color = q.srgb("_OutlineColor", [0.0, 0.0, 0.0, 1.0]).truncate();
    // `_OutlineColorMode`: 0 is a fixed colour, which 1.0 says with a lighting mix of 0.
    t.outline_lighting_mix =
        if choice(q.float("_OutlineColorMode", 0.0)) == 0 { 0.0 } else { q.float("_OutlineLightingMix", 1.0) };
    t.uv_animation_mask = tex("_UvAnimMaskTexture");
    t.uv_scroll_x = q.float("_UvAnimScrollX", 0.0);
    t.uv_scroll_y = -q.float("_UvAnimScrollY", 0.0);
    t.uv_rotation = q.float("_UvAnimRotation", 0.0) * TAU;
    t
}

// ---------------------------------------------------------------------------------------------------------------
// Writing

/// 0.x properties for a scene material: `MToon` for toon, the unlit shaders for unlit, and the glTF-material marker for
/// plain PBR (the glTF material beside it carries everything).
pub(crate) fn write(m: &Material, index: usize, report: &mut Report) -> WireMaterial0 {
    let mut out = WireMaterial0 { name: m.name.clone(), ..WireMaterial0::default() };
    match &m.shading {
        Shading::Toon(t) => write_toon(m, t, index, &mut out, report),
        Shading::Unlit => write_unlit(m, &mut out),
        Shading::Pbr(_) => {
            GLTF_SHADER.clone_into(&mut out.shader);
            out.render_queue = Some(render_queue(m, false, 0));
        }
    }
    out
}

fn put_texture(out: &mut WireMaterial0, name: &str, t: Option<&TexRef>) {
    if let Some(t) = t {
        out.texture_properties.insert(name.to_owned(), i64::from(ix32(t.texture.index())));
    }
}

/// The `[ox, oy, sx, sy]` vector for `t`'s transform (Unity's V axis points up).
fn st_vector(t: Option<UvTransform>) -> Vec<f32> {
    let t = t.unwrap_or_default();
    vec![t.offset.x, 1.0 - t.offset.y - t.scale.y, t.scale.x, t.scale.y]
}

fn srgb_vector(c: Vec4) -> Vec<f32> {
    vec![srgb_encode(c.x), srgb_encode(c.y), srgb_encode(c.z), c.w]
}

fn render_queue(m: &Material, zwrite: bool, offset: i32) -> i64 {
    match BlendMode::of(m, zwrite) {
        BlendMode::Opaque => -1,
        BlendMode::Cutout => QUEUE_CUTOUT,
        BlendMode::TransparentZWrite => QUEUE_ZWRITE + i64::from(offset.clamp(0, 9)),
        BlendMode::Transparent => QUEUE_TRANSPARENT + i64::from(offset.clamp(-9, 0)),
    }
}

fn cutoff_of(m: &Material) -> f32 {
    if let AlphaMode::Mask { cutoff } = m.alpha { cutoff } else { 0.5 }
}

/// The transform every texture of a toon material shares in 0.x (the first one found), and whether any texture
/// disagrees with it.
fn shared_transform(m: &Material, t: &Toon) -> (Option<UvTransform>, bool) {
    let refs = [
        &m.base_color_texture,
        &m.emissive_texture,
        &t.shade_multiply_texture,
        &t.rim_multiply_texture,
        &t.outline_width_texture,
        &t.uv_animation_mask,
    ];
    let transforms: Vec<Option<UvTransform>> = refs
        .into_iter()
        .filter_map(|x| x.as_ref().map(|r| r.transform))
        .chain(m.normal_texture.as_ref().map(|n| n.texture.transform))
        .collect();
    let main = transforms.first().copied().flatten();
    (main, transforms.iter().any(|x| *x != main))
}

fn toon_floats(m: &Material, t: &Toon, mode: BlendMode) -> BTreeMap<String, f32> {
    let (toony, shift) = shading_to_0x(t.shading_toony, t.shading_shift);
    let (outline_mode, width) = match t.outline_mode {
        OutlineMode::None => (0.0, 0.5),
        OutlineMode::WorldCoordinates => (1.0, t.outline_width / 0.01),
        OutlineMode::ScreenCoordinates => (2.0, t.outline_width / (0.01 * 0.5)),
    };
    let [src, dst, zwrite] = mode.blend_state();
    let fixed_outline = t.outline_lighting_mix.abs() < f32::EPSILON;
    [
        ("_BlendMode", mode.unity()),
        ("_CullMode", if m.double_sided { 0.0 } else { 2.0 }),
        ("_Cutoff", cutoff_of(m)),
        ("_BumpScale", m.normal_texture.as_ref().map_or(1.0, |n| n.scale)),
        ("_ShadeShift", shift),
        ("_ShadeToony", toony),
        ("_IndirectLightIntensity", (1.0 - t.gi_equalization).clamp(0.0, 1.0)),
        ("_LightColorAttenuation", 0.0),
        ("_ReceiveShadowRate", 1.0),
        ("_ShadingGradeRate", 1.0),
        ("_RimFresnelPower", t.rim_fresnel_power),
        ("_RimLift", t.rim_lift),
        ("_RimLightingMix", t.rim_lighting_mix),
        ("_OutlineWidthMode", outline_mode),
        ("_OutlineWidth", width),
        ("_OutlineScaledMaxDistance", 1.0),
        ("_OutlineColorMode", if fixed_outline { 0.0 } else { 1.0 }),
        ("_OutlineLightingMix", t.outline_lighting_mix),
        ("_OutlineCullMode", 1.0),
        ("_UvAnimScrollX", t.uv_scroll_x),
        ("_UvAnimScrollY", -t.uv_scroll_y),
        ("_UvAnimRotation", t.uv_rotation / TAU),
        ("_SrcBlend", src),
        ("_DstBlend", dst),
        ("_ZWrite", zwrite),
        ("_DebugMode", 0.0),
        ("_MToonVersion", 39.0),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect()
}

/// The texture slots of a toon material by Unity property name.
fn toon_textures<'a>(m: &'a Material, t: &'a Toon) -> [(&'static str, Option<&'a TexRef>); 8] {
    [
        ("_MainTex", m.base_color_texture.as_ref()),
        ("_ShadeTexture", t.shade_multiply_texture.as_ref()),
        ("_BumpMap", m.normal_texture.as_ref().map(|n| &n.texture)),
        ("_EmissionMap", m.emissive_texture.as_ref()),
        ("_RimTexture", t.rim_multiply_texture.as_ref()),
        ("_OutlineWidthTexture", t.outline_width_texture.as_ref()),
        ("_UvAnimMaskTexture", t.uv_animation_mask.as_ref()),
        ("_SphereAdd", t.matcap_texture.as_ref()),
    ]
}

fn write_toon(m: &Material, t: &Toon, index: usize, out: &mut WireMaterial0, report: &mut Report) {
    MTOON_SHADER.clone_into(&mut out.shader);
    let mode = BlendMode::of(m, t.transparent_with_zwrite);
    out.render_queue = Some(render_queue(m, t.transparent_with_zwrite, t.render_queue_offset));
    let at = format!("materials/{index}");
    if t.shading_shift_texture.is_some() {
        let msg = "0.x MToon has no shading shift texture; it was dropped";
        report.push(Finding::warning("vrm0.mtoon_shift_texture", msg).at(&at));
    }
    let (main, differs) = shared_transform(m, t);
    if differs {
        let msg = "0.x MToon has one texture transform for all textures; the first was used";
        report.push(Finding::warning("vrm0.texture_transform_shared", msg).at(&at));
    }
    out.float_properties = toon_floats(m, t, mode);
    let vectors = [
        ("_Color", srgb_vector(m.base_color)),
        ("_ShadeColor", srgb_vector(t.shade_color.extend(1.0))),
        ("_RimColor", srgb_vector(t.rim_color.extend(1.0))),
        ("_OutlineColor", srgb_vector(t.outline_color.extend(1.0))),
        ("_EmissionColor", vec![m.emissive.x, m.emissive.y, m.emissive.z, 1.0]),
    ];
    out.vector_properties = vectors.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
    for (name, texture) in toon_textures(m, t) {
        if texture.is_some() {
            // The matcap is mapped by view-space normal, so it takes no UV transform.
            out.vector_properties.insert(name.to_owned(), st_vector(if name == "_SphereAdd" { None } else { main }));
        }
        put_texture(out, name, texture);
    }
    let fixed_outline = t.outline_lighting_mix.abs() < f32::EPSILON;
    out.keyword_map = [
        ("MTOON_OUTLINE_COLOR_FIXED", fixed_outline),
        ("MTOON_OUTLINE_COLOR_MIXED", !fixed_outline),
        ("MTOON_OUTLINE_WIDTH_SCREEN", t.outline_mode == OutlineMode::ScreenCoordinates),
        ("MTOON_OUTLINE_WIDTH_WORLD", t.outline_mode == OutlineMode::WorldCoordinates),
        ("_ALPHABLEND_ON", mode.is_blended()),
        ("_ALPHATEST_ON", mode == BlendMode::Cutout),
        ("_NORMALMAP", m.normal_texture.is_some()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    out.tag_map = BTreeMap::from([("RenderType".to_owned(), mode.render_type().to_owned())]);
}

fn write_unlit(m: &Material, out: &mut WireMaterial0) {
    let shader = match m.alpha {
        AlphaMode::Opaque => "VRM/UnlitTexture",
        AlphaMode::Mask { .. } => "VRM/UnlitCutout",
        AlphaMode::Blend => "VRM/UnlitTransparent",
    };
    shader.clone_into(&mut out.shader);
    let mode = BlendMode::of(m, false);
    out.render_queue = Some(render_queue(m, false, 0));
    let [src, dst, zwrite] = mode.blend_state();
    out.float_properties = [
        ("_Cutoff", cutoff_of(m)),
        ("_BlendMode", mode.unity()),
        ("_SrcBlend", src),
        ("_DstBlend", dst),
        ("_ZWrite", zwrite),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    if let Some(t) = &m.base_color_texture {
        out.vector_properties.insert("_MainTex".into(), st_vector(t.transform));
    }
    put_texture(out, "_MainTex", m.base_color_texture.as_ref());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shading_range_converts_both_ways() {
        // Defaults of 0.x MToon: range [0, 0.1] -> margin 0.95, centre 0.05 -> shift -0.05 (worked out by hand).
        let (toony, shift) = shading_from_0x(0.9, 0.0);
        assert!((toony - 0.95).abs() < 1e-6 && (shift + 0.05).abs() < 1e-6, "{toony} {shift}");
        for (t0, s0) in [(0.9, 0.0), (0.5, -0.2), (1.0, 0.3), (0.0, 0.0), (0.7, 0.6)] {
            let (t, s) = shading_from_0x(t0, s0);
            let (back_t, back_s) = shading_to_0x(t, s);
            assert!(
                (back_t - t0).abs() < 1e-5 && (back_s - s0).abs() < 1e-5,
                "({t0},{s0}) -> ({t},{s}) -> ({back_t},{back_s})"
            );
        }
        // Toony 0 with shift 0 is the widest band, [0, 1] in 0.x.
        assert_eq!(shading_from_0x(0.0, 0.0), (0.5, -0.5));
    }

    #[test]
    fn render_queues_keep_their_order_and_start_at_zero() {
        let mat = |mode: f32, queue: i64| WireMaterial0 {
            shader: MTOON_SHADER.into(),
            render_queue: Some(queue),
            float_properties: [("_BlendMode".to_owned(), mode)].into(),
            ..WireMaterial0::default()
        };
        let props = [mat(2.0, 3000), mat(2.0, 2990), mat(2.0, 2990), mat(3.0, 2505), mat(3.0, 2501), mat(0.0, -1)];
        let mut materials: Vec<Material> = (0..6).map(|i| Material::new(format!("m{i}"), Shading::Unlit)).collect();
        read(&props, &mut materials, &mut Report::new());
        let offsets: Vec<i32> = materials
            .iter()
            .map(|m| if let Shading::Toon(t) = &m.shading { t.render_queue_offset } else { i32::MAX })
            .collect();
        assert_eq!(offsets, [0, -1, -1, 1, 0, 0]);
    }
}
