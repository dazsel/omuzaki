//! The scene as the VRM 0.x `VRM` extension. The caller has already rotated a copy of the scene to face −Z.
//!
//! 0.x can say less than 1.0, and every loss is a finding: no `corporation` commercial use or content flags in the meta,
//! no node constraints, no per-joint spring parameters, no capsule colliders, no `surprised` preset (it is written as
//! a custom clip, which the 0.x importer maps back), no `matcap` colour binds, and one texture transform per material.

use omuzaki_gltf::schema::Document;
use omuzaki_gltf::to_json;
use omuzaki_io_api::{
    ColorBindKind, Expression, ExpressionName, Finding, FirstPersonFlag, IoScene, LookAtKind, PresetExpression,
    RangeMap, Report,
};
use serde_json::Value;

use super::wire::{
    WireAnnotation0, WireBind0, WireBlendShapeMaster, WireDegreeMap, WireFirstPerson0, WireGroup, WireHumanBone0,
    WireHumanoid0, WireMaterialValue, WireVec3, WireVrm0,
};
use super::{EXTENSION, materials, meta, spring};

/// A Unity linear animation curve as 0.x stores it: two keyframes (time, value, in tangent, out tangent).
const LINEAR_CURVE: [f32; 8] = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 0.0];

/// Removes `null` members, recursively: 0.x readers expect a number or a string, not `null`.
pub(super) fn strip_nulls(v: &mut Value) {
    if let Value::Object(map) = v {
        map.retain(|_, x| !x.is_null());
        map.values_mut().for_each(strip_nulls);
    }
}

/// Adds the `VRM` extension for `scene.avatar` to the document.
pub(crate) fn write(scene: &IoScene, document: &mut Document, report: &mut Report) {
    let Some(av) = &scene.avatar else { return };
    let meta = meta::write(scene, document, report);
    let head = av.humanoid.node("head");
    let wire = WireVrm0 {
        exporter_version: None,
        spec_version: "0.0".to_owned(),
        meta,
        humanoid: humanoid(av),
        first_person: Some(first_person(scene, head.map(|n| i64::from(n.0)), &mut *report)),
        blend_shape_master: WireBlendShapeMaster { blend_shape_groups: expressions(scene, report) },
        secondary_animation: spring::write(scene, av, report),
        material_properties: scene.materials.iter().enumerate().map(|(i, m)| materials::write(m, i, report)).collect(),
    };
    let mut body = to_json(&wire);
    // The meta is written exactly as `Vrm0Meta` built it: a creator's own keys keep every value, `null` included.
    let meta = body.as_object_mut().and_then(|members| members.remove("meta"));
    strip_nulls(&mut body);
    if let (Some(meta), Some(members)) = (meta, body.as_object_mut()) {
        members.insert("meta".to_owned(), meta);
    }
    document.extensions.insert(EXTENSION.to_owned(), body);
    if !av.constraints.is_empty() {
        report.push(
            Finding::warning("vrm0.constraints_dropped", "0.x has no node constraints; they were dropped")
                .at("avatar/constraints"),
        );
    }
}

fn humanoid(av: &omuzaki_io_api::Avatar) -> WireHumanoid0 {
    WireHumanoid0 {
        human_bones: av
            .humanoid
            .bones
            .iter()
            .map(|(bone, node)| WireHumanBone0 {
                bone: bone.spec().vrm0_name().into_owned(),
                node: i64::from(node.0),
                use_default_values: Some(true),
            })
            .collect(),
        // Unity's defaults for its humanoid avatar description.
        arm_stretch: Some(0.05),
        leg_stretch: Some(0.05),
        upper_arm_twist: Some(0.5),
        lower_arm_twist: Some(0.5),
        upper_leg_twist: Some(0.5),
        lower_leg_twist: Some(0.5),
        feet_spacing: Some(0.0),
        has_translation_dof: Some(false),
    }
}

fn degree_map(r: RangeMap) -> WireDegreeMap {
    WireDegreeMap {
        curve: Some(LINEAR_CURVE.to_vec()),
        x_range: Some(r.input_max_value),
        y_range: Some(r.output_scale),
    }
}

