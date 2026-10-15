//! `ARKit` channel synthesis: builds the perfect-sync expressions a model can already express, from the morph targets
//! and expressions it has (spec v3 §27, EX-12: "a missing channel is synthesised from the nearest morphs and flagged").
//!
//! This reuses what the model carries and adds no geometry. Shapes that need new geometry (splitting a symmetric morph
//! into halves, cheeks, tongue) stay [`Synthesis::Missing`], and the export's `perfect_sync.missing_channel` rule then
//! reports them as before. The gains and aliases below are starting values for art review.
//!
//! Per channel, in this order:
//! 1. **Present.** An expression named exactly like the channel exists, or a morph target does (it gets an expression
//!    binding it at weight 1).
//! 2. **From a morph.** A morph target whose name matches ignoring case, underscores and a `blendShape.`-style prefix,
//!    or the `VRoid` name in [`VROID_MORPHS`], gets an expression binding it.
//! 3. **From a preset.** The VRM preset in [`PRESET_RULES`] exists and has binds: the new expression copies them, morph
//!    weights scaled by the rule's gain and colour and texture-transform targets interpolated from their rest values.
//! 4. **Missing.** Nothing is added.
//!
//! An existing expression is never replaced or edited.

use std::collections::BTreeMap;

use omuzaki_core::glam::{Vec2, Vec4};
use omuzaki_io_api::{
    ARKIT_52, ColorBind, ColorBindKind, Expression, ExpressionName, IoScene, Material, MorphBind, NodeIx,
    PresetExpression, Shading, UvBind,
};

/// Morph targets of a `VRoid`-made model and the channels they feed, with the weight each binds at. `E`, `Joy` and
/// `Angry` drive both sides of a pair at half weight because the source shape is symmetric.
const VROID_MORPHS: [(&str, &str, f32); 12] = [
    ("Fcl_MTH_A", "jawOpen", 1.0),
    ("Fcl_MTH_O", "mouthFunnel", 1.0),
    ("Fcl_MTH_U", "mouthPucker", 1.0),
    ("Fcl_MTH_E", "mouthStretchLeft", 0.5),
    ("Fcl_MTH_E", "mouthStretchRight", 0.5),
    ("Fcl_MTH_Joy", "mouthSmileLeft", 0.5),
    ("Fcl_MTH_Joy", "mouthSmileRight", 0.5),
    ("Fcl_EYE_Close_L", "eyeBlinkLeft", 1.0),
    ("Fcl_EYE_Close_R", "eyeBlinkRight", 1.0),
    ("Fcl_BRW_Angry", "browDownLeft", 0.5),
    ("Fcl_BRW_Angry", "browDownRight", 0.5),
    ("Fcl_BRW_Sorrow", "browInnerUp", 1.0),
];

/// `(channel, source preset, gain)`: which VRM preset composes a channel. A channel's rules are tried in table order,
/// so a specific preset listed first wins over a symmetric fallback. A preset that feeds both sides of a pair binds at
/// 0.5 each (smile, stretch, brow down, the combined blink, the gaze presets), so both channels at 1 reproduce the
/// preset at 1 and not at 2 (`docs/design/byom.md` D3). For gaze, looking left is the left eye out and the right eye in.
const PRESET_RULES: [(&str, PresetExpression, f32); 22] = [
    ("jawOpen", PresetExpression::Aa, 1.0),
    ("mouthFunnel", PresetExpression::Oh, 1.0),
    ("mouthPucker", PresetExpression::Ou, 1.0),
    ("mouthStretchLeft", PresetExpression::Ee, 0.5),
    ("mouthStretchRight", PresetExpression::Ee, 0.5),
    ("mouthSmileLeft", PresetExpression::Happy, 0.5),
    ("mouthSmileRight", PresetExpression::Happy, 0.5),
    ("eyeBlinkLeft", PresetExpression::BlinkLeft, 1.0),
    ("eyeBlinkLeft", PresetExpression::Blink, 0.5),
    ("eyeBlinkRight", PresetExpression::BlinkRight, 1.0),
    ("eyeBlinkRight", PresetExpression::Blink, 0.5),
    ("eyeLookUpLeft", PresetExpression::LookUp, 0.5),
    ("eyeLookUpRight", PresetExpression::LookUp, 0.5),
    ("eyeLookDownLeft", PresetExpression::LookDown, 0.5),
    ("eyeLookDownRight", PresetExpression::LookDown, 0.5),
    ("eyeLookOutLeft", PresetExpression::LookLeft, 0.5),
    ("eyeLookInRight", PresetExpression::LookLeft, 0.5),
    ("eyeLookInLeft", PresetExpression::LookRight, 0.5),
    ("eyeLookOutRight", PresetExpression::LookRight, 0.5),
    ("browDownLeft", PresetExpression::Angry, 0.5),
    ("browDownRight", PresetExpression::Angry, 0.5),
    ("browInnerUp", PresetExpression::Sad, 1.0),
];

