//! Materials, textures and the `MToon` findings (design D7, D8 "Materials are retained exactly"; BY10).
//!
//! Each source material becomes a runtime [`MaterialSpec`] through [`import_material`], with the person's edits of
//! `AvatarDef.materials` applied to its colours first, and its base texture's transform kept as the material's
//! `uv_transform` (what an expression's UV bind moves). The renderer cannot draw every `MToon` group yet; for each
//! group a material uses and `MtoonSupport` lacks, one info finding `look.mtoon_not_drawn` names it, in place of
//! `import_material`'s per-field notes for that group. Every other `material.*` finding passes through. A group the
//! renderer draws gets no finding: once an LK-07 task sets its capability bit, the finding disappears by itself.
//!
//! Only base-colour textures are decoded (the renderer draws nothing else of a material yet), once each, in the colour
//! space of their slot, at full size: nothing is downscaled. `retained` keeps every source material, texture and
//! image exactly as read, so export writes the file's own values, never one converted back from the runtime.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_avatar_api::def::{AvatarDef, MaterialEdit};
use omuzaki_avatar_api::retained::{RetainedImage, RetainedMaterial};
use omuzaki_char_api::{MaterialName, MaterialSpec, TextureName};
use omuzaki_core::colour::Colour;
use omuzaki_core::time::Clock;
use omuzaki_io_api::{self as io, Finding, IoScene, Report};
use omuzaki_render_api::{MtoonSupport, UvTransform};
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
}

impl MaterialNames {
    /// The runtime material of scene material `material`, or of the default material for `None`.
    pub fn of(&self, material: Option<u32>) -> Option<MaterialName> {
        match material {
            Some(m) => self.by_index.get(usize::try_from(m).ok()?).cloned(),
            None => self.default.clone(),
        }
    }

    /// Scene material `material` as the runtime starts it: the file's values with the person's edits.
    pub fn edited(&self, material: u32) -> Option<&io::Material> {
        self.edited.get(usize::try_from(material).ok()?)
    }
}

/// One `MToon` group the renderer may not draw (D7): its capability bit, its name for people, whether a material uses
/// it, and the `import_material` notes it replaces (`shading/<field>` locations).
struct Group {
    bit: MtoonSupport,
    name: &'static str,
    used: fn(&io::Toon) -> bool,
    notes: &'static [&'static str],
}

/// The seven groups of D7, in the order the design lists them.
const GROUPS: [Group; 7] = [
    Group {
        bit: MtoonSupport::MATCAP,
        name: "the matcap",
        used: |t| t.matcap_texture.is_some(),
        notes: &["matcap_texture"],
    },
    Group {
        bit: MtoonSupport::GI_EQUALISATION,
        name: "GI equalisation",
        used: |t| differs(t.gi_equalization, io::Toon::default().gi_equalization),
        notes: &["gi_equalization"],
    },
    Group {
        bit: MtoonSupport::UV_ANIMATION,
        name: "UV animation",
        used: |t| {
            [t.uv_scroll_x, t.uv_scroll_y, t.uv_rotation].iter().any(|v| differs(*v, 0.0))
                || t.uv_animation_mask.is_some()
        },
        notes: &["uv_animation"],
    },
    Group {
        bit: MtoonSupport::RENDER_QUEUE_OFFSET,
        name: "the render queue offset",
        used: |t| t.render_queue_offset != 0,
        notes: &["render_queue_offset"],
    },
    Group {
        bit: MtoonSupport::OUTLINE_WIDTH_SCREEN,
        name: "outline widths in screen coordinates",
        used: |t| t.outline_mode == io::OutlineMode::ScreenCoordinates,
        notes: &["outline_mode"],
    },
    Group {
        bit: MtoonSupport::RIM_LIGHTING_MIX,
        name: "the rim lighting mix",
        used: |t| differs(t.rim_lighting_mix, io::Toon::default().rim_lighting_mix),
        notes: &["rim_lighting_mix"],
    },
    Group {
        bit: MtoonSupport::OUTLINE_LIGHTING_MIX,
        name: "the outline lighting mix",
        used: |t| {
            t.outline_mode != io::OutlineMode::None
                && differs(t.outline_lighting_mix, io::Toon::default().outline_lighting_mix)
        },
        notes: &["outline_lighting_mix"],
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
    let textures = decode_textures(scene, avatar, report);
    if let Some((start, clock)) = started.zip(clock) {
        *decode = super::between(start, clock.now());
    }
    let edited = apply_edits(scene, def, &by_index, report);
    for (i, (name, material)) in by_index.iter().zip(&edited).enumerate() {
        let at = format!("materials/{i}");
        let mut notes = Report::new();
        let resolve = |t: &io::TexRef| textures.get(&t.texture.0).cloned();
        let mut spec = import_material(material, &resolve, &at, &mut notes);
        let carried = carry_uv_transform(material, &mut spec);
        report.extend(mtoon_findings(material, mtoon, &at, carried, notes));
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
    MaterialNames { by_index, default, edited }
}

/// Decodes every texture a material uses as its base colour, once, as sRGB; returns each one's runtime name by scene
/// texture index. A texture that cannot be decoded is reported (`texture.unreadable`) and left out, so the material
/// that uses it reports it missing.
fn decode_textures(scene: &IoScene, avatar: &mut ImportedAvatar, report: &mut Report) -> BTreeMap<u32, TextureName> {
    let used: BTreeSet<u32> =
        scene.materials.iter().filter_map(|m| m.base_color_texture.map(|t| t.texture.0)).collect();
    let mut taken = BTreeSet::new();
    let mut names = BTreeMap::new();
    for index in used {
        let at = format!("textures/{index}");
        let Some(texture) = usize::try_from(index).ok().and_then(|i| scene.textures.get(i)) else { continue };
        let Some(image) = scene.images.get(texture.image.index()) else { continue };
        match decode_png(image, slot_colour_space("baseColorTexture"), &texture.sampler, &at, report) {
            Ok(data) => {
                let stem = slug_stem(&image.name);
                let stem = if stem.is_empty() { format!("texture_{index}") } else { stem };
                let name = TextureName(unique_slug(&stem, &mut taken));
                avatar.textures.insert(name.clone(), data);
                names.insert(index, name);
            }
            Err(e) => {
                let hint = "save the texture as a PNG and import the model again";
                report.push(Finding::warning("texture.unreadable", e.to_string()).at(at).hint(hint));
            }
        }
    }
    names
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
fn apply_edit(material: &mut io::Material, edit: &MaterialEdit) {
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
    let group_note =
        |location: &str| GROUPS.iter().any(|g| g.notes.iter().any(|field| location == format!("{at}/shading/{field}")));
    let mut out = Report::new();
    for finding in notes.findings {
        let location = finding.location.as_deref().unwrap_or_default();
        let replaced = toon.is_some() && group_note(location);
        let uv_note = carried && finding.rule == "material.not_drawn" && location == format!("{at}/base_color_texture");
        if !replaced && !uv_note {
            out.push(finding);
        }
    }
    for group in GROUPS.iter().filter(|g| toon.is_some_and(|t| (g.used)(t)) && !mtoon.contains(g.bit)) {
        let msg = format!("material '{}' uses {}, which this renderer doesn't draw yet", material.name, group.name);
        let hint = "nothing to do: it is kept and exported as the file had it";
        out.push(Finding::info("look.mtoon_not_drawn", msg).at(format!("{at}/shading")).hint(hint));
    }
    out
}
