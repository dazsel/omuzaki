//! [`ExpressionEvaluator`]: channels in, morph weights and material bind deltas out (design D3).
//!
//! Per frame, in this order (VRM 1.0 `expressions.md`: "Procedural override", "Interaction between override and
//! isBinary", "Expression update algorithm"; the same order as `three-vrm`'s `VRMExpressionManager.update`, MIT):
//! 1. **Input.** Each expression's weight is the largest of its driving channels, each clamped to `0..=1`. On the
//!    presets gaze route the four look presets also take the weights the range maps give the gaze. A binary expression
//!    then steps (above 0.5 is 1, else 0), so it overrides with its stepped value.
//! 2. **Override factor per class** (blink, look-at, mouth): `1 − Σ amounts`, at least 0, where a `block` expression
//!    contributes 1 while its weight is above 0 and a `blend` expression contributes its weight. So `block` alone
//!    gives 0 and `blend` alone gives `1 − saturate(Σ weights)`.
//! 3. **Apply.** A target of a class (`Procedural::of`) is multiplied by its class's factor; a binary target that ends
//!    below 1 is 0, never attenuated, so it only ever shows 0 or 1.
//! 4. **Accumulate.** Morph weights add `bind weight · w` per target; colour binds add `(target − base) · w` per
//!    material colour; UV binds add to scale and offset the same way. A material whose values moved by more than
//!    [`MOVE_THRESHOLD`](crate::buffers::MOVE_THRESHOLD) since it was last marked is marked moved.
//!
//! The tables are built once in [`ExpressionEvaluator::new`]; [`evaluate`](ExpressionEvaluator::evaluate) reads them
//! in fixed order, allocates nothing, locks nothing, and gives the same output for the same input.

use omuzaki_anim_api::{Channel, ChannelBuf, Region};
use omuzaki_avatar_api::avatar::{AvatarFace, AvatarMesh, ImportedAvatar};
use omuzaki_avatar_api::body::BodyView;
use omuzaki_avatar_api::expression::{ExprIx, Procedural};
use omuzaki_avatar_api::gaze::{AvatarGaze, LookDirection, look_degrees, map_range};
use omuzaki_char_api::MaterialName;
use omuzaki_core::glam::{Vec2, Vec4};
use omuzaki_io_api::{ExpressionName, Override, RangeMap};
use omuzaki_render_api::MorphSet;

use crate::buffers::{BindDeltas, MorphWeights, colour_slot};

/// One channel driving one expression.
#[derive(Copy, Clone, Debug)]
struct Driver {
    channel: usize,
    expression: usize,
}

/// One morph bind, flattened to its slot in [`MorphWeights`].
#[derive(Copy, Clone, Debug)]
struct MorphTerm {
    expression: usize,
    slot: usize,
    weight: f32,
}

/// One colour bind, flattened to its material's slot in [`BindDeltas`].
#[derive(Copy, Clone, Debug)]
struct ColourTerm {
    expression: usize,
    material: usize,
    colour: usize,
    delta: Vec4,
}

/// One UV bind, flattened to its material's slot in [`BindDeltas`].
#[derive(Copy, Clone, Debug)]
struct UvTerm {
    expression: usize,
    material: usize,
    scale: Vec2,
    offset: Vec2,
}

/// What an expression is, as the override steps read it.
#[derive(Copy, Clone, Debug)]
struct ExpressionRule {
    binary: bool,
    /// The class it belongs to as a target, as an index into [`Procedural::ALL`].
    class: Option<usize>,
    /// Its effect on each class, in [`Procedural::ALL`] order.
    overrides: [Override; 3],
}

/// The presets gaze route: the look presets the avatar has, and the range maps that weight them.
#[derive(Copy, Clone, Debug)]
struct PresetGaze {
    /// The expression of each direction's preset, in [`LookDirection::ALL`] order.
    presets: [Option<usize>; 4],
    /// Each direction's range map, in the same order.
    maps: [RangeMap; 4],
}

/// Turns an imported avatar's channels into morph weights and material bind deltas each frame.
#[derive(Clone, Debug)]
pub struct ExpressionEvaluator {
    drivers: Vec<Driver>,
    gaze: Option<PresetGaze>,
    rules: Vec<ExpressionRule>,
    morphs: Vec<MorphTerm>,
    colours: Vec<ColourTerm>,
    uvs: Vec<UvTerm>,
    targets_per_mesh: Vec<usize>,
    materials: Vec<MaterialName>,
}

