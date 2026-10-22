//! A tiny VRM Animation (`VRMC_vrm_animation` 1.0) writer and reader for the animation half of the contract
//! (`docs/design/io-api.md` D11): enough to prove that an [`IoAnimation`] goes through [`run_export_animation`] and
//! comes back equal.
//!
//! It writes the rest tree as nodes, one node per expression and one for the look-at, a single animation with one
//! sampler per track, and the extension that maps them. Unlike the real exporter (`omuzaki-io-vrm`) it writes `STEP`
//! samplers as they are (no player reads its files), shares no input accessors and writes no provenance extras; its
//! reader reads only what its writer writes.
//!
//! [`run_export_animation`]: omuzaki_io_api::run_export_animation

use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_io_api::{
    AnimationExporter, ExportCtx, ExportOptions, ExportOutcome, ExpressionName, FormatInfo, HumanBoneName, IoAnimation,
    IoError, Keys, OutputFile, Report, RestBone, RestHumanoid, Track, register_animation_exporter,
};
use serde_json::{Map, Value, json};

use crate::glb;

const EXTENSION: &str = "VRMC_vrm_animation";

fn vrma_info() -> FormatInfo {
    FormatInfo { id: "vrma", name: "VRM Animation (mock)", extensions: &["vrma"], media_type: "model/gltf-binary" }
}

/// VRM Animation export.
#[derive(Default)]
pub struct MockVrmaExporter;

impl AnimationExporter for MockVrmaExporter {
    fn info(&self) -> FormatInfo {
        vrma_info()
    }

    fn check(&self, _animation: &IoAnimation, _options: &ExportOptions) -> Report {
        Report::new()
    }

    fn export(&self, animation: &IoAnimation, ctx: &ExportCtx<'_>) -> Result<ExportOutcome, IoError> {
        let bytes = write(animation, ctx.options.generator.as_deref());
        Ok(ExportOutcome::new(vec![OutputFile {
            path: "animation.vrma".to_owned(),
            media_type: "model/gltf-binary".to_owned(),
            bytes,
        }]))
    }
}

register_animation_exporter!(MockVrmaExporter);

/// Accessors, buffer views and the BIN chunk as they are written.
#[derive(Default)]
struct Bin {
    bytes: Vec<u8>,
    views: Vec<Value>,
    accessors: Vec<Value>,
}

impl Bin {
    /// Appends `values` (`width` floats per element) as one accessor; returns its index.
    fn push(&mut self, values: &[f32], width: usize, kind: &str, bounds: bool) -> usize {
        let offset = self.bytes.len();
        for v in values {
            self.bytes.extend_from_slice(&v.to_le_bytes());
        }
        self.views.push(json!({ "buffer": 0, "byteOffset": offset, "byteLength": values.len() * 4 }));
        let mut accessor = json!({
            "bufferView": self.views.len() - 1,
            "componentType": 5126,
            "count": values.len() / width,
            "type": kind,
        });
        if bounds {
            let min = values.iter().copied().fold(f32::INFINITY, f32::min);
            let max = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            accessor["min"] = json!([min]);
            accessor["max"] = json!([max]);
        }
        self.accessors.push(accessor);
        self.accessors.len() - 1
    }
}

fn interpolation(keys_step: bool) -> &'static str {
    if keys_step { "STEP" } else { "LINEAR" }
}

