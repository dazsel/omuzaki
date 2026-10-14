//! Derived message ids (ADR 0023 decision 2; design 3.1). One function each, so extraction, the UI, the CLI and the
//! checks all compute an id the same way.

use thiserror::Error;

/// Separates the owner id from each schema key in an argument id.
const KEY_SEP: &str = "--";

/// The id of an op's summary: `face.set_profile` gives `op-face-set_profile`. Its long text is the `.description`
/// attribute of the same id. Call [`check_op_name`] first; the mapping is one-to-one only for valid names.
pub fn op(name: &str) -> String {
    format!("op-{}", name.replace('.', "-"))
}

/// The id of a part kind's title: `twin_tails` gives `kind-twin_tails`. Its summary is the `.summary` attribute.
pub fn kind(id: &str) -> String {
    format!("kind-{id}")
}

/// The id of an argument's title: `owner_id`, then `--` and each key of the schema path. `.description` is its
/// attribute. An empty path gives the owner's own id.
pub fn arg(owner_id: &str, keys: &[&str]) -> String {
    let mut id = owner_id.to_owned();
    for key in keys {
        id.push_str(KEY_SEP);
        id.push_str(key);
    }
    id
}

/// Why an op name is not allowed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OpNameError {
    /// Fewer than two dot-separated segments (`face`).
    #[error("op name `{0}` needs at least two segments, like `face.set_profile`")]
    TooShort(String),
    /// A segment is empty, starts with something other than a lowercase letter, or holds a character outside
    /// `a-z`, `0-9` and `_` (a `-` would make the id mapping ambiguous).
    #[error(
        "op name `{name}` has a bad segment `{segment}`: use lowercase letters, digits and `_`, starting with a letter"
    )]
    BadSegment {
        /// The whole name.
        name: String,
        /// The offending segment.
        segment: String,
    },
}

/// Checks the op-name grammar of design 3.1: `^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$`. The crate check calls it for
/// every registered op, which keeps [`op`] one-to-one.
///
/// # Errors
/// [`OpNameError`] names the rule that failed.
pub fn check_op_name(name: &str) -> Result<(), OpNameError> {
    if !name.contains('.') {
        return Err(OpNameError::TooShort(name.to_owned()));
    }
    for segment in name.split('.') {
        let mut chars = segment.chars();
        let ok = chars.next().is_some_and(|c| c.is_ascii_lowercase())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !ok {
            return Err(OpNameError::BadSegment { name: name.to_owned(), segment: segment.to_owned() });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_of_the_design_examples() {
        assert_eq!(op("face.set_profile"), "op-face-set_profile");
        assert_eq!(arg(&op("face.set_profile"), &["blend"]), "op-face-set_profile--blend");
        assert_eq!(arg(&op("anim.ease"), &["mode", "Fast"]), "op-anim-ease--mode--Fast");
        assert_eq!(arg("op-x-y", &[]), "op-x-y");
        assert_eq!(kind("twin_tails"), "kind-twin_tails");
        assert_eq!(arg(&kind("twin_tails"), &["length"]), "kind-twin_tails--length");
    }

    #[test]
    fn the_op_name_grammar() {
        for good in ["face.set_profile", "a.b", "anim.ease2.in_out", "x1.y"] {
            assert_eq!(check_op_name(good), Ok(()), "{good}");
        }
        assert!(matches!(check_op_name("face"), Err(OpNameError::TooShort(_))));
        for bad in ["face-set.x", "Face.x", "face.", ".face", "face..x", "1a.b", "face.set profile", "face._x", ""] {
            assert!(check_op_name(bad).is_err(), "{bad:?} should be refused");
        }
    }
}
