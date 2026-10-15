//! Reads, as tokens, whether a crate really generates its `link()` calls (ADR 0054 section 6).
//!
//! Text search is not enough: `// include!(...)` in a comment, or an `emit()` call in a function nobody runs, would
//! look like wiring and silently drop every generated call. The lexer skips comments and strings' contents stay
//! intact, so these functions look at the code only.

use super::lex::{Tok, Token, tokens};

/// The generated file `omuzaki-link-build` writes.
const GENERATED: &str = "link_calls.rs";

/// Whether the build script's `fn main` calls `omuzaki_link_build::emit(...)`.
pub fn build_script_emits(source: &str) -> bool {
    let toks = tokens(source);
    let Some(body) = (0..toks.len())
        .find(|&at| toks[at].is_ident("fn") && toks.get(at + 1).is_some_and(|t| t.is_ident("main")))
        .and_then(|at| (at + 2..toks.len()).find(|&open| toks[open].is_punct('{')))
        .and_then(|open| group_at(&toks, open))
    else {
        return false;
    };
    toks[body.0..body.1].windows(5).any(|w| {
        w[0].is_ident("omuzaki_link_build")
            && w[1].is_punct(':')
            && w[2].is_punct(':')
            && w[3].is_ident("emit")
            && w[4].is_punct('(')
    })
}

/// Whether the library's `define_link!` invocation contains `include!(... "link_calls.rs" ...)`.
pub fn library_includes_generated(source: &str) -> bool {
    let toks = tokens(source);
    (0..toks.len()).any(|at| {
        toks[at].is_ident("define_link")
            && toks.get(at + 1).is_some_and(|t| t.is_punct('!'))
            && group_at(&toks, at + 2).is_some_and(|(from, to)| {
                toks[from..to].windows(3).enumerate().any(|(i, w)| {
                    w[0].is_ident("include")
                        && w[1].is_punct('!')
                        && w[2].is_punct('(')
                        && toks[from + i..to]
                            .iter()
                            .any(|t| matches!(&t.tok, Tok::Literal(text) if text.contains(GENERATED)))
                })
            })
    })
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
}
