//! A macro written out as a script (#1221, `scripting.md` section 7, "Save as script"): one
//! `ops::<area>::<verb>(#{ ... });` line per step with its arguments as literals, and the characters the macro edits
//! lifted into `let` bindings at the top, so the script is easy to point at another character. The source passes
//! `script.check` and replays to the same document (tested in `tests/script_console.rs`).

use std::fmt::Write as _;

use omuzaki_script::Macro;
use serde_json::Value;

/// The argument that names the character a step edits.
const CHARACTER: &str = "character";

/// The Rhai source of `m`; `spell` is [`omuzaki_script::script_call`]. A step whose op has no script spelling is
/// written as a comment, so the source still checks and says what it left out.
pub fn macro_source(m: &Macro, spell: impl Fn(&str) -> Option<String>) -> String {
    let mut characters: Vec<String> = Vec::new();
    for step in &m.steps {
        if let Some(Value::String(id)) = step.args.get(CHARACTER)
            && !characters.contains(id)
        {
            characters.push(id.clone());
        }
    }
    let binding =
        |i: usize| if characters.len() == 1 { CHARACTER.to_owned() } else { format!("{CHARACTER}_{}", i + 1) };
    let mut out = String::new();
    let _ = writeln!(out, "// {}", one_line(&m.name));
    for (i, id) in characters.iter().enumerate() {
        let _ = writeln!(out, "let {} = {};", binding(i), string(id));
    }
    if !characters.is_empty() {
        out.push('\n');
    }
    for step in &m.steps {
        let Some(call) = spell(&step.op) else {
            let _ = writeln!(out, "// {}: {}", step.op, one_line(&step.args.to_string()));
            continue;
        };
        let args = match &step.args {
            Value::Object(map) => {
                let fields: Vec<String> = map
                    .iter()
                    .map(|(key, value)| {
                        let value = match value {
                            Value::String(id) if key == CHARACTER => {
                                characters.iter().position(|c| c == id).map_or_else(|| literal(value), binding)
                            }
                            other => literal(other),
                        };
                        format!("{}: {value}", key_literal(key))
                    })
                    .collect();
                format!("#{{ {} }}", fields.join(", "))
            }
            other => literal(other),
        };
        let _ = writeln!(out, "{call}({args});");
    }
    out
}

/// `value` as a Rhai literal.
fn literal(value: &Value) -> String {
    match value {
        Value::Null => "()".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => i.to_string(),
            // Debug keeps a float a float: `1.0`, `0.5`, `1e16`.
            (None, Some(f)) => format!("{f:?}"),
            (None, None) => n.to_string(),
        },
        Value::String(s) => string(s),
        Value::Array(items) => format!("[{}]", items.iter().map(literal).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => {
            let fields: Vec<String> = map.iter().map(|(k, v)| format!("{}: {}", key_literal(k), literal(v))).collect();
            format!("#{{ {} }}", fields.join(", "))
        }
    }
}

/// A map key: bare when it is a plain name, quoted otherwise.
fn key_literal(key: &str) -> String {
    let plain = key.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain { key.to_owned() } else { string(key) }
}

/// A string literal with Rhai's escapes.
fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `text` on one line, for a comment.
fn one_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

#[cfg(test)]
mod tests {
    use omuzaki_script::RecordedCall;
    use serde_json::json;

    use super::*;

    fn spell(name: &str) -> Option<String> {
        name.split_once('.').map(|(a, v)| format!("ops::{a}::{v}"))
    }

    #[test]
    fn steps_become_calls_with_the_character_lifted() {
        let m = Macro {
            name: "oval and blush".into(),
            steps: vec![
                RecordedCall {
                    op: "face.set_profile".into(),
                    args: json!({ "character": "aki", "profile": "egg-oval", "blend": 0.5 }),
                },
                RecordedCall {
                    op: "face.set_blush".into(),
                    args: json!({ "character": "aki", "amount": 1.0, "tags": ["a\"b"], "two words": null }),
                },
                RecordedCall { op: "plain".into(), args: json!({ "x": 1 }) },
            ],
        };
        let source = macro_source(&m, spell);
        assert_eq!(
            source,
            "// oval and blush\nlet character = \"aki\";\n\n\
             ops::face::set_profile(#{ blend: 0.5, character: character, profile: \"egg-oval\" });\n\
             ops::face::set_blush(#{ amount: 1.0, character: character, tags: [\"a\\\"b\"], \"two words\": () });\n\
             // plain: {\"x\":1}\n"
        );
    }

    #[test]
    fn two_characters_get_numbered_bindings() {
        let m = Macro {
            name: "both".into(),
            steps: vec![
                RecordedCall { op: "a.b".into(), args: json!({ "character": "aki" }) },
                RecordedCall { op: "a.b".into(), args: json!({ "character": "ren" }) },
            ],
        };
        let source = macro_source(&m, spell);
        assert!(source.contains("let character_1 = \"aki\";\nlet character_2 = \"ren\";"), "{source}");
        assert!(source.contains("ops::a::b(#{ character: character_2 });"), "{source}");
    }
}