/// Writes `animation` as a `.vrma` (a GLB).
fn write(animation: &IoAnimation, generator: Option<&str>) -> Vec<u8> {
    let rest = animation.rest.sorted();
    let mut nodes: Vec<Value> = Vec::new();
    let mut bones = Map::new();
    for (i, b) in rest.iter().enumerate() {
        let children: Vec<usize> = rest
            .iter()
            .enumerate()
            .filter(|(_, c)| animation.rest.parent_of(c.bone) == Some(b.bone))
            .map(|(j, _)| j)
            .collect();
        let mut node = json!({ "name": b.bone.as_str(), "translation": b.translation.to_array() });
        if !children.is_empty() {
            node["children"] = json!(children);
        }
        nodes.push(node);
        bones.insert(b.bone.as_str().to_owned(), json!({ "node": i }));
    }
    let tracks = animation.canonical_tracks();
    let (mut presets, mut custom) = (Map::new(), Map::new());
    let mut targets: Vec<usize> = Vec::new();
    let mut look_at = None;
    for track in &tracks {
        let node = match track {
            Track::Rotation { bone, .. } => rest.iter().position(|b| b.bone == *bone).unwrap_or(0),
            Track::Hips(_) => rest.iter().position(|b| b.bone.as_str() == "hips").unwrap_or(0),
            Track::Expression { name, .. } => {
                nodes.push(json!({ "name": format!("expression_{name}") }));
                let entry = json!({ "node": nodes.len() - 1 });
                match name {
                    ExpressionName::Preset(_) => presets.insert(name.to_string(), entry),
                    ExpressionName::Custom(_) => custom.insert(name.to_string(), entry),
                };
                nodes.len() - 1
            }
            Track::LookAt(_) => {
                nodes.push(json!({ "name": "lookAt" }));
                look_at = Some(nodes.len() - 1);
                nodes.len() - 1
            }
            _ => 0,
        };
        targets.push(node);
    }
    let mut bin = Bin::default();
    let (mut samplers, mut channels) = (Vec::new(), Vec::new());
    for (track, node) in tracks.iter().zip(&targets) {
        let input = bin.push(track.times(), 1, "SCALAR", true);
        let (output, path) = match track {
            Track::Rotation { keys, .. } | Track::LookAt(keys) => {
                let flat: Vec<f32> = keys.values.iter().flat_map(Quat::to_array).collect();
                (bin.push(&flat, 4, "VEC4", false), "rotation")
            }
            Track::Hips(keys) => {
                let flat: Vec<f32> = keys.values.iter().flat_map(Vec3::to_array).collect();
                (bin.push(&flat, 3, "VEC3", false), "translation")
            }
            Track::Expression { keys, .. } => {
                let flat: Vec<f32> = keys.values.iter().flat_map(|w| [*w, 0.0, 0.0]).collect();
                (bin.push(&flat, 3, "VEC3", false), "translation")
            }
            _ => continue,
        };
        let step = track.interpolation() == omuzaki_io_api::Interpolation::Step;
        samplers.push(json!({ "input": input, "output": output, "interpolation": interpolation(step) }));
        channels.push(json!({ "sampler": samplers.len() - 1, "target": { "node": node, "path": path } }));
    }
    let mut extension = json!({ "specVersion": "1.0", "humanoid": { "humanBones": bones } });
    if !presets.is_empty() || !custom.is_empty() {
        extension["expressions"] = json!({ "preset": presets, "custom": custom });
    }
    if let Some(node) = look_at {
        let offset = animation.look_at_offset.unwrap_or(omuzaki_io_api::animation::DEFAULT_LOOK_AT_OFFSET);
        extension["lookAt"] = json!({ "node": node, "offsetFromHeadBone": offset.to_array() });
    }
    let mut roots = vec![0];
    roots.extend(rest.len()..nodes.len());
    let mut asset = json!({ "version": "2.0" });
    if let Some(generator) = generator {
        asset["generator"] = json!(generator);
    }
    let root = json!({
        "asset": asset,
        "extensionsUsed": [EXTENSION],
        "extensions": { EXTENSION: extension },
        "scene": 0,
        "scenes": [{ "nodes": roots }],
        "nodes": nodes,
        "animations": [{ "name": animation.name, "channels": channels, "samplers": samplers }],
        "accessors": bin.accessors,
        "bufferViews": bin.views,
        "buffers": [{ "byteLength": bin.bytes.len() }],
    });
    let text = serde_json::to_vec(&root).unwrap_or_default();
    glb::join(&text, &bin.bytes)
}

fn keys<T>(step: bool, times: &[f32], values: Vec<T>) -> Keys<T> {
    if step { Keys::step(times.to_vec(), values) } else { Keys::linear(times.to_vec(), values) }
}

#[allow(clippy::cast_possible_truncation, reason = "JSON numbers the mock wrote from f32")]
fn f32s(values: &[Value]) -> Vec<f32> {
    values.iter().filter_map(Value::as_f64).map(|x| x as f32).collect()
}

fn malformed(message: impl Into<String>) -> IoError {
    IoError::malformed(message.into())
}

fn index(value: &Value, what: &str) -> Result<usize, IoError> {
    value.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| malformed(format!("{what} is not an index")))
}

/// The floats of accessor `ix`, checked against the BIN chunk.
fn floats(root: &Value, bin: &[u8], ix: usize) -> Result<Vec<f32>, IoError> {
    let accessor = root.pointer(&format!("/accessors/{ix}")).ok_or_else(|| malformed("accessor out of range"))?;
    let width = match accessor["type"].as_str() {
        Some("SCALAR") => 1,
        Some("VEC3") => 3,
        Some("VEC4") => 4,
        _ => return Err(malformed("unexpected accessor type")),
    };
    let count = index(&accessor["count"], "accessor count")?;
    let view = root
        .pointer(&format!("/bufferViews/{}", index(&accessor["bufferView"], "buffer view")?))
        .ok_or_else(|| malformed("buffer view out of range"))?;
    let offset = index(&view["byteOffset"], "byte offset")?;
    let len = count.checked_mul(width * 4).ok_or_else(|| malformed("accessor too large"))?;
    let raw = offset
        .checked_add(len)
        .and_then(|end| bin.get(offset..end))
        .ok_or_else(|| malformed("accessor runs past the BIN chunk"))?;
    Ok(raw.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect())
}

