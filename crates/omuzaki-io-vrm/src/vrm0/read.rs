//! The VRM 0.x `VRM` extension into the scene's 1.0-shaped avatar layer, then the model turned to face +Z.
//!
//! The mapping follows the VRM 0.x specification (`specification/0.0/README.md`) and `UniVRM`'s migrator
//! (`Packages/VRM10/Runtime/Migration/*`, MIT; read for behaviour, nothing copied): thumb bones are renamed, blend shape
//! weights go from 0..100 to 0..1, `Joy`/`Sorrow`/`Fun` become `happy`/`sad`/`relaxed`, mesh references become the
//! node that holds the mesh, and the eye controller becomes `lookAt`.

use std::collections::BTreeSet;

use omuzaki_core::glam::{Vec2, Vec3, Vec4};
use omuzaki_io_api::{
    Avatar, ColorBind, ColorBindKind, CustomName, Expression, ExpressionName, Finding, FirstPerson, FirstPersonFlag,
    HumanBoneName, IoError, IoScene, LookAt, LookAtKind, MaterialIx, MeshAnnotation, MorphBind, NodeIx,
    PresetExpression, RangeMap, Report, UvBind,
};
use serde::Deserialize;

use super::wire::{WireDegreeMap, WireFirstPerson0, WireGroup, WireVrm0};
use super::{EXTENSION, materials, meta, spring};
use crate::ix32;
use crate::{avatar, orientation};
use omuzaki_gltf::schema::Document;

/// Reads the 0.x extension into `scene.meta`, `scene.provenance` and `scene.avatar`, and rotates the scene to face +Z.
/// `file` is the file name for the provenance record.
pub(crate) fn read(
    gltf: &Document,
    file: Option<&str>,
    scene: &mut IoScene,
    report: &mut Report,
) -> Result<(), IoError> {
    let body = gltf.extensions.get(EXTENSION).ok_or_else(|| {
        IoError::malformed(
            "the file has no `VRM` extension; if it is a VRM 1.0 file, import it as vrm1 (it has VRMC_vrm)",
        )
    })?;
    let at = format!("/extensions/{EXTENSION}");
    let wire = WireVrm0::deserialize(body)
        .map_err(|e| IoError::Malformed { message: format!("VRM is invalid: {e}"), at: Some(at.clone()) })?;
    meta::read(&wire.meta, file, scene, &at, report)?;
    let mut av = Avatar::new();
    for b in &wire.humanoid.human_bones {
        let Ok(node) = u32::try_from(b.node) else { continue }; // -1: the bone isn't assigned
        match HumanBoneName::from_vrm0(&b.bone) {
            Some(bone) => {
                av.humanoid.bones.insert(bone, NodeIx(node));
            }
            None => report.push(
                Finding::warning("vrm.humanoid_unknown_bone", format!("{:?} is not a VRM humanoid bone", b.bone))
                    .at(format!("{at}/humanoid/humanBones")),
            ),
        }
    }
    if let Some(fp) = &wire.first_person {
        first_person(fp, scene, &mut av);
    }
    expressions(&wire.blend_shape_master.blend_shape_groups, scene, &mut av, &at, report);
    spring::read(&wire.secondary_animation, scene, &mut av);
    materials::read(&wire.material_properties, &mut scene.materials, report);
    avatar::tidy(scene, &mut av, report);
    scene.avatar = Some(av);
    orientation::rotate_y180(scene);
    Ok(())
}

/// The first node that holds each mesh, which is what a 0.x mesh reference means in 1.0.
fn mesh_nodes(scene: &IoScene) -> Vec<Option<NodeIx>> {
    let mut out = vec![None; scene.meshes.len()];
    for (i, n) in scene.nodes.iter().enumerate() {
        if let Some(slot) = n.mesh.and_then(|m| out.get_mut(m.index())) {
            slot.get_or_insert(NodeIx(ix32(i)));
        }
    }
    out
}

