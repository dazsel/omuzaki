//! `#[cfg(...)]` predicates as the scan reads them, and their value for the build the gate checks.
//!
//! A registration under `#[cfg(test)]` is not in an App binary, so the gate must not expect it. The predicates the scan
//! can decide are evaluated here. Anything else, cargo features above all, it can't decide from source: the value
//! depends on cargo's feature resolution for the build. Such a predicate evaluates to `None`, and the scan stops with
//! an error naming the registration, rather than guessing.
//!
//! Decided here:
//! - `test` is false: an App links its dependencies' libraries, which are never built with `--test`.
//! - `debug_assertions`, `unix`, `windows`, `target_os`, `target_family`, `target_arch` and `target_pointer_width`
//!   take their value for the platform and profile this code was compiled for. Inside an App's link-gate test that is
//!   exactly the binary under test; for `cargo xtask registry` it is the host's dev build.
//! - `not`, `any` and `all` combine with three-valued logic, so `all(test, feature = "x")` is false without knowing the
//!   feature, while `any(test, feature = "x")` is undecided.

use std::env::consts;
use std::fmt;

use super::lex::{Tok, Token};

/// A `cfg` predicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cfg {
    /// A bare option: `test`, `unix`.
    Flag(String),
    /// `key = "value"`: `feature = "wgpu"`, `target_os = "linux"`.
    KeyValue(String, String),
    /// `not(p)`.
    Not(Box<Cfg>),
    /// `any(p, ...)`.
    Any(Vec<Cfg>),
    /// `all(p, ...)`.
    All(Vec<Cfg>),
    /// Something the scan could not read, kept as written so the error can show it.
    Unreadable(String),
}

impl Cfg {
    /// The predicate inside a `cfg(...)` attribute, from the tokens between the parentheses.
    pub fn parse(toks: &[Token]) -> Self {
        let mut at = 0;
        match parse_predicate(toks, &mut at) {
            Some(cfg) if at == toks.len() => cfg,
            _ => Self::Unreadable(toks.iter().map(Token::text).collect::<Vec<_>>().join(" ")),
        }
    }

    /// Whether the predicate holds for the build being checked; `None` when the scan can't tell.
    pub fn eval(&self) -> Option<bool> {
        match self {
            Self::Flag(flag) => match flag.as_str() {
                "test" => Some(false),
                "debug_assertions" => Some(cfg!(debug_assertions)),
                "unix" => Some(cfg!(unix)),
                "windows" => Some(cfg!(windows)),
                _ => None,
            },
            Self::KeyValue(key, value) => match key.as_str() {
                "target_os" => Some(value == consts::OS),
                "target_arch" => Some(value == consts::ARCH),
                "target_pointer_width" => Some(*value == usize::BITS.to_string()),
                "target_family" => match value.as_str() {
                    "unix" => Some(cfg!(unix)),
                    "windows" => Some(cfg!(windows)),
                    "wasm" => Some(cfg!(target_family = "wasm")),
                    _ => None,
                },
                _ => None,
            },
            Self::Not(inner) => inner.eval().map(|value| !value),
            Self::Any(options) => three_valued(options, true),
            Self::All(options) => three_valued(options, false),
            Self::Unreadable(_) => None,
        }
    }
}

impl Cfg {
    /// Whether the predicate can be `want` on some target a build could ship for, ignoring which one this is: target
    /// options (`unix`, `target_os`, `target_arch`, ...) and `debug_assertions` can go either way, `test` is never true
    /// in a shipped library, and anything else (a cargo feature) is `None`. `lint-registers` asks it with `want = true`,
    /// so a registration behind `cfg(target_arch = "wasm32")` still counts on a Linux host.
    ///
    /// Predicates are treated as independent, so `all(unix, windows)` can hold here: the answer errs towards "ships",
    /// which is the safe side for a lint that demands a flag.
    pub fn can_be(&self, want: bool) -> Option<bool> {
        match self {
            Self::Flag(flag) => match flag.as_str() {
                "test" => Some(!want),
                "debug_assertions" | "unix" | "windows" => Some(true),
                _ => None,
            },
            Self::KeyValue(key, _) => {
                matches!(key.as_str(), "target_os" | "target_family" | "target_arch" | "target_pointer_width")
                    .then_some(true)
            }
            Self::Not(inner) => inner.can_be(!want),
            Self::Any(options) => reachable(options, want, want),
            Self::All(options) => reachable(options, want, !want),
            Self::Unreadable(_) => None,
        }
    }
}

/// `can_be` of `any`/`all`: the group can be `want` if one option can (`one_suffices`) or every option can.
fn reachable(options: &[Cfg], want: bool, one_suffices: bool) -> Option<bool> {
    let mut undecided = false;
    for option in options {
        match option.can_be(want) {
            Some(value) if value == one_suffices => return Some(one_suffices),
            Some(_) => {}
            None => undecided = true,
        }
    }
    if undecided { None } else { Some(!one_suffices) }
}