/// Reads a `.vrma` the mock wrote back into an animation (provenance `None`; the file carries none).
///
/// # Errors
/// [`IoError::Malformed`] for anything the mock's writer would not have written.
pub fn read_vrma(bytes: &[u8]) -> Result<IoAnimation, IoError> {
    let (text, bin) = glb::split(bytes)?;
    let root: Value = serde_json::from_slice(text).map_err(|e| malformed(e.to_string()))?;
    let ext = root.pointer(&format!("/extensions/{EXTENSION}")).ok_or_else(|| malformed("no VRMC_vrm_animation"))?;
    let human = ext.pointer("/humanoid/humanBones").and_then(Value::as_object).ok_or_else(|| malformed("no bones"))?;
    let mut node_bone: Vec<(usize, HumanBoneName)> = Vec::new();
    for (name, entry) in human {
        let bone = HumanBoneName::new(name).map_err(|e| malformed(e.to_string()))?;
        node_bone.push((index(&entry["node"], "bone node")?, bone));
    }
    node_bone.sort_by_key(|(_, b)| *b);
    let mut rest = Vec::new();
    for (node, bone) in &node_bone {
        let v = root.pointer(&format!("/nodes/{node}/translation")).and_then(Value::as_array).map(|a| f32s(a));
        let v = v.unwrap_or_default();
        let translation = if v.len() == 3 { Vec3::new(v[0], v[1], v[2]) } else { Vec3::ZERO };
        rest.push(RestBone::new(*bone, translation));
    }
    let mut expressions: Vec<(usize, ExpressionName)> = Vec::new();
    for group in ["preset", "custom"] {
        if let Some(map) = ext.pointer(&format!("/expressions/{group}")).and_then(Value::as_object) {
            for (name, entry) in map {
                let name = ExpressionName::parse(name).map_err(|e| malformed(e.to_string()))?;
                expressions.push((index(&entry["node"], "expression node")?, name));
            }
        }
    }
    let look_at = ext.pointer("/lookAt/node").map(|n| index(n, "look-at node")).transpose()?;
    let offset = ext.pointer("/lookAt/offsetFromHeadBone").and_then(Value::as_array).map(|a| {
        let v = f32s(a);
        Vec3::new(v.first().copied().unwrap_or(0.0), v.get(1).copied().unwrap_or(0.0), v.get(2).copied().unwrap_or(0.0))
    });
    let clip = root.pointer("/animations/0").ok_or_else(|| malformed("no animation"))?;
    let mut animation = IoAnimation::new(clip["name"].as_str().unwrap_or_default(), RestHumanoid::new(rest));
    animation.look_at_offset = offset;
    let channels = clip["channels"].as_array().ok_or_else(|| malformed("no channels"))?;
    for channel in channels {
        let sampler = &clip["samplers"][index(&channel["sampler"], "sampler")?];
        let times = floats(&root, bin, index(&sampler["input"], "input")?)?;
        let out = floats(&root, bin, index(&sampler["output"], "output")?)?;
        let step = sampler["interpolation"].as_str() == Some("STEP");
        let node = index(&channel["target"]["node"], "target node")?;
        let path = channel["target"]["path"].as_str().unwrap_or_default();
        let track = if let Some((_, bone)) = node_bone.iter().find(|(n, _)| *n == node) {
            if path == "rotation" {
                Track::rotation(
                    *bone,
                    keys(step, &times, out.as_chunks::<4>().0.iter().map(|q| Quat::from_array(*q)).collect()),
                )
            } else {
                Track::hips(keys(step, &times, out.as_chunks::<3>().0.iter().map(|v| Vec3::from_array(*v)).collect()))
            }
        } else if let Some((_, name)) = expressions.iter().find(|(n, _)| *n == node) {
            Track::expression(name.clone(), keys(step, &times, out.as_chunks::<3>().0.iter().map(|v| v[0]).collect()))
        } else if look_at == Some(node) {
            Track::look_at(keys(step, &times, out.as_chunks::<4>().0.iter().map(|q| Quat::from_array(*q)).collect()))
        } else {
            return Err(malformed(format!("channel targets node {node}, which is no bone, expression or look-at")));
        };
        animation.tracks.push(track);
    }
    Ok(animation)
}
