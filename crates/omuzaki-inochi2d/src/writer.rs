//! An INP1 writer, for puppets this project makes (round-trip tests, fixtures). It writes what [`crate::import`] reads
//! back to the same [`PuppetDef`] wherever INP1 can hold it, and refuses what it can't rather than writing something
//! else.

use image::ImageEncoder;
use image::codecs::png::PngEncoder;
use omuzaki_puppet_api::{
    BindTarget, BindValues, Binding, BlendMode, Drawable, Interp, MapMode, MaskMode, Mesh, NodeKind, Param, Pendulum,
    PendulumModel, PuppetDef,
};
use serde_json::{Map, Value, json};

use crate::container::{INP1_MAGIC, TEX_SECT, TextureEncoding};
use crate::error::WriteError;
use crate::texture::Texture;

/// The deepest tree written: `serde_json` reads at most 128 nested arrays and objects, and each tree level takes two
/// (the node and its `children`), plus a few for the node's own fields.
pub const MAX_WRITE_DEPTH: usize = 60;

/// The node type names this writer writes for the kinds it knows; an unknown node can't take one of them.
const KNOWN_TYPES: [&str; 8] = ["Node", "Part", "Mask", "Composite", "MeshGroup", "SimplePhysics", "Shapes", "Tmp"];

fn unsupported(path: impl Into<String>, detail: impl Into<String>) -> WriteError {
    WriteError::Unsupported { path: path.into(), detail: detail.into() }
}

fn u32_of(n: usize, path: &str, what: &str) -> Result<u32, WriteError> {
    u32::try_from(n).map_err(|_| unsupported(path, format!("{n} {what}")))
}