/// `any` (`decisive = true`) or `all` (`decisive = false`): one decisive value settles it; otherwise any undecided
/// option leaves the whole undecided.
fn three_valued(options: &[Cfg], decisive: bool) -> Option<bool> {
    let mut undecided = false;
    for option in options {
        match option.eval() {
            Some(value) if value == decisive => return Some(decisive),
            Some(_) => {}
            None => undecided = true,
        }
    }
    if undecided { None } else { Some(!decisive) }
}

fn parse_predicate(toks: &[Token], at: &mut usize) -> Option<Cfg> {
    let Tok::Ident(name) = &toks.get(*at)?.tok else { return None };
    *at += 1;
    match toks.get(*at).map(|t| &t.tok) {
        Some(Tok::Punct('=')) => {
            let Tok::Literal(value) = &toks.get(*at + 1)?.tok else { return None };
            *at += 2;
            Some(Cfg::KeyValue(name.clone(), value.trim_matches('"').to_owned()))
        }
        Some(Tok::Punct('(')) if matches!(name.as_str(), "any" | "all" | "not") => {
            *at += 1;
            let mut options = Vec::new();
            while !toks.get(*at)?.is_punct(')') {
                options.push(parse_predicate(toks, at)?);
                if toks.get(*at)?.is_punct(',') {
                    *at += 1;
                }
            }
            *at += 1;
            match name.as_str() {
                "any" => Some(Cfg::Any(options)),
                "all" => Some(Cfg::All(options)),
                _ if options.len() == 1 => options.pop().map(|inner| Cfg::Not(Box::new(inner))),
                _ => None,
            }
        }
        _ => Some(Cfg::Flag(name.clone())),
    }
}

impl fmt::Display for Cfg {
    /// The predicate as it would be written in `cfg(...)`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let list = |f: &mut fmt::Formatter<'_>, name: &str, options: &[Cfg]| {
            let inner: Vec<String> = options.iter().map(ToString::to_string).collect();
            write!(f, "{name}({})", inner.join(", "))
        };
        match self {
            Self::Flag(flag) => f.write_str(flag),
            Self::KeyValue(key, value) => write!(f, "{key} = \"{value}\""),
            Self::Not(inner) => write!(f, "not({inner})"),
            Self::Any(options) => list(f, "any", options),
            Self::All(options) => list(f, "all", options),
            Self::Unreadable(text) => f.write_str(text),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::lex::tokens;

    fn cfg(text: &str) -> Cfg {
        Cfg::parse(&tokens(text))
    }

    #[test]
    fn predicates_parse_and_print_back() {
        for text in ["test", "feature = \"wgpu\"", "not(test)", "any(test, feature = \"x\")", "all(unix, not(test))"] {
            assert_eq!(cfg(text).to_string(), text);
        }
        assert!(matches!(cfg("not(a, b)"), Cfg::Unreadable(_)));
        assert!(matches!(cfg("feature ="), Cfg::Unreadable(_)));
    }

    #[test]
    fn test_is_false_and_features_are_undecided() {
        assert_eq!(cfg("test").eval(), Some(false));
        assert_eq!(cfg("not(test)").eval(), Some(true));
        assert_eq!(cfg("feature = \"wgpu\"").eval(), None);
        assert_eq!(cfg("doc").eval(), None);
    }

    #[test]
    fn combinators_use_three_valued_logic() {
        assert_eq!(cfg("all(test, feature = \"x\")").eval(), Some(false));
        assert_eq!(cfg("any(test, feature = \"x\")").eval(), None);
        assert_eq!(cfg("any(not(test), feature = \"x\")").eval(), Some(true));
        assert_eq!(cfg("any(test, debug_assertions)").eval(), Some(cfg!(debug_assertions)));
        assert_eq!(cfg("all()").eval(), Some(true));
        assert_eq!(cfg("any()").eval(), Some(false));
    }

    #[test]
    fn every_target_branch_counts_but_test_code_does_not() {
        for text in ["unix", "windows", "target_arch = \"wasm32\"", "not(unix)", "target_os = \"plan9\"", "not(test)"] {
            assert_eq!(cfg(text).can_be(true), Some(true), "{text}");
        }
        for text in ["test", "all(test, unix)", "not(not(test))"] {
            assert_eq!(cfg(text).can_be(true), Some(false), "{text}");
        }
        assert_eq!(cfg("any(test, target_arch = \"wasm32\")").can_be(true), Some(true));
        assert_eq!(cfg("feature = \"x\"").can_be(true), None);
        assert_eq!(cfg("all(test, feature = \"x\")").can_be(true), Some(false));
    }

    #[test]
    fn target_options_are_those_of_this_build() {
        assert_eq!(cfg(&format!("target_os = \"{}\"", consts::OS)).eval(), Some(true));
        assert_eq!(cfg("target_os = \"plan9\"").eval(), Some(false));
        assert_eq!(cfg("unix").eval(), Some(cfg!(unix)));
        assert_eq!(cfg(&format!("target_pointer_width = \"{}\"", usize::BITS)).eval(), Some(true));
    }
}
