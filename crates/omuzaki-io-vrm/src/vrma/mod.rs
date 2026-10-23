//! VRM Animation (`VRMC_vrm_animation` 1.0), format id `vrma`: an [`IoAnimation`] as one GLB with one animation over
//! the T-pose node tree (`docs/design/vrma-export.md` V1 to V7, `docs/design/io-api.md` D11).
//!
//! **Nodes**, in this order: the rest bones in `HUMAN_BONES` order (the hips the only humanoid scene root, each other
//! bone a child of its nearest present humanoid ancestor, translation as given, rotation identity), one node
//! `expression_<name>` per expression track, and one `lookAt` node when there is a gaze track.
//!
//! **The extension:** `specVersion` 1.0; `humanoid.humanBones` maps every rest bone; `expressions.preset` and `custom`
//! map names to their nodes; `lookAt` names its node (the JSON schema requires it) and the gaze origin. It is listed in
//! `extensionsUsed` (three-vrm reads it only from there) and not in `extensionsRequired` (V1).
//!
//! **The animation:** one, named after the animation, with channels and samplers in one canonical order chosen here,
//! not by the input: hips, bones in `HUMAN_BONES` order, expressions by name, look-at; expression nodes are numbered in
//! the same order. A bone is a `rotation`, the hips a `translation`, an expression a `translation` of `(weight, 0, 0)`
//! on its node, the gaze a `rotation` on its node.
//!
//! **Every sampler is `LINEAR`.** three-vrm 3.5.5 rebuilds the tracks without the sampler's interpolation, so a
//! `STEP` sampler would play as a ramp. A [`Step`](omuzaki_io_api::Interpolation::Step) track is written as a hold
//! two keys apart ([`expand_steps`]); a hold that `f32` times cannot place is refused with
//! `anim_export.step_unrepresentable` (V5).
//!
//! **Rights** (D11, "Rights"): `asset.copyright` is the record's attribution, else its author; `asset.extras.omuzaki`
//! carries the licence, commercial status, source kind, author, the loop hint and `private_test` when set. Only the
//! clip's own record: never the character's, never a consent holder's.

mod read;
mod steps;

use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_gltf::animation::{Animation, Channel, Interpolation as GltfInterpolation, Path, Sampler, write_animation};
use omuzaki_gltf::pack::Packer;
use omuzaki_gltf::schema::{Asset, Document, Node, Scene};
use omuzaki_io_api::animation::DEFAULT_LOOK_AT_OFFSET;
use omuzaki_io_api::{
    AnimationExporter, ExportCtx, ExportOptions, ExportOutcome, ExpressionName, Finding, FormatInfo, HumanBoneName,
    IoAnimation, IoError, Layout, OutputFile, Report, Track, register_animation_exporter,
};
use omuzaki_store_api::{Commercial, Provenance, Source};
use serde_json::{Map, Value, json};

pub use read::read_vrma;
pub use steps::{HOLD_S, expand_steps};

/// The extension's name.
pub const EXTENSION: &str = "VRMC_vrm_animation";

/// The name the single file of an export gets; the host renames it.
const FILE: &str = "animation.vrma";

/// A `.vrma` is a binary glTF.
const MEDIA_TYPE: &str = "model/gltf-binary";

fn vrma_info() -> FormatInfo {
    FormatInfo { id: "vrma", name: "VRM Animation", extensions: &["vrma"], media_type: MEDIA_TYPE }
}

/// VRM Animation export.
#[derive(Default)]
pub struct VrmaExporter;

impl AnimationExporter for VrmaExporter {
    fn info(&self) -> FormatInfo {
        vrma_info()
    }

    fn check(&self, animation: &IoAnimation, options: &ExportOptions) -> Report {
        let mut r = Report::new();
        let head = HumanBoneName::new("head").ok().and_then(|h| animation.rest.get(h));
        if animation.tracks.iter().any(|t| matches!(t, Track::LookAt(_))) && head.is_none() {
            r.push(
                Finding::error("vrma.look_at_without_head", "a gaze track needs a head bone in the rest tree")
                    .at("rest")
                    .hint("VRM Animation places the gaze relative to the head"),
            );
        }
        if options.layout == Layout::Separate {
            r.push(Finding::info("vrma.layout_ignored", "a .vrma is always one binary file; the layout was ignored"));
        }
        r
    }

    fn export(&self, animation: &IoAnimation, ctx: &ExportCtx<'_>) -> Result<ExportOutcome, IoError> {
        let bytes = write(animation, ctx.options.generator.as_deref())?;
        Ok(ExportOutcome::new(vec![OutputFile { path: FILE.to_owned(), media_type: MEDIA_TYPE.to_owned(), bytes }]))
    }
}

register_animation_exporter!(VrmaExporter);

/// The asset block: the generator, the credit line and the short record (D11, "Rights").
fn asset(animation: &IoAnimation, generator: Option<&str>) -> Asset {
    let mut asset = Asset { version: "2.0".into(), generator: generator.map(str::to_owned), ..Asset::default() };
    let mut record = Map::new();
    if let Some(p) = &animation.provenance {
        asset.copyright = p.attribution.clone().or_else(|| p.author.clone());
        record = provenance_record(p);
    }
    record.insert("looped".into(), json!(animation.looped));
    asset.extras = Some(json!({ "omuzaki": record }));
    asset
}

