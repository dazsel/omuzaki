//! The private test material the seed must never name (`docs/licensing/policy.md`, "Private test material").
//!
//! The list mirrors that policy section, so a reviewer checks the two side by side. It lives in code rather than being
//! read from the docs because a lint that parses prose can silently match nothing. Changing the policy's list means
//! changing [`PrivateList::policy`] in the same PR.
//!
//! This list duplicates `crates/omuzaki-test/evals/private.toml`; until both are merged into one data file,
//! change them together.

/// The names the policy's private-test section gives: private test characters and a private reference. Matched case-insensitively
/// as substrings, so a name with a suffix or inside a path is caught too.
const POLICY_NAMES: &[&str] = &["example-private", "example-private-2"];

/// Face-profile ids that name a private source. `sheet` was fitted to a sheet that cannot be shipped, so a
/// face profile with that id stands for the private material even though the word is common.
const POLICY_FACE_PROFILE_IDS: &[&str] = &["sheet"];

/// The section whose ids are face profiles (`Section::FaceProfiles`).
const FACE_PROFILES: &str = "face-profiles";

/// Names that must not appear in a seed item's id, title, tags or origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateList {
    names: Vec<String>,
    face_profile_ids: Vec<String>,
}

impl PrivateList {
    /// The list the policy names, for the real lint.
    pub fn policy() -> Self {
        Self::new(POLICY_NAMES, POLICY_FACE_PROFILE_IDS)
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
        let list = PrivateList::policy();
        assert_eq!(list.face_profile_named("face-profiles", &["omuzaki", "face-profiles", "sheet"]), Some("sheet"));
        assert_eq!(
            list.face_profile_named("hair", &["omuzaki", "hair", "sheet"]),
            None,
            "a hair item may be called sheet"
        );
        assert_eq!(list.face_profile_named("face-profiles", &["omuzaki", "face-profiles", "round-u"]), None);
    }
}
