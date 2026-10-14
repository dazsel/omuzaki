//! VRM 0.x end to end: round trips of generated avatars, the 0.x → 1.0 orientation conversion, the mapping of every
//! part of the extension, and `MToon` 0.x conversion with numbers worked out by hand.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::field_reassign_with_default,
    clippy::float_cmp,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::too_many_lines,
    clippy::assert_is_empty,
    clippy::many_single_char_names
)]

mod common;

use omuzaki_core::glam::{Mat4, Quat, Vec3};
use omuzaki_io_api::{
    AlphaMode, Confidence, ExpressionName, HumanBoneName, ImportInput, ImportOptions, Importer, IoScene, NodeIx,
    OutlineMode, Severity, Shading, Toon,
};
use omuzaki_io_vrm::{Vrm0Exporter, Vrm0Importer, Vrm1Importer};
use serde_json::{Value, json};

use common::{BONES0, avatar, export, export_with, glb, import, minimal0, options, parts, toon_numbers, vrm0_friendly};

fn assert_close(a: &[f32], b: &[f32], what: &str) {
    assert_eq!(a.len(), b.len(), "{what}: lengths");
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        assert!((x - y).abs() < 2e-5, "{what}[{i}]: {x} vs {y}");
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Round trip of a generated avatar

#[test]
fn a_generated_avatar_round_trips_through_vrm0() {
    let (scene, ids) = vrm0_friendly(avatar());
    let file = export(&Vrm0Exporter, &scene);
    let out = import(&Vrm0Importer, &file);
    let back = &out.scene;
    assert_eq!(out.report.count(Severity::Error), 0);

    // Nodes: everything the scene had comes back exactly (the orientation change is bit-exact); the two spring chains
    // end in leaves, so each got an explicit tail node `<leaf>_end`, 7 cm along the leaf's own direction.
    let n = scene.nodes.len();
    assert_eq!(back.nodes.len(), n + 2);
    for i in 0..n {
        let (a, b) = (&scene.nodes[i], &back.nodes[i]);
        assert_eq!(
            (&a.name, a.transform, a.mesh, a.skin, &a.passthrough),
            (&b.name, b.transform, b.mesh, b.skin, &b.passthrough),
            "node {i}"
        );
    }
    assert_eq!(back.roots, scene.roots);
    let tails: Vec<_> = back.nodes[n..].iter().map(|t| t.name.as_str()).collect();
    assert_eq!(tails, ["hairA3_end", "hairB2_end"]);
    for tail in &back.nodes[n..] {
        assert!((tail.transform.translation.length() - 0.07).abs() < 1e-6);
    }
    assert_eq!(back.nodes[ids.node("hairA3").index()].children, [NodeIx(u32::try_from(n).unwrap())]);

    // Geometry, skinning, textures: untouched.
    assert_eq!(back.meshes, scene.meshes);
    assert_eq!(back.skins, scene.skins);
    assert_eq!(back.images, scene.images);
    assert_eq!(back.textures, scene.textures);

    // Materials: glTF ones exactly, toon ones to float precision.
    for (i, (a, b)) in scene.materials.iter().zip(&back.materials).enumerate() {
        match (&a.shading, &b.shading) {
            (Shading::Toon(x), Shading::Toon(y)) => {
                assert_close(&toon_numbers(y), &toon_numbers(x), &format!("material {i} numbers"));
                for (p, q) in [
                    (&x.shade_multiply_texture, &y.shade_multiply_texture),
                    (&x.matcap_texture, &y.matcap_texture),
                    (&x.rim_multiply_texture, &y.rim_multiply_texture),
                    (&x.outline_width_texture, &y.outline_width_texture),
                    (&x.uv_animation_mask, &y.uv_animation_mask),
                ] {
                    assert_eq!(p, q, "material {i} textures");
                }
                assert_eq!(
                    (a.alpha, a.double_sided, &a.base_color_texture, &a.normal_texture),
                    (b.alpha, b.double_sided, &b.base_color_texture, &b.normal_texture),
                    "material {i}"
                );
                assert_close(&b.base_color.to_array(), &a.base_color.to_array(), "base colour");
                assert_close(&b.emissive.to_array(), &a.emissive.to_array(), "emissive");
            }
            _ => assert_eq!(a, b, "material {i}"),
        }
    }

    // Meta: what 0.x can say is kept.
    assert_eq!(back.meta, scene.meta);
    let provenance = back.provenance.as_ref().unwrap();
    assert_eq!(provenance.licence, "LicenseRef-VRM-Redistribution-Prohibited");
    assert_eq!(
        provenance.commercial,
        omuzaki_store_api::Commercial::Ok,
        "the avatar flag says commercial use is allowed"
    );

    // Avatar: humanoid (thumbs renamed and back), expressions, look-at, first person.
    let (a, b) = (scene.avatar.as_ref().unwrap(), back.avatar.as_ref().unwrap());
    assert_eq!(a.humanoid, b.humanoid);
    assert_eq!(a.expressions, b.expressions);
    assert_eq!(a.look_at, b.look_at);
    assert_eq!(a.first_person, b.first_person);

    // Springs: same chains plus their tails, groups regrouped by node, parameters carried.
    let (sa, sb) = (&a.spring_bones, &b.spring_bones);
    assert_eq!(sa.colliders, sb.colliders);
    assert_eq!(sa.collider_groups, sb.collider_groups);
    assert_eq!(sa.chains.len(), sb.chains.len());
    for (x, y) in sa.chains.iter().zip(&sb.chains) {
        assert_eq!((&x.name, &x.collider_groups, x.center), (&y.name, &y.collider_groups, y.center));
        assert_eq!(&y.joints[..x.joints.len()], &x.joints[..], "the chain's own joints");
        assert_eq!(y.joints.len(), x.joints.len() + 1, "plus the explicit tail");
    }
}

#[test]
fn exporting_to_vrm0_twice_gives_the_same_bytes_and_the_import_is_stable() {
    let (scene, _) = vrm0_friendly(avatar());
    let first = export(&Vrm0Exporter, &scene);
    assert_eq!(first, export(&Vrm0Exporter, &scene));
    // The import adds tail nodes the first time; after that the scene is in 0.x's own terms and stays put.
    let once = import(&Vrm0Importer, &first).scene;
    let again = import(&Vrm0Importer, &export(&Vrm0Exporter, &once)).scene;
    assert_eq!(once.meshes, again.meshes);
    assert_eq!(once.skins, again.skins);
    assert_eq!(once.avatar.as_ref().unwrap().humanoid, again.avatar.as_ref().unwrap().humanoid);
    assert_eq!(once.avatar.as_ref().unwrap().expressions, again.avatar.as_ref().unwrap().expressions);
}

#[test]
fn the_vrm0_file_has_the_spec_shape() {
    let (scene, ids) = vrm0_friendly(avatar());
    let file = export(&Vrm0Exporter, &scene);
    let (doc, _) = parts(&file);
    assert_eq!(doc["extensionsUsed"].as_array().unwrap().iter().filter(|e| *e == "VRM").count(), 1);
    assert!(doc["extensions"].get("VRMC_vrm").is_none(), "no 1.0 extensions in a 0.x file");
    let vrm = &doc["extensions"]["VRM"];
    assert_eq!(vrm["specVersion"], "0.0");
    assert_eq!(vrm["meta"]["title"], "Fixture Avatar");
    assert_eq!(vrm["meta"]["commercialUssageName"], "Allow");
    assert_eq!(vrm["meta"]["licenseName"], "Redistribution_Prohibited");
    assert_eq!(vrm["meta"]["otherLicenseUrl"], "https://example.org/terms", "the 1.0 other-licence URL is kept");
    assert_eq!(vrm["meta"]["otherPermissionUrl"], "https://example.org/terms");
    assert!(vrm["meta"].as_object().unwrap().values().all(|v| !v.is_null()), "null members are not written");
    assert_eq!(
        vrm["meta"]["texture"], 3,
        "the thumbnail is a texture index; the fixture's texture for that image is reused"
    );
    // Bones use the 0.x thumb names: 1.0 metacarpal/proximal/distal are 0.x proximal/intermediate/distal.
    let bones = vrm["humanoid"]["humanBones"].as_array().unwrap();
    let node_of = |bone: &str| bones.iter().find(|b| b["bone"] == bone).map(|b| b["node"].as_u64().unwrap());
    assert_eq!(node_of("leftThumbProximal"), Some(u64::from(ids.node("leftThumbMetacarpal").0)));
    assert_eq!(node_of("leftThumbIntermediate"), Some(u64::from(ids.node("leftThumbProximal").0)));
    assert_eq!(node_of("leftThumbMetacarpal"), None);
    // Blend shapes: weights 0..100, lower-case preset names, custom clips as Unknown.
    let groups = vrm["blendShapeMaster"]["blendShapeGroups"].as_array().unwrap();
    let group = |name: &str| groups.iter().find(|g| g["name"] == name).unwrap();
    assert_eq!(group("Joy")["presetName"], "joy");
    assert_eq!(group("Joy")["binds"][0], json!({"mesh": 0, "index": 3, "weight": 50.0}));
    assert_eq!(group("jawOpen")["presetName"], "unknown");
    assert_eq!(group("Joy")["materialValues"][0]["propertyName"], "_Color");
    assert_eq!(group("Joy")["materialValues"][1]["propertyName"], "_MainTex_ST");
    assert_eq!(group("Blink")["isBinary"], true);
    // Spring bones: one group per chain, rooted at the first joint, -1 for "no centre".
    let sec = &vrm["secondaryAnimation"];
    assert_eq!(sec["boneGroups"].as_array().unwrap().len(), 2);
    assert_eq!(sec["boneGroups"][0]["bones"], json!([ids.node("hairA1").0]));
    assert_eq!(sec["boneGroups"][1]["center"], -1);
    assert_eq!(sec["colliderGroups"][0]["node"], ids.node("head").0);
    // Materials: one entry per glTF material, MToon with Unity names.
    let mats = vrm["materialProperties"].as_array().unwrap();
    assert_eq!(mats.len(), scene.materials.len());
    assert_eq!(mats[0]["shader"], "VRM/MToon");
    assert_eq!(mats[1]["shader"], "VRM_USE_GLTFSHADER");
    assert_eq!(mats[4]["shader"], "VRM/UnlitTransparent");
    assert_eq!(mats[0]["textureProperties"]["_SphereAdd"], 2);
    assert_eq!(mats[2]["renderQueue"], 2501);
    // The model faces -Z: the left eye's local z (+0.08) is -0.08 in the file.
    let eye = &doc["nodes"][ids.node("leftEye").index()]["translation"];
    assert_eq!(eye, &json!([-0.03, 0.08, -0.08]));
}

// ---------------------------------------------------------------------------------------------------------------
// A file written by hand

#[test]
fn a_hand_written_vrm0_imports_facing_plus_z() {
    let (doc, bin) = minimal0(&json!({}));
    let out = import(&Vrm0Importer, &glb(&doc, &bin));
    let s = &out.scene;
    assert_eq!(s.avatar.as_ref().unwrap().humanoid.bones.len(), 17);
    for (i, (_, _)) in BONES0.iter().enumerate() {
        let t = doc["nodes"][i]["translation"].as_array().unwrap();
        let (x, y, z) = (t[0].as_f64().unwrap() as f32, t[1].as_f64().unwrap() as f32, t[2].as_f64().unwrap() as f32);
        assert_eq!(s.nodes[i].transform.translation, Vec3::new(-x, y, -z), "node {i}: x and z are negated");
    }
    let p = &s.meshes[0].primitives[0];
    assert_eq!(p.mesh.positions[2], Vec3::new(-0.5, 1.0, -3.0));
    assert_eq!(p.mesh.positions[0], Vec3::new(-0.5, 0.0, -2.0));
    // "texture": -1 means no thumbnail; the unknown-free file imports without a warning about it.
    assert_eq!(s.meta.thumbnail_image, None);
    assert_eq!(out.report.count(Severity::Warning), 0, "{:#?}", out.report);
    // And writing it back to 0.x restores the file's own numbers.
    let again = import(&Vrm0Importer, &export(&Vrm0Exporter, s));
    assert_eq!(again.scene.nodes[3].transform, s.nodes[3].transform);
    let (written, _) = parts(&export(&Vrm0Exporter, s));
    let floats = |v: &Value| -> Vec<f32> { v.as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect() };
    assert_eq!(floats(&written["nodes"][3]["translation"]), floats(&doc["nodes"][3]["translation"]));
}

/// The world position of vertex 0 of the skinned mesh in `scene`, computed from the nodes, skin and mesh alone.
fn skinned_world(scene: &IoScene) -> Vec3 {
    let local = |n: usize| {
        let t = scene.nodes[n].transform;
        Mat4::from_scale_rotation_translation(t.scale, t.rotation, t.translation)
    };
    let joint_world = local(0) * local(1);
    let vertex = scene.meshes[0].primitives[0].mesh.positions[0];
    joint_world.transform_point3(scene.skins[0].inverse_bind[0].transform_point3(vertex))
}

#[test]
fn rotated_bones_and_skinned_vertices_land_where_the_old_ones_were_turned_to() {
    // A file whose joint is rotated (0.x asks for no local rotation, but real files have some), with a skin.
    let axis = Vec3::new(1.0, 2.0, 3.0).normalize();
    let q = Quat::from_axis_angle(axis, 0.7);
    let (mut doc, bin) = minimal0(&json!({}));
    let ibm = Mat4::from_translation(Vec3::new(-0.3, -1.0, 0.2));
    let mut data = bin.clone();
    for c in ibm.to_cols_array() {
        data.extend(c.to_le_bytes());
    }
    doc["nodes"][0] = json!({"name": "hips", "translation": [0.5, 0.0, 0.25], "children": [1]});
    doc["nodes"][1] = json!({"name": "spine", "translation": [0.3, 1.0, -0.2], "rotation": [q.x, q.y, q.z, q.w]});
    doc["nodes"][17] = json!({"name": "tri", "mesh": 0, "skin": 0});
    doc["skins"] = json!([{"joints": [1], "inverseBindMatrices": 2}]);
    doc["accessors"]
        .as_array_mut()
        .unwrap()
        .push(json!({"bufferView": 2, "componentType": 5126, "count": 1, "type": "MAT4"}));
    doc["bufferViews"].as_array_mut().unwrap().push(json!({"buffer": 0, "byteOffset": 72, "byteLength": 64}));
    doc["buffers"][0]["byteLength"] = json!(136);
    // Joint weights for the three vertices.
    let mut joints = Vec::new();
    for _ in 0..3 {
        joints.extend([0u16, 0, 0, 0].iter().flat_map(|j| j.to_le_bytes()));
    }
    let mut weights = Vec::new();
    for _ in 0..3 {
        weights.extend([1.0f32, 0.0, 0.0, 0.0].iter().flat_map(|w| w.to_le_bytes()));
    }
    let joints_at = data.len();
    data.extend(joints);
    let weights_at = data.len();
    data.extend(weights);
    doc["accessors"]
        .as_array_mut()
        .unwrap()
        .push(json!({"bufferView": 3, "componentType": 5123, "count": 3, "type": "VEC4"}));
    doc["accessors"]
        .as_array_mut()
        .unwrap()
        .push(json!({"bufferView": 4, "componentType": 5126, "count": 3, "type": "VEC4"}));
    doc["bufferViews"].as_array_mut().unwrap().push(json!({"buffer": 0, "byteOffset": joints_at, "byteLength": 24}));
    doc["bufferViews"].as_array_mut().unwrap().push(json!({"buffer": 0, "byteOffset": weights_at, "byteLength": 48}));
    doc["buffers"][0]["byteLength"] = json!(data.len());
    doc["meshes"][0]["primitives"][0]["attributes"]["JOINTS_0"] = json!(3);
    doc["meshes"][0]["primitives"][0]["attributes"]["WEIGHTS_0"] = json!(4);
    // Humanoid: only the two nodes that exist as bones in this trimmed file (the rest only warn).
    let file = glb(&doc, &data);

    // Reference: the same skinning, evaluated straight from the file's numbers.
    let reference = {
        let t0 = Mat4::from_translation(Vec3::new(0.5, 0.0, 0.25));
        let t1 = Mat4::from_rotation_translation(q, Vec3::new(0.3, 1.0, -0.2));
        (t0 * t1).transform_point3(ibm.transform_point3(Vec3::new(0.5, 0.0, 2.0)))
    };
    let out = import(&Vrm0Importer, &file);
    let imported = skinned_world(&out.scene);
    let expected = Vec3::new(-reference.x, reference.y, -reference.z);
    assert!(
        (imported - expected).length() < 1e-5,
        "{imported:?} should be the file's {reference:?} turned about Y: {expected:?}"
    );
}

#[test]
fn sniffing_tells_vrm0_from_vrm1() {
    let (doc, bin) = minimal0(&json!({}));
    let file = glb(&doc, &bin);
    assert_eq!(Vrm0Importer.sniff(&ImportInput::new(&file)), Confidence::Certain);
    assert_eq!(Vrm1Importer.sniff(&ImportInput::new(&file)), Confidence::No);
    let (scene, _) = avatar();
    let one = export(&omuzaki_io_vrm::Vrm1Exporter, &scene);
    assert_eq!(Vrm0Importer.sniff(&ImportInput::new(&one)), Confidence::No);
    // Asking the wrong importer says what is wrong instead of guessing.
    let wrong = common::try_import(&Vrm1Importer, &file, &ImportOptions::default());
    assert!(matches!(wrong, Err(omuzaki_io_api::IoError::Malformed { ref message, .. }) if message.contains("vrm0")));
}

#[test]
fn blend_shapes_spring_bones_look_at_and_first_person_are_converted() {
    let extra = json!({
        "firstPerson": {
            "firstPersonBone": 4, "firstPersonBoneOffset": {"x": 0.0, "y": 0.06, "z": 0.1},
            "meshAnnotations": [{"mesh": 0, "firstPersonFlag": "ThirdPersonOnly"}],
            "lookAtTypeName": "BlendShape",
            "lookAtHorizontalInner": {"curve": [0, 0, 0, 1, 1, 1, 1, 0], "xRange": 80, "yRange": 0.5},
        },
        "blendShapeMaster": {"blendShapeGroups": [
            {"name": "Joy", "presetName": "joy", "binds": [{"mesh": 0, "index": 2, "weight": 100}, {"mesh": 7, "index": 0, "weight": 100}]},
            {"name": "A", "presetName": "a", "binds": [], "isBinary": true},
            {"name": "Blink_L", "presetName": "blink_l"},
            {"name": "surprised", "presetName": "unknown"},
            {"name": "jawOpen", "presetName": "unknown"},
            {"name": "aa", "presetName": "unknown"},
            {"name": "tint", "presetName": "unknown", "materialValues": [
                {"materialName": "mat", "propertyName": "_Color", "targetValue": [1, 0, 0, 1]},
                {"materialName": "mat", "propertyName": "_MainTex_ST", "targetValue": [0.5, 0.5, 0.25, 0.25]},
                {"materialName": "mat", "propertyName": "_Gloss", "targetValue": [1]},
                {"materialName": "nope", "propertyName": "_Color", "targetValue": [1, 1, 1, 1]},
            ]},
        ]},
        "secondaryAnimation": {
            "colliderGroups": [{"node": 4, "colliders": [{"offset": {"x": 0.0, "y": 0.1, "z": 0.2}, "radius": 0.05}]}, {"node": -1, "colliders": []}],
            "boneGroups": [{
                "comment": "hair", "stiffiness": 0.8, "gravityPower": 0.25, "gravityDir": {"x": 0.0, "y": -1.0, "z": 0.0},
                "dragForce": 0.4, "center": 0, "hitRadius": 0.02, "bones": [5], "colliderGroups": [0, 1],
            }],
        },
    });
    let (mut doc, bin) = minimal0(&extra);
    // Three zero morph targets on the triangle (an accessor without a buffer view is zero-filled), so the binds have targets.
    doc["accessors"]
        .as_array_mut()
        .unwrap()
        .push(json!({"componentType": 5126, "count": 3, "type": "VEC3", "min": [0, 0, 0], "max": [0, 0, 0]}));
    doc["meshes"][0]["primitives"][0]["targets"] = json!([{"POSITION": 2}, {"POSITION": 2}, {"POSITION": 2}]);
    // Give node 5 (leftUpperLeg) a branch: 5 -> 6 -> 7 and a second child 17? Use node 5's existing child 6 -> 7, plus a new sibling.
    doc["nodes"].as_array_mut().unwrap().push(json!({"name": "side", "translation": [0.0, -0.1, 0.0]}));
    doc["nodes"][5]["children"].as_array_mut().unwrap().push(json!(18));
    let out = import(&Vrm0Importer, &glb(&doc, &bin));
    let s = &out.scene;
    let av = s.avatar.as_ref().unwrap();
    let get = |n: &str| &av.expressions[&ExpressionName::parse(n).unwrap()];

    // Expressions.
    assert_eq!(get("happy").morph_binds.len(), 1, "the bind to mesh 7 (no node) is dropped");
    assert_eq!(get("happy").morph_binds[0].weight, 1.0, "0..100 becomes 0..1");
    assert!(get("aa").is_binary);
    assert!(av.expressions.contains_key(&ExpressionName::parse("blinkLeft").unwrap()));
    assert!(
        av.expressions.contains_key(&ExpressionName::parse("surprised").unwrap()),
        "a custom clip named surprised is the preset"
    );
    assert!(av.expressions.contains_key(&ExpressionName::parse("jawOpen").unwrap()));
    assert!(
        av.expressions.contains_key(&ExpressionName::parse("aa_custom").unwrap()),
        "a custom clip can't reuse a preset name"
    );
    let tint = get("tint");
    assert_eq!(tint.color_binds.len(), 1);
    assert_eq!(tint.uv_binds.len(), 1);
    // Unity's offset.y 0.25 with scale.y 0.5 becomes 1 - 0.25 - 0.5 = 0.25 in glTF UV space.
    assert_eq!((tint.uv_binds[0].scale.to_array(), tint.uv_binds[0].offset.to_array()), ([0.5, 0.5], [0.25, 0.25]));
    for rule in ["vrm0.bind_dropped", "vrm0.material_value_dropped"] {
        assert!(out.report.has_rule(rule), "{rule}");
    }

    // Look-at and first person.
    let look = av.look_at.unwrap();
    assert_eq!(look.kind, omuzaki_io_api::LookAtKind::Expression);
    assert_eq!(look.offset_from_head_bone, Vec3::new(0.0, 0.06, -0.1), "z is flipped with the model");
    assert_eq!((look.horizontal_inner.input_max_value, look.horizontal_inner.output_scale), (80.0, 0.5));
    assert_eq!(
        (look.vertical_up.input_max_value, look.vertical_up.output_scale),
        (90.0, 1.0),
        "defaults: 90 degrees, 1 for expressions"
    );
    assert_eq!(av.first_person.as_ref().unwrap().annotations[0].flag, omuzaki_io_api::FirstPersonFlag::ThirdPersonOnly);
    assert_eq!(av.first_person.as_ref().unwrap().annotations[0].node, NodeIx(17));

    // Spring bones: the subtree under node 5 is two chains (the first child continues, the second starts one), each
    // with an explicit tail; the 0.x collider group on node 4 is one 1.0 collider; the group on node -1 stays empty.
    let sb = &av.spring_bones;
    assert_eq!(sb.chains.len(), 2);
    let names: Vec<Vec<&str>> =
        sb.chains.iter().map(|c| c.joints.iter().map(|j| s.nodes[j.node.index()].name.as_str()).collect()).collect();
    assert_eq!(names, [vec!["leftUpperLeg", "leftLowerLeg", "leftFoot", "leftFoot_end"], vec!["side", "side_end"]]);
    assert_eq!(sb.chains[0].name, "hair");
    assert_eq!((sb.chains[0].center, sb.chains[0].collider_groups.as_slice()), (Some(NodeIx(0)), &[0, 1][..]));
    let j = sb.chains[0].joints[1];
    assert_eq!((j.stiffness, j.gravity_power, j.drag_force, j.hit_radius), (0.8, 0.25, 0.4, 0.02));
    assert_eq!(sb.colliders.len(), 1);
    assert_eq!(sb.collider_groups[0].name, "head");
    assert!(sb.collider_groups[1].colliders.is_empty());
    match sb.colliders[0].shape {
        omuzaki_io_api::ColliderShape::Sphere { offset, radius } => {
            assert_eq!((offset, radius), (Vec3::new(0.0, 0.1, -0.2), 0.05), "collider offsets turn with the model");
        }
        other => panic!("0.x colliders are spheres, got {other:?}"),
    }
}

#[test]
fn humanoid_thumbs_are_renamed_on_import() {
    let mut bones: Vec<Value> = Vec::new();
    for (i, (name, _)) in BONES0.iter().enumerate() {
        bones.push(json!({"bone": name, "node": i}));
    }
    bones.push(json!({"bone": "leftThumbProximal", "node": 18}));
    bones.push(json!({"bone": "leftThumbIntermediate", "node": 19}));
    bones.push(json!({"bone": "eyeOfNewt", "node": 17}));
    bones.push(json!({"bone": "rightHand", "node": -1}));
    let (mut doc, bin) = minimal0(&json!({}));
    for name in ["thumb1", "thumb2"] {
        doc["nodes"].as_array_mut().unwrap().push(json!({"name": name}));
    }
    doc["extensions"]["VRM"]["humanoid"]["humanBones"] = json!(bones);
    let out = import(&Vrm0Importer, &glb(&doc, &bin));
    let av = out.scene.avatar.unwrap();
    assert_eq!(av.humanoid.bones[&HumanBoneName::new("leftThumbMetacarpal").unwrap()], NodeIx(18));
    assert_eq!(av.humanoid.bones[&HumanBoneName::new("leftThumbProximal").unwrap()], NodeIx(19));
    assert!(out.report.has_rule("vrm.humanoid_unknown_bone"));
}

// ---------------------------------------------------------------------------------------------------------------
// MToon 0.x, with the numbers worked out by hand from the migrator's formulas

#[test]
fn mtoon_zero_x_converts_to_the_one_zero_model() {
    let mtoon = json!({
        "name": "mat", "shader": "VRM/MToon", "renderQueue": 3000,
        "floatProperties": {
            "_BlendMode": 2, "_CullMode": 0, "_Cutoff": 0.4, "_BumpScale": 0.7, "_ShadeShift": 0.0, "_ShadeToony": 0.9,
            "_IndirectLightIntensity": 0.25, "_RimFresnelPower": 2.0, "_RimLift": 0.1, "_OutlineWidthMode": 2, "_OutlineWidth": 1.0,
            "_OutlineColorMode": 1, "_OutlineLightingMix": 0.6, "_UvAnimScrollX": 0.5, "_UvAnimScrollY": 0.25, "_UvAnimRotation": 0.5,
        },
        "vectorProperties": {
            "_Color": [0.5, 0.5, 0.5, 0.8], "_ShadeColor": [1.0, 0.0, 0.5, 1.0], "_EmissionColor": [0.1, 0.2, 0.3, 1.0],
            "_RimColor": [0.0, 1.0, 0.0, 1.0], "_OutlineColor": [0.2, 0.2, 0.2, 1.0],
            "_MainTex": [0.25, 0.5, 2.0, 0.5],
        },
        "textureProperties": {"_MainTex": 0, "_BumpMap": 0, "_SphereAdd": 0},
    });
    let (mut doc, bin) = minimal0(&json!({"materialProperties": [mtoon]}));
    doc["images"] = json!([{"uri": format!("data:image/png;base64,{}", base64(&common::PNG))}]);
    doc["textures"] = json!([{"source": 0}]);
    let out = import(&Vrm0Importer, &glb(&doc, &bin));
    let m = &out.scene.materials[0];
    let Shading::Toon(t) = &m.shading else { panic!("MToon becomes a toon material") };
    let srgb = |c: f32| omuzaki_core::colour::srgb_decode(c);
    // Colours: sRGB in the file, linear in the scene; the emission colour is linear already.
    assert_close(&m.base_color.to_array(), &[srgb(0.5), srgb(0.5), srgb(0.5), 0.8], "_Color");
    assert_close(&t.shade_color.to_array(), &[1.0, 0.0, srgb(0.5)], "_ShadeColor");
    assert_close(&m.emissive.to_array(), &[0.1, 0.2, 0.3], "_EmissionColor");
    assert_close(&t.rim_color.to_array(), &[0.0, 1.0, 0.0], "_RimColor");
    // Shading: range [0, 0.1] -> toony (2 - 0.1) / 2, shift -0.05.
    assert!((t.shading_toony - 0.95).abs() < 1e-6 && (t.shading_shift + 0.05).abs() < 1e-6);
    assert!((t.gi_equalization - 0.75).abs() < 1e-6, "1 - intensity");
    assert_eq!((t.rim_fresnel_power, t.rim_lift, t.rim_lighting_mix), (2.0, 0.1, 1.0));
    // Outline: screen mode, width 1 -> 1 * 0.01 * 0.5; mixed colour with the file's mix.
    assert_eq!(t.outline_mode, OutlineMode::ScreenCoordinates);
    assert!((t.outline_width - 0.005).abs() < 1e-8 && (t.outline_lighting_mix - 0.6).abs() < 1e-8);
    // UV animation: Y scroll is negated, rotation is turns/s -> radians/s.
    assert_eq!((t.uv_scroll_x, t.uv_scroll_y), (0.5, -0.25));
    assert!((t.uv_rotation - std::f32::consts::PI).abs() < 1e-6);
    // Render state.
    assert_eq!(m.alpha, AlphaMode::Blend);
    assert!(m.double_sided && !t.transparent_with_zwrite);
    assert_eq!(t.render_queue_offset, 0, "the only transparent queue maps to offset 0");
    // Textures: the _MainTex ST (offset .25,.5 scale 2,.5) becomes offset (.25, 1 - .5 - .5 = 0) and scale (2, .5) on
    // every texture but the matcap; with no shade texture the lit one stands in.
    let transform = Some(omuzaki_io_api::UvTransform { offset: [0.25, 0.0].into(), scale: [2.0, 0.5].into() });
    assert_eq!(m.base_color_texture.as_ref().unwrap().transform, transform);
    assert_eq!(t.shade_multiply_texture, m.base_color_texture);
    assert_eq!(m.normal_texture.as_ref().unwrap().scale, 0.7);
    assert_eq!(m.normal_texture.as_ref().unwrap().texture.transform, transform);
    assert_eq!(t.matcap_texture.as_ref().unwrap().transform, None);
    assert_eq!(t.matcap_color, Vec3::ONE);
}

/// Standard base64, for embedding the fixture PNG in a data URI.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[test]
fn unlit_shaders_and_unknown_shaders_are_handled() {
    let props = [
        json!({"name": "a", "shader": "VRM/UnlitCutout", "floatProperties": {"_Cutoff": 0.3}}),
        json!({"name": "b", "shader": "VRM/UnlitTransparent"}),
        json!({"name": "c", "shader": "Standard"}),
    ];
    let (mut doc, bin) = minimal0(&json!({"materialProperties": props}));
    let base = doc["materials"][0].clone();
    doc["materials"] = json!([base.clone(), base.clone(), base]);
    let m = import(&Vrm0Importer, &glb(&doc, &bin)).scene.materials;
    assert!(matches!(m[0].shading, Shading::Unlit) && m[0].alpha == AlphaMode::Mask { cutoff: 0.3 });
    assert!(matches!(m[1].shading, Shading::Unlit) && m[1].alpha == AlphaMode::Blend);
    assert!(matches!(m[2].shading, Shading::Pbr(_)), "an unknown shader keeps the glTF material");
}

#[test]
fn a_toon_material_survives_0x_with_the_numbers_it_started_with() {
    let (scene, _) = vrm0_friendly(avatar());
    let back = import(&Vrm0Importer, &export(&Vrm0Exporter, &scene)).scene;
    let (Shading::Toon(a), Shading::Toon(b)) = (&scene.materials[0].shading, &back.materials[0].shading) else {
        panic!("the skin material is toon");
    };
    let one = |t: &Toon| toon_numbers(t);
    assert_close(&one(b), &one(a), "skin toon");
}

/// 0.x has only outside spheres and no joint limits: the extended shapes and limits are dropped, each with its finding.
#[test]
fn extended_colliders_and_limits_are_dropped_with_warnings() {
    let (mut scene, ids) = vrm0_friendly(avatar());
    let head = ids.node("head");
    let sb = &mut scene.avatar.as_mut().unwrap().spring_bones;
    let before = sb.colliders.len();
    sb.colliders.push(omuzaki_io_api::Collider {
        node: head,
        shape: omuzaki_io_api::ColliderShape::Plane { offset: Vec3::ZERO, normal: Vec3::Y },
    });
    sb.collider_groups[0].colliders.push(before as u32);
    sb.chains[0].joints[0].limit = Some(omuzaki_io_api::JointLimit::Hinge { angle: 0.5, rotation: Quat::IDENTITY });
    let (file, report) = export_with(&Vrm0Exporter, &scene, &options()).unwrap();
    assert!(report.has_rule("vrm0.collider_unsupported") && report.has_rule("vrm0.limit_unsupported"), "{report:#?}");
    let back = import(&Vrm0Importer, &file).scene;
    assert_eq!(back.avatar.unwrap().spring_bones.colliders.len(), before, "the plane is not written");
}

/// The warning counts every unsupported collider once: an ungrouped one is dropped too, a shared one is one collider.
#[test]
fn unsupported_colliders_are_counted_once_each_grouped_or_not() {
    let (mut scene, ids) = vrm0_friendly(avatar());
    let head = ids.node("head");
    let sb = &mut scene.avatar.as_mut().unwrap().spring_bones;
    let shape = omuzaki_io_api::ColliderShape::Plane { offset: Vec3::ZERO, normal: Vec3::Y };
    let first = sb.colliders.len();
    sb.colliders.push(omuzaki_io_api::Collider { node: head, shape }); // in two groups
    sb.colliders.push(omuzaki_io_api::Collider { node: head, shape }); // in none
    sb.collider_groups[0].colliders.push(first as u32);
    sb.collider_groups[1].colliders.push(first as u32);
    let (_, report) = export_with(&Vrm0Exporter, &scene, &options()).unwrap();
    let finding = report.findings.iter().find(|f| f.rule == "vrm0.collider_unsupported").expect("the warning");
    assert!(finding.message.contains(" 2 "), "two colliders, not three or one: {}", finding.message);
}

/// As for 1.0: the 0.x `secondaryAnimation` of a scene without extended shapes or limits is what `main` wrote before
///.
#[test]
fn existing_secondary_animation_json_is_byte_identical_to_the_pre_extension_baseline() {
    let (scene, _) = vrm0_friendly(avatar());
    let (doc, _) = parts(&export(&Vrm0Exporter, &scene));
    let baseline = include_str!("baseline/spring_vrm0.json").trim_end();
    assert_eq!(doc["extensions"]["VRM"]["secondaryAnimation"].to_string(), baseline);
}
