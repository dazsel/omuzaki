//! A hand-written FBX 7.4 ASCII writer for the tests: no third-party file, every number in the document is one the test
//! chose. Not every test uses every item.
#![allow(dead_code)]

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
}

/// The id of FBX's implicit scene root, the target of a top-level model's connection.
pub const SCENE_ROOT: i64 = 0;

/// Builds an ASCII FBX with `GlobalSettings`, `Model` nodes and their parent links.
///
/// `up`, `front` and `coord` are FBX's `UpAxis`, `FrontAxis` and `CoordAxis`. ufbx reads `FrontAxis` as the direction a
/// model faces toward the viewer, so a Maya-style file (Y up) has `front` +Z and a 3ds Max-style file (Z up) has
/// `front` -Y: the tests assert that convention, and that the importer puts the declared front at +Z.
pub struct AsciiFbx {
    up: Axis,
    front: Axis,
    coord: Axis,
    /// `UnitScaleFactor`: centimetres per unit, so 1.0 is a Maya-style file and 100.0 is metres.
    unit_cm: f64,
    models: String,
    connections: String,
    next_id: i64,
}

impl AsciiFbx {
    /// A file with the given axes and unit scale, and no models.
    pub fn new(up: Axis, front: Axis, coord: Axis, unit_cm: f64) -> Self {
        Self { up, front, coord, unit_cm, models: String::new(), connections: String::new(), next_id: 1000 }
    }

    /// A model named `name` under `parent` ([`SCENE_ROOT`] or an earlier model's id), at `translation` with Euler
    /// `rotation` in degrees. Returns its id. Siblings keep the order they were added in.
    pub fn model(&mut self, name: &str, parent: i64, translation: [f64; 3], rotation: [f64; 3]) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        let [tx, ty, tz] = translation;
        let [rx, ry, rz] = rotation;
        // Writing to a `String` cannot fail, but the result is still matched, never dropped.
        let wrote = write!(
            self.models,
            "\tModel: {id}, \"Model::{name}\", \"Null\" {{\n\t\tVersion: 232\n\t\tProperties70:  {{\n\
             \t\t\tP: \"Lcl Translation\", \"Lcl Translation\", \"\", \"A\",{tx},{ty},{tz}\n\
             \t\t\tP: \"Lcl Rotation\", \"Lcl Rotation\", \"\", \"A\",{rx},{ry},{rz}\n\
             \t\t\tP: \"Lcl Scaling\", \"Lcl Scaling\", \"\", \"A\",1,1,1\n\t\t}}\n\t}}\n"
        )
        .and_then(|()| writeln!(self.connections, "\tC: \"OO\",{id},{parent}"));
        assert!(wrote.is_ok(), "writing to a String failed");
        id
    }

    /// The document text.
    pub fn text(&self) -> String {
        let axis = |name: &str, a: Axis| {
            format!(
                "\t\t\tP: \"{name}Axis\", \"int\", \"Integer\", \"\",{}\n\t\t\tP: \"{name}AxisSign\", \"int\", \"Integer\", \"\",{}\n",
                a.index, a.sign
            )
        };
        format!(
            "; FBX 7.4.0 project file\n\
             FBXHeaderExtension:  {{\n\tFBXHeaderVersion: 1003\n\tFBXVersion: 7400\n\tCreator: \"Omuzaki test builder\"\n}}\n\
             GlobalSettings:  {{\n\tVersion: 1000\n\tProperties70:  {{\n{}{}{}\
             \t\t\tP: \"UnitScaleFactor\", \"double\", \"Number\", \"\",{}\n\t}}\n}}\n\
             Objects:  {{\n{}}}\nConnections:  {{\n{}}}\n",
            axis("Up", self.up),
            axis("Front", self.front),
            axis("Coord", self.coord),
            self.unit_cm,
            self.models,
            self.connections
        )
    }

    /// The document as bytes.
    pub fn bytes(&self) -> Vec<u8> {
        self.text().into_bytes()
    }
}
