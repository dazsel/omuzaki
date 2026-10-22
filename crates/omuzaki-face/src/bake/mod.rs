//! The face's expression baker (spec v3 §19 FE-09, #172, #173): bakes the painted face into a file's head mesh, so an
//! exported model keeps its face, and every `ARKit` channel and VRM preset outside Omuzaki.
//!
//! [`FaceBakerFactory`] registers with `register_face_baker!` (priority 10, above the mock's 0); `io.export` finds it
//! through `face_baker()`. A bake does three things to the scene:
//!
//! 1. **Patches** (`patch`). The live face is a planar overlay the renderer projects onto the head; a file has none. So
//!    each painted feature (the static face, the cheeks at rest and at full blush, each eye, each brow, the mouth) gets
//!    a patch: the head's triangles over the feature, lifted a fraction of a millimetre, with its own transparent toon
//!    material whose texture is the feature's atlas.
//! 2. **Atlases** ([`flipbook`]). Every state a feature shows is painted on the CPU raster into binary-coded cells, and
//!    a channel's expression moves its feature's UV offset by its bit (`isBinary`), so the channels combine.
//! 3. **Morph targets** ([`morph`]). The jaw, the lips' pinch and shift, and the cheeks move the head's surface (and the
//!    patches on it) by analytic displacement fields.
//!
//! Then every requested channel becomes an expression with its binds, and the 18 VRM presets are derived: from the
//! face's own expression of the same name (the library has `happy`, `aa`, `blink`, ...) or else from the channels (io
//! design D6). The report says per channel what it became, and lists each atlas's layout. Extras are requested as
//! `omuzaki_<id>`, where `id` is the extra channel's name in kebab case (`omuzaki_eyes-heart`, `omuzaki_blush`).
//!
//! The bake is deterministic: the same face, scene and request give the same scene, bytes included.

pub mod flipbook;
pub mod morph;
mod patch;

use std::collections::BTreeMap;

use omuzaki_anim_api::Channel;
use omuzaki_core::glam::{Vec2, Vec3, Vec4};
use omuzaki_face_api::{BakerInput, FaceBakerFactory, FaceProfile, FaceProjection, FaceRect, register_face_baker};
use omuzaki_io_api::{
    AlphaMode, Avatar, BakeReport, BakeRequest, Blob, ChannelBake, ColorBind, ColorBindKind, EXTRA_PREFIX,
    EncodedImage, Expression, ExpressionBaker, ExpressionName, Filter, Finding, ImageMime, IoError, IoScene, Material,
    MaterialIx, Mesh, MeshIx, MorphBind, MorphTarget, OutlineMode, PresetExpression, Progress, Realisation, Sampler,
    Shading, TexRef, Texture, Toon, UvBind, Wrap,
};

use self::flipbook::{Atlas, Feature, MAX_ATLAS_PX};
use self::morph::{Anchors, MORPH_CHANNELS};
use self::patch::HeadFrame;

/// Builds [`FaceBaker`]s. Registered with the face registry at link time, above the mock.
#[derive(Default)]
pub struct RealFaceBakerFactory;

impl FaceBakerFactory for RealFaceBakerFactory {
    fn priority(&self) -> i32 {
        10
    }

    fn create(&self, input: BakerInput<'_>) -> Result<Box<dyn ExpressionBaker>, IoError> {
        Ok(Box::new(FaceBaker::new(input)))
    }
}

register_face_baker!(RealFaceBakerFactory);

/// Bakes one character's face (see the module docs). The profile must have its palette resolved, as for
/// [`FaceSystem::update`](crate::FaceSystem::update).
#[derive(Clone, Debug)]
pub struct FaceBaker {
    profile: FaceProfile,
    projection: FaceProjection,
    frame: HeadFrame,
}

impl FaceBaker {
    /// A baker for `input`'s face.
    pub fn new(input: BakerInput<'_>) -> Self {
        Self {
            profile: input.profile.clone(),
            projection: input.projection,
            frame: HeadFrame { centre: input.head_centre, front_from: input.projection.front_fade[0] },
        }
    }
}

