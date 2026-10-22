//! Expressions and their channel drivers (design D3).
//!
//! **Binds.** Every bind of the file reaches its [`AvatarExpression`]: a morph bind names a node and a target, and
//! moves that target on every runtime mesh the node's instance made (a face mesh split into primitives moves as one);
//! a colour bind starts from the material's colour as the runtime starts it (the file's, or the person's edit); a UV
//! bind starts from the material's texture transform. A bind that names nothing the model has is reported
//! (`avatar.bind_dropped`) and left out.
//!
//! **Drivers**, by D3's table, then the person's overrides in `AvatarDef.channels`, which win:
//! - a custom expression named like an `ARKit` channel (perfect sync, or one synthesis built) drives that channel; the
//!   eight `eyeLook*` ones only on the native `ARKit` gaze route;
//! - the blink and lip-sync presets are not driven themselves: synthesis built their `ARKit` copies, which are;
//! - the look presets belong to the look-at stage on the presets route;
//! - the emotion presets and every other custom expression take `custom00` to `custom15` in the file's order, each
//!   named in the report (`avatar.expression_custom`); any beyond sixteen are listed as undriven
//!   (`avatar.expression_undriven`).

use omuzaki_anim_api::{CUSTOM_COUNT, Channel, Region};
use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_avatar_api::def::{AvatarDef, DriverChoice};
use omuzaki_avatar_api::expression::{AvatarExpression, ColourBind, ExprIx, MorphBind, Overrides, UvBind};
use omuzaki_avatar_api::gaze::AvatarGaze;
use omuzaki_core::glam::Vec4;
use omuzaki_io_api::{self as io, ColorBindKind, ExpressionName, Finding, IoScene, PresetExpression, Report};

use super::gaze;
use super::materials::MaterialNames;
use crate::prepare::arkit::Arkit52Report;

/// Fills `avatar.expressions`, `avatar.drivers`, the gaze route and the look-at. Runs after the meshes and materials,
/// whose runtime indices and names the binds use.
pub fn convert(
    scene: &IoScene,
    def: &AvatarDef,
    arkit: &Arkit52Report,
    names: &MaterialNames,
    avatar: &mut ImportedAvatar,
    report: &mut Report,
) {
    let Some(file) = &scene.avatar else { return };
    for (name, expression) in &file.expressions {
        let converted = convert_one(scene, name, expression, names, avatar, report);
        avatar.expressions.push(converted);
    }
    gaze::choose(scene, arkit, avatar);
    drive_by_table(avatar, report);
    drive_by_overrides(def, avatar, report);
    gaze::findings(avatar, report);
}

/// The runtime form of the file's expression `name`.
fn convert_one(
    scene: &IoScene,
    name: &ExpressionName,
    expression: &io::Expression,
    names: &MaterialNames,
    avatar: &ImportedAvatar,
    report: &mut Report,
) -> AvatarExpression {
    let at = format!("avatar/expressions/{name}");
    let mut dropped = |what: String| {
        let hint = "the model's expression refers to something the model doesn't have; fix it in the source file";
        report.push(Finding::warning("avatar.bind_dropped", format!("{name}: {what}")).at(at.as_str()).hint(hint));
    };
    let mut out = AvatarExpression::new(name.clone());
    for bind in &expression.morph_binds {
        let morphs = morph_binds(scene, avatar, bind);
        if morphs.is_empty() {
            dropped(format!("target {} of node {} doesn't exist", bind.index, bind.node.0));
        }
        out.morph.extend(morphs);
    }
    for bind in &expression.color_binds {
        match colour_bind(names, bind) {
            Some(colour) => out.colour.push(colour),
            None => dropped(format!("material {} has no {:?} colour", bind.material.0, bind.kind)),
        }
    }
    for bind in &expression.uv_binds {
        let Some(material) = names.of(Some(bind.material.0)) else {
            dropped(format!("material {} doesn't exist", bind.material.0));
            continue;
        };
        let base = avatar.materials.get(&material).map(|m| m.uv_transform).unwrap_or_default();
        out.uv.push(UvBind::new(material, bind.scale, bind.offset).from_base(base.scale, base.offset));
    }
    out.binary = expression.is_binary;
    out.overrides = Overrides::new(expression.override_blink, expression.override_look_at, expression.override_mouth);
    out
}

/// The runtime binds of one file morph bind: the target on each runtime mesh of the node's instance that has it.
fn morph_binds(scene: &IoScene, avatar: &ImportedAvatar, bind: &io::MorphBind) -> Vec<MorphBind> {
    let (Ok(target), Some(mesh)) = (u16::try_from(bind.index), scene.nodes.get(bind.node.index()).and_then(|n| n.mesh))
    else {
        return Vec::new();
    };
    let instance = avatar.retained.meshes.get(mesh.index()).and_then(|m| m.instance(bind.node.0));
    instance
        .into_iter()
        .flat_map(|i| &i.meshes)
        .filter(|&&m| avatar.meshes.get(usize::from(m)).is_some_and(|m| usize::from(target) < m.morph_count()))
        .map(|&m| MorphBind::new(m, target, bind.weight).with_node(bind.node.0))
        .collect()
}

/// The runtime colour bind of a file one, starting from the material's colour as the runtime starts it; `None` when
/// the material or that colour does not exist (an `MToon` colour on a material without `MToon`).
fn colour_bind(names: &MaterialNames, bind: &io::ColorBind) -> Option<ColourBind> {
    let material = names.of(Some(bind.material.0))?;
    let base = base_colour(names.edited(bind.material.0)?, bind.kind, bind.target.w)?;
    Some(ColourBind::new(material, bind.kind, base, bind.target))
}

