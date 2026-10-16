//! The private test material the seed must never name (`docs/licensing/policy.md`, "Private test material").
//!
//! The list is one data file, `docs/licensing/private-material.toml`, which the eval harness (`omuzaki-test eval`) reads as
//! well, so the two lints cannot drift. Its `names` are matched case-insensitively as substrings, so a name with a suffix
//! or inside a path is caught too; its `face_profile_ids` name a source that cannot be shipped, so a face profile with
//! that id stands for the private material even though the word (`sheet`) is common. The file is data, not prose, and a
//! list that comes back empty is an error: a lint that matched nothing would pass for the wrong reason.

use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};

/// Where the list is, relative to the workspace root.
const FILE: &str = "docs/licensing/private-material.toml";

/// The section whose ids are face profiles (`Section::FaceProfiles`).
const FACE_PROFILES: &str = "face-profiles";

/// Names that must not appear in a seed item's id, title, tags or origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateList {
    names: Vec<String>,
    face_profile_ids: Vec<String>,
}

/// The parts of the shared file this lint reads; the eval harness reads the rest.
#[derive(Deserialize)]
struct File {
    names: Vec<String>,
    face_profile_ids: Vec<String>,
}

impl PrivateList {
    /// The list in `docs/licensing/private-material.toml` under `root`.
    ///
    /// # Errors
    /// [`Error::Failed`] when the file cannot be read or parsed, or names nothing.
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(FILE);
        let fail = |why: String| Error::Failed { command: format!("read {FILE}"), stderr: why };
        let text = fs::read_to_string(&path).map_err(|e| fail(format!("{}: {e}", path.display())))?;
        let file: File = toml::from_str(&text).map_err(|e| fail(e.to_string()))?;
        // A key that is missing or empty makes the lint match nothing and pass for the wrong reason.
        for (key, list) in [("names", &file.names), ("face_profile_ids", &file.face_profile_ids)] {
            if list.is_empty() {
                return Err(fail(format!("`{key}` is empty: the lint would match nothing")));
            }
        }
        let (names, ids): (Vec<&str>, Vec<&str>) = (
            file.names.iter().map(String::as_str).collect(),
            file.face_profile_ids.iter().map(String::as_str).collect(),
        );
        Ok(Self::new(&names, &ids))
    }

    /// A list of `names` (any text) and `face_profile_ids` (whole id segments). Tests configure their own, so the
    /// negative cases never need the real private names written into a fixture.
    pub fn new(names: &[&str], face_profile_ids: &[&str]) -> Self {
        Self {
            names: names.iter().map(|name| name.to_lowercase()).collect(),
            face_profile_ids: face_profile_ids.iter().map(|id| id.to_lowercase()).collect(),
        }
    }

    /// The listed name `text` contains, if any.
    pub fn named_in(&self, text: &str) -> Option<&str> {
        let lower = text.to_lowercase();
        self.names.iter().find(|name| lower.contains(name.as_str())).map(String::as_str)
    }

    /// Whether the id `segments` is a listed face-profile id. Only the face-profile section counts: `sheet` is an
    /// ordinary word elsewhere.
    pub fn face_profile_named(&self, section: &str, id_segments: &[&str]) -> Option<&str> {
        if section != FACE_PROFILES {
            return None;
        }
        id_segments
            .iter()
            .find_map(|segment| self.face_profile_ids.iter().find(|listed| listed.as_str() == segment.to_lowercase()))
            .map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listed_name_is_caught_wherever_it_sits_in_the_text() {
        let list = PrivateList::new(&["zorblax"], &[]);
        assert_eq!(list.named_in("Zorblax tails"), Some("zorblax"));
        assert_eq!(list.named_in("omuzaki/hair/zorblax-2"), Some("zorblax"));
        assert_eq!(list.named_in("Twintails"), None);
    }

    #[test]
    fn sheet_counts_only_as_a_face_profile_id() {
        let list = PrivateList::new(&["example-private"], &["sheet"]);
        assert_eq!(list.face_profile_named("face-profiles", &["omuzaki", "face-profiles", "sheet"]), Some("sheet"));
        assert_eq!(
            list.face_profile_named("hair", &["omuzaki", "hair", "sheet"]),
            None,
            "a hair item may be called sheet"
        );
        assert_eq!(list.face_profile_named("face-profiles", &["omuzaki", "face-profiles", "round-u"]), None);
    }

    #[test]
    fn the_shared_file_loads_and_names_the_sheet_profile() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let list = PrivateList::load(root).unwrap();
        assert!(list.named_in("characters/example-private/a.png").is_some());
        assert_eq!(list.face_profile_named("face-profiles", &["x", "sheet"]), Some("sheet"));
        assert!(PrivateList::load(&root.join("no-such-folder")).is_err());
    }

    #[test]
    fn a_missing_or_empty_list_is_an_error_but_other_keys_are_allowed() {
        let dir = std::env::temp_dir().join(format!("private-list-{}", std::process::id()));
        let file = dir.join(FILE);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let load = |text: &str| {
            std::fs::write(&file, text).unwrap();
            PrivateList::load(&dir)
        };
        let ok = load("names = [\"a\"]\nface_profile_ids = [\"b\"]\nprefixes = [\"x/\"]\nhashes = []\n");
        assert!(ok.is_ok(), "the eval harness's keys are allowed");
        for (text, key) in [
            ("names = [\"a\"]\n", "face_profile_ids"),
            ("names = [\"a\"]\nface_profile_ids = []\n", "face_profile_ids"),
            ("face_profile_ids = [\"b\"]\n", "names"),
            ("names = []\nface_profile_ids = [\"b\"]\n", "names"),
        ] {
            let error = load(text).unwrap_err().to_string();
            assert!(error.contains(key), "{text:?}: {error}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
