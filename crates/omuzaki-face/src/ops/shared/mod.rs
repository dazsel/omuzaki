//! The mechanics the face edit ops share: `check` decides what an edit may introduce, `edit` copies, writes and diffs a
//! face, and `path` reads the paths into it. The style ops (`face.set_style`, marks) and the mapping and rest ops all
//! build on these, so each rule (every error named, no null, the smallest patch) is written once.

pub(crate) mod check;
pub(crate) mod edit;
pub(crate) mod path;
