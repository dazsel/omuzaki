//! The payload's mapping: node types, transforms, meshes, masks, parameters, bindings, physics and metadata.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp, reason = "tests; the expected values are exact")]

mod common;

use std::f32::consts::PI;

use common::{identity, inp1, minimal, part, pixels, png};
use omuzaki_inochi2d::{FindingKind, ImportError, Imported, Limits, import, rule};
use omuzaki_puppet_api::{
    BindTarget, BindValues, BlendMode, Interp, MapMode, MaskMode, NodeKind, PendulumModel, PhysicsWorld,
};
use serde_json::{Value, json};

fn run(puppet: &Value) -> Result<Imported<'static>, ImportError> {
    let bytes = inp1(puppet, Some(&[(0, png(2, 2, &pixels(2, 2)))]));
    let bytes: &'static [u8] = Box::leak(bytes.into_boxed_slice());
    import(bytes, &Limits::default())
}

fn children(p: &mut Value) -> &mut Vec<Value> {
    p["nodes"]["children"].as_array_mut().unwrap()
}

#[test]
fn an_unknown_node_type_is_kept_with_a_finding() {
    let mut p = minimal();
    children(&mut p).push(json!({
        "uuid": 9, "name": "Future", "type": "AnimatedPart", "transform": identity(),
        "children": [part(10, "Inside", 0)],
    }));
    let imported = run(&p).unwrap();
    let def = &imported.def;
    let future = def.node("Future").unwrap().get();
    assert_eq!(def.nodes[future].kind, NodeKind::Unknown { type_name: "AnimatedPart".into() });
    let inside = def.node("Inside").unwrap().get();
    assert_eq!(
        def.nodes[inside].parent.map(omuzaki_puppet_api::NodeIx::get),
        Some(future),
        "its subtree is kept under it"
    );
    let f = imported.report.with_id(rule::UNKNOWN_NODE).next().unwrap();
    assert_eq!(f.path, "nodes/children/1/type");
    assert_eq!(imported.report.mapped.unknown, 1);
}

#[test]
fn a_mask_source_that_is_no_node_is_refused() {
    let mut p = minimal();
    children(&mut p)[0]["masks"] = json!([{ "source": 4242, "mode": "Mask" }]);
    let e = run(&p).unwrap_err();
    assert_eq!((e.rule(), e.path()), (rule::MASK_SOURCE, "nodes/children/0/masks/0/source"));
}

#[test]
fn masks_resolve_by_uuid_with_their_modes() {
    let mut p = minimal();
    let mut masked = part(3, "Masked", 0);
    masked["masks"] = json!([{ "source": 2, "mode": "DodgeMask" }]);
    let mut legacy = part(4, "Legacy", 0);
    legacy["masked_by"] = json!([2]);
    legacy["mask_mode"] = json!("Mask");
    children(&mut p).extend([masked, legacy]);
    let imported = run(&p).unwrap();
    let def = &imported.def;
    let quad = def.node("Quad").unwrap();
    let NodeKind::Part(m) = &def.nodes[def.node("Masked").unwrap().get()].kind else { panic!() };
    assert_eq!(m.draw.masks.len(), 1);
    assert_eq!((m.draw.masks[0].source, m.draw.masks[0].mode), (quad, MaskMode::Dodge));
    let NodeKind::Part(l) = &def.nodes[def.node("Legacy").unwrap().get()].kind else { panic!() };
    assert_eq!((l.draw.masks[0].source, l.draw.masks[0].mode), (quad, MaskMode::Mask));
}

#[test]
fn a_node_with_an_ancestors_uuid_is_a_cycle() {
    let mut p = minimal();
    children(&mut p)[0]["children"] = json!([part(1, "Loop", 0)]);
    let e = run(&p).unwrap_err();
    assert_eq!((e.rule(), e.path()), (rule::TREE_CYCLE, "nodes/children/0/children/0/uuid"));
}

#[test]
fn a_duplicate_uuid_elsewhere_is_noted_and_the_first_wins() {
    let mut p = minimal();
    children(&mut p).push(part(2, "Twin", 0));
    let imported = run(&p).unwrap();
    assert_eq!(imported.report.with_id(rule::DUPLICATE_UUID).next().unwrap().path, "nodes/children/1/uuid");
    let b = &imported.def.params[0].bindings[0];
    assert_eq!(b.node, imported.def.node("Quad").unwrap());
}

#[test]
fn a_triangle_past_the_vertices_is_refused() {
    let mut p = minimal();
    children(&mut p)[0]["mesh"]["indices"] = json!([0, 1, 2, 0, 2, 4]);
    let e = run(&p).unwrap_err();
    assert_eq!((e.rule(), e.path()), (rule::MESH, "nodes/children/0/mesh/indices/5"));
}