fn range(map: Option<&WireDegreeMap>, kind: LookAtKind) -> RangeMap {
    let out = if kind == LookAtKind::Bone { 10.0 } else { 1.0 };
    RangeMap {
        input_max_value: map.and_then(|m| m.x_range).unwrap_or(90.0),
        output_scale: map.and_then(|m| m.y_range).unwrap_or(out),
    }
}

fn first_person(fp: &WireFirstPerson0, scene: &IoScene, av: &mut Avatar) {
    let nodes = mesh_nodes(scene);
    let annotations: Vec<MeshAnnotation> = fp
        .mesh_annotations
        .iter()
        .filter_map(|a| {
            let node = usize::try_from(a.mesh).ok().and_then(|m| nodes.get(m).copied().flatten())?;
            let flag = match a.first_person_flag.to_lowercase().as_str() {
                "both" => FirstPersonFlag::Both,
                "thirdpersononly" => FirstPersonFlag::ThirdPersonOnly,
                "firstpersononly" => FirstPersonFlag::FirstPersonOnly,
                _ => FirstPersonFlag::Auto,
            };
            Some(MeshAnnotation { node, flag })
        })
        .collect();
    if !annotations.is_empty() {
        av.first_person = Some(FirstPerson { annotations });
    }
    let has_look_at = fp.look_at_type_name.is_some()
        || fp.first_person_bone_offset.is_some()
        || [
            &fp.look_at_horizontal_inner,
            &fp.look_at_horizontal_outer,
            &fp.look_at_vertical_down,
            &fp.look_at_vertical_up,
        ]
        .iter()
        .any(|m| m.is_some());
    if has_look_at {
        let kind = if fp.look_at_type_name.as_deref().is_some_and(|t| t.eq_ignore_ascii_case("blendshape")) {
            LookAtKind::Expression
        } else {
            LookAtKind::Bone
        };
        av.look_at = Some(LookAt {
            kind,
            offset_from_head_bone: fp.first_person_bone_offset.map_or(Vec3::ZERO, |o| Vec3::from_array(o.to_array())),
            horizontal_inner: range(fp.look_at_horizontal_inner.as_ref(), kind),
            horizontal_outer: range(fp.look_at_horizontal_outer.as_ref(), kind),
            vertical_down: range(fp.look_at_vertical_down.as_ref(), kind),
            vertical_up: range(fp.look_at_vertical_up.as_ref(), kind),
        });
    }
}

/// The preset a 0.x clip stands for: its `presetName`, or for `Unknown` its own name lower-cased (0.x compares ids
/// upper-cased), plus the one 1.0 preset 0.x lacks, `surprised`, which a 0.x file may carry as a custom clip.
fn preset_of(preset_name: Option<&str>, name: &str) -> Option<PresetExpression> {
    let src = preset_name.map(str::to_lowercase).filter(|p| p != "unknown").unwrap_or_else(|| name.to_lowercase());
    if src == "surprised" { Some(PresetExpression::Surprised) } else { PresetExpression::from_vrm0_preset(&src) }
}

/// A free custom expression name close to `wanted`: presets and empty names can't be custom, and names must be unique.
fn custom_key(wanted: &str, taken: &BTreeSet<String>) -> (ExpressionName, bool) {
    let base = if wanted.is_empty() { "unnamed".to_owned() } else { wanted.to_owned() };
    let mut candidate = if CustomName::new(base.clone()).is_ok() { base.clone() } else { format!("{base}_custom") };
    let mut n = 2;
    while taken.contains(&candidate) {
        candidate = format!("{base}_{n}");
        n += 1;
    }
    let renamed = candidate != wanted;
    // `candidate` is non-empty and not a preset name by construction, so this cannot fail; the fallback documents that.
    let key =
        CustomName::new(candidate).map_or(ExpressionName::Preset(PresetExpression::Neutral), ExpressionName::Custom);
    (key, renamed)
}

