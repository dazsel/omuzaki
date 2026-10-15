//! `#[cfg(...)]` predicates as the scan reads them, and their value for the build the gate checks.
//!
//! A registration under `#[cfg(test)]` is not in an App binary, so the gate must not expect it. The predicates the scan
//! can decide are evaluated here. Anything else, cargo features above all, it can't decide from source: the value
//! depends on cargo's feature resolution for the build. Such a predicate evaluates to `None`, and the scan stops with
//! an error naming the registration, rather than guessing.
//!
//! Decided here:
//! - `test` is false: an App links its dependencies' libraries, which are never built with `--test`.
//! - `debug_assertions`, `unix`, `windows`, `target_os`, `target_family`, `target_arch`, `target_pointer_width` and
//!   `target_endian` take their value from a [`Target`]. Inside an App's link-gate test that is the platform and profile
//!   the test was compiled for ([`Target::host`]), exactly the binary under test; for `cargo xtask registry` it is the
//!   host's dev build. `cargo xtask wasm-link-proof` passes the WebAssembly target instead, because the host's
//!   options say nothing about what a module for another target compiles.
//! - `not`, `any` and `all` combine with three-valued logic, so `all(test, feature = "x")` is false without knowing the
//!   feature, while `any(test, feature = "x")` is undecided.

use std::env::consts;
use std::fmt;

use super::lex::{Tok, Token};

/// The configuration of the build a `cfg` is evaluated for: the target options rustc sets, and whether the profile has
/// `debug_assertions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    os: &'static str,
    arch: &'static str,
    /// `target_family` values; `unix` and `windows` are also bare options when listed.
    families: &'static [&'static str],
    pointer_width: u32,
    endian: &'static str,
    debug_assertions: bool,
}

impl Target {
    /// The platform and profile this code was compiled for.
    pub fn host() -> Self {
        Self {
            os: consts::OS,
            arch: consts::ARCH,
            families: if cfg!(unix) {
                &["unix"]
            } else if cfg!(windows) {
                &["windows"]
            } else if cfg!(target_family = "wasm") {
                &["wasm"]
            } else {
                &[]
            },
            pointer_width: usize::BITS,
            endian: if cfg!(target_endian = "big") { "big" } else { "little" },
            debug_assertions: cfg!(debug_assertions),
        }
    }

    /// `wasm32-unknown-unknown`, the target the web player ships on: neither `unix` nor `windows`. `debug_assertions`
    /// is the profile's (on in dev, off in release), which the host's value says nothing about.
    pub fn wasm32_unknown_unknown(debug_assertions: bool) -> Self {
        Self {
            os: "unknown",
            arch: "wasm32",
            families: &["wasm"],
            pointer_width: 32,
            endian: "little",
            debug_assertions,
        }
    }

    fn has_family(&self, family: &str) -> bool {
        self.families.contains(&family)
    }
}

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

    /// Whether the predicate holds for a build of `target`; `None` when the scan can't tell.
    pub fn eval(&self, target: &Target) -> Option<bool> {
        match self {
            Self::Flag(flag) => match flag.as_str() {
                "test" => Some(false),
                "debug_assertions" => Some(target.debug_assertions),
                "unix" | "windows" => Some(target.has_family(flag)),
                _ => None,
            },
            Self::KeyValue(key, value) => match key.as_str() {
                "target_os" => Some(value == target.os),
                "target_arch" => Some(value == target.arch),
                "target_pointer_width" => Some(*value == target.pointer_width.to_string()),
                "target_endian" => Some(value == target.endian),
                "target_family" => {
                    matches!(value.as_str(), "unix" | "windows" | "wasm").then(|| target.has_family(value))
                }
                _ => None,
            },
            Self::Not(inner) => inner.eval(target).map(|value| !value),
            Self::Any(options) => three_valued(options, true, target),
            Self::All(options) => three_valued(options, false, target),
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
            Self::KeyValue(key, _) => matches!(
                key.as_str(),
                "target_os" | "target_family" | "target_arch" | "target_pointer_width" | "target_endian"
            )
            .then_some(true),
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
fn three_valued(options: &[Cfg], decisive: bool, target: &Target) -> Option<bool> {
    let mut undecided = false;
    for option in options {
        match option.eval(target) {
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

    fn host() -> Target {
        Target::host()
    }

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
        assert_eq!(cfg("test").eval(&host()), Some(false));
        assert_eq!(cfg("not(test)").eval(&host()), Some(true));
        assert_eq!(cfg("feature = \"wgpu\"").eval(&host()), None);
        assert_eq!(cfg("doc").eval(&host()), None);
    }

    #[test]
    fn combinators_use_three_valued_logic() {
        assert_eq!(cfg("all(test, feature = \"x\")").eval(&host()), Some(false));
        assert_eq!(cfg("any(test, feature = \"x\")").eval(&host()), None);
        assert_eq!(cfg("any(not(test), feature = \"x\")").eval(&host()), Some(true));
        assert_eq!(cfg("any(test, debug_assertions)").eval(&host()), Some(cfg!(debug_assertions)));
        assert_eq!(cfg("all()").eval(&host()), Some(true));
        assert_eq!(cfg("any()").eval(&host()), Some(false));
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
        assert_eq!(cfg(&format!("target_os = \"{}\"", consts::OS)).eval(&host()), Some(true));
        assert_eq!(cfg("target_os = \"plan9\"").eval(&host()), Some(false));
        assert_eq!(cfg("unix").eval(&host()), Some(cfg!(unix)));
        assert_eq!(cfg(&format!("target_pointer_width = \"{}\"", usize::BITS)).eval(&host()), Some(true));
    }

    #[test]
    fn wasm32_unknown_unknown_is_neither_unix_nor_windows() {
        let wasm = Target::wasm32_unknown_unknown(true);
        for text in [
            "target_arch = \"wasm32\"",
            "target_os = \"unknown\"",
            "target_family = \"wasm\"",
            "target_pointer_width = \"32\"",
            "target_endian = \"little\"",
            "debug_assertions",
            "not(unix)",
            "not(windows)",
            "not(target_family = \"unix\")",
        ] {
            assert_eq!(cfg(text).eval(&wasm), Some(true), "{text}");
        }
        for text in ["unix", "windows", "target_os = \"linux\"", "target_arch = \"x86_64\"", "target_endian = \"big\""]
        {
            assert_eq!(cfg(text).eval(&wasm), Some(false), "{text}");
        }
        assert_eq!(cfg("debug_assertions").eval(&Target::wasm32_unknown_unknown(false)), Some(false));
        // Not a target option the scan knows.
        assert_eq!(cfg("target_family = \"itanium\"").eval(&wasm), None);
    }
}
