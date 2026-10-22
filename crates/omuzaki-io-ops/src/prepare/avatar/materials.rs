//! Materials, textures and the `MToon` findings (design D7, D8 "Materials are retained exactly"; BY10).
//!
//! Each source material becomes a runtime [`MaterialSpec`] through [`import_material`], with the person's edits of
//! `AvatarDef.materials` applied to its colours first, and its base texture's transform kept as the material's
//! `uv_transform` (what an expression's UV bind moves). The renderer cannot draw every `MToon` group yet; for each
//! group a material uses and `MtoonSupport` lacks, one info finding `look.mtoon_not_drawn` names it, in place of
//! `import_material`'s per-field notes for that group. Every other `material.*` finding passes through. A group the
//! renderer draws gets no finding: once an LK-07 task sets its capability bit, the finding disappears by itself.
//!
//! For a renderer that draws `MToon`'s lighting (`MtoonSupport::LIGHTING`, #592), each `MToon` material's lighting
//! goes in `avatar.mtoon` as the file has it (the shade, shift, matcap, rim, emissive, normal and UV animation, with
//! their textures), and its render-queue offset becomes its meshes' draw order as three-vrm sets it. The
//! [`MaterialSpec`]'s toon approximation stays for a renderer without it.
//!
//! Only the textures the renderer draws are decoded: base colours, outline width textures and the `MToon` textures
//! where it draws them, once per texture and colour space, in the colour space of their slot, at full size: nothing is
//! downscaled. A material's `MToon` outline beyond the kit's line (its width mode, width texture and lighting mix, #647)
//! goes in `avatar.outlines`, as far as the renderer draws it. `retained` keeps every source material, texture and
//! image exactly as read, so export writes the file's own values, never one converted back from the runtime.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use omuzaki_avatar_api::avatar::{AvatarOutline, ImportedAvatar};
use omuzaki_avatar_api::def::{AvatarDef, MaterialEdit};
use omuzaki_avatar_api::retained::{RetainedImage, RetainedMaterial};
use omuzaki_char_api::{MaterialName, MaterialSpec, TextureName};
use omuzaki_core::colour::Colour;
use omuzaki_core::time::Clock;
use omuzaki_io_api::{self as io, Finding, IoScene, Report};
use omuzaki_render_api::{ColourSpace, Mtoon, MtoonSupport, OutlineWidthMode, UvTransform};
use omuzaki_store_api::ContentHash;

use super::skeleton::{slug_stem, unique_slug};
use crate::convert::material::import_material;
use crate::convert::texture::{decode_png, slot_colour_space};

/// The runtime names the scene's materials became, and the colours each starts from.
#[derive(Clone, Debug, Default)]
pub struct MaterialNames {
    /// The runtime name of each scene material, by index.
    by_index: Vec<MaterialName>,
    /// The material made for primitives the file gives none (glTF's default material), if any primitive needs it.
    default: Option<MaterialName>,
    /// Each scene material with the person's edits applied: the colours expressions' binds start from.
    edited: Vec<io::Material>,
    /// The draw order of each scene material's meshes among blended ones, by index (`MToon`'s render queue).
    orders: Vec<i32>,
}

impl MaterialNames {
    /// The runtime material of scene material `material`, or of the default material for `None`.
    pub fn of(&self, material: Option<u32>) -> Option<MaterialName> {
        match material {
            Some(m) => self.by_index.get(usize::try_from(m).ok()?).cloned(),
            None => self.default.clone(),
        }
    }

    /// The draw order among blended meshes of a mesh drawn with scene material `material` (0 for the default
    /// material): three-vrm's `renderOrder`, 0 for an `MToon` material that writes depth and 19 for one that doesn't,
    /// plus its render-queue offset, where the renderer orders by it.
    pub fn order(&self, material: Option<u32>) -> i32 {
        material.and_then(|m| self.orders.get(usize::try_from(m).ok()?).copied()).unwrap_or(0)
    }

    /// Scene material `material` as the runtime starts it: the file's values with the person's edits.
    pub fn edited(&self, material: u32) -> Option<&io::Material> {
        self.edited.get(usize::try_from(material).ok()?)
    }
}

/// One `MToon` group the renderer may not draw (D7): its capability bit, its name for people, whether a material uses
/// it, and the `import_material` notes it replaces (locations under the material, such as `shading/<field>`).
struct Group {
    bit: MtoonSupport,
    name: &'static str,
    used: fn(&io::Material, &io::Toon) -> bool,
    notes: &'static [&'static str],
}