/// How one `ARKit` channel was obtained.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Synthesis {
    /// An expression or morph target already carries the exact `ARKit` name.
    Present,
    /// A morph target under a known alias (a `VRoid` `Fcl_*` name, or the same name in another case).
    FromMorph {
        /// The morph target's name, as the mesh spells it.
        target: String,
    },
    /// Composed from a VRM preset expression.
    FromPreset {
        /// The source preset.
        preset: PresetExpression,
        /// The factor the preset's morph weights were scaled by.
        gain: f32,
    },
    /// Nothing to build it from.
    Missing,
}

/// The outcome for one channel.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ChannelOutcome {
    /// The channel, one of [`ARKIT_52`].
    pub channel: &'static str,
    /// How it was obtained.
    pub how: Synthesis,
}

impl ChannelOutcome {
    /// A note for the user when the channel shares its source with its mirror image, as one symmetric shape drives
    /// both sides (smile, stretch, brow down, the combined blink, gaze). `None` otherwise.
    pub fn note(&self) -> Option<&'static str> {
        let shared = match &self.how {
            Synthesis::FromPreset { preset, .. } => PRESET_RULES.iter().filter(|r| r.1 == *preset).count() > 1,
            Synthesis::FromMorph { target } => {
                VROID_MORPHS.iter().filter(|r| name_key(r.0) == name_key(target)).count() > 1
            }
            Synthesis::Present | Synthesis::Missing => false,
        };
        shared.then_some("symmetric: one source shape drives both the left and the right channel")
    }
}

/// What [`synthesise`] did.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Arkit52Report {
    /// All 52 channels, in [`ARKIT_52`] order.
    pub channels: Vec<ChannelOutcome>,
}

/// A morph target of the scene's meshes.
struct Morph<'a> {
    name: &'a str,
    node: NodeIx,
    index: u32,
}

/// Adds custom expressions named like `ARKit` for channels that are not [`Synthesis::Present`] and can be built;
/// never replaces one. A scene without an avatar is left alone and reports every channel [`Synthesis::Missing`].
pub fn synthesise(scene: &mut IoScene) -> Arkit52Report {
    let morphs = morph_targets(scene);
    let mut channels = Vec::with_capacity(ARKIT_52.len());
    let mut added = Vec::new();
    for channel in ARKIT_52 {
        let built = scene.avatar.as_ref().and_then(|avatar| build(channel, &morphs, scene, &avatar.expressions));
        let (how, expression) = match built {
            Some((how, expression)) => (how, expression),
            None => (Synthesis::Missing, None),
        };
        if let Some(expression) = expression {
            added.push((channel, expression));
        }
        channels.push(ChannelOutcome { channel, how });
    }
    if let Some(avatar) = &mut scene.avatar {
        for (channel, expression) in added {
            // ARKIT_52 names are never empty or presets, so parsing cannot fail; `entry` keeps a defensive guarantee
            // that nothing is replaced.
            if let Ok(name) = ExpressionName::parse(channel) {
                avatar.expressions.entry(name).or_insert(expression);
            } else {
                debug_assert!(false, "{channel} is not a valid expression name");
            }
        }
    }
    Arkit52Report { channels }
}