/// How a channel is carried: its morph target, the UV moves of its features' atlases, its colour binds.
#[derive(Clone, Debug, Default)]
struct Forms {
    morph: Option<u32>,
    uv: Vec<(MaterialIx, Vec2)>,
    colour: Vec<ColorBind>,
}

impl Forms {
    fn realisation(&self, inert: &str) -> Realisation {
        let kinds = [self.morph.is_some(), !self.uv.is_empty(), !self.colour.is_empty()];
        match kinds {
            [true, false, false] => Realisation::Morph { targets: 1 },
            [false, true, false] => Realisation::Flipbook { frames: 2 },
            [false, false, true] => Realisation::Colour,
            [false, false, false] => Realisation::Inert { reason: inert.to_owned() },
            _ => Realisation::Mixed,
        }
    }
}

/// What a bake made of each channel's name.
#[derive(Default)]
struct Baked {
    forms: BTreeMap<String, Forms>,
}

impl Baked {
    fn forms(&self, channel: &str) -> Forms {
        self.forms.get(channel).cloned().unwrap_or_default()
    }

    /// One expression from `channels` at their weights: each morph at its weight, and the UV moves and colours of
    /// the channels at 0.5 or more, the UV moves of one material summed into one bind.
    fn expression(&self, node: omuzaki_io_api::NodeIx, channels: &[(&str, f32)]) -> Expression {
        let mut e = Expression::new();
        let mut uv: BTreeMap<MaterialIx, Vec2> = BTreeMap::new();
        for &(channel, weight) in channels {
            let forms = self.forms(channel);
            if let Some(index) = forms.morph
                && weight > 0.0
            {
                e.morph_binds.push(MorphBind { node, index, weight: weight.min(1.0) });
            }
            if weight >= 0.5 {
                for (material, offset) in forms.uv {
                    *uv.entry(material).or_insert(Vec2::ZERO) += offset;
                }
                e.color_binds.extend(forms.colour);
            }
        }
        e.uv_binds = uv.into_iter().map(|(material, offset)| UvBind { material, scale: Vec2::ONE, offset }).collect();
        e.is_binary = !e.uv_binds.is_empty();
        e
    }
}

/// Why a channel the baker has no form for stays inert.
fn inert_reason(channel: &str) -> &'static str {
    if channel.starts_with("eyeLook") {
        "the painted eyes have no gaze cells; the look-at block turns the eyes"
    } else if channel.starts_with("mouth") || channel.starts_with("jaw") || channel == "tongueOut" {
        "the mouth atlas holds the jaw, smiles, frowns, pucker, funnel and tongue; this shape has no cell or morph"
    } else if channel.starts_with(EXTRA_PREFIX) {
        "no baked form for this extra"
    } else {
        "the painter draws this channel, but the bake has no cell or morph for it"
    }
}

/// The channel an extra's expression name stands for: `omuzaki_eyes-heart` is `eyesHeart`.
fn extra_channel(name: &str) -> Option<&'static str> {
    let id = name.strip_prefix(EXTRA_PREFIX)?;
    let mut camel = String::new();
    let mut upper = false;
    for c in id.chars() {
        if c == '-' {
            upper = true;
        } else if upper {
            camel.extend(c.to_uppercase());
            upper = false;
        } else {
            camel.push(c);
        }
    }
    Channel::from_name(&camel).map(Channel::name)
}

