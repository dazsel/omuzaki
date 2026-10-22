//! Tool names. Ops are named `area.verb_noun` (`face.set_profile`), but Anthropic's tool-name rule is
//! `^[a-zA-Z0-9_-]{1,64}$` and many clients follow it, so a dotted name may be rejected or mangled. The tool an agent
//! sees is the op name with `.` replaced by `_` (`face_set_profile`); the dotted op name stays in the tool's `title`
//! and on the first line of its description, and is what the runner is called with.

use crate::error::ServerError;

/// The longest tool name clients accept.
const MAX_LEN: usize = 64;

/// The tool name for an op (or built-in) name.
pub(crate) fn tool_name(op_name: &str) -> String {
    omuzaki_ops_api::tool_name(op_name)
}

/// The tool name for `op_name`, checked against the client rule.
///
/// # Errors
/// [`ServerError::UnusableOp`] when the result is empty, longer than 64 characters, or has characters other than
/// letters, digits, `_` and `-`.
pub(crate) fn checked_tool_name(op_name: &str) -> Result<String, ServerError> {
    let name = tool_name(op_name);
    let valid_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-');
    if (1..=MAX_LEN).contains(&name.len()) && name.chars().all(valid_char) {
        return Ok(name);
    }
    Err(ServerError::UnusableOp {
        op: op_name.to_owned(),
        reason: format!(
            "its tool name `{name}` must be 1 to {MAX_LEN} characters of letters, digits, `_` and `-` (dots become \
             underscores)"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dots_become_underscores() {
        assert_eq!(tool_name("face.set_profile"), "face_set_profile");
        assert_eq!(checked_tool_name("history.undo").unwrap(), "history_undo");
        assert_eq!(checked_tool_name("a-b.c").unwrap(), "a-b_c");
    }

    #[test]
    fn names_clients_would_reject_are_refused() {
        for bad in ["", "face set", "face/set", "café.set", &"x".repeat(65)] {
            assert!(matches!(checked_tool_name(bad), Err(ServerError::UnusableOp { .. })), "{bad:?}");
        }
        assert!(checked_tool_name(&"x".repeat(64)).is_ok());
    }
}
