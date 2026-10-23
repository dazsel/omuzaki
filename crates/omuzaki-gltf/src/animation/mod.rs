//! glTF 2.0 animations, typed (glTF 2.0, "Animations"; `docs/design/vrma-export.md` §6): samplers, channels, and the
//! input and output accessors they index.
//!
//! The [`Document`] keeps `animations` as raw JSON, so a file with an animation this crate cannot use still imports as
//! a model (`gltf.animations_dropped`). A format that wants them reads them with [`read_animations`] and writes them
//! with [`write_animation`].
//!
//! **Writing** is deterministic: accessors, samplers and channels are numbered in the order given, so equal input gives
//! equal bytes. Every value is `FLOAT`; every sampler's input accessor carries `min` and `max` (glTF 2.0 says it
//! "MUST"); samplers whose time arrays are bit-identical share one input accessor (the first wins); `LINEAR` and `STEP`
//! are written and `CUBICSPLINE` is refused; NaN and infinity are refused (glTF forbids them in animation data); a
//! node and path is a target once. The views carry no GPU `target`: animation data is never bound as a vertex buffer.
//!
//! **Reading** checks each animation's channel and sampler counts and every accessor's element count against
//! [`MAX_KEYS`] before allocating, and reads `CUBICSPLINE` samplers as they are stored (in-tangent, value,
//! out-tangent per key).

use omuzaki_io_api::IoError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::accessor::{Accessors, Bound, Flavor, Kind};
use crate::pack::Packer;
use crate::schema::{Accessor as AccessorDef, Document, code};

/// The most elements one animation accessor may hold when read: a 600 s clip at 120 fps, with room for step holds,
/// is 144,000 keys, and a weights output multiplies that by its morph target count.
pub const MAX_KEYS: usize = 4 << 20;

/// The most channels (or samplers) one animation may have when read: one per node and path of a large rig.
pub const MAX_CHANNELS: usize = 65_536;

/// Which node property a channel animates.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Path {
    /// `translation`, `VEC3`.
    Translation,
    /// `rotation`, a unit quaternion `x, y, z, w` as `VEC4`.
    Rotation,
    /// `scale`, `VEC3`.
    Scale,
    /// `weights`, one `SCALAR` per morph target per key.
    Weights,
}

impl Path {
    /// The name in the file.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Translation => "translation",
            Self::Rotation => "rotation",
            Self::Scale => "scale",
            Self::Weights => "weights",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "translation" => Self::Translation,
            "rotation" => Self::Rotation,
            "scale" => Self::Scale,
            "weights" => Self::Weights,
            _ => return None,
        })
    }

    /// Floats per value: 3, 4, 3, or 1 per morph target for weights.
    fn width(self) -> usize {
        match self {
            Self::Translation | Self::Scale => 3,
            Self::Rotation => 4,
            Self::Weights => 1,
        }
    }
}

/// How a sampler interpolates.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum Interpolation {
    /// Linear; spherical linear for rotations.
    #[default]
    Linear,
    /// Held until the next key.
    Step,
    /// Cubic Hermite with stored tangents. Read only.
    CubicSpline,
}

impl Interpolation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Linear => "LINEAR",
            Self::Step => "STEP",
            Self::CubicSpline => "CUBICSPLINE",
        }
    }

    fn parse(name: Option<&str>) -> Option<Self> {
        Some(match name {
            None | Some("LINEAR") => Self::Linear,
            Some("STEP") => Self::Step,
            Some("CUBICSPLINE") => Self::CubicSpline,
            Some(_) => return None,
        })
    }
}

/// One sampler's keys.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sampler {
    /// How it interpolates.
    pub interpolation: Interpolation,
    /// Key times in seconds.
    pub times: Vec<f32>,
    /// The values, flat: `width` floats per key (`3 * width` for `CUBICSPLINE`), `width` from the path (for weights,
    /// the morph target count, so `values.len() / times.len()`).
    pub values: Vec<f32>,
}

/// One channel: a node's property driven by a sampler.
#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    /// The node animated.
    pub node: usize,
    /// Which property.
    pub path: Path,
    /// Its keys.
    pub sampler: Sampler,
}

/// One animation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Animation {
    /// Its name, if any.
    pub name: Option<String>,
    /// Its channels, in the order written.
    pub channels: Vec<Channel>,
}

