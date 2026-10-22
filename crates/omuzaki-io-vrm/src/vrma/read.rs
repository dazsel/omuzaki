//! Reading a `.vrma` back into an [`IoAnimation`]: what the conformance tests compare against, and the start of motion
//! import (RG-08), which will add the clip conversion and the hips scaling of `vrma-export.md` V10.
//!
//! It reads the rest tree from `humanoid.humanBones` and the nodes' translations, and the first animation's channels
//! by their target: a bone's `rotation`, the hips' `translation`, an expression node's `translation.x` and the look-at
//! node's `rotation`. Every track comes back as it is stored (`LINEAR` as linear, `STEP` as step); a hold the exporter
//! wrote as two keys stays two keys. Provenance is not read (the file carries a credit line, not a record).

use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_gltf::accessor::Accessors;
use omuzaki_gltf::animation::{Interpolation as GltfInterpolation, Path, read_animations};
use omuzaki_gltf::buffers::{Buffers, Loader};
use omuzaki_io_api::{
    ExpressionName, HumanBoneName, IoAnimation, IoError, Keys, Limits, NoResolver, RestBone, RestHumanoid, Track,
};
use serde_json::Value;

use super::EXTENSION;

fn malformed(message: impl Into<String>, at: &str) -> IoError {
    IoError::Malformed { message: message.into(), at: Some(at.to_owned()) }
}

fn node_index(value: Option<&Value>, at: &str) -> Result<usize, IoError> {
    value
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| malformed("expected a node index", at))
}

/// Reads a `.vrma`.
///
/// # Errors
/// What `omuzaki-gltf` returns for a broken GLB or animation; [`IoError::Malformed`] for a file with no
/// `VRMC_vrm_animation`, an unknown bone or expression name, a node out of range, a channel on a node that is neither a
/// bone, an expression nor the look-at, or a path the extension does not allow there.
pub fn read_vrma(bytes: &[u8]) -> Result<IoAnimation, IoError> {
    let parsed = omuzaki_gltf::parse(bytes, &Limits::default())?;
    let doc = &parsed.document;
    let ext = doc.extensions.get(EXTENSION).ok_or_else(|| malformed("not a VRM Animation", "/extensions"))?;
    let mut bones: Vec<(usize, HumanBoneName)> = Vec::new();
    if let Some(map) = ext.pointer("/humanoid/humanBones").and_then(Value::as_object) {
        for (name, entry) in map {
            let at = format!("/extensions/{EXTENSION}/humanoid/humanBones/{name}");
            let bone = HumanBoneName::new(name).map_err(|e| malformed(e.to_string(), &at))?;
            bones.push((node_index(entry.get("node"), &at)?, bone));
        }
    }
    bones.sort_by_key(|(_, b)| *b);
    let mut rest = Vec::with_capacity(bones.len());
    for (node, bone) in &bones {
        let def = doc.nodes.get(*node).ok_or_else(|| malformed(format!("node {node} does not exist"), "/nodes"))?;
        rest.push(RestBone::new(*bone, def.translation.map_or(Vec3::ZERO, Vec3::from_array)));
    }
    let mut expressions: Vec<(usize, ExpressionName)> = Vec::new();
    for group in ["preset", "custom"] {
        if let Some(map) = ext.pointer(&format!("/expressions/{group}")).and_then(Value::as_object) {
            for (name, entry) in map {
                let at = format!("/extensions/{EXTENSION}/expressions/{group}/{name}");
                let name = ExpressionName::parse(name).map_err(|e| malformed(e.to_string(), &at))?;
                expressions.push((node_index(entry.get("node"), &at)?, name));
            }
        }
    }
    let look_at = ext.pointer("/lookAt/node").map(|n| node_index(Some(n), "/lookAt/node")).transpose()?;
    let offset =
        ext.pointer("/lookAt/offsetFromHeadBone").and_then(|v| serde_json::from_value::<[f32; 3]>(v.clone()).ok());

    let mut loader = Loader::new(&NoResolver, Limits::default().max_input_bytes);
    let buffers = Buffers::load(doc, parsed.chunk, &mut loader)?;
    let animations = read_animations(doc, &Accessors::new(doc, &buffers))?;
    let first = animations.into_iter().next().unwrap_or_default();
    let mut animation = IoAnimation::new(first.name.unwrap_or_default(), RestHumanoid::new(rest));
    animation.look_at_offset = offset.map(Vec3::from_array);
    animation.looped =
        doc.asset.extras.as_ref().and_then(|e| e.pointer("/omuzaki/looped")).and_then(Value::as_bool) == Some(true);
    for (c, channel) in first.channels.into_iter().enumerate() {
        let at = format!("/animations/0/channels/{c}");
        let s = channel.sampler;
        let step = s.interpolation == GltfInterpolation::Step;
        let keys =
            |values| if step { Keys::step(s.times.clone(), values) } else { Keys::linear(s.times.clone(), values) };
        let bone = bones.iter().find(|(n, _)| *n == channel.node).map(|(_, b)| *b);
        let track = match (bone, channel.path) {
            (Some(bone), Path::Rotation) => Track::rotation(bone, quats(&s.values, keys)),
            (Some(bone), Path::Translation) if bone.as_str() == "hips" => {
                Track::hips(keys_of(&s.values, 3, |v| Vec3::from_slice(v), step, &s.times))
            }
            (None, Path::Translation) => {
                let Some((_, name)) = expressions.iter().find(|(n, _)| *n == channel.node) else {
                    return Err(malformed("a translation on a node that is no expression", &at));
                };
                Track::expression(name.clone(), keys_of(&s.values, 3, |v| v[0], step, &s.times))
            }
            (None, Path::Rotation) if look_at == Some(channel.node) => Track::look_at(quats(&s.values, keys)),
            _ => return Err(malformed(format!("{} is not allowed on this node", channel.path.as_str()), &at)),
        };
        animation.tracks.push(track);
    }
    Ok(animation)
}

fn quats(values: &[f32], keys: impl Fn(Vec<Quat>) -> Keys<Quat>) -> Keys<Quat> {
    keys(values.chunks_exact(4).map(Quat::from_slice).collect())
}

fn keys_of<T>(values: &[f32], width: usize, f: impl Fn(&[f32]) -> T, step: bool, times: &[f32]) -> Keys<T> {
    let values = values.chunks_exact(width).map(f).collect();
    if step { Keys::step(times.to_vec(), values) } else { Keys::linear(times.to_vec(), values) }
}