impl ExpressionEvaluator {
    /// Cold: builds the tables from the avatar's expressions, drivers, gaze route and look-at.
    ///
    /// The avatar is expected to have passed [`ImportedAvatar::validate`]. A bind or driver that names nothing (which
    /// validation refuses) is left out of the tables, and a debug build stops on it.
    pub fn new(avatar: &ImportedAvatar) -> Self {
        Self::build(avatar.meshes.iter().map(AvatarMesh::morph_count).collect(), &avatar.face())
    }

    /// Cold: the evaluator of the body `view` shows, from the view alone, or `None` for a body without imported
    /// expressions (a built character, whose face is painted). The same tables as [`new`](Self::new) on the avatar.
    pub fn from_view(view: &BodyView<'_>) -> Option<Self> {
        let face = view.expressions?;
        let targets_per_mesh = view.meshes.iter().map(|m| m.morphs.map_or(0, MorphSet::len)).collect();
        Some(Self::build(targets_per_mesh, &face))
    }

    /// The tables for meshes with `targets_per_mesh` morph targets, driven by `face`.
    fn build(targets_per_mesh: Vec<usize>, face: &AvatarFace<'_>) -> Self {
        let mesh_starts: Vec<usize> = targets_per_mesh
            .iter()
            .scan(0, |start, n| {
                let here = *start;
                *start += n;
                Some(here)
            })
            .collect();
        let mut materials: Vec<MaterialName> = face
            .expressions
            .iter()
            .flat_map(|e| e.colour.iter().map(|b| &b.material).chain(e.uv.iter().map(|b| &b.material)))
            .cloned()
            .collect();
        materials.sort();
        materials.dedup();
        let material_slot = |m: &MaterialName| materials.binary_search(m).ok();

        let (mut morphs, mut colours, mut uvs) = (Vec::new(), Vec::new(), Vec::new());
        for (expression, e) in face.expressions.iter().enumerate() {
            for bind in &e.morph {
                let mesh = usize::from(bind.mesh);
                let target = usize::from(bind.target);
                match (mesh_starts.get(mesh), targets_per_mesh.get(mesh)) {
                    (Some(start), Some(count)) if target < *count => {
                        morphs.push(MorphTerm { expression, slot: start + target, weight: bind.weight });
                    }
                    _ => debug_assert!(false, "expression {expression} binds a target that does not exist"),
                }
            }
            for bind in &e.colour {
                let colour = colour_slot(bind.kind);
                match material_slot(&bind.material) {
                    Some(material) => colours.push(ColourTerm { expression, material, colour, delta: bind.delta() }),
                    None => debug_assert!(false, "the material list holds every bound material"),
                }
            }
            for bind in &e.uv {
                match material_slot(&bind.material) {
                    Some(material) => uvs.push(UvTerm {
                        expression,
                        material,
                        scale: bind.scale - bind.base_scale,
                        offset: bind.offset - bind.base_offset,
                    }),
                    None => debug_assert!(false, "the material list holds every bound material"),
                }
            }
        }

        let rules = face
            .expressions
            .iter()
            .map(|e| ExpressionRule {
                binary: e.binary,
                class: e.procedural().and_then(|c| Procedural::ALL.iter().position(|p| *p == c)),
                overrides: Procedural::ALL.map(|c| e.overrides.get(c)),
            })
            .collect();

        let native = face.gaze == AvatarGaze::NativeArkit;
        let drivers = face
            .drivers
            .iter()
            .filter(|(channel, _)| native || channel.region() != Region::Gaze)
            .filter_map(|(channel, e)| {
                let known = e.index() < face.expressions.len();
                debug_assert!(known, "channel {channel} drives an expression that does not exist");
                known.then_some(Driver { channel: channel.index(), expression: e.index() })
            })
            .collect();

        let gaze = (face.gaze == AvatarGaze::Presets).then(|| PresetGaze {
            presets: LookDirection::ALL
                .map(|d| face.expression(&ExpressionName::Preset(d.preset())).map(ExprIx::index)),
            maps: LookDirection::ALL.map(|d| face.look_at.preset_map(d)),
        });

        Self { drivers, gaze, rules, morphs, colours, uvs, targets_per_mesh, materials }
    }

    /// Cold: morph weight buffers of the right size for this avatar.
    pub fn morph_weights(&self) -> MorphWeights {
        MorphWeights::new(self.targets_per_mesh.iter().copied(), self.rules.len())
    }

    /// Cold: bind delta buffers for every material this avatar's binds move.
    pub fn bind_deltas(&self) -> BindDeltas {
        BindDeltas::new(self.materials.clone())
    }

