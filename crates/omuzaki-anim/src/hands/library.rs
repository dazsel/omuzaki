//! The built-in shapes: one TOML file each in `library/hand-shapes/`, embedded and parsed once.

use std::borrow::Cow;
use std::sync::OnceLock;

use serde::Deserialize;

use super::shape::{FingerCurl, HandShape, HandShapeError};

/// The built-in files, in the order `hand.list_shapes` shows them. Adding a shape is adding a file and a line here.
const BUILTIN: [(&str, &str); 10] = [
    ("open", include_str!("../../library/hand-shapes/open.toml")),
    ("fist", include_str!("../../library/hand-shapes/fist.toml")),
    ("point", include_str!("../../library/hand-shapes/point.toml")),
    ("peace", include_str!("../../library/hand-shapes/peace.toml")),
    ("heart", include_str!("../../library/hand-shapes/heart.toml")),
    ("grip", include_str!("../../library/hand-shapes/grip.toml")),
    ("thumbs-up", include_str!("../../library/hand-shapes/thumbs-up.toml")),
    ("rest", include_str!("../../library/hand-shapes/rest.toml")),
    ("spread", include_str!("../../library/hand-shapes/spread.toml")),
    ("flat", include_str!("../../library/hand-shapes/flat.toml")),
];

/// One finger as written in a library file: degrees.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FingerFile {
    spread: f32,
    mcp: f32,
    pip: f32,
    dip: f32,
}

impl From<FingerFile> for FingerCurl {
    fn from(f: FingerFile) -> Self {
        Self {
            spread: f.spread.to_radians(),
            mcp: f.mcp.to_radians(),
            pip: f.pip.to_radians(),
            dip: f.dip.to_radians(),
        }
    }
}

/// A library file. Every finger is required, so a shape never depends on a default.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShapeFile {
    id: String,
    summary: String,
    thumb: FingerFile,
    index: FingerFile,
    middle: FingerFile,
    ring: FingerFile,
    little: FingerFile,
}

impl From<ShapeFile> for HandShape {
    fn from(f: ShapeFile) -> Self {
        Self {
            id: Cow::Owned(f.id),
            summary: Cow::Owned(f.summary),
            curls: [f.thumb, f.index, f.middle, f.ring, f.little].map(FingerCurl::from),
        }
    }
}

/// A set of hand shapes with unique ids, in a stable order.
#[derive(Clone, Debug, PartialEq)]
pub struct HandLibrary {
    shapes: Vec<HandShape>,
}

/// The built-in library, or why it failed to load, parsed once.
static BUILTIN_LIBRARY: OnceLock<Result<HandLibrary, HandShapeError>> = OnceLock::new();

impl HandLibrary {
    /// Parses library files (TOML, angles in degrees) and checks each shape. `sources` pairs a label for error messages
    /// with the file's text.
    ///
    /// # Errors
    /// The first [`HandShapeError`]: bad TOML or layout, an invalid id or summary, an angle out of range, a repeated id.
    pub fn parse(sources: &[(&str, &str)]) -> Result<Self, HandShapeError> {
        let mut shapes: Vec<HandShape> = Vec::with_capacity(sources.len());
        for (label, text) in sources {
            let file: ShapeFile = toml::from_str(text)
                .map_err(|e| HandShapeError::Parse { file: (*label).to_owned(), message: e.to_string() })?;
            let shape = HandShape::from(file);
            shape.validate()?;
            if shapes.iter().any(|s| s.id == shape.id) {
                return Err(HandShapeError::DuplicateId(shape.id.into_owned()));
            }
            shapes.push(shape);
        }
        Ok(Self { shapes })
    }

    /// The shapes that ship with Omuzaki: open, fist, point, peace, heart, grip, thumbs-up and rest.
    ///
    /// # Errors
    /// Only if the embedded files are broken, which the crate's tests rule out.
    pub fn builtin() -> Result<&'static Self, HandShapeError> {
        BUILTIN_LIBRARY.get_or_init(|| Self::parse(&BUILTIN)).as_ref().map_err(Clone::clone)
    }

    /// The shape called `id`.
    pub fn get(&self, id: &str) -> Option<&HandShape> {
        self.shapes.iter().find(|s| s.id == id)
    }

    /// Every shape, in file order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &HandShape> {
        self.shapes.iter()
    }

    /// The `open` and `fist` shapes that [`HandShape::from_curls`] mixes between.
    pub(super) fn tracking_anchors() -> Result<(&'static HandShape, &'static HandShape), HandShapeError> {
        let library = Self::builtin()?;
        let find = |id: &'static str| library.get(id).ok_or(HandShapeError::Missing(id));
        Ok((find("open")?, find("fist")?))
    }
}
