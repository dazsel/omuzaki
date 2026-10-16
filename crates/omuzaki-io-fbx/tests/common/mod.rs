//! A hand-written FBX 7.4 writer for the tests, ASCII and binary: no third-party file, every number in the document is
//! one the test chose. Not every test uses every item.
#![allow(dead_code)]
// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used)]

use std::fmt::Write;

/// A signed world axis as FBX's `*Axis` and `*AxisSign` properties spell it.
#[derive(Copy, Clone)]
pub struct Axis {
    /// 0 for X, 1 for Y, 2 for Z.
    index: u8,
    /// 1 or -1.
    sign: i8,
}

impl Axis {
    /// +X.
    pub const X: Axis = Axis { index: 0, sign: 1 };
    /// +Y.
    pub const Y: Axis = Axis { index: 1, sign: 1 };
    /// +Z.
    pub const Z: Axis = Axis { index: 2, sign: 1 };
    /// -Y.
    pub const NEG_Y: Axis = Axis { index: 1, sign: -1 };
    /// -Z.
    pub const NEG_Z: Axis = Axis { index: 2, sign: -1 };
}

/// The id of FBX's implicit scene root, the target of a top-level model's connection.
pub const SCENE_ROOT: i64 = 0;

/// One model: its id, name, parent and local transform (Euler rotation in degrees).
struct Model {
    id: i64,
    name: String,
    parent: i64,
    translation: [f64; 3],
    rotation: [f64; 3],
    scale: [f64; 3],
}

/// A mesh or a material, attached to a model.
enum Extra {
    Animation { id: i64 },
    Mesh { id: i64, model: i64 },
    Material { id: i64, model: i64 },
}

/// Builds an FBX with `GlobalSettings`, `Model` nodes and their parent links, as ASCII text or as binary.
///
/// `up`, `front` and `coord` are FBX's `UpAxis`, `FrontAxis` and `CoordAxis`. ufbx reads `FrontAxis` as the direction a
/// model faces toward the viewer, so a Maya-style file (Y up) has `front` +Z and a 3ds Max-style file (Z up) has
/// `front` -Y: the tests assert that convention, and that the importer puts the declared front at +Z.
///
/// The binary form carries the same models and settings as the ASCII one; meshes and materials are ASCII only.
pub struct FbxBuilder {
    up: Axis,
    front: Axis,
    coord: Axis,
    /// `UnitScaleFactor`: centimetres per unit, so 1.0 is a Maya-style file and 100.0 is metres.
    unit_cm: f64,
    models: Vec<Model>,
    extras: Vec<Extra>,
    next_id: i64,
}

impl FbxBuilder {
    /// A file with the given axes and unit scale, and no models.
    pub fn new(up: Axis, front: Axis, coord: Axis, unit_cm: f64) -> Self {
        Self { up, front, coord, unit_cm, models: Vec::new(), extras: Vec::new(), next_id: 1000 }
    }

    /// A model named `name` under `parent` ([`SCENE_ROOT`] or an earlier model's id), at `translation` with Euler
    /// `rotation` in degrees. Returns its id. Siblings keep the order they were added in.
    pub fn model(&mut self, name: &str, parent: i64, translation: [f64; 3], rotation: [f64; 3]) -> i64 {
        self.scaled_model(name, parent, translation, rotation, [1.0; 3])
    }

