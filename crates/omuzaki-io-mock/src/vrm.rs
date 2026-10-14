//! The `VRMC_vrm` 1.0 extension, mock subset: `meta`, `humanoid`, morph-bind `expressions`, `lookAt` and `firstPerson`.
//! Spring bones are a root extension of their own and live in [`crate::spring`]. Node constraints, and the material and
//! texture-transform binds, are dropped with a finding; the real `omuzaki-io-vrm` handles them.
//!
//! The key names are written out from the 1.0 spec rather than shared with `omuzaki-io-vrm`: the mock is the independent
//! oracle the real importer is tested against, and it stops being one once the two share code (`crates/README.md`).

use omuzaki_core::glam::Vec3;
use omuzaki_io_api::{
    Avatar, Expression, ExpressionName, Finding, FirstPerson, FirstPersonFlag, HumanBoneName, IoError, IoScene, LookAt,
    LookAtKind, MeshAnnotation, MorphBind, NodeIx, Override, PresetExpression, RangeMap, Report, VrmMeta,
};
use serde_json::{Value, json};

use crate::json::Gltf;
use crate::spring;

/// The root extension this module reads and writes.
pub(crate) const EXTENSION: &str = "VRMC_vrm";

/// The range-map defaults for a file that leaves a range out. The 1.0 text gives none, so these are the values the 0.x
/// migration uses; the two importers then agree on files that omit them.
const RANGE_INPUT_DEGREES: f32 = 90.0;
const RANGE_OUTPUT_BONE_DEGREES: f32 = 10.0;
const RANGE_OUTPUT_EXPRESSION: f32 = 1.0;

fn override_name(o: Override) -> &'static str {
    match o {
        Override::None => "none",
        Override::Block => "block",
        Override::Blend => "blend",
    }
}

fn override_of(v: Option<&Value>) -> Override {
    match v.and_then(Value::as_str) {
        Some("block") => Override::Block,
        Some("blend") => Override::Blend,
        _ => Override::None,
    }
}

/// Adds the VRM extensions for `scene.avatar` to `gltf`: `VRMC_vrm`, and `VRMC_springBone` when there are springs.
pub(crate) fn write(scene: &IoScene, gltf: &mut Gltf, report: &mut Report) {
    let Some(av) = &scene.avatar else { return };
    // The mock writes no images, so a thumbnail index would dangle.
    let mut meta = scene.meta.clone();
    if meta.thumbnail_image.take().is_some() {
        report.push(
            Finding::warning("gltf.images_dropped", "the thumbnail was dropped with the images")
                .at("meta/thumbnailImage"),
        );
    }
    let human_bones: serde_json::Map<String, Value> =
        av.humanoid.bones.iter().map(|(b, n)| (b.as_str().to_owned(), json!({ "node": n.0 }))).collect();
    let (mut preset, mut custom) = (serde_json::Map::new(), serde_json::Map::new());
    for (name, expr) in &av.expressions {
        let target = if matches!(name, ExpressionName::Preset(_)) { &mut preset } else { &mut custom };
        target.insert(name.as_str().to_owned(), expression(name, expr, report));
    }
    let mut expressions = serde_json::Map::new();
    expressions.insert("preset".into(), Value::Object(preset));
    if !custom.is_empty() {
        expressions.insert("custom".into(), Value::Object(custom));
    }
    let mut ext = json!({
        "specVersion": "1.0",
        "meta": serde_json::to_value(&meta).unwrap_or(Value::Null),
        "humanoid": { "humanBones": human_bones },
        "expressions": expressions,
    });
    if let Some(look) = &av.look_at {
        ext["lookAt"] = look_at_json(look);
    }
    if let Some(first) = &av.first_person {
        ext["firstPerson"] = first_person_json(first);
    }
    let mut extensions = vec![(EXTENSION, ext)];
    if let Some(body) = spring::body(av) {
        extensions.push((spring::EXTENSION, body));
    }
    for (name, body) in extensions {
        gltf.extensions.insert(name.to_owned(), body);
        gltf.extensions_used.push(name.to_owned());
    }
    gltf.extensions_used.sort();
    gltf.extensions_used.dedup();
    if !av.constraints.is_empty() {
        report
            .push(Finding::warning("vrm.mock_dropped", "the mock exporter doesn't write constraint data").at("avatar"));
    }
}