#[test]
fn indices_that_are_not_whole_triangles_draw_nothing() {
    let mut p = minimal();
    children(&mut p)[0]["mesh"]["indices"] = json!([0, 1, 2, 0]);
    let imported = run(&p).unwrap();
    let NodeKind::Part(part) = &imported.def.nodes[1].kind else { panic!() };
    assert_eq!(part.mesh.triangles, [] as [[u32; 3]; 0]);
    assert!(imported.report.has(rule::INDICES));
}

#[test]
fn a_part_naming_a_missing_texture_is_refused() {
    let mut p = minimal();
    children(&mut p)[0]["textures"] = json!([3, u32::MAX, u32::MAX]);
    let e = run(&p).unwrap_err();
    assert_eq!((e.rule(), e.path()), (rule::TEXTURE_REF, "nodes/children/0/textures/0"));
}

#[test]
fn part_vertices_are_drawn_relative_to_the_origin() {
    let mut p = minimal();
    children(&mut p)[0]["mesh"]["origin"] = json!([2.0, -3.0]);
    let imported = run(&p).unwrap();
    let NodeKind::Part(part) = &imported.def.nodes[1].kind else { panic!() };
    assert_eq!(part.mesh.vertices[0], [-7.0, -2.0]);
    assert_eq!(part.mesh.uvs[0], [0.0, 0.0], "uvs are unchanged");
}

#[test]
fn transforms_fold_x_and_y_turns_into_scale() {
    let mut p = minimal();
    children(&mut p)[0]["transform"] = json!({ "trans": [3.0, 4.0, 7.0], "rot": [0.0, PI, 0.5], "scale": [2.0, 3.0] });
    let imported = run(&p).unwrap();
    let t = imported.def.nodes[1].transform;
    assert_eq!(t.translation, [3.0, 4.0]);
    assert_eq!(t.rotation, 0.5);
    assert!((t.scale[0] + 2.0).abs() < 1e-6, "a half turn about y mirrors x: {:?}", t.scale);
    assert_eq!(t.scale[1], 3.0);
    assert!(imported.report.has(rule::TRANSLATE_Z));
    assert!(!imported.report.has(rule::ROTATION_XY), "a half turn is exact");

    children(&mut p)[0]["transform"]["rot"] = json!([0.3, 0.0, 0.0]);
    let imported = run(&p).unwrap();
    assert!((imported.def.nodes[1].transform.scale[1] - 3.0 * 0.3f32.cos()).abs() < 1e-6);
    assert!(imported.report.of_kind(FindingKind::Approximated).any(|f| f.id == rule::ROTATION_XY));
}

#[test]
fn drawable_fields_map() {
    let mut p = minimal();
    let q = &mut children(&mut p)[0];
    q["blend_mode"] = json!("Screen");
    q["opacity"] = json!(0.25);
    q["tint"] = json!([1.0, 0.5, 0.0]);
    q["screenTint"] = json!([0.0, 0.25, 0.0]);
    q["mask_threshold"] = json!(0.75);
    q["zsort"] = json!(-0.5);
    q["enabled"] = json!(false);
    let imported = run(&p).unwrap();
    let node = &imported.def.nodes[1];
    assert_eq!((node.zsort, node.enabled), (-0.5, false));
    let NodeKind::Part(part) = &node.kind else { panic!() };
    let d = &part.draw;
    assert_eq!(
        (d.blend, d.opacity, d.tint, d.screen_tint, d.mask_threshold),
        (BlendMode::Screen, 0.25, [1.0, 0.5, 0.0], [0.0, 0.25, 0.0], 0.75)
    );
}

#[test]
fn blend_modes_we_lack_map_to_the_nearest_with_a_finding() {
    for (name, expected) in
        [("AddGlow", BlendMode::LinearDodge), ("Inverse", BlendMode::Normal), ("Bogus", BlendMode::Normal)]
    {
        let mut p = minimal();
        children(&mut p)[0]["blend_mode"] = json!(name);
        let imported = run(&p).unwrap();
        let NodeKind::Part(part) = &imported.def.nodes[1].kind else { panic!() };
        assert_eq!(part.draw.blend, expected, "{name}");
        assert_eq!(imported.report.with_id(rule::BLEND_MODE).next().unwrap().path, "nodes/children/0/blend_mode");
    }
    // Older writers store the enum's position: 1 is Multiply.
    let mut p = minimal();
    children(&mut p)[0]["blend_mode"] = json!(1);
    let imported = run(&p).unwrap();
    let NodeKind::Part(part) = &imported.def.nodes[1].kind else { panic!() };
    assert_eq!(part.draw.blend, BlendMode::Multiply);
}