fn provenance_record(p: &Provenance) -> Map<String, Value> {
    let mut record = Map::new();
    record.insert("licence".into(), json!(p.licence));
    let commercial = match p.commercial {
        Commercial::Ok => "ok",
        Commercial::No => "no",
        Commercial::Unknown => "unknown",
    };
    record.insert("commercial".into(), json!(commercial));
    let source = match &p.source {
        Source::Authored => "authored",
        Source::Imported { .. } => "imported",
        Source::Generated { .. } => "generated",
        Source::Library { .. } => "library",
    };
    record.insert("source".into(), json!(source));
    if let Some(author) = &p.author {
        record.insert("author".into(), json!(author));
    }
    if p.private_test {
        record.insert("private_test".into(), json!(true));
    }
    record
}

/// Writes the GLB.
fn write(animation: &IoAnimation, generator: Option<&str>) -> Result<Vec<u8>, IoError> {
    let rest = animation.rest.sorted();
    let mut doc = Document { asset: asset(animation, generator), ..Document::default() };
    let mut bones = Map::new();
    for (i, b) in rest.iter().enumerate() {
        let children =
            rest.iter().enumerate().filter(|(_, c)| animation.rest.parent_of(c.bone) == Some(b.bone)).map(|(j, _)| j);
        doc.nodes.push(Node {
            name: b.bone.as_str().to_owned(),
            children: children.collect(),
            translation: Some(b.translation.to_array()),
            ..Node::default()
        });
        bones.insert(b.bone.as_str().to_owned(), json!({ "node": i }));
    }
    let node_of = |bone: HumanBoneName| rest.iter().position(|b| b.bone == bone);
    let hips = HumanBoneName::new("hips").ok().and_then(node_of).unwrap_or(0);
    let tracks = animation.canonical_tracks();
    let (mut presets, mut custom) = (Map::new(), Map::new());
    let mut look_at = None;
    let mut channels = Vec::with_capacity(tracks.len());
    let mut steps = Report::new();
    for (i, track) in tracks.iter().enumerate() {
        let at = format!("tracks/{i}");
        let (node, path, sampler) = match track {
            Track::Hips(keys) => {
                let (times, values) = expand_steps(keys, &at, &mut steps);
                (hips, Path::Translation, sampler(times, values.iter().flat_map(Vec3::to_array)))
            }
            Track::Rotation { bone, keys, .. } => {
                let Some(node) = node_of(*bone) else { continue };
                let (times, values) = expand_steps(keys, &at, &mut steps);
                (node, Path::Rotation, sampler(times, values.iter().flat_map(Quat::to_array)))
            }
            Track::Expression { name, keys, .. } => {
                doc.nodes.push(Node { name: format!("expression_{name}"), ..Node::default() });
                let node = doc.nodes.len() - 1;
                let entry = json!({ "node": node });
                match name {
                    ExpressionName::Preset(_) => presets.insert(name.to_string(), entry),
                    ExpressionName::Custom(_) => custom.insert(name.to_string(), entry),
                };
                let (times, values) = expand_steps(keys, &at, &mut steps);
                (node, Path::Translation, sampler(times, values.iter().flat_map(|w| [*w, 0.0, 0.0])))
            }
            Track::LookAt(keys) => {
                doc.nodes.push(Node { name: "lookAt".into(), ..Node::default() });
                look_at = Some(doc.nodes.len() - 1);
                let (times, values) = expand_steps(keys, &at, &mut steps);
                (doc.nodes.len() - 1, Path::Rotation, sampler(times, values.iter().flat_map(Quat::to_array)))
            }
            _ => continue,
        };
        channels.push(Channel { node, path, sampler });
    }
    if steps.has_errors() {
        return Err(IoError::Rejected(steps));
    }
    let mut extension = json!({ "specVersion": "1.0", "humanoid": { "humanBones": bones } });
    if !presets.is_empty() || !custom.is_empty() {
        let mut expressions = Map::new();
        if !presets.is_empty() {
            expressions.insert("preset".into(), Value::Object(presets));
        }
        if !custom.is_empty() {
            expressions.insert("custom".into(), Value::Object(custom));
        }
        extension["expressions"] = Value::Object(expressions);
    }
    if let Some(node) = look_at {
        let offset = animation.look_at_offset.unwrap_or(DEFAULT_LOOK_AT_OFFSET);
        extension["lookAt"] = omuzaki_gltf::to_json(&LookAtDef { node, offset_from_head_bone: offset.to_array() });
    }
    doc.extensions.insert(EXTENSION.to_owned(), extension);
    let mut roots = vec![hips];
    roots.extend(rest.len()..doc.nodes.len());
    doc.scene = Some(0);
    doc.scenes = vec![Scene { nodes: roots, ..Scene::default() }];
    let mut pack = Packer::default();
    let gltf = Animation { name: Some(animation.name.clone()), channels };
    write_animation(&mut doc, &mut pack, &gltf, Some(EXTENSION))?;
    omuzaki_gltf::glb(doc, pack)
}

/// The `lookAt` object, typed so its floats print short.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LookAtDef {
    node: usize,
    offset_from_head_bone: [f32; 3],
}

fn sampler(times: Vec<f32>, values: impl Iterator<Item = f32>) -> Sampler {
    Sampler { interpolation: GltfInterpolation::Linear, times, values: values.collect() }
}