/// Writes `def` and its textures (in [`PuppetDef::textures`] order, as decoded RGBA8) as an INP1 file, its textures as
/// PNG.
///
/// Node uuids are the node indices and parameter uuids follow them. A pendulum is written as its anchor node when that
/// node is a plain group of the same name that anchors only it (`Inochi2D`'s `SimplePhysics` is a node), and otherwise
/// as a new `SimplePhysics` child of its anchor, which reads back as one more node.
///
/// # Errors
/// [`WriteError::Invalid`] when `def` fails [`PuppetDef::check`], [`WriteError::Texture`] when `textures` doesn't match
/// its texture list, [`WriteError::Unsupported`] for what INP1 can't hold (a lattice, an unknown node named like a known
/// type, a parameter with several keys over an empty range, a tree deeper than [`MAX_WRITE_DEPTH`]) and
/// [`WriteError::Encode`] when a texture fails to encode.
pub fn write_inp1(def: &PuppetDef, textures: &[Texture]) -> Result<Vec<u8>, WriteError> {
    def.check()?;
    if textures.len() != def.textures.len() {
        return Err(WriteError::Texture {
            index: textures.len().min(def.textures.len()),
            detail: format!("{} textures passed for {} in the puppet", textures.len(), def.textures.len()),
        });
    }
    for (index, (tex, r)) in textures.iter().zip(&def.textures).enumerate() {
        let expected = u64::from(tex.size[0]) * u64::from(tex.size[1]) * 4;
        if tex.size != r.size || tex.rgba.len() as u64 != expected {
            return Err(WriteError::Texture {
                index,
                detail: format!("{:?} with {} bytes, for a {:?} texture", tex.size, tex.rgba.len(), r.size),
            });
        }
    }

    let n = def.nodes.len();
    let param_base = n;
    let extra_base = n + def.params.len();
    let mut values: Vec<Option<Map<String, Value>>> = Vec::with_capacity(n);
    let mut kids: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, node) in def.nodes.iter().enumerate() {
        values.push(Some(node_json(def, i, n)?));
        if let Some(p) = node.parent {
            kids[p.get()].push(i);
        }
    }
    depth_check(def)?;

    // Pendulums: in place on a plain group of the same name that anchors only this one, else a new child.
    let mut extra: Vec<(usize, Map<String, Value>)> = Vec::new();
    for (k, pendulum) in def.physics.iter().enumerate() {
        let a = pendulum.anchor.get();
        let alone = def.physics.iter().filter(|p| p.anchor == pendulum.anchor).count() == 1;
        let in_place = alone && matches!(def.nodes[a].kind, NodeKind::Group) && def.nodes[a].name == pendulum.name;
        let fields = pendulum_json(pendulum, param_base);
        if in_place {
            if let Some(Some(obj)) = values.get_mut(a) {
                obj.insert("type".into(), json!("SimplePhysics"));
                obj.extend(fields);
            }
        } else {
            let uuid = u32_of(extra_base + extra.len(), &format!("physics/{k}"), "uuids")?;
            let mut obj = Map::new();
            obj.insert("uuid".into(), json!(uuid));
            obj.insert("name".into(), json!(pendulum.name));
            obj.insert("type".into(), json!("SimplePhysics"));
            obj.insert("enabled".into(), json!(true));
            obj.insert("zsort".into(), json!(0.0));
            obj.insert(
                "transform".into(),
                json!({ "trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0] }),
            );
            obj.extend(fields);
            extra.push((a, obj));
        }
    }

    // Children have higher indices than their parents, so building from the last node up nests every subtree before
    // its parent takes it, with no recursion.
    for i in (0..n).rev() {
        let mut children: Vec<Value> = Vec::with_capacity(kids[i].len());
        for &c in &kids[i] {
            if let Some(obj) = values.get_mut(c).and_then(Option::take) {
                children.push(Value::Object(obj));
            }
        }
        for (_, obj) in extra.iter().filter(|(a, _)| *a == i) {
            children.push(Value::Object(obj.clone()));
        }
        if let Some(Some(obj)) = values.get_mut(i)
            && !children.is_empty()
        {
            obj.insert("children".into(), Value::Array(children));
        }
    }
    let root = values.first_mut().and_then(Option::take).unwrap_or_default();

    let mut params = Vec::with_capacity(def.params.len());
    for (p, param) in def.params.iter().enumerate() {
        params.push(param_json(param, p, param_base)?);
    }

    let payload = json!({
        "meta": meta_json(def),
        "physics": { "pixelsPerMeter": def.world.pixels_per_meter, "gravity": def.world.gravity },
        "nodes": Value::Object(root),
        "param": params,
    });
    let json = serde_json::to_vec(&payload).map_err(|e| unsupported("", e.to_string()))?;

    let mut out = Vec::with_capacity(json.len() + 64);
    out.extend_from_slice(&INP1_MAGIC);
    out.extend_from_slice(&u32_of(json.len(), "", "payload bytes")?.to_be_bytes());
    out.extend_from_slice(&json);
    out.extend_from_slice(&TEX_SECT);
    out.extend_from_slice(&u32_of(textures.len(), "textures", "textures")?.to_be_bytes());
    for (index, tex) in textures.iter().enumerate() {
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(&tex.rgba, tex.size[0], tex.size[1], image::ExtendedColorType::Rgba8)
            .map_err(|e| WriteError::Encode { index, detail: e.to_string() })?;
        out.extend_from_slice(&u32_of(png.len(), &format!("textures/{index}"), "encoded bytes")?.to_be_bytes());
        out.push(u8::try_from(TextureEncoding::Png.code()).unwrap_or(0));
        out.extend_from_slice(&png);
    }
    Ok(out)
}

fn depth_check(def: &PuppetDef) -> Result<(), WriteError> {
    let mut depth = vec![1usize; def.nodes.len()];
    for (i, node) in def.nodes.iter().enumerate() {
        if let Some(p) = node.parent {
            depth[i] = depth[p.get()] + 1;
        }
        if depth[i] > MAX_WRITE_DEPTH {
            return Err(unsupported(format!("nodes/{i}"), format!("a tree deeper than {MAX_WRITE_DEPTH}")));
        }
    }
    Ok(())
}

fn node_json(def: &PuppetDef, i: usize, n: usize) -> Result<Map<String, Value>, WriteError> {
    let node = &def.nodes[i];
    let path = format!("nodes/{i}");
    let t = &node.transform;
    let mut obj = Map::new();
    obj.insert("uuid".into(), json!(u32_of(i, &path, "nodes")?));
    obj.insert("name".into(), json!(node.name));
    obj.insert("enabled".into(), json!(node.enabled));
    obj.insert("zsort".into(), json!(node.zsort));
    obj.insert(
        "transform".into(),
        json!({
            "trans": [t.translation[0], t.translation[1], 0.0],
            "rot": [0.0, 0.0, t.rotation],
            "scale": [t.scale[0], t.scale[1]],
        }),
    );
    let ty = match &node.kind {
        NodeKind::Group => "Node",
        NodeKind::Part(part) => {
            obj.insert("mesh".into(), mesh_json(&part.mesh, true));
            obj.insert("textures".into(), json!([part.texture.0, u32::MAX, u32::MAX]));
            drawable_json(&part.draw, &mut obj, &path, n)?;
            "Part"
        }
        NodeKind::Mask(mesh) => {
            obj.insert("mesh".into(), mesh_json(mesh, !mesh.uvs.is_empty()));
            "Mask"
        }
        NodeKind::Composite(c) => {
            drawable_json(&c.draw, &mut obj, &path, n)?;
            // Our mesh deformers carry everything under them, composites included.
            obj.insert("propagate_meshgroup".into(), json!(true));
            "Composite"
        }
        NodeKind::MeshDeformer(mesh) => {
            obj.insert("mesh".into(), mesh_json(mesh, false));
            obj.insert("dynamic_deformation".into(), json!(false));
            obj.insert("translate_children".into(), json!(false));
            "MeshGroup"
        }
        NodeKind::Unknown { type_name } => {
            if KNOWN_TYPES.contains(&type_name.as_str()) {
                return Err(unsupported(path, format!("an unknown node named like the known type {type_name:?}")));
            }
            type_name.as_str()
        }
        NodeKind::Lattice(_) => return Err(unsupported(path, "a lattice deformer (Inochi2D 0.8 has none)")),
        _ => return Err(unsupported(path, "a node kind this writer doesn't know")),
    };
    obj.insert("type".into(), json!(ty));
    Ok(obj)
}

fn mesh_json(mesh: &Mesh, uvs: bool) -> Value {
    let verts: Vec<f32> = mesh.vertices.iter().flatten().copied().collect();
    let indices: Vec<u32> = mesh.triangles.iter().flatten().copied().collect();
    let mut obj = Map::new();
    obj.insert("verts".into(), json!(verts));
    if uvs {
        let uvs: Vec<f32> = mesh.uvs.iter().flatten().copied().collect();
        obj.insert("uvs".into(), json!(uvs));
    }
    obj.insert("indices".into(), json!(indices));
    obj.insert("origin".into(), json!([0.0, 0.0]));
    Value::Object(obj)
}

fn drawable_json(draw: &Drawable, obj: &mut Map<String, Value>, path: &str, n: usize) -> Result<(), WriteError> {
    let blend = match draw.blend {
        BlendMode::Normal => "Normal",
        BlendMode::Multiply => "Multiply",
        BlendMode::Screen => "Screen",
        BlendMode::Overlay => "Overlay",
        BlendMode::Darken => "Darken",
        BlendMode::Lighten => "Lighten",
        BlendMode::ColorDodge => "ColorDodge",
        BlendMode::LinearDodge => "LinearDodge",
        BlendMode::ColorBurn => "ColorBurn",
        BlendMode::HardLight => "HardLight",
        BlendMode::SoftLight => "SoftLight",
        BlendMode::Difference => "Difference",
        BlendMode::Exclusion => "Exclusion",
        BlendMode::Subtract => "Subtract",
        BlendMode::ClipToLower => "ClipToLower",
        BlendMode::SliceFromLower => "SliceFromLower",
        _ => return Err(unsupported(format!("{path}/draw/blend"), "a blend mode this writer doesn't know")),
    };
    obj.insert("blend_mode".into(), json!(blend));
    obj.insert("opacity".into(), json!(draw.opacity));
    obj.insert("mask_threshold".into(), json!(draw.mask_threshold));
    obj.insert("tint".into(), json!(draw.tint));
    obj.insert("screenTint".into(), json!(draw.screen_tint));
    let mut masks = Vec::with_capacity(draw.masks.len());
    for m in &draw.masks {
        let mode = match m.mode {
            MaskMode::Mask => "Mask",
            MaskMode::Dodge => "DodgeMask",
        };
        // Node uuids are node indices, and `check` has proved the source is a node.
        debug_assert!(m.source.get() < n, "a checked mask source");
        masks.push(json!({ "source": m.source.0, "mode": mode }));
    }
    obj.insert("masks".into(), Value::Array(masks));
    Ok(())
}

fn pendulum_json(p: &Pendulum, param_base: usize) -> Map<String, Value> {
    let model = match p.model {
        PendulumModel::Rigid => "Pendulum",
        PendulumModel::Spring => "SpringPendulum",
    };
    let map = match p.map {
        MapMode::AngleLength => "AngleLength",
        MapMode::LengthAngle => "LengthAngle",
        MapMode::Xy => "XY",
        MapMode::Yx => "YX",
    };
    let mut obj = Map::new();
    obj.insert("param".into(), json!(param_base + p.param.get()));
    obj.insert("model_type".into(), json!(model));
    obj.insert("map_mode".into(), json!(map));
    obj.insert("gravity".into(), json!(p.gravity));
    obj.insert("length".into(), json!(p.length));
    obj.insert("frequency".into(), json!(p.frequency));
    obj.insert("angle_damping".into(), json!(p.angle_damping));
    obj.insert("length_damping".into(), json!(p.length_damping));
    obj.insert("output_scale".into(), json!(p.output_scale));
    obj.insert("local_only".into(), json!(p.local_only));
    obj
}

/// Key positions back to `Inochi2D`'s normalised `0..1`.
fn axis_points(param: &Param, axis: usize, path: &str) -> Result<Vec<f32>, WriteError> {
    let (min, max) = (param.min[axis], param.max[axis]);
    let keys = &param.keys[axis];
    let span = max - min;
    if span == 0.0 {
        if keys.len() > 1 {
            return Err(unsupported(path, "several key points over an empty range"));
        }
        return Ok(vec![0.0; keys.len()]);
    }
    Ok(keys.iter().map(|k| (k - min) / span).collect())
}

fn target_name(target: BindTarget, path: &str) -> Result<&'static str, WriteError> {
    Ok(match target {
        BindTarget::Deform => "deform",
        BindTarget::TranslateX => "transform.t.x",
        BindTarget::TranslateY => "transform.t.y",
        BindTarget::Rotate => "transform.r.z",
        BindTarget::RotateY => "transform.r.y",
        BindTarget::RotateX => "transform.r.x",
        BindTarget::ScaleX => "transform.s.x",
        BindTarget::ScaleY => "transform.s.y",
        BindTarget::ZSort => "zSort",
        BindTarget::Opacity => "opacity",
        BindTarget::TintR => "tint.r",
        BindTarget::TintG => "tint.g",
        BindTarget::TintB => "tint.b",
        _ => return Err(unsupported(path, "a binding target this writer doesn't know")),
    })
}

