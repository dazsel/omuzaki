//! The built-in poses: one TOML file each in `library/poses/`, embedded and parsed once.

use std::sync::OnceLock;

use super::def::{PoseDef, PoseError, PoseFile};

/// The built-in files, in the order `pose.list` shows them. Adding a pose is adding a file and a line here.
const BUILTIN: [(&str, &str); 3] = [
    ("wave-right", include_str!("../../library/poses/wave-right.toml")),
    ("wave-left", include_str!("../../library/poses/wave-left.toml")),
    ("stand-relaxed", include_str!("../../library/poses/stand-relaxed.toml")),
];

/// A set of poses with unique ids, in a stable order.
#[derive(Clone, Debug, PartialEq)]
pub struct PoseLibrary {
    poses: Vec<PoseDef>,
}

/// The built-in library, or why it failed to load, parsed once.
static BUILTIN_LIBRARY: OnceLock<Result<PoseLibrary, PoseError>> = OnceLock::new();

impl PoseLibrary {
    /// Parses library files (TOML, rotations in degrees) and checks each pose. `sources` pairs a label for error
    /// messages with the file's text.
    ///
    /// # Errors
    /// The first [`PoseError`]: bad TOML or layout, an invalid name, a non-finite number, a repeated id.
    pub fn parse(sources: &[(&str, &str)]) -> Result<Self, PoseError> {
        let mut poses: Vec<PoseDef> = Vec::with_capacity(sources.len());
        for (label, text) in sources {
            let file: PoseFile = toml::from_str(text)
                .map_err(|e| PoseError::Parse { file: (*label).to_owned(), message: e.to_string() })?;
            let pose = PoseDef::from(file);
            pose.validate()?;
            if poses.iter().any(|p| p.id == pose.id) {
                return Err(PoseError::DuplicateId(pose.id));
            }
            poses.push(pose);
        }
        Ok(Self { poses })
    }

    /// The poses that ship with Omuzaki.
    ///
    /// # Errors
    /// Only if the embedded files are broken, which the crate's tests rule out.
    pub fn builtin() -> Result<&'static Self, PoseError> {
        BUILTIN_LIBRARY.get_or_init(|| Self::parse(&BUILTIN)).as_ref().map_err(Clone::clone)
    }

    /// The pose called `id`.
    pub fn get(&self, id: &str) -> Option<&PoseDef> {
        self.poses.iter().find(|p| p.id == id)
    }

    /// Every pose, in file order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &PoseDef> {
        self.poses.iter()
    }
}