/// One expression. Spec defaults are written explicitly, and an empty bind array is omitted because the schema
/// requires at least one entry when the array is present (`VRMC_vrm.expressions.expression.schema.json`).
fn expression(name: &ExpressionName, e: &Expression, report: &mut Report) -> Value {
    let mut out = json!({
        "isBinary": e.is_binary,
        "overrideBlink": override_name(e.override_blink),
        "overrideLookAt": override_name(e.override_look_at),
        "overrideMouth": override_name(e.override_mouth),
    });
    if !e.morph_binds.is_empty() {
        let binds: Vec<Value> =
            e.morph_binds.iter().map(|b| json!({ "node": b.node.0, "index": b.index, "weight": b.weight })).collect();
        out["morphTargetBinds"] = Value::Array(binds);
    }
    if !e.color_binds.is_empty() || !e.uv_binds.is_empty() {
        report.push(
            Finding::warning("vrm.mock_dropped", "the mock exporter writes morph binds only")
                .at(format!("avatar/expressions/{name}")),
        );
    }
    out
}

/// `lookAt` as the 1.0 keys name it. Every field is written, defaults included.
fn look_at_json(l: &LookAt) -> Value {
    let kind = match l.kind {
        LookAtKind::Bone => "bone",
        LookAtKind::Expression => "expression",
    };
    json!({
        "type": kind,
        "offsetFromHeadBone": l.offset_from_head_bone.to_array(),
        "rangeMapHorizontalInner": range_json(l.horizontal_inner),
        "rangeMapHorizontalOuter": range_json(l.horizontal_outer),
        "rangeMapVerticalDown": range_json(l.vertical_down),
        "rangeMapVerticalUp": range_json(l.vertical_up),
    })
}

fn range_json(r: RangeMap) -> Value {
    json!({ "inputMaxValue": r.input_max_value, "outputScale": r.output_scale })
}

/// `firstPerson`. An empty list is omitted, as the bind arrays are; the block itself stays, because it says the file
/// has first-person settings.
fn first_person_json(f: &FirstPerson) -> Value {
    let mut out = json!({});
    if !f.annotations.is_empty() {
        out["meshAnnotations"] = Value::Array(f.annotations.iter().copied().map(annotation_json).collect());
    }
    out
}

fn annotation_json(a: MeshAnnotation) -> Value {
    json!({ "node": a.node.index(), "type": flag_name(a.flag) })
}

fn flag_name(flag: FirstPersonFlag) -> &'static str {
    match flag {
        FirstPersonFlag::Auto => "auto",
        FirstPersonFlag::Both => "both",
        FirstPersonFlag::ThirdPersonOnly => "thirdPersonOnly",
        FirstPersonFlag::FirstPersonOnly => "firstPersonOnly",
    }
}

/// Reads the VRM extensions into `scene.meta` and `scene.avatar`.
pub(crate) fn read(gltf: &Gltf, scene: &mut IoScene, report: &mut Report) -> Result<(), IoError> {
    let ext = gltf.extensions.get(EXTENSION).ok_or_else(|| IoError::malformed("the file has no VRMC_vrm extension"))?;
    if ext.get("specVersion").and_then(Value::as_str) != Some("1.0") {
        return Err(IoError::unsupported(
            "a VRMC_vrm specVersion other than 1.0",
            "use the VRM 0.x importer for older files",
        ));
    }
    let meta = ext.get("meta").cloned().ok_or_else(|| IoError::malformed("VRMC_vrm.meta is required"))?;
    let keep_copyright = scene.meta.copyright_information.take();
    scene.meta = serde_json::from_value::<VrmMeta>(meta)
        .map_err(|e| IoError::Malformed { message: e.to_string(), at: Some("/extensions/VRMC_vrm/meta".into()) })?;
    if scene.meta.copyright_information.is_none() {
        scene.meta.copyright_information = keep_copyright;
    }
    let mut avatar = Avatar::new();
    read_humanoid(ext, &mut avatar, report)?;
    read_expressions(ext, &mut avatar, report)?;
    avatar.look_at = read_look_at(ext)?;
    avatar.first_person = read_first_person(ext)?;
    spring::read(gltf, &mut avatar, report)?;
    scene.avatar = Some(avatar);
    Ok(())
}

/// A malformed-file error at `at`, a JSON pointer into the file, so the message says where the fault is.
pub(crate) fn malformed(at: &str, message: impl Into<String>) -> IoError {
    IoError::Malformed { message: message.into(), at: Some(at.to_owned()) }
}

