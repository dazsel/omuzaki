//! A puppet of every kind INP1 holds, written and read back: the same puppet, the same pixels.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

mod common;

use std::collections::BTreeMap;

use omuzaki_inochi2d::{FindingKind, Limits, Texture, WriteError, import, write_inp1};
use omuzaki_puppet_api::{
    BindTarget, BindValues, Binding, BlendMode, Composite, Drawable, Interp, Lattice, MapMode, MaskMode, MaskRef, Mesh,
    Node, NodeIx, NodeKind, Param, ParamIx, Part, Pendulum, PendulumModel, PhysicsWorld, PuppetDef, PuppetMeta, TexIx,
    TextureRef, Transform2,
};

fn quad(size: f32, uvs: bool) -> Mesh {
    let h = size / 2.0;
    Mesh {
        vertices: vec![[-h, -h], [h, -h], [h, h], [-h, h]],
        uvs: if uvs { vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]] } else { Vec::new() },
        triangles: vec![[0, 1, 2], [0, 2, 3]],
    }
}

fn node(name: &str, parent: Option<u32>, kind: NodeKind) -> Node {
    Node {
        name: name.into(),
        parent: parent.map(NodeIx),
        enabled: true,
        transform: Transform2::default(),
        zsort: 0.0,
        kind,
    }
}

/// Every node kind INP1 holds, a one-axis and a two-axis parameter, every scalar target, a deform binding with unset
/// keys, masks of both modes, a pendulum, metadata with rights and a world. Keys sit at exact binary fractions of their
/// range, so normalising and back is exact.
fn puppet() -> PuppetDef {
    let draw = Drawable {
        blend: BlendMode::Multiply,
        opacity: 0.75,
        tint: [1.0, 0.5, 0.25],
        screen_tint: [0.0, 0.25, 0.0],
        masks: vec![MaskRef { source: NodeIx(2), mode: MaskMode::Dodge }],
        mask_threshold: 0.25,
    };
    let mut nodes = vec![
        node("Root", None, NodeKind::Group),
        node("Body", Some(0), NodeKind::Part(Part { mesh: quad(20.0, true), texture: TexIx(0), draw: draw.clone() })),
        node("Cut", Some(0), NodeKind::Mask(quad(8.0, false))),
        node("Layer", Some(0), NodeKind::Composite(Composite { draw: Drawable { masks: Vec::new(), ..draw.clone() } })),
        node(
            "Face",
            Some(3),
            NodeKind::Part(Part {
                mesh: quad(6.0, true),
                texture: TexIx(1),
                draw: Drawable {
                    masks: vec![MaskRef { source: NodeIx(1), mode: MaskMode::Mask }],
                    ..Drawable::default()
                },
            }),
        ),
        node("Warp", Some(0), NodeKind::MeshDeformer(quad(30.0, false))),
        node(
            "Under",
            Some(5),
            NodeKind::Part(Part { mesh: quad(4.0, true), texture: TexIx(0), draw: Drawable::default() }),
        ),
        node("Later", Some(0), NodeKind::Unknown { type_name: "LatticeDeformer".into() }),
        node("Tail", Some(0), NodeKind::Group),
    ];
    nodes[1].transform = Transform2 { translation: [3.0, -4.5], rotation: 0.25, scale: [1.5, -1.0] };
    nodes[3].zsort = -2.0;
    nodes[6].enabled = false;

    let rights: BTreeMap<String, String> =
        [("allowCommercial", "false"), ("requireAttribution", "true"), ("note", "ours")]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
    PuppetDef {
        meta: PuppetMeta {
            name: "Round trip".into(),
            artist: Some("Omuzaki tests".into()),
            rigger: None,
            copyright: Some("Synthetic".into()),
            licence_url: None,
            rights,
            source: "inochi2d".into(),
        },
        canvas: [1.0, 1.0],
        textures: vec![
            TextureRef { file: "textures/0.png".into(), size: [4, 4] },
            TextureRef { file: "textures/1.png".into(), size: [3, 2] },
        ],
        nodes,
        params: params(),
        physics: vec![Pendulum {
            name: "Tail".into(),
            anchor: NodeIx(8),
            param: ParamIx(1),
            model: PendulumModel::Spring,
            map: MapMode::Yx,
            gravity: 1.5,
            length: 40.0,
            frequency: 2.0,
            angle_damping: 0.25,
            length_damping: 0.75,
            output_scale: [2.0, -1.0],
            local_only: true,
        }],
        world: PhysicsWorld { pixels_per_meter: 800.0, gravity: 9.5 },
    }
}