/// Axis points are normalised: a key at `t` sits at `min + t * (max - min)`.
#[test]
fn axis_points_convert_to_parameter_units() {
    let mut p = minimal();
    p["param"] = json!([{
        "uuid": 100, "name": "Angle", "is_vec2": true, "min": [-30.0, 10.0], "max": [30.0, 20.0],
        "defaults": [0.0, 15.0], "axis_points": [[0.0, 0.25, 0.5, 1.0], [0.0, 0.3, 1.0]],
        "bindings": [{
            "node": 2, "param_name": "transform.t.x",
            "values": [[0.0, 1.0, 2.0], [10.0, 11.0, 12.0], [20.0, 21.0, 22.0], [30.0, 31.0, 32.0]],
            "isSet": [[true, true, true], [true, false, true], [true, true, true], [true, true, true]],
            "interpolate_mode": "Stepped",
        }],
    }]);
    let imported = run(&p).unwrap();
    let param = &imported.def.params[0];
    assert_eq!(param.keys[0], vec![-30.0, -15.0, 0.0, 30.0]);
    let expected_y = [10.0, 10.0 + 0.3 * 10.0, 20.0];
    for (k, e) in param.keys[1].iter().zip(expected_y) {
        assert!((k - e).abs() < 1e-5, "{:?}", param.keys[1]);
    }
    assert_eq!((param.min, param.max, param.default, param.two_axes), ([-30.0, 10.0], [30.0, 20.0], [0.0, 15.0], true));

    // values[x][y] land at key_index(x, y), x-major.
    let b = &param.bindings[0];
    let BindValues::Scalar(v) = &b.values else { panic!() };
    for ix in 0..4u8 {
        for iy in 0..3u8 {
            assert_eq!(v[param.key_index(ix.into(), iy.into())], 10.0 * f32::from(ix) + f32::from(iy));
        }
    }
    assert!(!b.set[param.key_index(1, 1)]);
    assert_eq!(b.set.iter().filter(|s| !**s).count(), 1);
    assert_eq!((b.target, b.interp), (BindTarget::TranslateX, Interp::Nearest));
    assert!(imported.report.has(rule::INTERPOLATION));
}

#[test]
fn a_reversed_range_reverses_the_keys_and_their_values() {
    let mut p = minimal();
    p["param"] = json!([{
        "uuid": 100, "name": "Flip", "min": [1.0, 0.0], "max": [-1.0, 0.0], "axis_points": [[0.0, 0.25, 1.0], [0.0]],
        "bindings": [{ "node": 2, "param_name": "zSort", "values": [[1.0], [2.0], [3.0]], "isSet": [[true], [true], [true]] }],
    }]);
    let imported = run(&p).unwrap();
    let param = &imported.def.params[0];
    assert_eq!(param.keys[0], vec![-1.0, 0.5, 1.0]);
    assert_eq!((param.min[0], param.max[0]), (-1.0, 1.0));
    assert_eq!(param.bindings[0].values, BindValues::Scalar(vec![3.0, 2.0, 1.0]));
}

#[test]
fn a_one_axis_parameter_uses_its_first_y_key_only() {
    let mut p = minimal();
    p["param"][0]["axis_points"] = json!([[0.0, 0.5, 1.0], [0.0, 1.0]]);
    let e = run(&p).unwrap_err();
    // Its values are [x][2] now in the file's eyes, but the binding has one per x: refused with the path.
    assert_eq!((e.rule(), e.path()), (rule::BINDING_VALUES, "param/0/bindings/0/values/0"));

    let mut p = minimal();
    p["param"][0]["axis_points"] = json!([[0.0, 0.5, 1.0], [0.0, 1.0]]);
    p["param"][0]["bindings"] = json!([]);
    let imported = run(&p).unwrap();
    assert_eq!(imported.def.params[0].keys[1], vec![0.0]);
    assert!(imported.report.has(rule::PARAM_AXIS));
}

#[test]
fn deform_offsets_are_checked_against_the_mesh() {
    let mut p = minimal();
    p["param"][0]["bindings"][0]["values"][1][0] = json!([[0.0, 0.0]]);
    let e = run(&p).unwrap_err();
    assert_eq!((e.rule(), e.path()), (rule::BINDING_VALUES, "param/0/bindings/0/values/1/0"));
}