    /// As [`FbxBuilder::model`], with `scale` as the file writes `Lcl Scaling`.
    pub fn scaled_model(
        &mut self,
        name: &str,
        parent: i64,
        translation: [f64; 3],
        rotation: [f64; 3],
        scale: [f64; 3],
    ) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        self.push_model(id, name, parent, translation, rotation, scale);
        id
    }

    /// A model with a given `id`, which may repeat an earlier one: a file ufbx has to repair.
    pub fn model_with_id(&mut self, id: i64, name: &str, parent: i64) {
        self.push_model(id, name, parent, [0.0; 3], [0.0; 3], [1.0; 3]);
    }

    fn push_model(
        &mut self,
        id: i64,
        name: &str,
        parent: i64,
        translation: [f64; 3],
        rotation: [f64; 3],
        scale: [f64; 3],
    ) {
        self.models.push(Model { id, name: name.to_owned(), parent, translation, rotation, scale });
    }

    /// A one-triangle mesh drawn by `model`.
    pub fn mesh(&mut self, model: i64) {
        let id = self.next_id;
        self.next_id += 1;
        self.extras.push(Extra::Mesh { id, model });
    }

    /// An animation stack, which this version does not read.
    pub fn animation(&mut self) {
        let id = self.next_id;
        self.next_id += 1;
        self.extras.push(Extra::Animation { id });
    }

    /// A material used by `model`.
    pub fn material(&mut self, model: i64) {
        let id = self.next_id;
        self.next_id += 1;
        self.extras.push(Extra::Material { id, model });
    }

    /// The ASCII document text.
    pub fn text(&self) -> String {
        let axis = |name: &str, a: Axis| {
            format!(
                "\t\t\tP: \"{name}Axis\", \"int\", \"Integer\", \"\",{}\n\t\t\tP: \"{name}AxisSign\", \"int\", \"Integer\", \"\",{}\n",
                a.index, a.sign
            )
        };
        let mut objects = String::new();
        let mut connections = String::new();
        // Writing to a `String` cannot fail, but the results are still checked, never dropped.
        let mut ok = true;
        for m in &self.models {
            let Model { id, name, parent, translation: [tx, ty, tz], rotation: [rx, ry, rz], scale: [sx, sy, sz] } = m;
            ok &= write!(
                objects,
                "\tModel: {id}, \"Model::{name}\", \"Null\" {{\n\t\tVersion: 232\n\t\tProperties70:  {{\n\
                 \t\t\tP: \"Lcl Translation\", \"Lcl Translation\", \"\", \"A\",{tx},{ty},{tz}\n\
                 \t\t\tP: \"Lcl Rotation\", \"Lcl Rotation\", \"\", \"A\",{rx},{ry},{rz}\n\
                 \t\t\tP: \"Lcl Scaling\", \"Lcl Scaling\", \"\", \"A\",{sx},{sy},{sz}\n\t\t}}\n\t}}\n"
            )
            .is_ok();
            ok &= writeln!(connections, "\tC: \"OO\",{id},{parent}").is_ok();
        }
        for extra in &self.extras {
            match extra {
                Extra::Animation { id } => {
                    ok &= write!(objects, "\tAnimationStack: {id}, \"AnimStack::Take 001\", \"\" {{\n\t}}\n").is_ok();
                    ok &= writeln!(connections, "\tC: \"OO\",{id},{SCENE_ROOT}").is_ok();
                }
                Extra::Mesh { id, model } => {
                    ok &= write!(
                        objects,
                        "\tGeometry: {id}, \"Geometry::\", \"Mesh\" {{\n\t\tVertices: *9 {{\n\t\t\ta: 0,0,0,1,0,0,0,1,0\n\t\t}}\n\
                         \t\tPolygonVertexIndex: *3 {{\n\t\t\ta: 0,1,-3\n\t\t}}\n\t}}\n"
                    )
                    .is_ok();
                    ok &= writeln!(connections, "\tC: \"OO\",{id},{model}").is_ok();
                }
                Extra::Material { id, model } => {
                    ok &= write!(
                        objects,
                        "\tMaterial: {id}, \"Material::skin\", \"\" {{\n\t\tVersion: 102\n\t\tShadingModel: \"lambert\"\n\t}}\n"
                    )
                    .is_ok();
                    ok &= writeln!(connections, "\tC: \"OO\",{id},{model}").is_ok();
                }
            }
        }
        assert!(ok, "writing to a String failed");
        format!(
            "; FBX 7.4.0 project file\n\
             FBXHeaderExtension:  {{\n\tFBXHeaderVersion: 1003\n\tFBXVersion: 7400\n\tCreator: \"Omuzaki test builder\"\n}}\n\
             GlobalSettings:  {{\n\tVersion: 1000\n\tProperties70:  {{\n{}{}{}\
             \t\t\tP: \"UnitScaleFactor\", \"double\", \"Number\", \"\",{}\n\t}}\n}}\n\
             Objects:  {{\n{objects}}}\nConnections:  {{\n{connections}}}\n",
            axis("Up", self.up),
            axis("Front", self.front),
            axis("Coord", self.coord),
            self.unit_cm,
        )
    }

    /// The ASCII document as bytes.
    pub fn bytes(&self) -> Vec<u8> {
        self.text().into_bytes()
    }

    /// The same models and settings as binary FBX 7.4 (32-bit record offsets, no footer).
    pub fn binary(&self) -> Vec<u8> {
        let mut out = b"Kaydara FBX Binary  \0\x1A\x00".to_vec();
        out.extend(7400_u32.to_le_bytes());
        record(&mut out, "FBXHeaderExtension", &[], &|out| {
            record(out, "FBXHeaderVersion", &[Prop::Int(1003)], &|_| {});
            record(out, "FBXVersion", &[Prop::Int(7400)], &|_| {});
            record(out, "Creator", &[Prop::Str("Omuzaki test builder".into())], &|_| {});
        });
        record(&mut out, "GlobalSettings", &[], &|out| {
            record(out, "Version", &[Prop::Int(1000)], &|_| {});
            record(out, "Properties70", &[], &|out| {
                for (name, a) in [("UpAxis", self.up), ("FrontAxis", self.front), ("CoordAxis", self.coord)] {
                    property(out, name, "int", "Integer", "", &[Prop::Int(i32::from(a.index))]);
                    property(out, &format!("{name}Sign"), "int", "Integer", "", &[Prop::Int(i32::from(a.sign))]);
                }
                property(out, "UnitScaleFactor", "double", "Number", "", &[Prop::Double(self.unit_cm)]);
            });
        });
        record(&mut out, "Objects", &[], &|out| {
            for m in &self.models {
                let props = [Prop::Long(m.id), Prop::Str(format!("{}\0\u{1}Model", m.name)), Prop::Str("Null".into())];
                record(out, "Model", &props, &|out| {
                    record(out, "Version", &[Prop::Int(232)], &|_| {});
                    record(out, "Properties70", &[], &|out| {
                        for (name, v) in
                            [("Lcl Translation", m.translation), ("Lcl Rotation", m.rotation), ("Lcl Scaling", m.scale)]
                        {
                            property(out, name, name, "", "A", &v.map(Prop::Double));
                        }
                    });
                });
            }
        });
        record(&mut out, "Connections", &[], &|out| {
            for m in &self.models {
                record(out, "C", &[Prop::Str("OO".into()), Prop::Long(m.id), Prop::Long(m.parent)], &|_| {});
            }
        });
        // The top level ends with a null record.
        out.extend([0_u8; 13]);
        out
    }
}

