//! Reads, as tokens, whether a crate really generates its `link()` calls (ADR 0054 section 6).
//!
//! Text search is not enough: `// include!(...)` in a comment, or an `emit()` call in a function nobody runs, would
//! look like wiring and silently drop every generated call. The lexer skips comments and strings' contents stay
//! intact, so these functions look at the code only.

use super::lex::{Tok, Token, tokens};

/// The generated file `omuzaki-link-build` writes.
const GENERATED: &str = "link_calls.rs";

/// The nesting depth of every token: how many delimiters are open in front of it.
fn depths(toks: &[Token]) -> Vec<usize> {
    let mut depth = 0usize;
    toks.iter()
        .map(|token| {
            if matches!(token.tok, Tok::Punct('}' | ')' | ']')) {
                depth = depth.saturating_sub(1);
            }
            let here = depth;
            if matches!(token.tok, Tok::Punct('{' | '(' | '[')) {
                depth += 1;
            }
            here
        })
        .collect()
}

/// Whether `main`, a top-level function of the build script, runs `omuzaki_link_build::emit(...)` unconditionally: as
/// a statement of its own body, not inside an `if`, a loop, a closure, a block or another function, and without an
/// attribute.
pub fn build_script_emits(source: &str) -> bool {
    let toks = tokens(source);
    let depth = depths(&toks);
    let Some((from, to)) = (0..toks.len())
        .find(|&at| depth[at] == 0 && toks[at].is_ident("fn") && toks.get(at + 1).is_some_and(|t| t.is_ident("main")))
        .and_then(|at| (at + 2..toks.len()).find(|&open| depth[open] == 0 && toks[open].is_punct('{')))
        .and_then(|open| group_at(&toks, open))
    else {
        return false;
    };
    (from..to).any(|at| {
        depth[at] == depth[from]
            && toks[at].is_ident("omuzaki_link_build")
            && toks[at + 1..to].starts_with_emit_call()
            // A statement starts after the body's `{`, a `;` or a block's `}`; `|| emit()` or `#[cfg(..)] emit()` do not.
            && (at == from || toks[at - 1].is_punct(';') || toks[at - 1].is_punct('}'))
    })
}

/// `:: emit (` at the start of a token slice.
trait EmitCall {
    fn starts_with_emit_call(&self) -> bool;
}

impl EmitCall for [Token] {
    fn starts_with_emit_call(&self) -> bool {
        self.len() >= 4
            && self[0].is_punct(':')
            && self[1].is_punct(':')
            && self[2].is_ident("emit")
            && self[3].is_punct('(')
    }
}

/// Whether the crate root's only `define_link!` is `define_link! { include!(concat!(env!("OUT_DIR"),
/// "/link_calls.rs")); }` and nothing else.
///
/// The file has exactly one `define_link!`, at its top level (not in a `mod`, function or any other item) with no
/// attribute on it, so it defines the exported `link()`. Its contents are the bare include and an optional `;`:
/// nothing else at all, so a closure, a block or a condition around the include cannot hide it.
pub fn library_includes_generated(source: &str) -> bool {
    let toks = tokens(source);
    let depth = depths(&toks);
    let invocations: Vec<usize> = (0..toks.len())
        .filter(|&at| toks[at].is_ident("define_link") && toks.get(at + 1).is_some_and(|t| t.is_punct('!')))
        .collect();
    let [at] = invocations[..] else { return false };
    depth[at] == 0
        && !attribute_before(&toks, at)
        && group_at(&toks, at + 2).is_some_and(|(from, to)| {
            let inside = &toks[from..to];
            is_bare_include(inside)
                && matches!(inside.len(), 15 | 16)
                && (inside.len() == 15 || inside[15].is_punct(';'))
        })
}

/// Whether the tokens before the path that ends at `at` end an attribute (`#[cfg(...)]`).
fn attribute_before(toks: &[Token], at: usize) -> bool {
    let mut start = at;
    // Walk back over `omuzaki_core ::`.
    while start >= 3 && toks[start - 1].is_punct(':') && toks[start - 2].is_punct(':') {
        start -= 3;
    }
    start > 0 && toks[start - 1].is_punct(']')
}