/// The groups of D7, in the order the design lists them, the outline width texture (#647), then the lighting model
/// and blended depth writes (#592).
const GROUPS: [Group; 10] = [
    Group {
        bit: MtoonSupport::MATCAP,
        name: "the matcap",
        used: |_, t| t.matcap_texture.is_some(),
        notes: &["shading/matcap_texture"],
    },
    Group {
        bit: MtoonSupport::GI_EQUALISATION,
        name: "GI equalisation",
        used: |_, t| differs(t.gi_equalization, io::Toon::default().gi_equalization),
        notes: &["shading/gi_equalization"],
    },
    Group {
        bit: MtoonSupport::UV_ANIMATION,
        name: "UV animation",
        used: |_, t| {
            [t.uv_scroll_x, t.uv_scroll_y, t.uv_rotation].iter().any(|v| differs(*v, 0.0))
                || t.uv_animation_mask.is_some()
        },
        notes: &["shading/uv_animation"],
    },
    Group {
        bit: MtoonSupport::RENDER_QUEUE_OFFSET,
        name: "the render queue offset",
        used: |_, t| t.render_queue_offset != 0,
        notes: &["shading/render_queue_offset"],
    },
    Group {
        bit: MtoonSupport::OUTLINE_WIDTH_SCREEN,
        name: "outline widths in screen coordinates",
        used: |_, t| t.outline_mode == io::OutlineMode::ScreenCoordinates,
        notes: &["shading/outline_mode"],
    },
    Group {
        bit: MtoonSupport::RIM_LIGHTING_MIX,
        name: "the rim lighting mix",
        used: |_, t| differs(t.rim_lighting_mix, io::Toon::default().rim_lighting_mix),
        notes: &["shading/rim_lighting_mix"],
    },
    Group {
        bit: MtoonSupport::OUTLINE_LIGHTING_MIX,
        name: "the outline lighting mix",
        used: |_, t| {
            t.outline_mode != io::OutlineMode::None
                && differs(t.outline_lighting_mix, io::Toon::default().outline_lighting_mix)
        },
        notes: &["shading/outline_lighting_mix"],
    },
    Group {
        bit: MtoonSupport::OUTLINE_WIDTH_TEXTURE,
        name: "the outline width texture",
        used: |_, t| t.outline_mode != io::OutlineMode::None && t.outline_width_texture.is_some(),
        notes: &["shading/outline_width_texture"],
    },
    Group {
        bit: MtoonSupport::LIGHTING,
        name: "MToon's own lighting (shade and shift textures, rim lift and texture, emissive and normal textures)",
        used: |m, t| {
            (t.shade_multiply_texture.is_some() && t.shade_multiply_texture != m.base_color_texture)
                || t.shading_shift_texture.is_some()
                || differs(t.rim_lift, 0.0)
                || t.rim_multiply_texture.is_some()
                || m.emissive_texture.is_some()
                || m.normal_texture.is_some()
        },
        notes: &[
            "shading/shade_color",
            "shading/shading_shift",
            "shading/shade_multiply_texture",
            "shading/shading_shift_texture",
            "shading/rim_lift",
            "shading/rim_multiply_texture",
            "emissive_texture",
            "normal_texture",
        ],
    },
    Group {
        bit: MtoonSupport::BLEND_DEPTH_WRITE,
        name: "depth writes for blended surfaces",
        used: |_, t| t.transparent_with_zwrite,
        notes: &["shading/transparent_with_zwrite"],
    },
];

/// How far a value read from a file may differ from a default and still be the default (file floats pass through
/// JSON); `import_material` uses the same margin.
const DEFAULT_TOLERANCE: f32 = 1e-6;

/// True if `value` is not `default` (a field the file sets).
fn differs(value: f32, default: f32) -> bool {
    (value - default).abs() > DEFAULT_TOLERANCE
}