/// The JSON of an animation.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct AnimationDef {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    channels: Vec<ChannelDef>,
    samplers: Vec<SamplerDef>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct ChannelDef {
    sampler: usize,
    target: TargetDef,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct TargetDef {
    #[serde(skip_serializing_if = "Option::is_none")]
    node: Option<usize>,
    path: String,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct SamplerDef {
    input: usize,
    output: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    interpolation: Option<String>,
}

fn refused(message: String, at: String) -> IoError {
    IoError::Malformed { message, at: Some(at) }
}

/// One float accessor with no GPU target; `with_bounds` adds per-component `min` and `max`.
fn floats(pack: &mut Packer, data: &[f32], kind: &str, width: usize, with_bounds: bool) -> usize {
    let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
    let view = pack.view(&bytes, None);
    let (min, max) = if with_bounds {
        let mut lo = vec![f32::INFINITY; width];
        let mut hi = vec![f32::NEG_INFINITY; width];
        for chunk in data.chunks_exact(width) {
            for (k, v) in chunk.iter().enumerate() {
                lo[k] = lo[k].min(*v);
                hi[k] = hi[k].max(*v);
            }
        }
        (Some(lo), Some(hi))
    } else {
        (None, None)
    };
    pack.accessors.push(AccessorDef {
        buffer_view: Some(view),
        component_type: code::F32,
        count: data.len() / width.max(1),
        kind: kind.to_owned(),
        min,
        max,
        ..AccessorDef::default()
    });
    pack.accessors.len() - 1
}

/// Appends `animation` to `doc`, its data to `pack`, and `extension` (when given) to the document's
/// `extensionsUsed`; returns the animation's index.
///
/// # Errors
/// [`IoError::Unsupported`] for a `CUBICSPLINE` sampler; [`IoError::Malformed`] (locating the channel) for a channel
/// with no keys, times that are not finite or not strictly increasing, a value count that does not match the times, a
/// NaN or infinite value, or a node and path animated twice.
pub fn write_animation(
    doc: &mut Document,
    pack: &mut Packer,
    animation: &Animation,
    extension: Option<&str>,
) -> Result<usize, IoError> {
    let at = |c: usize| format!("/animations/{}/channels/{c}", doc.animations.len());
    let mut def = AnimationDef { name: animation.name.clone(), ..AnimationDef::default() };
    let mut inputs: Vec<(&[f32], usize)> = Vec::new();
    for (c, channel) in animation.channels.iter().enumerate() {
        let s = &channel.sampler;
        if s.interpolation == Interpolation::CubicSpline {
            return Err(IoError::unsupported("writing a CUBICSPLINE sampler", "sample the curve and write it LINEAR"));
        }
        if animation.channels[..c].iter().any(|o| o.node == channel.node && o.path == channel.path) {
            return Err(refused(format!("node {} {} is animated twice", channel.node, channel.path.as_str()), at(c)));
        }
        if s.times.is_empty() {
            return Err(refused("a channel with no keys".into(), at(c)));
        }
        if s.times.iter().any(|t| !t.is_finite()) || s.times.windows(2).any(|w| w[1] <= w[0]) {
            return Err(refused("key times must be finite and strictly increasing".into(), at(c)));
        }
        if s.values.iter().any(|v| !v.is_finite()) {
            return Err(refused("glTF forbids NaN and infinity in animation data".into(), at(c)));
        }
        let width = channel.path.width();
        let per_key = s.values.len() / s.times.len();
        let fits = per_key * s.times.len() == s.values.len()
            && per_key > 0
            && (channel.path == Path::Weights || per_key == width);
        if !fits {
            return Err(refused(
                format!("{} values for {} keys of {}", s.values.len(), s.times.len(), channel.path.as_str()),
                at(c),
            ));
        }
        let input = if let Some((_, ix)) = inputs.iter().find(|(times, _)| bit_equal(times, &s.times)) {
            *ix
        } else {
            let ix = floats(pack, &s.times, "SCALAR", 1, true);
            inputs.push((&s.times, ix));
            ix
        };
        let output = match channel.path {
            Path::Rotation => floats(pack, &s.values, "VEC4", 4, false),
            Path::Translation | Path::Scale => floats(pack, &s.values, "VEC3", 3, false),
            Path::Weights => floats(pack, &s.values, "SCALAR", 1, false),
        };
        def.samplers.push(SamplerDef { input, output, interpolation: Some(s.interpolation.as_str().to_owned()) });
        def.channels.push(ChannelDef {
            sampler: def.samplers.len() - 1,
            target: TargetDef { node: Some(channel.node), path: channel.path.as_str().to_owned() },
        });
    }
    doc.animations.push(crate::to_json(&def));
    if let Some(name) = extension
        && !doc.extensions_used.iter().any(|e| e == name)
    {
        doc.extensions_used.push(name.to_owned());
    }
    Ok(doc.animations.len() - 1)
}

fn bit_equal(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

/// Reads every animation of `doc`, its data through `accessors`. Channels with no `node` (an extension's target) are
/// skipped.
///
/// # Errors
/// [`IoError::Limit`] over [`MAX_CHANNELS`] or [`MAX_KEYS`], checked before allocating; [`IoError::Malformed`] for a
/// sampler or accessor that does not exist or has the wrong type, an unknown path or interpolation, a node out of
/// range, or an output whose count does not match its input.
pub fn read_animations(doc: &Document, accessors: &Accessors<'_>) -> Result<Vec<Animation>, IoError> {
    let mut out = Vec::with_capacity(doc.animations.len().min(MAX_CHANNELS));
    for (a, raw) in doc.animations.iter().enumerate() {
        let here = format!("/animations/{a}");
        for (what, key) in [("animation channel count", "channels"), ("animation sampler count", "samplers")] {
            let n = raw.get(key).and_then(Value::as_array).map_or(0, Vec::len);
            if n > MAX_CHANNELS {
                return Err(IoError::Limit { what, limit: MAX_CHANNELS as u64, got: n as u64 });
            }
        }
        let def: AnimationDef =
            serde_json::from_value(raw.clone()).map_err(|e| refused(format!("not an animation: {e}"), here.clone()))?;
        let mut animation = Animation { name: def.name.clone(), channels: Vec::with_capacity(def.channels.len()) };
        for (c, channel) in def.channels.iter().enumerate() {
            let at = format!("{here}/channels/{c}");
            let Some(node) = channel.target.node else { continue };
            if node >= doc.nodes.len() {
                return Err(refused(format!("node {node} does not exist"), at));
            }
            let path = Path::parse(&channel.target.path)
                .ok_or_else(|| refused(format!("unknown path {:?}", channel.target.path), at.clone()))?;
            let sampler = def
                .samplers
                .get(channel.sampler)
                .ok_or_else(|| refused(format!("sampler {} does not exist", channel.sampler), at.clone()))?;
            let interpolation = Interpolation::parse(sampler.interpolation.as_deref())
                .ok_or_else(|| refused(format!("unknown interpolation {:?}", sampler.interpolation), at.clone()))?;
            let bound = Bound::AtMost { max: MAX_KEYS, what: "animation key count" };
            let times: Vec<f32> =
                accessors.floats::<1>(sampler.input, bound, Flavor::Matrix)?.into_iter().map(|[t]| t).collect();
            let values: Vec<f32> = match accessors.kind(sampler.output)? {
                Kind::Vec4 => accessors.floats::<4>(sampler.output, bound, Flavor::Attribute)?.concat(),
                Kind::Vec3 => accessors.floats::<3>(sampler.output, bound, Flavor::Attribute)?.concat(),
                Kind::Scalar => accessors.floats::<1>(sampler.output, bound, Flavor::Attribute)?.concat(),
                other => return Err(refused(format!("an animation output of {other:?}"), at)),
            };
            let per_key = if interpolation == Interpolation::CubicSpline { 3 } else { 1 } * path.width();
            let ok = match path {
                Path::Weights => !times.is_empty() && values.len().is_multiple_of(times.len() * per_key),
                _ => values.len() == times.len() * per_key,
            };
            if !ok {
                return Err(refused(
                    format!("{} output values for {} keys of {}", values.len(), times.len(), path.as_str()),
                    at,
                ));
            }
            animation.channels.push(Channel { node, path, sampler: Sampler { interpolation, times, values } });
        }
        out.push(animation);
    }
    Ok(out)
}