#[test]
fn bindings_to_missing_nodes_and_unmodelled_targets_are_dropped() {
    let mut p = minimal();
    let b = p["param"][0]["bindings"][0].clone();
    let mut missing = b.clone();
    missing["node"] = json!(77);
    let mut screen = json!({ "node": 2, "param_name": "screenTint.r", "values": [[0.0], [0.5], [1.0]] });
    screen["isSet"] = json!([[true], [true], [true]]);
    p["param"][0]["bindings"] = json!([b, missing, screen]);
    let imported = run(&p).unwrap();
    assert_eq!(imported.def.params[0].bindings.len(), 1);
    assert_eq!(imported.report.with_id(rule::BINDING_NODE).next().unwrap().path, "param/0/bindings/1/node");
    assert_eq!(imported.report.with_id(rule::BINDING_TARGET).next().unwrap().path, "param/0/bindings/2/param_name");
}

#[test]
fn a_physics_node_becomes_a_pendulum_on_itself() {
    let mut p = minimal();
    children(&mut p).push(json!({
        "uuid": 20, "name": "Tail", "type": "SimplePhysics", "transform": identity(), "param": 100,
        "model_type": "SpringPendulum", "map_mode": "XY", "length": 40.0, "frequency": 2.0, "local_only": true,
    }));
    p["physics"] = json!({ "pixelsPerMeter": 500.0, "gravity": 4.0 });
    let imported = run(&p).unwrap();
    let def = &imported.def;
    let tail = def.node("Tail").unwrap();
    assert_eq!(def.nodes[tail.get()].kind, NodeKind::Group);
    let pendulum = &def.physics[0];
    assert_eq!((pendulum.anchor, pendulum.param.get(), pendulum.name.as_str()), (tail, 0, "Tail"));
    assert_eq!((pendulum.model, pendulum.map), (PendulumModel::Spring, MapMode::Xy));
    assert_eq!((pendulum.length, pendulum.frequency, pendulum.local_only), (40.0, 2.0, true));
    // Inochi2D's defaults for what the node leaves out.
    assert_eq!(
        (pendulum.gravity, pendulum.angle_damping, pendulum.length_damping, pendulum.output_scale),
        (1.0, 0.5, 0.5, [1.0, 1.0])
    );
    assert_eq!(def.world, PhysicsWorld { pixels_per_meter: 500.0, gravity: 4.0 });
}

#[test]
fn a_physics_node_without_its_parameter_writes_nothing() {
    let mut p = minimal();
    children(&mut p).push(json!({ "uuid": 20, "name": "Tail", "type": "SimplePhysics", "param": 999 }));
    let imported = run(&p).unwrap();
    assert_eq!(imported.def.physics, [] as [omuzaki_puppet_api::Pendulum; 0]);
    assert_eq!(imported.report.with_id(rule::PHYSICS_PARAM).next().unwrap().path, "nodes/children/1/param");
}

#[test]
fn mesh_groups_composites_and_masks_map() {
    let mut p = minimal();
    children(&mut p).extend([
        json!({ "uuid": 30, "name": "Warp", "type": "MeshGroup", "mesh": common::quad_mesh(),
                "children": [{ "uuid": 31, "name": "Holder", "type": "Node", "children": [part(32, "Deep", 0)] }] }),
        json!({ "uuid": 33, "name": "Layer", "type": "Composite", "opacity": 0.5, "blend_mode": "Overlay" }),
        json!({ "uuid": 34, "name": "Stencil", "type": "Mask", "mesh": common::quad_mesh() }),
        json!({ "uuid": 35, "name": "Empty", "type": "MeshGroup" }),
    ]);
    let imported = run(&p).unwrap();
    let def = &imported.def;
    let kind = |name: &str| &def.nodes[def.node(name).unwrap().get()].kind;
    let NodeKind::MeshDeformer(mesh) = kind("Warp") else { panic!() };
    assert_eq!((mesh.vertices.len(), mesh.triangles.len(), mesh.uvs.len()), (4, 2, 0));
    let NodeKind::Composite(c) = kind("Layer") else { panic!() };
    assert_eq!((c.draw.opacity, c.draw.blend), (0.5, BlendMode::Overlay));
    assert!(matches!(kind("Stencil"), NodeKind::Mask(m) if m.vertices.len() == 4));
    assert_eq!(kind("Empty"), &NodeKind::Group);
    assert!(imported.report.has(rule::MESHGROUP_EMPTY));
    // The part under a plain node under the group: Inochi2D leaves it alone, our deformer moves it.
    assert_eq!(imported.report.with_id(rule::MESHGROUP_MODE).next().unwrap().path, "nodes/children/1/children");
}

