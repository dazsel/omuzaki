//! Placeholders in recipe arguments (design D3).
//!
//! A string argument that is *exactly* a placeholder is replaced by a typed value at apply time. A placeholder is
//! never spliced into a longer string, and a literal string that starts with `$` is written `$$...`.

use serde_json::Value;

/// A whole-value placeholder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Placeholder {
    /// `$character`
    Character,
    /// `$scene`
    Scene,
    /// `$target`
    Target,
    /// `$name`
    Name,
    /// `$template`
    Template,
    /// `$parts.<key>`
    Part(String),
    /// `$params.<key>`
    Param(String),
    /// `$assets.<key>`
    Asset(String),
}

/// What a string argument is.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Classified {
    /// A literal. A leading `$$` has already been unescaped.
    Literal(String),
    /// A whole-value placeholder.
    Placeholder(Placeholder),
}

/// Why a string argument is not allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Misuse {
    /// A placeholder sits inside a longer string.
    Embedded,
    /// Starts with `$` but is neither a placeholder nor `$$`.
    Unknown,
}

impl Misuse {
    /// The sentence `check` and `expand` report.
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::Embedded => "a placeholder must be the whole value, never part of a longer string",
            Self::Unknown => "a string starting with `$` must be a placeholder; write `$$` for a literal `$`",
        }
    }
}

/// The root words after `$`. Those ending in `.` take a key.
const ROOTS: [&str; 8] = ["character", "scene", "target", "name", "template", "parts.", "params.", "assets."];

/// A key (`tail_length`, `hair-2`): lowercase letters, digits, `_` and `-`.
pub(crate) fn is_key(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn mentions_placeholder(s: &str) -> bool {
    s.match_indices('$').any(|(i, _)| ROOTS.iter().any(|root| s[i + 1..].starts_with(root)))
}

/// Classifies one string argument.
pub(crate) fn classify(s: &str) -> Result<Classified, Misuse> {
    if let Some(rest) = s.strip_prefix("$$") {
        return Ok(Classified::Literal(format!("${rest}")));
    }
    let Some(body) = s.strip_prefix('$') else {
        return if mentions_placeholder(s) { Err(Misuse::Embedded) } else { Ok(Classified::Literal(s.to_owned())) };
    };
    let whole = match body {
        "character" => Some(Placeholder::Character),
        "scene" => Some(Placeholder::Scene),
        "target" => Some(Placeholder::Target),
        "name" => Some(Placeholder::Name),
        "template" => Some(Placeholder::Template),
        _ => body.split_once('.').and_then(|(root, key)| {
            let key = is_key(key).then(|| key.to_owned())?;
            match root {
                "parts" => Some(Placeholder::Part(key)),
                "params" => Some(Placeholder::Param(key)),
                "assets" => Some(Placeholder::Asset(key)),
                _ => None,
            }
        }),
    };
    match whole {
        Some(p) => Ok(Classified::Placeholder(p)),
        None if mentions_placeholder(s) => Err(Misuse::Embedded),
        None => Err(Misuse::Unknown),
    }
}

/// Calls `visit(path, string)` for every string inside `value`, with the path joined by `/` under `base`
/// (`step/2/args/params/groups`). Object keys are not visited: keys are literals.
pub(crate) fn walk_strings(value: &Value, base: &str, visit: &mut impl FnMut(&str, &str)) {
    match value {
        Value::String(s) => visit(base, s),
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                walk_strings(v, &format!("{base}/{i}"), visit);
            }
        }
        Value::Object(map) => {
            for (k, v) in map {
                walk_strings(v, &format!("{base}/{k}"), visit);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_value_placeholders_are_recognised() {
        use Placeholder::*;
        let ph = |s| classify(s).unwrap();
        assert_eq!(ph("$character"), Classified::Placeholder(Character));
        assert_eq!(ph("$parts.hair-2"), Classified::Placeholder(Part("hair-2".into())));
        assert_eq!(ph("$params.tail_length"), Classified::Placeholder(Param("tail_length".into())));
        assert_eq!(ph("$assets.decal"), Classified::Placeholder(Asset("decal".into())));
        assert_eq!(ph("$template"), Classified::Placeholder(Template));
    }

    #[test]
    fn literals_and_the_dollar_escape() {
        assert_eq!(classify("#2a2346").unwrap(), Classified::Literal("#2a2346".into()));
        assert_eq!(classify("@outfit_a").unwrap(), Classified::Literal("@outfit_a".into()));
        assert_eq!(classify("$$character").unwrap(), Classified::Literal("$character".into()));
        assert_eq!(classify("costs $5 today").unwrap(), Classified::Literal("costs $5 today".into()));
    }

    #[test]
    fn a_placeholder_inside_a_longer_string_is_refused() {
        for s in ["prefix-$params.x", "$character-2", "$params.x and more", "a $name b", "$parts."] {
            assert_eq!(classify(s), Err(Misuse::Embedded), "{s}");
        }
    }

    #[test]
    fn an_unknown_dollar_string_is_refused() {
        for s in ["$5", "$nope", "$params"] {
            assert_eq!(classify(s), Err(Misuse::Unknown), "{s}");
        }
    }

    #[test]
    fn walking_visits_nested_strings_with_paths() {
        let v = serde_json::json!({"a": ["x", {"b": "y"}], "n": 1, "c": "z"});
        let mut seen = Vec::new();
        walk_strings(&v, "args", &mut |p, s| seen.push(format!("{p}={s}")));
        seen.sort();
        assert_eq!(seen, ["args/a/0=x", "args/a/1/b=y", "args/c=z"]);
    }
}