/// A binary FBX property.
enum Prop {
    Int(i32),
    Long(i64),
    Double(f64),
    Str(String),
}

fn encode(prop: &Prop) -> Vec<u8> {
    let (tag, bytes): (u8, Vec<u8>) = match prop {
        Prop::Int(v) => (b'I', v.to_le_bytes().to_vec()),
        Prop::Long(v) => (b'L', v.to_le_bytes().to_vec()),
        Prop::Double(v) => (b'D', v.to_le_bytes().to_vec()),
        Prop::Str(s) => {
            let len = u32::try_from(s.len()).expect("a short test string");
            (b'S', [len.to_le_bytes().as_slice(), s.as_bytes()].concat())
        }
    };
    [vec![tag], bytes].concat()
}

/// A `P` record of `Properties70`, written into `out`: four strings, then the values.
fn property(out: &mut Vec<u8>, name: &str, kind: &str, label: &str, flags: &str, values: &[Prop]) {
    let mut props =
        vec![Prop::Str(name.into()), Prop::Str(kind.into()), Prop::Str(label.into()), Prop::Str(flags.into())];
    props.extend(values.iter().map(|v| match v {
        Prop::Int(x) => Prop::Int(*x),
        Prop::Long(x) => Prop::Long(*x),
        Prop::Double(x) => Prop::Double(*x),
        Prop::Str(x) => Prop::Str(x.clone()),
    }));
    record(out, "P", &props, &|_| {});
}

/// One binary record: end offset, property count and byte length, name, properties, then its children and a null record
/// when it has any. The offsets are absolute, so `out` must hold the whole file so far.
fn record(out: &mut Vec<u8>, name: &str, props: &[Prop], children: &dyn Fn(&mut Vec<u8>)) {
    let start = out.len();
    out.extend([0_u8; 12]);
    out.push(u8::try_from(name.len()).expect("a short record name"));
    out.extend(name.as_bytes());
    let props_start = out.len();
    for prop in props {
        out.extend(encode(prop));
    }
    let props_len = out.len() - props_start;
    let before_children = out.len();
    children(out);
    if out.len() > before_children {
        out.extend([0_u8; 13]);
    }
    // `out` is the whole file, and a test file is far below 4 GiB.
    let end = u32::try_from(out.len()).expect("a small file");
    out[start..start + 4].copy_from_slice(&end.to_le_bytes());
    out[start + 4..start + 8].copy_from_slice(&u32::try_from(props.len()).expect("few properties").to_le_bytes());
    out[start + 8..start + 12].copy_from_slice(&u32::try_from(props_len).expect("short properties").to_le_bytes());
}