/// A JSON index (node, collider, group...) as the `u32` the scene stores.
pub(crate) fn index_of(v: &Value, at: &str) -> Result<u32, IoError> {
    v.as_u64().and_then(|n| u32::try_from(n).ok()).ok_or_else(|| malformed(at, "expected an index"))
}

/// A node index, as [`index_of`].
pub(crate) fn node_ix(v: &Value, at: &str) -> Result<NodeIx, IoError> {
    index_of(v, at).map(NodeIx)
}

/// A JSON number as the scene's `f32`. Files store doubles and the scene is single precision, as glTF data is, so the
/// narrowing is the format's.
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn number(v: &Value, at: &str) -> Result<f32, IoError> {
    v.as_f64().map(|x| x as f32).ok_or_else(|| malformed(at, "expected a number"))
}

/// Three JSON numbers as a vector.
pub(crate) fn vec3(v: &Value, at: &str) -> Result<Vec3, IoError> {
    match v.as_array().map(Vec::as_slice) {
        Some([x, y, z]) => Ok(Vec3::new(number(x, at)?, number(y, at)?, number(z, at)?)),
        _ => Err(malformed(at, "expected three numbers")),
    }
}

/// A JSON string.
pub(crate) fn text(v: &Value, at: &str) -> Result<String, IoError> {
    v.as_str().map(str::to_owned).ok_or_else(|| malformed(at, "expected a string"))
}

/// The elements of `obj[key]`: none when the key is absent, and an error naming the key when it isn't an array.
pub(crate) fn array<'a>(obj: &'a Value, key: &str, at: &str) -> Result<&'a [Value], IoError> {
    match obj.get(key) {
        None => Ok(&[]),
        Some(Value::Array(list)) => Ok(list.as_slice()),
        Some(_) => Err(malformed(&format!("{at}/{key}"), format!("{key} must be an array"))),
    }
}

/// The indices in `obj[key]`, as [`array`] reads the list.
pub(crate) fn indices(obj: &Value, key: &str, at: &str) -> Result<Vec<u32>, IoError> {
    array(obj, key, at)?.iter().enumerate().map(|(i, v)| index_of(v, &format!("{at}/{key}/{i}"))).collect()
}

/// Reads `obj[key]` with `read`, or gives `default` when the key is absent. The spec's defaults are applied here, at
/// the one place a file can leave a value out.
pub(crate) fn optional<T>(
    obj: &Value,
    key: &str,
    at: &str,
    read: impl Fn(&Value, &str) -> Result<T, IoError>,
    default: T,
) -> Result<T, IoError> {
    match obj.get(key) {
        None => Ok(default),
        Some(v) => read(v, &format!("{at}/{key}")),
    }
}

fn read_humanoid(ext: &Value, avatar: &mut Avatar, report: &mut Report) -> Result<(), IoError> {
    let bones = ext
        .pointer("/humanoid/humanBones")
        .and_then(Value::as_object)
        .ok_or_else(|| IoError::malformed("VRMC_vrm.humanoid.humanBones is required"))?;
    for (name, v) in bones {
        let at = format!("/extensions/VRMC_vrm/humanoid/humanBones/{name}");
        let node = node_ix(v.get("node").unwrap_or(&Value::Null), &at)?;
        match HumanBoneName::new(name) {
            Ok(bone) => {
                avatar.humanoid.bones.insert(bone, node);
            }
            Err(e) => report.push(Finding::warning("vrm.humanoid_unknown_bone", e.to_string()).at(at)),
        }
    }
    Ok(())
}

fn read_expressions(ext: &Value, avatar: &mut Avatar, report: &mut Report) -> Result<(), IoError> {
    for group in ["preset", "custom"] {
        let Some(map) = ext.pointer(&format!("/expressions/{group}")).and_then(Value::as_object) else { continue };
        for (name, v) in map {
            let at = format!("/extensions/VRMC_vrm/expressions/{group}/{name}");
            let parsed = match group {
                "preset" => PresetExpression::parse(name)
                    .map(ExpressionName::Preset)
                    .ok_or_else(|| format!("{name:?} is not a preset expression")),
                _ => ExpressionName::parse(name).map_err(|e| e.to_string()),
            };
            match parsed {
                Ok(n) => {
                    avatar.expressions.insert(n, expression_from(v, &at)?);
                }
                Err(why) => report.push(Finding::warning("vrm.expression_name_invalid", why).at(at)),
            }
        }
    }
    Ok(())
}