/// A one-axis parameter with deform bindings (one with unset keys) and a two-axis one with every scalar target.
fn params() -> Vec<Param> {
    let one_axis = Param {
        name: "Breath".into(),
        two_axes: false,
        min: [0.0, 0.0],
        max: [1.0, 0.0],
        default: [0.0, 0.0],
        keys: [vec![0.0, 0.5, 1.0], vec![0.0]],
        bindings: vec![
            Binding {
                node: NodeIx(1),
                target: BindTarget::Deform,
                interp: Interp::Cubic,
                set: vec![true, false, true],
                values: BindValues::Offsets(vec![vec![[0.0, 1.0]; 4], vec![[0.0, 0.0]; 4], vec![[0.5, -1.5]; 4]]),
            },
            Binding {
                node: NodeIx(5),
                target: BindTarget::Deform,
                interp: Interp::Linear,
                set: Vec::new(),
                values: BindValues::Offsets(vec![vec![[0.0, 0.0]; 4], vec![[1.0, 0.0]; 4], vec![[2.0, 0.0]; 4]]),
            },
        ],
    };
    let scalar = |node: u32, target, values: Vec<f32>| Binding {
        node: NodeIx(node),
        target,
        interp: Interp::Nearest,
        set: Vec::new(),
        values: BindValues::Scalar(values),
    };
    let nine = |base: f32| (0..9u8).map(|k| base + f32::from(k) * 0.25).collect::<Vec<_>>();
    let two_axes = Param {
        name: "Head".into(),
        two_axes: true,
        min: [-1.0, -2.0],
        max: [1.0, 2.0],
        default: [0.0, 0.5],
        keys: [vec![-1.0, 0.0, 1.0], vec![-2.0, 0.0, 2.0]],
        bindings: vec![
            scalar(1, BindTarget::TranslateX, nine(0.0)),
            scalar(1, BindTarget::TranslateY, nine(1.0)),
            scalar(1, BindTarget::Rotate, nine(2.0)),
            scalar(1, BindTarget::RotateX, nine(3.0)),
            scalar(1, BindTarget::RotateY, nine(4.0)),
            scalar(1, BindTarget::ScaleX, nine(5.0)),
            scalar(1, BindTarget::ScaleY, nine(6.0)),
            scalar(4, BindTarget::ZSort, nine(7.0)),
            scalar(4, BindTarget::Opacity, nine(8.0)),
            scalar(3, BindTarget::TintR, nine(9.0)),
            scalar(3, BindTarget::TintG, nine(10.0)),
            scalar(3, BindTarget::TintB, nine(11.0)),
        ],
    };

    vec![one_axis, two_axes]
}

fn textures() -> Vec<Texture> {
    vec![Texture { size: [4, 4], rgba: common::pixels(4, 4) }, Texture { size: [3, 2], rgba: common::pixels(3, 2) }]
}

#[test]
fn write_then_read_is_the_same_puppet() {
    let mut def = puppet();
    def.check().unwrap();
    let bytes = write_inp1(&def, &textures()).unwrap();
    let imported = import(&bytes, &Limits::default()).unwrap();
    // INP1 has no canvas: the reader makes one up, and that is the only field the file doesn't carry.
    def.canvas = imported.def.canvas;
    assert_eq!(imported.def, def);
    assert_eq!(imported.textures, textures());
    let lossy: Vec<_> = imported
        .report
        .findings
        .iter()
        .filter(|f| f.kind != FindingKind::Note && f.kind != FindingKind::Synthesised)
        .collect();
    assert!(lossy.is_empty(), "{lossy:#?}");
    // The canvas covers the body, which reaches furthest from the root.
    assert!(imported.def.canvas[0] >= 30.0 && imported.def.canvas[1] >= 30.0, "{:?}", imported.def.canvas);
}

#[test]
fn writing_is_deterministic() {
    assert_eq!(write_inp1(&puppet(), &textures()).unwrap(), write_inp1(&puppet(), &textures()).unwrap());
}

#[test]
fn a_pendulum_on_a_node_it_cannot_become_gets_a_physics_child() {
    let mut def = puppet();
    def.physics[0].anchor = NodeIx(1);
    let bytes = write_inp1(&def, &textures()).unwrap();
    let imported = import(&bytes, &Limits::default()).unwrap();
    assert_eq!(imported.def.nodes.len(), def.nodes.len() + 1);
    let p = &imported.def.physics[0];
    let anchor = &imported.def.nodes[p.anchor.get()];
    assert_eq!((anchor.name.as_str(), anchor.parent), ("Tail", Some(NodeIx(1))));
}

#[test]
fn what_inp1_cannot_hold_is_refused() {
    let mut def = puppet();
    def.nodes.push(node(
        "Grid",
        Some(0),
        NodeKind::Lattice(Lattice { cols: 2, rows: 2, origin: [0.0, 0.0], size: [1.0, 1.0] }),
    ));
    assert!(matches!(write_inp1(&def, &textures()), Err(WriteError::Unsupported { path, .. }) if path == "nodes/9"));

    let mut def = puppet();
    def.nodes[7].kind = NodeKind::Unknown { type_name: "Part".into() };
    assert!(matches!(write_inp1(&def, &textures()), Err(WriteError::Unsupported { .. })));

    let mut def = puppet();
    def.params[0].max = [0.0, 0.0];
    def.params[0].keys[0] = vec![0.0];
    def.params[0].bindings.clear();
    assert!(write_inp1(&def, &textures()).is_ok(), "one key over an empty range is fine");
    def.params[1].max[0] = -1.0;
    assert!(
        matches!(write_inp1(&def, &textures()), Err(WriteError::Unsupported { path, .. }) if path == "params/1/keys/0"),
        "several keys over an empty range"
    );

    let def = puppet();
    assert!(matches!(write_inp1(&def, &textures()[..1]), Err(WriteError::Texture { .. })));
    let mut wrong = textures();
    wrong[1].rgba.pop();
    assert!(matches!(write_inp1(&def, &wrong), Err(WriteError::Texture { index: 1, .. })));

    let mut def = puppet();
    def.nodes[1].parent = Some(NodeIx(5000));
    assert!(matches!(write_inp1(&def, &textures()), Err(WriteError::Invalid(_))));
}