/// The channels a preset is made of when the face has no expression of that name (io design D6).
fn preset_channels(preset: PresetExpression) -> Vec<(&'static str, f32)> {
    match preset {
        PresetExpression::Blink => vec![("eyeBlinkLeft", 1.0), ("eyeBlinkRight", 1.0)],
        PresetExpression::BlinkLeft => vec![("eyeBlinkLeft", 1.0)],
        PresetExpression::BlinkRight => vec![("eyeBlinkRight", 1.0)],
        PresetExpression::Aa => vec![("jawOpen", 1.0)],
        PresetExpression::Ih => vec![("mouthSmileLeft", 1.0), ("mouthSmileRight", 1.0), ("jawOpen", 0.3)],
        PresetExpression::Ou => vec![("mouthPucker", 1.0), ("mouthFunnel", 0.5)],
        PresetExpression::Ee => vec![("mouthSmileLeft", 1.0), ("mouthSmileRight", 1.0)],
        PresetExpression::Oh => vec![("mouthFunnel", 1.0), ("jawOpen", 0.6)],
        PresetExpression::Happy => {
            vec![("mouthSmileLeft", 1.0), ("mouthSmileRight", 1.0), ("cheekSquintLeft", 0.5), ("blush", 0.5)]
        }
        PresetExpression::Angry => vec![("browDownLeft", 1.0), ("browDownRight", 1.0), ("mouthFrownLeft", 0.6)],
        PresetExpression::Sad => {
            vec![("browInnerUp", 1.0), ("mouthFrownLeft", 1.0), ("mouthFrownRight", 1.0)]
        }
        PresetExpression::Relaxed => vec![("eyeSquintLeft", 0.6), ("eyeSquintRight", 0.6), ("mouthSmileLeft", 0.5)],
        PresetExpression::Surprised => vec![
            ("eyeWideLeft", 1.0),
            ("eyeWideRight", 1.0),
            ("browInnerUp", 1.0),
            ("browOuterUpLeft", 1.0),
            ("browOuterUpRight", 1.0),
            ("jawOpen", 0.6),
        ],
        PresetExpression::LookUp
        | PresetExpression::LookDown
        | PresetExpression::LookLeft
        | PresetExpression::LookRight
        | PresetExpression::Neutral => Vec::new(),
    }
}

impl FaceBaker {
    /// The channels and weights of `preset`: the face's own expression of that name if it has one, else D6's table.
    fn preset(&self, preset: PresetExpression) -> Vec<(&'static str, f32)> {
        let own = omuzaki_anim_api::ExpressionId::new(preset.as_str())
            .ok()
            .and_then(|id| self.profile.expressions.0.get(&id));
        match own {
            Some(expression) => expression
                .deltas
                .0
                .iter()
                .map(|(channel, delta)| (channel.name(), omuzaki_face_api::narrow(*delta)))
                .filter(|(_, w)| *w > 0.0)
                .collect(),
            None => preset_channels(preset),
        }
    }

    /// Where the morph fields sit: the face window's width and the mouth's centre.
    fn anchors(&self, mouth: Option<FaceRect>) -> Anchors {
        let rect = self.projection.rect;
        let centre = |r: FaceRect| Vec2::new(r.x0.midpoint(r.x1), r.y0.midpoint(r.y1));
        // Without a painted mouth, the lower third of the window.
        let fallback = Vec2::new(rect.x0.midpoint(rect.x1), rect.y0 + rect.height() / 3.0);
        Anchors { width: rect.width(), mouth: mouth.map_or(fallback, centre), front: self.projection.front_fade }
    }
}

/// `done / total` as a progress fraction; the counts are a handful.
fn fraction(done: usize, total: usize) -> f32 {
    let f = |n: usize| f32::from(u16::try_from(n).unwrap_or(u16::MAX));
    f(done) / f(total.max(1))
}

/// Fails with [`IoError::Internal`] naming what the face painter refused.
fn painted<T>(result: Result<T, omuzaki_face_api::FaceError>) -> Result<T, IoError> {
    result.map_err(|e| IoError::Internal(format!("the face painter failed while baking: {e}")))
}

/// The toon material of a patch: the head's own toon look (or the default one), with the atlas as both its lit and its
/// shaded texture, so the painted feature keeps its colours in shadow; transparent where nothing is painted.
fn patch_material(name: &str, head: Option<&Material>, texture: TexRef, alpha: f32) -> Material {
    let mut toon = if let Some(Shading::Toon(t)) = head.map(|m| &m.shading) {
        (**t).clone()
    } else {
        let mut t = Toon::default();
        t.shade_color = Vec3::splat(0.85);
        t
    };
    // The head's own textures follow the head's UVs, not the patch's: none of them carries over.
    toon.shade_multiply_texture = Some(texture);
    toon.shading_shift_texture = None;
    toon.rim_multiply_texture = None;
    toon.outline_width_texture = None;
    toon.uv_animation_mask = None;
    toon.outline_mode = OutlineMode::None;
    (toon.uv_scroll_x, toon.uv_scroll_y, toon.uv_rotation) = (0.0, 0.0, 0.0);
    let mut material = Material::new(name, Shading::Toon(Box::new(toon)));
    material.base_color = Vec4::new(1.0, 1.0, 1.0, alpha);
    material.base_color_texture = Some(texture);
    material.alpha = AlphaMode::Blend;
    material
}