#[test]
fn a_part_with_no_texture_becomes_a_mask_mesh() {
    let mut p = minimal();
    children(&mut p).push({
        let mut q = part(40, "Bare", 0);
        q["textures"] = json!([u32::MAX, u32::MAX, u32::MAX]);
        q
    });
    let imported = run(&p).unwrap();
    assert!(matches!(imported.def.nodes[imported.def.node("Bare").unwrap().get()].kind, NodeKind::Mask(_)));
    assert!(imported.report.has(rule::PART_NO_TEXTURE));
}

#[test]
fn rights_are_kept_verbatim_and_unknown_stays_unknown() {
    let imported = run(&minimal()).unwrap();
    let meta = &imported.def.meta;
    assert_eq!(meta.name, "Synthetic");
    assert_eq!(meta.artist.as_deref(), Some("Omuzaki tests"));
    assert_eq!(meta.rigger, None);
    assert_eq!(meta.rights.get("allowCommercial").map(String::as_str), Some("false"));
    assert_eq!(meta.rights.get("allowRedistribution").map(String::as_str), Some("true"));
    assert_eq!(meta.rights.len(), 3, "nothing is added");
    assert_eq!(meta.source, "inochi2d");
    assert!(!imported.report.has(rule::RIGHTS_UNKNOWN));
    assert_eq!(imported.report.with_id(rule::META).next().unwrap().path, "meta/version");

    let mut p = minimal();
    p["meta"]["rights"] = Value::Null;
    let imported = run(&p).unwrap();
    assert!(imported.def.meta.rights.is_empty());
    assert_eq!(imported.report.with_id(rule::RIGHTS_UNKNOWN).next().unwrap().path, "meta/rights");

    let mut p = minimal();
    p.as_object_mut().unwrap().remove("meta");
    let imported = run(&p).unwrap();
    assert!(imported.def.meta.rights.is_empty());
    assert!(imported.report.has(rule::RIGHTS_UNKNOWN));
}

#[test]
fn lock_to_root_automation_and_animations_are_reported() {
    let mut p = minimal();
    children(&mut p)[0]["lockToRoot"] = json!(true);
    p["automation"] = json!([{ "type": "sine" }]);
    p["animations"] = json!({ "idle": {} });
    let imported = run(&p).unwrap();
    assert_eq!(imported.report.with_id(rule::LOCK_TO_ROOT).next().unwrap().path, "nodes/children/0/lockToRoot");
    assert!(imported.report.has(rule::AUTOMATION));
    assert!(imported.report.has(rule::ANIMATION));
}

#[test]
fn malformed_json_and_wrong_shapes_name_their_path() {
    let bytes = inp1(&json!("not a puppet"), None);
    assert_eq!(import(&bytes, &Limits::default()).unwrap_err().rule(), rule::JSON);

    let mut p = minimal();
    children(&mut p)[0]["mesh"]["verts"] = json!("nope");
    let e = run(&p).unwrap_err();
    assert_eq!((e.rule(), e.path()), (rule::JSON, "nodes/children/0/mesh"));

    let mut p = minimal();
    p.as_object_mut().unwrap().remove("nodes");
    assert_eq!(run(&p).unwrap_err().rule(), rule::TREE);
}

#[test]
fn tree_size_and_depth_limits_hold() {
    let mut p = minimal();
    let mut deep = part(500, "Leaf", 0);
    for i in 0..80u32 {
        deep = json!({ "uuid": 501 + i, "name": format!("Level {i}"), "type": "Node", "children": [deep] });
    }
    children(&mut p).push(deep);
    // serde_json stops at 128 levels of nesting, before the tree depth limit is reached.
    let e = run(&p).unwrap_err();
    assert!(matches!(e.rule(), rule::JSON | rule::LIMIT), "{e:?}");

    let mut p = minimal();
    for i in 0..20u32 {
        children(&mut p).push(json!({ "uuid": 600 + i, "type": "Node" }));
    }
    let bytes = inp1(&p, Some(&[(0, png(2, 2, &pixels(2, 2)))]));
    let tight = Limits { max_nodes: 10, ..Limits::default() };
    assert!(matches!(import(&bytes, &tight), Err(ImportError::Limit { limit: "max_nodes", .. })));
    let tight = Limits { max_depth: 1, ..Limits::default() };
    assert!(matches!(import(&bytes, &tight), Err(ImportError::Limit { limit: "max_depth", .. })));
    let tight = Limits { max_vertices: 3, ..Limits::default() };
    assert!(matches!(import(&bytes, &tight), Err(ImportError::Limit { limit: "max_vertices", .. })));
    let tight = Limits { max_offsets: 3, ..Limits::default() };
    assert!(matches!(import(&bytes, &tight), Err(ImportError::Limit { limit: "max_offsets", .. })));
}
