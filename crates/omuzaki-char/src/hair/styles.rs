//! The hairstyle library (#2486): whole hairstyles as data, applied in one step (`hair.style.apply`).
//!
//! A style is the hair part's groups and its shared volume and flow ([`HairParams::volume`]), built only from the
//! generators and the layers every hair has: the scalp shell, the fringe, the curtain with its drape, framing locks,
//! sweeps, tails, braids, buns and the ahoge, each lock shaped by its group's [`LockShape`](omuzaki_char_api::LockShape).
//! Applying one replaces the part's groups and volume and keeps its colours, shading and physics, so a style changes
//! how the hair is cut and worn, never how it is lit.
//!
//! **Adding a style is adding a file** (`library/hair-styles/<id>.toml`; `build.rs` embeds every file there, so no list
//! in code names them):
//!
//! ```toml
//! id = "bob"                  # the file's stem
//! name = "Bob"
//! summary = "..."             # one line: what it looks like
//! origin = "..."              # where the numbers come from (a written description, tuned by eye on the kit)
//! [volume]                    # the shared parting, whorl and fullness (optional)
//! [groups.a-cap]              # one table per group, as `hair.group.add` takes it
//! ```
//!
//! Every style is checked as the hair part checks its parameters, and the crate's tests build each on every template.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use omuzaki_char_api::{GroupId, HairGroup, HairParams, HairVolume};
use omuzaki_face_api::Validate;
use serde::{Deserialize, Serialize};

include!(concat!(env!("OUT_DIR"), "/hair_styles.rs"));

/// One hairstyle of the library.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HairStyle {
    /// The style's id, its file's stem.
    pub id: String,
    /// The name pickers show.
    pub name: String,
    /// One line: what it looks like.
    pub summary: String,
    /// Where its numbers come from.
    pub origin: String,
    /// The shared volume and flow, or none.
    #[serde(default)]
    pub volume: Option<HairVolume>,
    /// The groups, by id.
    pub groups: BTreeMap<GroupId, HairGroup>,
}

impl HairStyle {
    /// `hair` cut and worn in this style: its groups and volume replaced, its colours, shading and physics kept.
    #[must_use]
    pub fn applied_to(&self, hair: &HairParams) -> HairParams {
        HairParams { groups: self.groups.clone(), volume: self.volume.clone(), ..hair.clone() }
    }
}

/// Why the library could not be read, or a style found.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum HairStyleError {
    /// A style file doesn't read, or holds hair the hair part would refuse.
    #[error("library/hair-styles/{file}.toml: {message}")]
    File {
        /// The file's stem.
        file: String,
        /// What is wrong.
        message: String,
    },
    /// No style has this id.
    #[error("there is no hairstyle `{0}`")]
    Unknown(String),
}

fn parse(file: &str, text: &str) -> Result<HairStyle, HairStyleError> {
    let fail = |message: String| HairStyleError::File { file: file.to_owned(), message };
    let style: HairStyle = toml::from_str(text).map_err(|e| fail(e.to_string()))?;
    if style.id != file {
        return Err(fail(format!("its id is `{}`, not the file's name", style.id)));
    }
    if style.groups.is_empty() {
        return Err(fail("a style has at least one group".into()));
    }
    style.applied_to(&HairParams::default()).validate().map_err(|e| fail(e.to_string()))?;
    Ok(style)
}

/// Every style, in id order, parsed once.
///
/// # Errors
/// [`HairStyleError::File`] if a library file is malformed (the crate's tests keep that from shipping).
pub fn list() -> Result<&'static [HairStyle], HairStyleError> {
    static STYLES: OnceLock<Result<Vec<HairStyle>, HairStyleError>> = OnceLock::new();
    STYLES
        .get_or_init(|| FILES.iter().map(|(file, text)| parse(file, text)).collect())
        .as_ref()
        .map(Vec::as_slice)
        .map_err(Clone::clone)
}

/// The style called `id`.
///
/// # Errors
/// [`HairStyleError::Unknown`] for an id no style has; [`HairStyleError::File`] as [`list`].
pub fn find(id: &str) -> Result<&'static HairStyle, HairStyleError> {
    list()?.iter().find(|s| s.id == id).ok_or_else(|| HairStyleError::Unknown(id.to_owned()))
}