impl ExpressionBaker for FaceBaker {
    fn bake(&self, scene: &mut IoScene, request: &BakeRequest, progress: &dyn Progress) -> Result<BakeReport, IoError> {
        let node = request.target;
        let mesh_ix: MeshIx = scene
            .nodes
            .get(node.index())
            .and_then(|n| n.mesh)
            .filter(|m| m.index() < scene.meshes.len())
            .ok_or_else(|| IoError::unsupported("a face bake target without a mesh", "pass the head's node"))?;
        let max_px = request.budget.max_atlas_px.unwrap_or(MAX_ATLAS_PX).max(64);
        let names: Vec<String> = request.channels.iter().map(ToString::to_string).collect();
        let extras: Vec<&str> = names.iter().filter_map(|n| extra_channel(n)).collect();
        let mut report = BakeReport::default();
        let mut baked = Baked::default();

        // Patches and atlases.
        let features = flipbook::features(&extras);
        let head_material =
            scene.meshes[mesh_ix.index()].primitives.first().and_then(|p| p.material).map(MaterialIx::index);
        let head_material = head_material.and_then(|m| scene.materials.get(m)).cloned();
        let sampler = Sampler {
            wrap_u: Wrap::ClampToEdge,
            wrap_v: Wrap::ClampToEdge,
            mag: Filter::Linear,
            min: Filter::Linear,
            mipmaps: false,
        };
        let mut mouth_rect = None;
        let total = features.len() + 2;
        for (step, feature) in features.iter().enumerate() {
            progress.checkpoint("bake", fraction(step, total))?;
            let Some(rect) = painted(flipbook::bounds(&self.profile, self.projection, feature))? else { continue };
            let mesh = &scene.meshes[mesh_ix.index()];
            let Some(selection) = patch::select(mesh, self.frame, rect) else {
                report.findings.push(Finding::warning(
                    "face.bake_no_patch",
                    format!("the head has no forward-facing triangles over the {} feature", feature.name),
                ));
                continue;
            };
            if feature.name == "mouth" {
                mouth_rect = Some(rect);
            }
            let atlas = painted(flipbook::atlas(&self.profile, self.projection, feature, selection.bounds, max_px))?;
            let primitive =
                patch::build(mesh, &selection, self.frame, feature.lift, |q| atlas.uv(q), mesh.morph_target_count());
            let bytes = atlas.png().map_err(|e| IoError::Internal(format!("encoding the face atlas: {e}")))?;
            let name = format!("face.{}", feature.name);
            let image = scene.add_image(EncodedImage::new(name.clone(), ImageMime::Png, Blob::new(bytes)));
            let texture = scene.add_texture(Texture { image, sampler });
            let tex = TexRef { texture, uv_set: 0, transform: None };
            let alpha = if feature.name == "blush" { 0.0 } else { 1.0 };
            let material = scene.add_material(patch_material(&name, head_material.as_ref(), tex, alpha));
            let mut primitive = primitive;
            primitive.material = Some(material);
            scene.meshes[mesh_ix.index()].primitives.push(primitive);
            record_feature(&mut baked, feature, &atlas, material);
            report.findings.push(Finding::info("face.bake_atlas", layout(feature, &atlas)));
        }

        // Morph targets, on the head and on the patches.
        progress.checkpoint("bake", fraction(total - 2, total))?;
        let anchors = self.anchors(mouth_rect);
        self.morphs(&mut scene.meshes[mesh_ix.index()], anchors, request, &mut report, &mut baked)?;

        // Expressions: every requested channel, then the presets.
        progress.checkpoint("bake", fraction(total - 1, total))?;
        let avatar = scene.avatar.get_or_insert_with(Avatar::new);
        for (name, channel) in names.iter().zip(&request.channels) {
            let source = extra_channel(name).unwrap_or(name.as_str());
            let forms = baked.forms(source);
            let realisation = forms.realisation(inert_reason(name));
            if let Realisation::Inert { reason } = &realisation {
                report.findings.push(Finding::warning("perfect_sync.inert_channel", format!("{name}: {reason}")));
            }
            report.channels.push(ChannelBake { channel: name.clone(), realisation });
            avatar.expressions.insert(channel.clone(), baked.expression(node, &[(source, 1.0)]));
        }
        for preset in PresetExpression::ALL {
            let name = ExpressionName::Preset(preset);
            if request.channels.contains(&name) {
                continue;
            }
            avatar.expressions.insert(name, baked.expression(node, &self.preset(preset)));
        }
        Ok(report)
    }
}