fn expression_from(v: &Value, at: &str) -> Result<Expression, IoError> {
    let mut e = Expression::new();
    e.is_binary = v.get("isBinary").and_then(Value::as_bool).unwrap_or(false);
    e.override_blink = override_of(v.get("overrideBlink"));
    e.override_look_at = override_of(v.get("overrideLookAt"));
    e.override_mouth = override_of(v.get("overrideMouth"));
    for b in v.get("morphTargetBinds").and_then(Value::as_array).into_iter().flatten() {
        let index = b.get("index").and_then(Value::as_u64).and_then(|i| u32::try_from(i).ok());
        let weight = b.get("weight").and_then(Value::as_f64);
        let (Some(index), Some(weight)) = (index, weight) else {
            return Err(IoError::Malformed {
                message: "a morph bind needs index and weight".into(),
                at: Some(format!("{at}/morphTargetBinds")),
            });
        };
        let node = node_ix(b.get("node").unwrap_or(&Value::Null), at)?;
        #[allow(clippy::cast_possible_truncation)] // glTF numbers are f32 in practice; the narrowing is the format's.
        e.morph_binds.push(MorphBind { node, index, weight: weight as f32 });
    }
    Ok(e)
}

/// `lookAt`, or `None` when the file has none. A missing `type` is "bone", a missing offset is the origin, and a range
/// map that is left out takes the defaults above.
fn read_look_at(ext: &Value) -> Result<Option<LookAt>, IoError> {
    let Some(v) = ext.get("lookAt") else { return Ok(None) };
    let at = format!("/extensions/{EXTENSION}/lookAt");
    let kind = optional(v, "type", &at, look_kind, LookAtKind::Bone)?;
    let output = match kind {
        LookAtKind::Bone => RANGE_OUTPUT_BONE_DEGREES,
        LookAtKind::Expression => RANGE_OUTPUT_EXPRESSION,
    };
    let range = |key: &str| {
        let default = RangeMap { input_max_value: RANGE_INPUT_DEGREES, output_scale: output };
        optional(v, key, &at, |r, p| range_from(r, p, output), default)
    };
    Ok(Some(LookAt {
        kind,
        offset_from_head_bone: optional(v, "offsetFromHeadBone", &at, vec3, Vec3::ZERO)?,
        horizontal_inner: range("rangeMapHorizontalInner")?,
        horizontal_outer: range("rangeMapHorizontalOuter")?,
        vertical_down: range("rangeMapVerticalDown")?,
        vertical_up: range("rangeMapVerticalUp")?,
    }))
}

fn look_kind(v: &Value, at: &str) -> Result<LookAtKind, IoError> {
    match v.as_str() {
        Some("bone") => Ok(LookAtKind::Bone),
        Some("expression") => Ok(LookAtKind::Expression),
        _ => Err(malformed(at, "lookAt.type must be \"bone\" or \"expression\"")),
    }
}

fn range_from(v: &Value, at: &str, output: f32) -> Result<RangeMap, IoError> {
    Ok(RangeMap {
        input_max_value: optional(v, "inputMaxValue", at, number, RANGE_INPUT_DEGREES)?,
        output_scale: optional(v, "outputScale", at, number, output)?,
    })
}

/// `firstPerson`, or `None` when the file has none. An empty block still counts: it says the file has first-person
/// settings, even when no mesh is annotated.
fn read_first_person(ext: &Value) -> Result<Option<FirstPerson>, IoError> {
    let Some(v) = ext.get("firstPerson") else { return Ok(None) };
    let at = format!("/extensions/{EXTENSION}/firstPerson");
    let mut annotations = Vec::new();
    for (i, a) in array(v, "meshAnnotations", &at)?.iter().enumerate() {
        let here = format!("{at}/meshAnnotations/{i}");
        annotations.push(MeshAnnotation {
            node: node_ix(a.get("node").unwrap_or(&Value::Null), &here)?,
            flag: optional(a, "type", &here, flag_from, FirstPersonFlag::Auto)?,
        });
    }
    Ok(Some(FirstPerson { annotations }))
}

fn flag_from(v: &Value, at: &str) -> Result<FirstPersonFlag, IoError> {
    match v.as_str() {
        Some("auto") => Ok(FirstPersonFlag::Auto),
        Some("both") => Ok(FirstPersonFlag::Both),
        Some("thirdPersonOnly") => Ok(FirstPersonFlag::ThirdPersonOnly),
        Some("firstPersonOnly") => Ok(FirstPersonFlag::FirstPersonOnly),
        _ => Err(malformed(at, "firstPerson type must be auto, both, thirdPersonOnly or firstPersonOnly")),
    }
}