fn param_json(param: &Param, p: usize, param_base: usize) -> Result<Value, WriteError> {
    let path = format!("params/{p}");
    let px = axis_points(param, 0, &format!("{path}/keys/0"))?;
    let py = axis_points(param, 1, &format!("{path}/keys/1"))?;
    let ny = param.keys[1].len();
    let mut bindings = Vec::with_capacity(param.bindings.len());
    for (b, binding) in param.bindings.iter().enumerate() {
        bindings.push(binding_json(binding, &format!("{path}/bindings/{b}"), px.len(), ny)?);
    }
    Ok(json!({
        "uuid": param_base + p,
        "name": param.name,
        "is_vec2": param.two_axes,
        "min": param.min,
        "max": param.max,
        "defaults": param.default,
        "axis_points": [px, py],
        "merge_mode": "Additive",
        "bindings": bindings,
    }))
}

/// A binding's x-major values as `Inochi2D`'s `[x][y]`.
fn binding_json(binding: &Binding, path: &str, nx: usize, ny: usize) -> Result<Value, WriteError> {
    let grid = |at: &dyn Fn(usize) -> Value| -> Value {
        Value::Array((0..nx).map(|ix| Value::Array((0..ny).map(|iy| at(ix * ny + iy)).collect())).collect())
    };
    let values = match &binding.values {
        BindValues::Scalar(v) => grid(&|k| json!(v[k])),
        BindValues::Offsets(sets) => grid(&|k| json!(sets[k])),
    };
    let set = grid(&|k| json!(binding.set.get(k).copied().unwrap_or(true)));
    let interp = match binding.interp {
        Interp::Nearest => "Nearest",
        Interp::Linear => "Linear",
        Interp::Cubic => "Cubic",
    };
    Ok(json!({
        "node": binding.node.0,
        "param_name": target_name(binding.target, path)?,
        "values": values,
        "isSet": set,
        "interpolate_mode": interp,
    }))
}

fn meta_json(def: &PuppetDef) -> Value {
    let m = &def.meta;
    let rights: Map<String, Value> = m
        .rights
        .iter()
        .map(|(k, v)| {
            let value = match v.as_str() {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ => Value::String(v.clone()),
            };
            (k.clone(), value)
        })
        .collect();
    let mut obj = Map::new();
    obj.insert("name".into(), json!(m.name));
    for (key, value) in
        [("artist", &m.artist), ("rigger", &m.rigger), ("copyright", &m.copyright), ("licenseURL", &m.licence_url)]
    {
        if let Some(value) = value {
            obj.insert(key.into(), json!(value));
        }
    }
    if !rights.is_empty() {
        obj.insert("rights".into(), Value::Object(rights));
    }
    obj.insert("thumbnailId".into(), json!(u32::MAX));
    obj.insert("preservePixels".into(), json!(false));
    Value::Object(obj)
}