impl FaceBaker {
    /// Adds a morph target for each of [`MORPH_CHANNELS`] to every primitive of `mesh` (head and patches alike), within
    /// the request's budget, and records each target's index in `baked`.
    fn morphs(
        &self,
        mesh: &mut Mesh,
        anchors: Anchors,
        request: &BakeRequest,
        report: &mut BakeReport,
        baked: &mut Baked,
    ) -> Result<(), IoError> {
        for channel in MORPH_CHANNELS {
            let used = mesh.morph_target_count();
            if request.budget.max_morph_targets.is_some_and(|max| used >= max as usize) {
                report.findings.push(Finding::warning(
                    "perfect_sync.morph_budget",
                    format!("{channel}: the morph target budget is used up, so it has no morph target"),
                ));
                continue;
            }
            let mut moved = false;
            for primitive in &mut mesh.primitives {
                let data = &primitive.mesh;
                let deltas: Vec<Vec3> = data
                    .positions
                    .iter()
                    .enumerate()
                    .map(|(i, &p)| {
                        let normal = data.normals.get(i).map_or(Vec3::ZERO, |n| n.normalize_or_zero());
                        anchors.displacement(channel, self.frame.local(p), normal)
                    })
                    .collect();
                moved |= deltas.iter().any(|d| *d != Vec3::ZERO);
                primitive.morph_targets.resize_with(used, || MorphTarget::new(vec![Vec3::ZERO; data.positions.len()]));
                primitive.morph_targets.push(MorphTarget::new(deltas));
            }
            mesh.morph_target_names.resize(used, String::new());
            mesh.morph_target_names.push(channel.to_owned());
            if !moved {
                report.findings.push(Finding::warning(
                    "face.bake_morph_empty",
                    format!("{channel}: no vertex of the head is in its region, so its target moves nothing"),
                ));
            }
            let index = u32::try_from(used).map_err(|_| IoError::Internal("more than 2^32 morph targets".into()))?;
            baked.forms.entry(channel.to_owned()).or_default().morph = Some(index);
        }
        Ok(())
    }
}

/// Records what each bit of `feature`'s atlas carries: its channel moves the material's UV offset by the bit's step.
/// The blush feature has no bits: the `blush` channel cross-fades it in, and the cheeks at rest out.
fn record_feature(baked: &mut Baked, feature: &Feature, atlas: &Atlas, material: MaterialIx) {
    for (bit, channel) in feature.bits.iter().enumerate() {
        baked.forms.entry(channel.name().to_owned()).or_default().uv.push((material, atlas.bit_offset(bit)));
    }
    let fade = |alpha| ColorBind { material, kind: ColorBindKind::Color, target: Vec4::new(1.0, 1.0, 1.0, alpha) };
    match feature.name.as_str() {
        "blush" => baked.forms.entry("blush".to_owned()).or_default().colour.push(fade(1.0)),
        "cheeks" => baked.forms.entry("blush".to_owned()).or_default().colour.push(fade(0.0)),
        _ => {}
    }
}

/// One atlas's layout for the report.
fn layout(feature: &Feature, atlas: &Atlas) -> String {
    format!(
        "{}: {} x {} cells of {} x {} px ({} channels), atlas {} x {} px at {:.0} px/m",
        feature.name,
        atlas.grid.0,
        atlas.grid.1,
        atlas.cell_px.0,
        atlas.cell_px.1,
        feature.bits.len(),
        atlas.image.width(),
        atlas.image.height(),
        atlas.px_per_m
    )
}