    /// Hot, no allocation, no locking. Runs after the anim blend and before the renderer, on the frame thread.
    ///
    /// `weights` and `binds` must come from this evaluator's [`morph_weights`](Self::morph_weights) and
    /// [`bind_deltas`](Self::bind_deltas); `binds` keeps what it last marked moved between frames.
    pub fn evaluate(&self, channels: &ChannelBuf, weights: &mut MorphWeights, binds: &mut BindDeltas) {
        debug_assert_eq!(weights.expressions.len(), self.rules.len(), "buffers made for another avatar");
        debug_assert_eq!(binds.materials, self.materials, "buffers made for another avatar");
        let w = &mut weights.expressions;
        self.inputs(channels, w);
        let factors = self.factors(w);
        for (weight, rule) in w.iter_mut().zip(&self.rules) {
            if let Some(class) = rule.class {
                *weight *= factors[class];
                if rule.binary && *weight < 1.0 {
                    *weight = 0.0;
                }
            }
        }
        self.accumulate(w, &mut weights.weights, binds);
    }

    /// Step 1: each expression's input weight, stepped when binary.
    fn inputs(&self, channels: &ChannelBuf, w: &mut [f32]) {
        w.fill(0.0);
        let values = channels.as_slice();
        for d in &self.drivers {
            if let (Some(value), Some(weight)) = (values.get(d.channel), w.get_mut(d.expression)) {
                *weight = weight.max(saturate(*value));
            }
        }
        if let Some(gaze) = &self.gaze {
            let looks = preset_weights(channels, &gaze.maps);
            for (preset, look) in gaze.presets.iter().zip(looks) {
                if let Some(weight) = preset.and_then(|e| w.get_mut(e)) {
                    *weight = weight.max(saturate(look));
                }
            }
        }
        for (weight, rule) in w.iter_mut().zip(&self.rules) {
            if rule.binary {
                *weight = if *weight > 0.5 { 1.0 } else { 0.0 };
            }
        }
    }

    /// Step 2: the override factor of each class, in [`Procedural::ALL`] order.
    fn factors(&self, w: &[f32]) -> [f32; 3] {
        let mut amounts = [0.0f32; 3];
        for (weight, rule) in w.iter().zip(&self.rules) {
            for (amount, effect) in amounts.iter_mut().zip(rule.overrides) {
                *amount += match effect {
                    Override::Block if *weight > 0.0 => 1.0,
                    Override::Blend => *weight,
                    Override::None | Override::Block => 0.0,
                };
            }
        }
        amounts.map(|a| (1.0 - a).max(0.0))
    }

    /// Step 4: morph weights and bind deltas from the applied expression weights.
    fn accumulate(&self, w: &[f32], morph: &mut [f32], binds: &mut BindDeltas) {
        morph.fill(0.0);
        for t in &self.morphs {
            if let (Some(slot), Some(weight)) = (morph.get_mut(t.slot), w.get(t.expression)) {
                *slot += t.weight * weight;
            }
        }
        binds.clear();
        for t in &self.colours {
            if let (Some(delta), Some(weight)) = (binds.deltas.get_mut(t.material), w.get(t.expression)) {
                delta.colour[t.colour] += t.delta * *weight;
            }
        }
        for t in &self.uvs {
            if let (Some(delta), Some(weight)) = (binds.deltas.get_mut(t.material), w.get(t.expression)) {
                delta.uv_scale += t.scale * *weight;
                delta.uv_offset += t.offset * *weight;
            }
        }
        binds.mark_moved();
    }
}

/// `value` clamped to `0..=1`, with NaN as 0 (`clamp` would keep the NaN and poison every sum after it).
fn saturate(value: f32) -> f32 {
    if value.is_nan() { 0.0 } else { value.clamp(0.0, 1.0) }
}

/// The four look-preset weights of the gaze in `channels`, in [`LookDirection::ALL`] order (VRM `lookAt.md`, expression
/// type): yaw and pitch from the `eyeLook*` channels, each through its direction's range map. A symmetric pair never
/// adds: each side of a direction reads the larger of its two channels, so both `eyeLookUp*` at 1 is one full look up.
fn preset_weights(channels: &ChannelBuf, maps: &[RangeMap; 4]) -> [f32; 4] {
    // Each channel is sanitised before it meets its opposite, so a NaN or out-of-range channel can't cancel or swamp a
    // valid one in the subtraction below.
    let pair = |[a, b]: [Channel; 2]| saturate(channels.get(a)).max(saturate(channels.get(b)));
    let [up, down, left, right] = LookDirection::ALL.map(|d| pair(d.channels()));
    let (yaw, pitch) = (left - right, up - down);
    let [up_map, down_map, left_map, right_map] = *maps;
    [
        map_range(up_map, look_degrees(pitch)),
        map_range(down_map, look_degrees(-pitch)),
        map_range(left_map, look_degrees(yaw)),
        map_range(right_map, look_degrees(-yaw)),
    ]
}