/// How `channel` is obtained and the expression to add for it (`None` for [`Synthesis::Present`] by an expression),
/// or `None` when it is missing.
///
/// Within the naming tier that matches (exact, then alias, then `VRoid`), the expression binds **every** matching morph
/// target, so a face mesh and a teeth mesh that both carry `jawOpen` move together.
fn build(
    channel: &str,
    morphs: &[Morph<'_>],
    scene: &IoScene,
    expressions: &BTreeMap<ExpressionName, Expression>,
) -> Option<(Synthesis, Option<Expression>)> {
    if ExpressionName::parse(channel).is_ok_and(|name| expressions.contains_key(&name)) {
        return Some((Synthesis::Present, None));
    }
    let matching = |wanted: &str, exact: bool| -> Vec<&Morph<'_>> {
        let key = name_key(wanted);
        morphs.iter().filter(|m| if exact { m.name == wanted } else { name_key(m.name) == key }).collect()
    };
    let exact = matching(channel, true);
    if !exact.is_empty() {
        return Some((Synthesis::Present, Some(bind_morphs(&exact, 1.0))));
    }
    let alias = Some((matching(channel, false), 1.0))
        .into_iter()
        .chain(
            VROID_MORPHS.iter().filter(|r| r.1 == channel).map(|&(alias, _, weight)| (matching(alias, false), weight)),
        )
        .find(|(found, _)| !found.is_empty());
    if let Some((found, weight)) = alias {
        return Some((Synthesis::FromMorph { target: found[0].name.to_owned() }, Some(bind_morphs(&found, weight))));
    }
    PRESET_RULES.iter().filter(|r| r.0 == channel).find_map(|&(_, preset, gain)| {
        let expression = scale(expressions.get(&ExpressionName::Preset(preset))?, gain, &scene.materials)?;
        Some((Synthesis::FromPreset { preset, gain }, Some(expression)))
    })
}

/// An expression driving each of `morphs` at `weight`.
fn bind_morphs(morphs: &[&Morph<'_>], weight: f32) -> Expression {
    let mut expression = Expression::new();
    expression.morph_binds = morphs.iter().map(|m| MorphBind { node: m.node, index: m.index, weight }).collect();
    expression
}

/// A copy of `source` driving everything it drives at `gain` times the strength, or `None` if that leaves nothing.
///
/// Morph weights scale directly. A colour bind moves its material colour from the base value to its target as the
/// weight rises, and a texture-transform bind moves from the identity (scale 1, offset 0), so scaling interpolates
/// the target from that base (`docs/design/byom.md` D3). A colour bind whose material or colour does not exist is
/// dropped: it drives nothing. At a gain of 1 everything is copied as it is, including `is_binary`; below it the copy
/// is continuous, because a stepped expression cannot be halved.
fn scale(source: &Expression, gain: f32, materials: &[Material]) -> Option<Expression> {
    let mut expression = Expression::new();
    if (gain - 1.0).abs() < f32::EPSILON {
        expression.is_binary = source.is_binary;
        expression.morph_binds.clone_from(&source.morph_binds);
        expression.color_binds.clone_from(&source.color_binds);
        expression.uv_binds.clone_from(&source.uv_binds);
    } else {
        expression.morph_binds =
            source.morph_binds.iter().map(|b| MorphBind { weight: b.weight * gain, ..*b }).collect();
        expression.color_binds = source
            .color_binds
            .iter()
            .filter_map(|b| {
                let base = base_color(materials.get(b.material.index())?, b.kind, b.target.w)?;
                Some(ColorBind { target: base + (b.target - base) * gain, ..*b })
            })
            .collect();
        expression.uv_binds = source
            .uv_binds
            .iter()
            .map(|b| UvBind { scale: Vec2::ONE + (b.scale - Vec2::ONE) * gain, offset: b.offset * gain, ..*b })
            .collect();
    }
    (!expression.is_inert()).then_some(expression)
}

/// The value of the colour `kind` on `material` at rest, or `None` if the material has no such colour (an `MToon`
/// colour on a plain PBR material). Colours without an alpha take `alpha`, so interpolating leaves it alone.
fn base_color(material: &Material, kind: ColorBindKind, alpha: f32) -> Option<Vec4> {
    let toon = match &material.shading {
        Shading::Toon(toon) => Some(toon),
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

/// Every morph target of every mesh node, in node order then target order, so the first match is deterministic.
fn morph_targets(scene: &IoScene) -> Vec<Morph<'_>> {
    let mut out = Vec::new();
    for (n, node) in scene.nodes.iter().enumerate() {
        let (Some(mesh), Ok(node_ix)) = (node.mesh.and_then(|m| scene.meshes.get(m.index())), u32::try_from(n)) else {
            continue;
        };
        let names = mesh.morph_target_names.iter().take(mesh.morph_target_count());
        // A mesh index is a `u32` in the file, so the count always fits.
        out.extend(
            names
                .enumerate()
                .filter_map(|(i, name)| Some(Morph { name, node: NodeIx(node_ix), index: u32::try_from(i).ok()? })),
        );
    }
    out
}

/// A name with its prefix up to the last `.` (`blendShape1.jawOpen`) and its underscores removed, lower-cased.
fn name_key(name: &str) -> String {
    let bare = name.rsplit('.').next().unwrap_or(name);
    bare.chars().filter(|c| *c != '_').flat_map(char::to_lowercase).collect()
}