fn first_person(scene: &IoScene, head: Option<i64>, report: &mut Report) -> WireFirstPerson0 {
    let av = scene.avatar.as_ref();
    let mut out = WireFirstPerson0 { first_person_bone: Some(head.unwrap_or(-1)), ..WireFirstPerson0::default() };
    if let Some(look) = av.and_then(|a| a.look_at.as_ref()) {
        out.first_person_bone_offset = Some(WireVec3::from_array(look.offset_from_head_bone.to_array()));
        out.look_at_type_name = Some(if look.kind == LookAtKind::Bone { "Bone" } else { "BlendShape" }.to_owned());
        out.look_at_horizontal_inner = Some(degree_map(look.horizontal_inner));
        out.look_at_horizontal_outer = Some(degree_map(look.horizontal_outer));
        out.look_at_vertical_down = Some(degree_map(look.vertical_down));
        out.look_at_vertical_up = Some(degree_map(look.vertical_up));
    }
    for a in av.and_then(|a| a.first_person.as_ref()).into_iter().flat_map(|f| &f.annotations) {
        let Some(mesh) = scene.nodes.get(a.node.index()).and_then(|n| n.mesh) else {
            report.push(
                Finding::warning("vrm0.annotation_dropped", "a first-person annotation names a node without a mesh")
                    .at("avatar/firstPerson"),
            );
            continue;
        };
        out.mesh_annotations.push(WireAnnotation0 {
            mesh: i64::from(mesh.0),
            first_person_flag: match a.flag {
                FirstPersonFlag::Auto => "Auto",
                FirstPersonFlag::Both => "Both",
                FirstPersonFlag::ThirdPersonOnly => "ThirdPersonOnly",
                FirstPersonFlag::FirstPersonOnly => "FirstPersonOnly",
            }
            .to_owned(),
        });
    }
    out
}

/// `(presetName, name)` of a clip. 0.x ids compare upper-cased, and `UniVRM` writes preset names lower-case, which
/// every reader accepts; `surprised` has no 0.x preset, so it is a custom clip the 0.x importer recognises by name.
fn clip_names(name: &ExpressionName) -> (String, String) {
    match name {
        ExpressionName::Preset(p) => match p.vrm0_preset() {
            Some(v0) => (v0.to_lowercase(), v0.to_owned()),
            None => ("unknown".to_owned(), p.as_str().to_owned()),
        },
        ExpressionName::Custom(c) => ("unknown".to_owned(), c.as_str().to_owned()),
    }
}

fn expressions(scene: &IoScene, report: &mut Report) -> Vec<WireGroup> {
    let Some(av) = &scene.avatar else { return Vec::new() };
    av.expressions
        .iter()
        .map(|(name, e)| {
            let (preset, label) = clip_names(name);
            if *name == ExpressionName::Preset(PresetExpression::Surprised) {
                report.push(
                    Finding::info(
                        "vrm0.preset_surprised",
                        "0.x has no `surprised` preset; it is written as a custom clip",
                    )
                    .at("avatar/expressions/surprised"),
                );
            }
            clip(scene, e, preset, label, name, report)
        })
        .collect()
}

fn clip(
    scene: &IoScene,
    e: &Expression,
    preset: String,
    label: String,
    name: &ExpressionName,
    report: &mut Report,
) -> WireGroup {
    let at = format!("avatar/expressions/{name}");
    let binds = e
        .morph_binds
        .iter()
        .filter_map(|b| {
            let mesh = scene.nodes.get(b.node.index()).and_then(|n| n.mesh)?;
            Some(WireBind0 { mesh: i64::from(mesh.0), index: i64::from(b.index), weight: b.weight * 100.0 })
        })
        .collect();
    let mut values = Vec::new();
    for b in &e.color_binds {
        let Some(m) = scene.materials.get(b.material.index()) else { continue };
        let property = match b.kind {
            ColorBindKind::Color => "_Color",
            ColorBindKind::EmissionColor => "_EmissionColor",
            ColorBindKind::ShadeColor => "_ShadeColor",
            ColorBindKind::RimColor => "_RimColor",
            ColorBindKind::OutlineColor => "_OutlineColor",
            ColorBindKind::MatcapColor => {
                report.push(
                    Finding::warning("vrm0.material_value_dropped", "0.x has no matcap colour bind; dropped").at(&at),
                );
                continue;
            }
        };
        values.push(WireMaterialValue {
            material_name: m.name.clone(),
            property_name: property.to_owned(),
            target_value: b.target.to_array().to_vec(),
        });
    }
    for b in &e.uv_binds {
        let Some(m) = scene.materials.get(b.material.index()) else { continue };
        // Back to Unity's UV space, scale first (see the reader).
        let offset_y = 1.0 - b.offset.y - b.scale.y;
        values.push(WireMaterialValue {
            material_name: m.name.clone(),
            property_name: "_MainTex_ST".to_owned(),
            target_value: vec![b.scale.x, b.scale.y, b.offset.x, offset_y],
        });
    }
    WireGroup { name: label, preset_name: Some(preset), binds, material_values: values, is_binary: e.is_binary }
}