/// Whether the tokens start with `include ! ( concat ! ( env ! ( "OUT_DIR" ) , "/link_calls.rs" ) )`, optionally
/// followed by `;`.
fn is_bare_include(toks: &[Token]) -> bool {
    let shape: [&dyn Fn(&Token) -> bool; 15] = [
        &|t| t.is_ident("include"),
        &|t| t.is_punct('!'),
        &|t| t.is_punct('('),
        &|t| t.is_ident("concat"),
        &|t| t.is_punct('!'),
        &|t| t.is_punct('('),
        &|t| t.is_ident("env"),
        &|t| t.is_punct('!'),
        &|t| t.is_punct('('),
        &|t| matches!(&t.tok, Tok::Literal(text) if text.trim_matches('"') == "OUT_DIR"),
        &|t| t.is_punct(')'),
        &|t| t.is_punct(','),
        &|t| matches!(&t.tok, Tok::Literal(text) if text.trim_matches('"').trim_start_matches('/') == GENERATED),
        &|t| t.is_punct(')'),
        &|t| t.is_punct(')'),
    ];
    toks.len() >= 15 && shape.iter().zip(toks).all(|(test, token)| test(token))
}

/// The tokens strictly inside the delimited group that opens at `start`, as a half-open range; `None` when the token
/// there opens nothing or the group is never closed.
fn group_at(toks: &[Token], start: usize) -> Option<(usize, usize)> {
    let Tok::Punct(open @ ('{' | '(' | '[')) = toks.get(start)?.tok else { return None };
    let close = match open {
        '{' => '}',
        '(' => ')',
        _ => ']',
    };
    let mut depth = 0usize;
    for (at, token) in toks.iter().enumerate().skip(start) {
        if token.is_punct(open) {
            depth += 1;
        } else if token.is_punct(close) {
            depth -= 1;
            if depth == 0 {
                return Some((start + 1, at));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_script_emits_when_main_calls_the_generator() {
        assert!(build_script_emits("fn main() {\n    omuzaki_link_build::emit();\n}\n"));
        assert!(build_script_emits("fn main() -> Result<(), E> {\n    omuzaki_link_build::emit();\n    Ok(())\n}\n"));
    }

    #[test]
    fn emit_must_run_unconditionally_from_main() {
        for body in [
            "if false { omuzaki_link_build::emit(); }",
            "let _ = || omuzaki_link_build::emit();",
            "for _ in 0..0 { omuzaki_link_build::emit(); }",
            "{ omuzaki_link_build::emit(); }",
            "#[cfg(any())]\n    omuzaki_link_build::emit();",
            "let f = |_| omuzaki_link_build::emit();",
        ] {
            assert!(!build_script_emits(&format!("fn main() {{\n    {body}\n}}\n")), "{body}");
        }
        assert!(build_script_emits("fn main() {\n    println!(\"x\");\n    omuzaki_link_build::emit();\n}\n"));
        assert!(!build_script_emits("mod m { fn main() { omuzaki_link_build::emit(); } }\nfn other() {}\n"));
    }

    #[test]
    fn exactly_one_define_link_with_only_the_include_counts() {
        let include = "include!(concat!(env!(\"OUT_DIR\"), \"/link_calls.rs\"))";
        assert!(library_includes_generated(&format!("omuzaki_core::define_link! {{ {include}; }}\n")));
        assert!(library_includes_generated(&format!("omuzaki_core::define_link! {{ {include} }}\n")));
        for other in [
            // A disabled nested definition beside an empty root one.
            format!(
                "#[cfg(any())]\nmod off {{ omuzaki_core::define_link! {{ {include}; }} }}\nomuzaki_core::define_link!();\n"
            ),
            // The root one is wired but a second definition exists.
            format!("omuzaki_core::define_link! {{ {include}; }}\nmod m {{ omuzaki_core::define_link!(); }}\n"),
            format!("omuzaki_core::define_link! {{ {include}; other::link(); }}\n"),
            format!("omuzaki_core::define_link! {{ let _ = || {include}; }}\n"),
            format!("omuzaki_core::define_link! {{ {include}; {include}; }}\n"),
        ] {
            assert!(!library_includes_generated(&other), "{other}");
        }
    }

    #[test]
    fn the_define_link_must_be_the_crate_roots_own() {
        let include = "include!(concat!(env!(\"OUT_DIR\"), \"/link_calls.rs\"));";
        for wrapped in [
            format!(
                "#[cfg(any())]\nmod off {{\n    omuzaki_core::define_link! {{ {include} }}\n}}\nomuzaki_core::define_link!();\n"
            ),
            format!("mod inner {{\n    omuzaki_core::define_link! {{ {include} }}\n}}\n"),
            format!("fn f() {{\n    omuzaki_core::define_link! {{ {include} }}\n}}\n"),
            format!("omuzaki_core::define_link! {{\n    let _ = || {include}\n}}\n"),
        ] {
            assert!(!library_includes_generated(&wrapped), "{wrapped}");
        }
    }

    #[test]
    fn a_comment_or_another_function_is_not_a_call_from_main() {
        assert!(!build_script_emits("fn main() {\n    // omuzaki_link_build::emit();\n}\n"));
        assert!(!build_script_emits("fn main() {}\nfn unused() { omuzaki_link_build::emit(); }\n"));
        assert!(!build_script_emits("/* fn main() { omuzaki_link_build::emit(); } */\nfn helper() {}\n"));
    }

    #[test]
    fn the_include_counts_inside_define_link_only() {
        let wired = "omuzaki_core::define_link! {\n    include!(concat!(env!(\"OUT_DIR\"), \"/link_calls.rs\"));\n}\n";
        assert!(library_includes_generated(wired));
        let commented =
            "omuzaki_core::define_link! {\n    // include!(concat!(env!(\"OUT_DIR\"), \"/link_calls.rs\"));\n}\n";
        assert!(!library_includes_generated(commented));
        let outside = "include!(concat!(env!(\"OUT_DIR\"), \"/link_calls.rs\"));\nomuzaki_core::define_link!();\n";
        assert!(!library_includes_generated(outside));
        let other_file = "omuzaki_core::define_link! { include!(concat!(env!(\"OUT_DIR\"), \"/other.rs\")); }\n";
        assert!(!library_includes_generated(other_file));
    }

    #[test]
    fn only_the_exact_bare_include_counts() {
        let wrap = |inner: &str| format!("omuzaki_core::define_link! {{\n    {inner}\n}}\n");
        assert!(library_includes_generated(&wrap("include!(concat!(env!(\"OUT_DIR\"), \"/link_calls.rs\"));")));
        for other in [
            "include!(concat!(env!(\"OUT_DIR\"), \"/link_calls.rs\", \"x\"));",
            "include!(concat!(env!(\"MANIFEST\"), \"/link_calls.rs\"));",
            "include!(\"link_calls.rs\");",
            "include!(concat!(env!(\"OUT_DIR\"), \"/other_link_calls.rs\"));",
        ] {
            assert!(!library_includes_generated(&wrap(other)), "{other}");
        }
    }

    #[test]
    fn a_conditional_or_nested_include_is_not_wiring() {
        let include = "include!(concat!(env!(\"OUT_DIR\"), \"/link_calls.rs\"));";
        for wrapped in [
            format!("omuzaki_core::define_link! {{\n    #[cfg(any())]\n    {include}\n}}\n"),
            format!("omuzaki_core::define_link! {{\n    #[cfg(feature = \"x\")]\n    {include}\n}}\n"),
            format!("omuzaki_core::define_link! {{\n    #[cfg(feature = \"x\")]\n    {{ {include} }}\n}}\n"),
            format!("#[cfg(any())]\nomuzaki_core::define_link! {{\n    {include}\n}}\n"),
            format!("#[cfg(any())]\ndefine_link! {{\n    {include}\n}}\n"),
            format!("omuzaki_core::define_link! {{\n    if false {{ {include} }}\n}}\n"),
        ] {
            assert!(!library_includes_generated(&wrapped), "{wrapped}");
        }
    }
}