fn expressions(groups: &[WireGroup], scene: &IoScene, av: &mut Avatar, at: &str, report: &mut Report) {
    let nodes = mesh_nodes(scene);
    let mut taken: BTreeSet<String> = BTreeSet::new();
    for (i, g) in groups.iter().enumerate() {
        let here = format!("{at}/blendShapeMaster/blendShapeGroups/{i}");
        let key = match preset_of(g.preset_name.as_deref(), &g.name) {
            Some(p) if !av.expressions.contains_key(&ExpressionName::Preset(p)) => ExpressionName::Preset(p),
            _ => {
                let (key, renamed) = custom_key(&g.name, &taken);
                if renamed {
                    let msg = format!("expression {:?} was renamed {:?} to keep names unique", g.name, key.as_str());
                    report.push(Finding::info("vrm0.expression_renamed", msg).at(&here));
                }
                key
            }
        };
        taken.insert(key.as_str().to_owned());
        let mut e = Expression::new();
        e.is_binary = g.is_binary;
        let mut unresolved = 0;
        for b in &g.binds {
            let node = usize::try_from(b.mesh).ok().and_then(|m| nodes.get(m).copied().flatten());
            match (node, u32::try_from(b.index)) {
                // 0.x weights are 0..100.
                (Some(node), Ok(index)) => e.morph_binds.push(MorphBind { node, index, weight: b.weight / 100.0 }),
                _ => unresolved += 1,
            }
        }
        if unresolved > 0 {
            report.push(
                Finding::warning(
                    "vrm0.bind_dropped",
                    format!("{unresolved} morph bind(s) name a mesh no node uses, so they were dropped"),
                )
                .at(&here),
            );
        }
        material_values(g, scene, &mut e, &here, report);
        av.expressions.insert(key, e);
    }
}

/// `materialValues`: colour properties become colour binds; the `_MainTex_ST` family becomes one texture transform bind
/// per material (the first wins, like in `UniVRM`).
fn material_values(g: &WireGroup, scene: &IoScene, e: &mut Expression, at: &str, report: &mut Report) {
    for v in &g.material_values {
        let Some(material) = scene.materials.iter().position(|m| m.name == v.material_name) else {
            let msg = format!("material {:?} doesn't exist", v.material_name);
            report.push(Finding::warning("vrm0.material_value_dropped", msg).at(at));
            continue;
        };
        let material = MaterialIx(ix32(material));
        let t = &v.target_value;
        // 0.x gives `_MainTex_ST` as scale then offset in Unity's UV space; glTF's V axis points the other way.
        let transform = |scale: Vec2, offset: Vec2| UvBind {
            material,
            scale,
            offset: Vec2::new(offset.x, 1.0 - offset.y - scale.y),
        };
        let uv = match (v.property_name.as_str(), t.as_slice()) {
            ("_MainTex_ST", [sx, sy, ox, oy, ..]) => Some(transform(Vec2::new(*sx, *sy), Vec2::new(*ox, *oy))),
            ("_MainTex_ST_S", [sx, _, ox, ..]) => Some(transform(Vec2::new(*sx, 1.0), Vec2::new(*ox, 0.0))),
            ("_MainTex_ST_T", [_, sy, _, oy, ..]) => Some(transform(Vec2::new(1.0, *sy), Vec2::new(0.0, *oy))),
            _ => None,
        };
        let kind = match v.property_name.as_str() {
            "_Color" => Some(ColorBindKind::Color),
            "_EmissionColor" => Some(ColorBindKind::EmissionColor),
            "_RimColor" => Some(ColorBindKind::RimColor),
            "_ShadeColor" => Some(ColorBindKind::ShadeColor),
            "_OutlineColor" => Some(ColorBindKind::OutlineColor),
            _ => None,
        };
        if let Some(uv) = uv {
            if !e.uv_binds.iter().any(|b| b.material == material) {
                e.uv_binds.push(uv);
            }
        } else if let (Some(kind), [r, g, b, rest @ ..]) = (kind, t.as_slice()) {
            e.color_binds.push(ColorBind {
                material,
                kind,
                target: Vec4::new(*r, *g, *b, rest.first().copied().unwrap_or(1.0)),
            });
        } else {
            let msg = format!("material value {:?} can't be expressed in 1.0", v.property_name);
            report.push(Finding::info("vrm0.material_value_dropped", msg).at(at));
        }
    }
}