/// Fills `avatar.materials`, `avatar.textures` and the retained materials, textures and images; returns the names
/// the meshes and binds use.
/// Decoding is timed on `clock`, when given, into `decode`.
pub fn convert(
    scene: &IoScene,
    def: &AvatarDef,
    mtoon: MtoonSupport,
    clock: Option<&dyn Clock>,
    decode: &mut Duration,
    avatar: &mut ImportedAvatar,
    report: &mut Report,
) -> MaterialNames {
    let mut taken = BTreeSet::new();
    let by_index: Vec<MaterialName> = scene
        .materials
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let stem = slug_stem(&m.name);
            let stem = if stem.is_empty() { format!("material_{i}") } else { stem };
            MaterialName(unique_slug(&stem, &mut taken))
        })
        .collect();
    let started = clock.map(Clock::now);
    let mut decoded = Decoded::default();
    for (slot, refs) in slots_drawn(scene, mtoon) {
        decoded.decode(scene, &refs, slot, avatar, report);
    }
    if let Some((start, clock)) = started.zip(clock) {
        *decode = super::between(start, clock.now());
    }
    let edited = apply_edits(scene, def, &by_index, report);
    let mut orders = Vec::with_capacity(edited.len());
    for (i, (name, material)) in by_index.iter().zip(&edited).enumerate() {
        let at = format!("materials/{i}");
        let mut notes = Report::new();
        let resolve = |t: &io::TexRef| decoded.get(t.texture.0, BASE_SLOT);
        let mut spec = import_material(material, &resolve, &at, &mut notes);
        let carried = carry_uv_transform(material, &mut spec);
        report.extend(mtoon_findings(material, mtoon, &at, carried, notes));
        if let Some(outline) = outline_of(material, mtoon, &decoded) {
            avatar.outlines.insert(name.clone(), outline);
        }
        if let Some(lighting) = lighting_of(material, mtoon, &decoded, &at, report) {
            avatar.mtoon.insert(name.clone(), lighting);
        }
        orders.push(order_of(material, mtoon));
        avatar.materials.insert(name.clone(), spec);
    }
    let needs_default = scene.meshes.iter().flat_map(|m| &m.primitives).any(|p| p.material.is_none());
    let default = needs_default.then(|| {
        let name = MaterialName(unique_slug("default", &mut taken));
        avatar.materials.insert(name.clone(), MaterialSpec::toon(Colour::WHITE));
        name
    });
    let retained = &mut avatar.retained;
    retained.materials =
        by_index.iter().zip(&scene.materials).map(|(n, m)| RetainedMaterial::new(n.clone(), m.clone())).collect();
    retained.textures.clone_from(&scene.textures);
    retained.images = scene
        .images
        .iter()
        .map(|image| RetainedImage::new(image.name.clone(), image.mime, ContentHash::of(image.bytes.as_slice())))
        .collect();
    MaterialNames { by_index, default, edited, orders }
}

/// The glTF slot of a base colour texture, which fixes its colour space (sRGB).
const BASE_SLOT: &str = "baseColorTexture";
/// The `MToon` slot of an outline width texture (linear data).
const WIDTH_SLOT: &str = "outlineWidthMultiplyTexture";