/// The value of colour `kind` on `material`, as linear RGBA; colours without an alpha take `alpha`, so the bind leaves
/// it alone.
fn base_colour(material: &io::Material, kind: ColorBindKind, alpha: f32) -> Option<Vec4> {
    let toon = match &material.shading {
        io::Shading::Toon(toon) => Some(toon),
        _ => None,
    };
    let rgb = match kind {
        ColorBindKind::Color => return Some(material.base_color),
        ColorBindKind::EmissionColor => material.emissive,
        ColorBindKind::ShadeColor => toon?.shade_color,
        ColorBindKind::MatcapColor => toon?.matcap_color,
        ColorBindKind::RimColor => toon?.rim_color,
        ColorBindKind::OutlineColor => toon?.outline_color,
    };
    Some(rgb.extend(alpha))
}

/// The table's drivers (see the module docs).
fn drive_by_table(avatar: &mut ImportedAvatar, report: &mut Report) {
    let native = avatar.gaze == AvatarGaze::NativeArkit;
    let mut next_custom = 0usize;
    for index in 0..avatar.expressions.len() {
        let Ok(ix) = u16::try_from(index).map(ExprIx) else { break };
        let name = &avatar.expressions[index].name;
        let arkit = match name {
            ExpressionName::Custom(custom) => Channel::from_name(custom.as_str()).filter(|c| c.is_arkit()),
            ExpressionName::Preset(_) => None,
        };
        if let Some(channel) = arkit {
            if native || channel.region() != Region::Gaze {
                avatar.drivers.set(channel, Some(ix));
            }
            continue;
        }
        if !takes_custom_slot(name) {
            continue;
        }
        let at = format!("avatar/expressions/{name}");
        if let Some(slot) = Channel::custom(next_custom) {
            avatar.drivers.set(slot, Some(ix));
            let msg = format!("{name} is on {slot}, so hotkeys and scores can trigger it");
            report.push(Finding::info("avatar.expression_custom", msg).at(at));
        } else {
            let msg = format!("{name} has no channel left: only {CUSTOM_COUNT} custom expressions can be driven");
            let hint = "map it to a channel with avatar.map_channel, in place of one you use less";
            report.push(Finding::warning("avatar.expression_undriven", msg).at(at).hint(hint));
        }
        next_custom += 1;
    }
}

/// Whether an expression of this name takes a `customNN` slot: the emotion presets and the custom expressions not named
/// like an `ARKit` channel.
fn takes_custom_slot(name: &ExpressionName) -> bool {
    match name {
        ExpressionName::Preset(preset) => matches!(
            preset,
            PresetExpression::Happy
                | PresetExpression::Angry
                | PresetExpression::Sad
                | PresetExpression::Relaxed
                | PresetExpression::Surprised
                | PresetExpression::Neutral
        ),
        ExpressionName::Custom(custom) => Channel::from_name(custom.as_str()).is_none_or(|c| !c.is_arkit()),
    }
}

/// The person's driver choices, which win over the table. A choice naming an expression the model does not have, or
/// driving a gaze channel off the native route (where the look-at stage owns gaze), is reported and skipped.
fn drive_by_overrides(def: &AvatarDef, avatar: &mut ImportedAvatar, report: &mut Report) {
    for (channel, choice) in &def.channels {
        let at = format!("avatar/channels/{channel}");
        let expression = match choice {
            DriverChoice::Nothing => None,
            DriverChoice::Expression(name) => {
                let found = ExpressionName::parse(name).ok().and_then(|n| avatar.expression(&n));
                let Some(found) = found else {
                    let msg = format!("{channel} is mapped to {name:?}, which the model doesn't have");
                    let hint = "map the channel again with avatar.map_channel";
                    report.push(Finding::warning("avatar.channel_override_invalid", msg).at(at).hint(hint));
                    continue;
                };
                if channel.region() == Region::Gaze && avatar.gaze != AvatarGaze::NativeArkit {
                    let msg =
                        format!("{channel} is mapped to {name:?}, but this model's gaze runs through its look-at");
                    let hint = "gaze channels drive expressions only on a model with its own eyeLook shapes";
                    report.push(Finding::warning("avatar.channel_override_invalid", msg).at(at).hint(hint));
                    continue;
                }
                Some(found)
            }
        };
        avatar.drivers.set(*channel, expression);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emotion_presets_and_plain_customs_take_custom_slots() {
        let name = |n: &str| ExpressionName::parse(n).unwrap_or(ExpressionName::Preset(PresetExpression::Neutral));
        for (n, slot) in [("happy", true), ("neutral", true), ("blink", false), ("aa", false), ("lookUp", false)] {
            assert_eq!(takes_custom_slot(&name(n)), slot, "{n}");
        }
        for (n, slot) in [("jawOpen", false), ("eyeLookUpLeft", false), ("Puff", true), ("eyesHeart", true)] {
            assert_eq!(takes_custom_slot(&name(n)), slot, "{n}");
        }
    }

    #[test]
    fn a_colour_bind_starts_from_its_colour_with_its_alpha_kept() {
        let mut m = io::Material::new("m", io::Shading::Unlit);
        m.emissive = omuzaki_core::glam::Vec3::splat(0.25);
        assert_eq!(base_colour(&m, ColorBindKind::EmissionColor, 0.5), Some(Vec4::new(0.25, 0.25, 0.25, 0.5)));
        assert_eq!(base_colour(&m, ColorBindKind::ShadeColor, 1.0), None, "no MToon, no shade colour");
    }
}