/// Every texture slot the renderer draws (`mtoon`), with the texture references the scene's materials put in it. The
/// slot names are `omuzaki_io_api::Material::texture_refs`', which fix each one's colour space.
fn slots_drawn(scene: &IoScene, mtoon: MtoonSupport) -> Vec<(&'static str, Vec<io::TexRef>)> {
    type Slot = (&'static str, MtoonSupport, fn(&io::Material, &io::Toon) -> Option<io::TexRef>);
    const TOON_SLOTS: [Slot; 8] = [
        (WIDTH_SLOT, MtoonSupport::OUTLINE_WIDTH_TEXTURE, |_, t| {
            t.outline_width_texture.filter(|_| t.outline_mode != io::OutlineMode::None)
        }),
        ("shadeMultiplyTexture", MtoonSupport::LIGHTING, |_, t| t.shade_multiply_texture),
        ("shadingShiftTexture", MtoonSupport::LIGHTING, |_, t| t.shading_shift_texture.map(|s| s.texture)),
        ("matcapTexture", MtoonSupport::MATCAP, |_, t| t.matcap_texture),
        ("rimMultiplyTexture", MtoonSupport::LIGHTING, |_, t| t.rim_multiply_texture),
        ("emissiveTexture", MtoonSupport::LIGHTING, |m, _| m.emissive_texture),
        ("normalTexture", MtoonSupport::LIGHTING, |m, _| m.normal_texture.map(|n| n.texture)),
        ("uvAnimationMaskTexture", MtoonSupport::UV_ANIMATION, |_, t| t.uv_animation_mask),
    ];
    let mut slots = vec![(BASE_SLOT, scene.materials.iter().filter_map(|m| m.base_color_texture).collect())];
    for (slot, bit, read) in TOON_SLOTS {
        // A slot past the outline is drawn only with the lighting model it belongs to.
        if mtoon.contains(bit) && (slot == WIDTH_SLOT || mtoon.contains(MtoonSupport::LIGHTING)) {
            let refs = scene.materials.iter().filter_map(|m| toon_of(m).and_then(|t| read(m, t))).collect();
            slots.push((slot, refs));
        }
    }
    slots
}

/// The textures decoded so far: each scene texture once per colour space, by its runtime name.
#[derive(Default)]
struct Decoded {
    names: BTreeMap<(u32, bool), TextureName>,
    taken: BTreeSet<String>,
}

impl Decoded {
    /// The runtime name of scene texture `index` as `slot` reads it, if it was decoded.
    fn get(&self, index: u32, slot: &str) -> Option<TextureName> {
        self.names.get(&(index, slot_colour_space(slot) == ColourSpace::Srgb)).cloned()
    }

    /// Decodes each texture `refs` use that isn't decoded in `slot`'s colour space yet, naming it uniquely. A texture
    /// that cannot be decoded is reported (`texture.unreadable`) and left out, so the material that uses it reports it
    /// missing.
    fn decode(
        &mut self,
        scene: &IoScene,
        refs: &[io::TexRef],
        slot: &str,
        avatar: &mut ImportedAvatar,
        report: &mut Report,
    ) {
        let space = slot_colour_space(slot);
        let used: BTreeSet<u32> = refs.iter().map(|t| t.texture.0).collect();
        for index in used {
            let key = (index, space == ColourSpace::Srgb);
            if self.names.contains_key(&key) {
                continue;
            }
            let at = format!("textures/{index}");
            let Some(texture) = usize::try_from(index).ok().and_then(|i| scene.textures.get(i)) else { continue };
            let Some(image) = scene.images.get(texture.image.index()) else { continue };
            match decode_png(image, space, &texture.sampler, &at, report) {
                Ok(data) => {
                    let stem = slug_stem(&image.name);
                    let stem = if stem.is_empty() { format!("texture_{index}") } else { stem };
                    let name = TextureName(unique_slug(&stem, &mut self.taken));
                    avatar.textures.insert(name.clone(), data);
                    self.names.insert(key, name);
                }
                Err(e) => {
                    let hint = "save the texture as a PNG and import the model again";
                    report.push(Finding::warning("texture.unreadable", e.to_string()).at(at).hint(hint));
                }
            }
        }
    }
}

/// The `MToon` block of a material, if it is toon.
fn toon_of(material: &io::Material) -> Option<&io::Toon> {
    match &material.shading {
        io::Shading::Toon(toon) => Some(toon),
        _ => None,
    }
}

/// The outline of `material` as far as the renderer draws its `MToon` outline (#647), or `None` for no outline. A
/// world width is drawn as metres at any distance on every renderer; a screen width, the lighting mix and the width
/// texture only where `mtoon` says so, and otherwise the kit line's approximation stands (`import_material`).
fn outline_of(material: &io::Material, mtoon: MtoonSupport, decoded: &Decoded) -> Option<AvatarOutline> {
    let toon = toon_of(material)?;
    let mut outline = match toon.outline_mode {
        io::OutlineMode::WorldCoordinates => AvatarOutline::new(OutlineWidthMode::World, toon.outline_width),
        io::OutlineMode::ScreenCoordinates if mtoon.contains(MtoonSupport::OUTLINE_WIDTH_SCREEN) => {
            AvatarOutline::new(OutlineWidthMode::Screen, toon.outline_width)
        }
        // No line, or a screen width this renderer can't draw: the kit line's approximation stands.
        io::OutlineMode::None | io::OutlineMode::ScreenCoordinates => return None,
    };
    if !(outline.width >= 0.0 && outline.width.is_finite()) {
        return None;
    }
    if mtoon.contains(MtoonSupport::OUTLINE_LIGHTING_MIX) {
        outline.lighting_mix = toon.outline_lighting_mix.clamp(0.0, 1.0);
    }
    outline.width_texture = toon.outline_width_texture.and_then(|t| decoded.get(t.texture.0, WIDTH_SLOT));
    Some(outline)
}

/// The `MToon` lighting of `material` (#592), for a renderer that draws it, with its textures as decoded. A group the
/// renderer lacks is left at the value that turns it off. A texture read through a UV set or transform of its own is
/// read with the base texture's (the material has one UV transform) and reported.
fn lighting_of(
    material: &io::Material,
    mtoon: MtoonSupport,
    decoded: &Decoded,
    at: &str,
    report: &mut Report,
) -> Option<Mtoon<TextureName>> {
    let toon = toon_of(material)?;
    if !mtoon.contains(MtoonSupport::LIGHTING) {
        return None;
    }
    let base_transform = material.base_color_texture.and_then(|t| t.transform).unwrap_or_default();
    let mut texture = |texture: Option<io::TexRef>, slot: &str, field: &str| {
        let texture = texture?;
        if texture.uv_set != 0 || texture.transform.unwrap_or_default() != base_transform {
            let msg = "the texture has a UV set or transform of its own; it is read like the base colour texture";
            let hint = "nothing to do: it is kept and exported as the file had it";
            report.push(Finding::info("material.approximated", msg).at(format!("{at}/{field}")).hint(hint));
        }
        decoded.get(texture.texture.0, slot)
    };
    let has = |bit| mtoon.contains(bit);
    let animated = has(MtoonSupport::UV_ANIMATION);
    let colour = |c: omuzaki_core::glam::Vec3| Colour::from_linear(c, 1.0);
    let lighting = Mtoon {
        shade: colour(toon.shade_color),
        shade_texture: texture(toon.shade_multiply_texture, "shadeMultiplyTexture", "shading/shade_multiply_texture"),
        shading_shift: finite_or(toon.shading_shift, 0.0),
        shading_shift_texture: texture(
            toon.shading_shift_texture.map(|s| s.texture),
            "shadingShiftTexture",
            "shading/shading_shift_texture",
        ),
        shading_shift_scale: toon.shading_shift_texture.map_or(1.0, |s| finite_or(s.scale, 1.0)),
        shading_toony: unit(toon.shading_toony),
        gi_equalisation: if has(MtoonSupport::GI_EQUALISATION) { unit(toon.gi_equalization) } else { 0.0 },
        matcap: colour(toon.matcap_color),
        matcap_texture: if has(MtoonSupport::MATCAP) {
            texture(toon.matcap_texture, "matcapTexture", "shading/matcap_texture")
        } else {
            None
        },
        rim: colour(toon.rim_color),
        rim_texture: texture(toon.rim_multiply_texture, "rimMultiplyTexture", "shading/rim_multiply_texture"),
        rim_fresnel_power: finite_or(toon.rim_fresnel_power, 5.0).max(0.0),
        rim_lift: finite_or(toon.rim_lift, 0.0),
        rim_lighting_mix: if has(MtoonSupport::RIM_LIGHTING_MIX) { unit(toon.rim_lighting_mix) } else { 0.0 },
        emissive_texture: texture(material.emissive_texture, "emissiveTexture", "emissive_texture"),
        normal_texture: texture(material.normal_texture.map(|n| n.texture), "normalTexture", "normal_texture"),
        normal_scale: material.normal_texture.map_or(1.0, |n| finite_or(n.scale, 1.0)),
        uv_scroll: if animated {
            omuzaki_core::glam::Vec2::new(finite_or(toon.uv_scroll_x, 0.0), finite_or(toon.uv_scroll_y, 0.0))
        } else {
            omuzaki_core::glam::Vec2::ZERO
        },
        uv_rotation: if animated { finite_or(toon.uv_rotation, 0.0) } else { 0.0 },
        uv_animation_mask: if animated {
            texture(toon.uv_animation_mask, "uvAnimationMaskTexture", "shading/uv_animation")
        } else {
            None
        },
        blend_writes_depth: has(MtoonSupport::BLEND_DEPTH_WRITE) && toon.transparent_with_zwrite,
    };
    Some(lighting)
}

/// `value`, or `fallback` for a value that isn't finite (the file's own value stays in `retained`).
fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

/// `value` clamped to `0..=1`, 0 for one that isn't finite.
fn unit(value: f32) -> f32 {
    finite_or(value, 0.0).clamp(0.0, 1.0)
}

/// The draw order of `material`'s meshes among blended ones (three-vrm's `_parseRenderOrder`): 0 for an `MToon`
/// material that writes depth, 19 for one that doesn't, plus its render-queue offset (the spec's -9..=9); 0 for any
/// other material or a renderer that doesn't order by it.
fn order_of(material: &io::Material, mtoon: MtoonSupport) -> i32 {
    let Some(toon) = toon_of(material).filter(|_| mtoon.contains(MtoonSupport::RENDER_QUEUE_OFFSET)) else {
        return 0;
    };
    let writes_depth = toon.transparent_with_zwrite && mtoon.contains(MtoonSupport::BLEND_DEPTH_WRITE);
    (if writes_depth { 0 } else { 19 }) + toon.render_queue_offset.clamp(-9, 9)
}

/// The scene's materials with `def.materials`' edits applied field by field; an edit naming no material is reported.
fn apply_edits(scene: &IoScene, def: &AvatarDef, names: &[MaterialName], report: &mut Report) -> Vec<io::Material> {
    let mut edited = scene.materials.clone();
    for (name, edit) in &def.materials {
        if let Some(material) = names.iter().position(|n| n == name).and_then(|i| edited.get_mut(i)) {
            apply_edit(material, edit);
        } else {
            let msg = format!("the material edit for '{name}' names a material the model doesn't have");
            let hint = "the model changed since the edit was made; edit the material again";
            report.push(
                Finding::warning("avatar.material_edit_unknown", msg).at(format!("avatar/materials/{name}")).hint(hint),
            );
        }
    }
    edited
}

/// Applies one material's edits: each field set replaces the file's value, the rest stay (D8).
pub(crate) fn apply_edit(material: &mut io::Material, edit: &MaterialEdit) {
    let rgb = |c: [f32; 3]| omuzaki_core::glam::Vec3::from_array(c);
    if let Some(base) = edit.base {
        material.base_color = omuzaki_core::glam::Vec4::from_array(base);
    }
    if let Some(emission) = edit.emission {
        material.emissive = rgb(emission);
    }
    if let io::Shading::Toon(toon) = &mut material.shading {
        if let Some(c) = edit.shade {
            toon.shade_color = rgb(c);
        }
        if let Some(c) = edit.matcap {
            toon.matcap_color = rgb(c);
        }
        if let Some(c) = edit.rim {
            toon.rim_color = rgb(c);
        }
        if let Some(c) = edit.outline {
            toon.outline_color = rgb(c);
        }
    }
}

/// Keeps the base texture's transform as the material's UV transform, so UV binds start from it. Returns true when it
/// was carried (UV set 0), so `import_material`'s note that it is dropped no longer applies.
fn carry_uv_transform(material: &io::Material, spec: &mut MaterialSpec) -> bool {
    let Some(texture) = material.base_color_texture.filter(|t| t.uv_set == 0) else { return false };
    if let Some(t) = texture.transform {
        spec.uv_transform = UvTransform { scale: t.scale, offset: t.offset };
    }
    true
}

/// `import_material`'s `notes` for the material at `at`, with each `MToon` group's notes replaced: one
/// `look.mtoon_not_drawn` for a group the material uses and the renderer cannot draw, nothing for one it can. When
/// `carried`, the base texture's "transform not kept" note is dropped.
fn mtoon_findings(material: &io::Material, mtoon: MtoonSupport, at: &str, carried: bool, notes: Report) -> Report {
    let toon = match &material.shading {
        io::Shading::Toon(toon) => Some(toon.as_ref()),
        _ => None,
    };
    // A group's notes go when the renderer draws the group, or when the group's own finding takes their place.
    let group_note = |location: &str| {
        GROUPS.iter().any(|g| {
            let covered = mtoon.contains(g.bit) || toon.is_some_and(|t| (g.used)(material, t));
            covered && g.notes.iter().any(|field| location == format!("{at}/{field}"))
        })
    };
    let mut out = Report::new();
    for finding in notes.findings {
        let location = finding.location.as_deref().unwrap_or_default();
        let replaced = toon.is_some() && group_note(location);
        let uv_note = carried && finding.rule == "material.not_drawn" && location == format!("{at}/base_color_texture");
        if !replaced && !uv_note {
            out.push(finding);
        }
    }
    for group in GROUPS.iter().filter(|g| toon.is_some_and(|t| (g.used)(material, t)) && !mtoon.contains(g.bit)) {
        let msg = format!("material '{}' uses {}, which this renderer doesn't draw yet", material.name, group.name);
        let hint = "nothing to do: it is kept and exported as the file had it";
        out.push(Finding::info("look.mtoon_not_drawn", msg).at(format!("{at}/shading")).hint(hint));
    }
    out
}
