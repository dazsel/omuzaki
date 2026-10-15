//! What one source file says about self-registration: the `register_*!` macros it defines, the registrations it
//! makes and the `cfg`s they sit under, the `mod` declarations that lead to other files, whether it defines the crate's
//! `link()` and which crates' `link()` it calls.

use super::cfg::Cfg;
use super::lex::{Tok, Token, tokens};

/// The prefix every self-registration macro's name starts with (`register_op!`, `register_part_kind!`, ...).
pub const MACRO_PREFIX: &str = "register_";

/// The `omuzaki-core` macro that defines a crate's `link()` (ADR 0015).
pub const LINK_MACRO: &str = "define_link";

/// A `cfg` predicate and the line of the attribute that states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgAt {
    /// The predicate.
    pub cfg: Cfg,
    /// The attribute's line.
    pub line: u32,
}

/// One `register_<kind>!(<name>, ...)` call at item level: the top level of the file or of an inline `mod` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    /// The macro's name without `register_`: the registry it adds to.
    pub kind: String,
    /// The first argument as written, without spaces or a leading `label:` (`CharacterCreate`, `"button"`).
    pub name: String,
    /// The line the macro name is on, which is what `line!()` reports inside the expansion.
    pub line: u32,
    /// Every `cfg` the call is compiled under within this file: its own attributes and its inline modules'.
    pub cfgs: Vec<CfgAt>,
}

/// One `mod name;` declaration, which compiles another file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModDecl {
    /// The module's name.
    pub name: String,
    /// Its `#[path = "..."]`, if it has one.
    pub path: Option<String>,
    /// The inline modules it is declared in (`mod a { mod b; }` gives `["a"]`), which are directories on disk.
    pub inline: Vec<String>,
    /// Every `cfg` the declaration is compiled under within this file.
    pub cfgs: Vec<CfgAt>,
}

/// A module form the scan can't follow, so it can't say which files the form compiles. The gate refuses the file
/// rather than skip it (ADR 0015).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsupported {
    /// A `mod` inside a function, block or macro body.
    ModInBody,
    /// An `include!` other than the generated `include!(concat!(env!("OUT_DIR"), "/<name>.rs"))`, which pulls in a file
    /// the scan never reads.
    Include,
    /// A `#[cfg_attr(..., path = ...)]` on a `mod`: whether the attribute applies, and so which file it names, is not
    /// known from the source.
    CfgAttrPath,
    /// A `#[path]` on an inline `mod { }` block, which moves the directory of the modules inside it.
    PathOnInlineMod,
}

impl Unsupported {
    /// What was found and what to write instead, for the error message.
    pub fn describe(self) -> &'static str {
        match self {
            Self::ModInBody => {
                "a `mod` inside a function, block or macro body; declare it at the top level of the file or in an \
                 inline `mod` block"
            }
            Self::Include => {
                "`include!` of a file the link gate doesn't read; only the generated \
                 `include!(concat!(env!(\"OUT_DIR\"), \"/<name>.rs\"))` is allowed, so declare the modules as `mod name;`"
            }
            Self::CfgAttrPath => {
                "`#[cfg_attr(..., path = ...)]` on a `mod`; write `#[cfg(p)] #[path = \"x.rs\"] mod m;` and \
                 `#[cfg(not(p))] mod m;` as two declarations, so each build keeps its file"
            }
            Self::PathOnInlineMod => {
                "`#[path]` on an inline `mod { }` block; put the `#[path]` on a `mod name;` declaration instead"
            }
        }
    }
}

/// An [`Unsupported`] form and the line it starts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedAt {
    /// The form.
    pub form: Unsupported,
    /// The line: the `mod`, `include` or `path` it is written on.
    pub line: u32,
}

/// Everything the gate needs from one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileScan {
    /// The `register_<kind>` macros the file defines, as kinds.
    pub defines: Vec<String>,
    /// Calls of macros named `register_*`, whether or not the macro turns out to be a registration macro.
    pub calls: Vec<Call>,
    /// Its `mod name;` declarations.
    pub mods: Vec<ModDecl>,
    /// Its `#![cfg(...)]` attributes, which apply to the whole file.
    pub inner_cfgs: Vec<CfgAt>,
    /// Whether it defines `link()` at its top level: `pub fn link()` or `omuzaki_core::define_link!`.
    pub defines_link: bool,
    /// The crate in front of every `<crate>::link()` call: `omuzaki_char` for `omuzaki_char::link()`.
    pub link_calls: Vec<String>,
    /// The module forms in the file the scan can't follow, in source order.
    pub unsupported: Vec<UnsupportedAt>,
}

/// An open delimiter while scanning: the body of an inline `mod`, or anything else (a function body, a macro's
/// arguments, a list), inside which nothing is at item level.
enum Frame {
    Mod { name: String, cfgs: Vec<CfgAt> },
    Other,
}

/// The attributes read at item level that apply to the next item.
#[derive(Default)]
struct Pending {
    cfgs: Vec<CfgAt>,
    path: Option<String>,
    /// A `#[cfg_attr(..., path = ...)]` is on the next item.
    cfg_attr_path: bool,
}

/// Scans one file's source.
///
/// Registrations count where the registration macros are documented to go: at item level, in the file or in an inline
/// `mod` block, together with the `cfg`s they sit under. A call inside a function, a `const` block or another macro's
/// arguments may not be in every build and isn't counted. If such a call does reach an App binary, the gate reports
/// it as unexpected, so it can't hide.
///
/// A module form the scan can't follow is recorded in [`FileScan::unsupported`] wherever it is, and the gate refuses
/// the file: a `mod` that is followed here could otherwise hide the registrations it compiles.
pub fn scan(source: &str) -> FileScan {
    let toks = tokens(source);
    let mut out = FileScan::default();
    let mut frames: Vec<Frame> = Vec::new();
    let mut pending = Pending::default();
    let mut next_brace_is_mod: Option<(String, Vec<CfgAt>)> = None;
    // Token ranges of `macro_rules!` matchers: `mod $name` there is input the macro accepts, not a module it declares.
    let mut matchers: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while let Some(token) = toks.get(i) {
        let item_level = frames.iter().all(|f| matches!(f, Frame::Mod { .. }));
        match &token.tok {
            Tok::Punct('#') if item_level => {
                let inner = is_punct(&toks, i + 1, '!');
                let open = if inner { i + 2 } else { i + 1 };
                if let Some(close) = closing(&toks, open) {
                    let attr = &toks[open + 1..close];
                    if inner {
                        apply_inner(attr, token.line, &mut frames, &mut out.inner_cfgs);
                    } else {
                        read_outer(attr, token.line, &mut pending);
                    }
                    i = close + 1;
                    continue;
                }
            }
            Tok::Punct(';') if item_level => pending = Pending::default(),
            Tok::Punct('{' | '(' | '[') => {
                let frame = match next_brace_is_mod.take() {
                    Some((name, cfgs)) if token.is_punct('{') => Frame::Mod { name, cfgs },
                    _ => Frame::Other,
                };
                frames.push(frame);
            }
            Tok::Punct('}' | ')' | ']') => {
                frames.pop();
                if token.is_punct('}') && frames.iter().all(|f| matches!(f, Frame::Mod { .. })) {
                    pending = Pending::default(); // an item with a body ended
                }
            }
            Tok::Ident(word) => {
                if let Some(form) = body_form(&toks, i, item_level, &matchers) {
                    out.unsupported.push(UnsupportedAt { form, line: token.line });
                }
                if word == "macro_rules" && is_punct(&toks, i + 1, '!') {
                    if let Some(kind) = ident_at(&toks, i + 2).and_then(|name| name.strip_prefix(MACRO_PREFIX)) {
                        out.defines.push(kind.to_owned());
                    }
                    matchers.extend(macro_matchers(&toks, i + 3));
                    i += 3; // past the name, so the definition is not read as a call
                    continue;
                }
                if word == "link"
                    && is_punct(&toks, i + 1, '(')
                    && is_path_sep(&toks, i.wrapping_sub(2))
                    && let Some(path) = ident_at(&toks, i.wrapping_sub(3))
                {
                    out.link_calls.push(path.to_owned());
                }
                if item_level {
                    if frames.is_empty() && (is_link_fn(&toks, i) || is_link_macro(word, &toks, i)) {
                        out.defines_link = true;
                    } else if word == "mod"
                        && let Some(name) = ident_at(&toks, i + 1)
                    {
                        if pending.cfg_attr_path {
                            out.unsupported.push(UnsupportedAt { form: Unsupported::CfgAttrPath, line: token.line });
                        }
                        if is_punct(&toks, i + 2, ';') {
                            let inline = frames.iter().filter_map(mod_name).map(str::to_owned).collect();
                            let cfgs = cfgs_here(&frames, &pending);
                            out.mods.push(ModDecl { name: name.to_owned(), path: pending.path.take(), inline, cfgs });
                        } else if is_punct(&toks, i + 2, '{') {
                            if pending.path.is_some() {
                                out.unsupported
                                    .push(UnsupportedAt { form: Unsupported::PathOnInlineMod, line: token.line });
                            }
                            next_brace_is_mod = Some((name.to_owned(), std::mem::take(&mut pending.cfgs)));
                        }
                        pending = Pending::default();
                        i += 2;
                        continue;
                    } else if let Some(kind) = word.strip_prefix(MACRO_PREFIX)
                        && is_punct(&toks, i + 1, '!')
                        && let Some(name) = first_argument(&toks, i + 2)
                    {
                        let cfgs = cfgs_here(&frames, &pending);
                        out.calls.push(Call { kind: kind.to_owned(), name, line: token.line, cfgs });
                        pending = Pending::default();
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

fn mod_name(frame: &Frame) -> Option<&str> {
    match frame {
        Frame::Mod { name, .. } => Some(name),
        Frame::Other => None,
    }
}

/// The `cfg`s an item at this point is compiled under: its inline modules' and its own pending attributes.
fn cfgs_here(frames: &[Frame], pending: &Pending) -> Vec<CfgAt> {
    frames
        .iter()
        .flat_map(|frame| match frame {
            Frame::Mod { cfgs, .. } => cfgs.as_slice(),
            Frame::Other => &[],
        })
        .chain(&pending.cfgs)
        .cloned()
        .collect()
}

/// `#![cfg(...)]`: applies to the innermost inline module, or to the file at its top level.
fn apply_inner(attr: &[Token], line: u32, frames: &mut [Frame], file: &mut Vec<CfgAt>) {
    let Some(cfg) = cfg_of(attr) else { return };
    match frames.last_mut() {
        Some(Frame::Mod { cfgs, .. }) => cfgs.push(CfgAt { cfg, line }),
        _ => file.push(CfgAt { cfg, line }),
    }
}

/// An outer attribute: `cfg` and `path` are kept for the next item, the rest don't matter here.
fn read_outer(attr: &[Token], line: u32, pending: &mut Pending) {
    if let Some(cfg) = cfg_of(attr) {
        pending.cfgs.push(CfgAt { cfg, line });
    }
    if cfg_attr_applies_path(attr) {
        pending.cfg_attr_path = true;
    } else if attr.first().is_some_and(|t| t.is_ident("path"))
        && attr.get(1).is_some_and(|t| t.is_punct('='))
        && let Some(Tok::Literal(path)) = attr.get(2).map(|t| &t.tok)
    {
        pending.path = Some(path.trim_matches('"').to_owned());
    }
}

/// The predicate of a `cfg(...)` attribute. A `cfg_attr` that can add a `cfg` is unreadable: whether it applies
/// depends on a predicate the scan would have to guess.
fn cfg_of(attr: &[Token]) -> Option<Cfg> {
    let first = attr.first()?;
    if first.is_ident("cfg") && is_punct(attr, 1, '(') && attr.last().is_some_and(|t| t.is_punct(')')) {
        return Some(Cfg::parse(&attr[2..attr.len() - 1]));
    }
    if first.is_ident("cfg_attr") && attr.iter().skip(1).any(|t| t.is_ident("cfg")) {
        return Some(Cfg::Unreadable(attr.iter().map(Token::text).collect::<Vec<_>>().join(" ")));
    }
    None
}

/// The index of the delimiter that closes the one opening at `open`.
fn closing(toks: &[Token], open: usize) -> Option<usize> {
    if !matches!(toks.get(open)?.tok, Tok::Punct('(' | '[' | '{')) {
        return None;
    }
    let mut depth = 0usize;
    for (at, token) in toks.iter().enumerate().skip(open) {
        match token.tok {
            Tok::Punct('(' | '[' | '{') => depth += 1,
            Tok::Punct(')' | ']' | '}') => {
                depth -= 1;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
}

/// The unsupported form the identifier at `at` starts, if any: an `include!` that is not the generated one, or a `mod`
/// below item level that declares a module (a name or a `$name` after it). Tokens inside the `matchers` of a
/// `macro_rules!` are input patterns the macro accepts, not code it compiles, so they never count; a transcriber's
/// `mod $name;` is still a module the scan can't name.
fn body_form(toks: &[Token], at: usize, item_level: bool, matchers: &[(usize, usize)]) -> Option<Unsupported> {
    if matchers.iter().any(|&(from, to)| (from..=to).contains(&at)) {
        return None;
    }
    if toks[at].is_ident("include") && is_punct(toks, at + 1, '!') && !is_generated_include(toks, at) {
        return Some(Unsupported::Include);
    }
    let named = ident_at(toks, at + 1).is_some() || is_punct(toks, at + 1, '$');
    (toks[at].is_ident("mod") && !item_level && named).then_some(Unsupported::ModInBody)
}

/// The token ranges of the matchers in the `macro_rules!` body opening at `open`: in `{ (matcher) => { body }; ... }`
/// the first group of each rule. A body that does not have that shape yields the rules read so far.
fn macro_matchers(toks: &[Token], open: usize) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let Some(end) = closing(toks, open) else { return found };
    let mut at = open + 1;
    while at < end {
        let Some(matcher_end) = closing(toks, at) else { break };
        found.push((at, matcher_end));
        if !(is_punct(toks, matcher_end + 1, '=') && is_punct(toks, matcher_end + 2, '>')) {
            break;
        }
        let Some(body_end) = closing(toks, matcher_end + 3) else { break };
        at = body_end + 1;
        if is_punct(toks, at, ';') {
            at += 1;
        }
    }
    found
}

/// Whether the `include!` at `at` is the generated `include!(concat!(env!("OUT_DIR"), "/<name>.rs"))`: the form the
/// build scripts' module lists and link wiring use (ADR 0015, ADR 0054). The build scripts that write those files are
/// checked by their own tests, so this is the one `include!` the scan accepts.
fn is_generated_include(toks: &[Token], at: usize) -> bool {
    const HEAD: [&str; 12] = ["include", "!", "(", "concat", "!", "(", "env", "!", "(", "\"OUT_DIR\"", ")", ","];
    let head = HEAD.iter().enumerate().all(|(n, want)| toks.get(at + n).is_some_and(|t| t.text() == *want));
    let tail = toks.get(at + HEAD.len() + 1).is_some_and(|t| t.is_punct(')'))
        && toks.get(at + HEAD.len() + 2).is_some_and(|t| t.is_punct(')'));
    let file = toks.get(at + HEAD.len()).and_then(|t| match &t.tok {
        Tok::Literal(literal) => literal.strip_prefix("\"/")?.strip_suffix(".rs\""),
        _ => None,
    });
    head && tail
        && file.is_some_and(|name| !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || "_-".contains(c)))
}

/// Whether a `cfg_attr(predicate, ...)` applies a `path = ...` attribute: one of the attributes after the predicate is
/// that meta item, possibly inside a nested `cfg_attr`. A predicate named `path` is not one.
fn cfg_attr_applies_path(attr: &[Token]) -> bool {
    let is_cfg_attr = attr.first().is_some_and(|t| t.is_ident("cfg_attr"))
        && is_punct(attr, 1, '(')
        && attr.last().is_some_and(|t| t.is_punct(')'));
    is_cfg_attr && split_top_level(&attr[2..attr.len() - 1]).into_iter().skip(1).any(is_path_meta)
}

/// Whether one attribute is `path = ...`, or a `cfg_attr` that applies one.
fn is_path_meta(item: &[Token]) -> bool {
    match item {
        [first, eq, ..] if first.is_ident("path") && eq.is_punct('=') => true,
        [first, ..] if first.is_ident("cfg_attr") => cfg_attr_applies_path(item),
        _ => false,
    }
}

/// `toks` split at the commas outside any brackets.
fn split_top_level(toks: &[Token]) -> Vec<&[Token]> {
    let (mut parts, mut depth, mut start) = (Vec::new(), 0usize, 0usize);
    for (at, token) in toks.iter().enumerate() {
        match token.tok {
            Tok::Punct('(' | '[' | '{') => depth += 1,
            Tok::Punct(')' | ']' | '}') => depth = depth.saturating_sub(1),
            Tok::Punct(',') if depth == 0 => {
                parts.push(&toks[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(&toks[start..]);
    parts
}

fn is_punct(toks: &[Token], at: usize, c: char) -> bool {
    toks.get(at).is_some_and(|t| t.is_punct(c))
}

fn ident_at(toks: &[Token], at: usize) -> Option<&str> {
    match toks.get(at).map(|t| &t.tok) {
        Some(Tok::Ident(name)) => Some(name),
        _ => None,
    }
}

/// `::` at `at` and `at + 1`.
fn is_path_sep(toks: &[Token], at: usize) -> bool {
    is_punct(toks, at, ':') && is_punct(toks, at.wrapping_add(1), ':')
}

/// `pub fn link(` from `at`.
fn is_link_fn(toks: &[Token], at: usize) -> bool {
    toks[at].is_ident("pub")
        && toks.get(at + 1).is_some_and(|t| t.is_ident("fn"))
        && toks.get(at + 2).is_some_and(|t| t.is_ident("link"))
        && is_punct(toks, at + 3, '(')
}

/// `define_link!` at `at`, plain or path-qualified.
fn is_link_macro(word: &str, toks: &[Token], at: usize) -> bool {
    word == LINK_MACRO && is_punct(toks, at + 1, '!')
}

/// The first argument of the delimited group opening at `open`: its tokens up to the first top-level comma, joined
/// without spaces, with a leading `label:` dropped (`register_story! { id: "button", ... }` gives `"button"`).
///
/// A comma inside generic arguments (`Pair<u8, i32>`) belongs to the argument. `<` and `>` can also be operators, so
/// angle brackets are only trusted when they balance before the argument ends; otherwise the argument is read again
/// with them as plain tokens.
fn first_argument(toks: &[Token], open: usize) -> Option<String> {
    if !matches!(toks.get(open)?.tok, Tok::Punct('(' | '[' | '{')) {
        return None;
    }
    let arg = argument_tokens(toks, open + 1, true).or_else(|| argument_tokens(toks, open + 1, false))?;
    let labelled =
        arg.len() > 2 && matches!(arg[0].tok, Tok::Ident(_)) && arg[1].is_punct(':') && !arg[2].is_punct(':');
    let name: String = arg.iter().skip(if labelled { 2 } else { 0 }).map(|t| t.text()).collect();
    (!name.is_empty()).then_some(name)
}

/// The tokens from `start` to the first comma outside any nesting, or to the group's end. With `angles`, `<...>` nests
/// too, and an argument whose angle brackets don't balance gives `None`.
fn argument_tokens(toks: &[Token], start: usize, angles: bool) -> Option<Vec<&Token>> {
    let (mut nested, mut angle) = (0usize, 0usize);
    let mut arg: Vec<&Token> = Vec::new();
    for (at, token) in toks.iter().enumerate().skip(start) {
        let previous = at.checked_sub(1).and_then(|p| toks.get(p));
        match token.tok {
            // The group's closing delimiter, or the comma that ends the first argument.
            Tok::Punct(')' | ']' | '}') if nested == 0 => break,
            Tok::Punct(',') if nested == 0 && angle == 0 => break,
            Tok::Punct('(' | '[' | '{') => nested += 1,
            Tok::Punct(')' | ']' | '}') => nested -= 1,
            // `<=` is a comparison, not an opening angle bracket.
            Tok::Punct('<') if angles && !toks.get(at + 1).is_some_and(|t| t.is_punct('=')) => angle += 1,
            // `->` and `=>` are arrows; `>=` is a comparison.
            Tok::Punct('>')
                if angles
                    && !previous.is_some_and(|p| p.is_punct('-') || p.is_punct('='))
                    && !toks.get(at + 1).is_some_and(|t| t.is_punct('=')) =>
            {
                angle = angle.checked_sub(1)?;
            }
            _ => {}
        }
        arg.push(token);
    }
    (angle == 0).then_some(arg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calls(source: &str) -> Vec<(String, String, u32)> {
        scan(source).calls.into_iter().map(|c| (c.kind, c.name, c.line)).collect()
    }

    fn call(kind: &str, name: &str, line: u32) -> (String, String, u32) {
        (kind.to_owned(), name.to_owned(), line)
    }

    #[test]
    fn item_level_calls_are_found_with_their_first_argument_and_line() {
        let source = r#"
use omuzaki_ops_api::register_op;
register_op!(CharacterCreate);
omuzaki_ops_api::register_op!(crate::ops::Rename);
register_contrast!("button", pairs);
register_story! {
    id: "toggle", title: "Toggle", show: |ui, s| { register_op!(Nested); },
}
register_importer!(Gltf < u8 >);
mod inline { register_op!(InInlineMod); }
"#;
        assert_eq!(
            calls(source),
            [
                call("op", "CharacterCreate", 3),
                call("op", "crate::ops::Rename", 4),
                call("contrast", "\"button\"", 5),
                call("story", "\"toggle\"", 6),
                call("importer", "Gltf<u8>", 9),
                call("op", "InInlineMod", 10),
            ]
        );
    }

    #[test]
    fn calls_in_bodies_comments_strings_and_definitions_are_not_registrations() {
        let source = r#"
/// register_op!(InDocs);
fn f() { register_op!(InFunction); }
const _: () = { register_op!(InConstBlock); };
const S: &str = "register_op!(InString)";
#[macro_export]
macro_rules! register_op { ($ty:ty) => { $crate::submit!($ty); }; }
"#;
        let scan = scan(source);
        assert!(scan.calls.is_empty(), "{:?}", scan.calls);
        assert_eq!(scan.defines, ["op"]);
    }

    #[test]
    fn a_comma_inside_generic_arguments_is_part_of_the_name() {
        assert_eq!(calls("register_importer!(Pair<u8, i32>);"), [call("importer", "Pair<u8,i32>", 1)]);
        assert_eq!(calls("register_op!(Map<K, Vec<(u8, u16)>>, extra);"), [call("op", "Map<K,Vec<(u8,u16)>>", 1)]);
        assert_eq!(calls("register_op!(Fn<fn(u8) -> u8, X>);"), [call("op", "Fn<fn(u8)->u8,X>", 1)]);
        // Unbalanced angle brackets are operators: the comma ends the argument as usual.
        assert_eq!(
            calls("register_face_baker!(if a < b { X } else { Y }, 2);"),
            [call("face_baker", "ifa<b{X}else{Y}", 1)]
        );
        assert_eq!(calls("register_face_baker!(a <= b, c > d);"), [call("face_baker", "a<=b", 1)]);
    }

    #[test]
    fn cfgs_on_a_call_and_on_its_inline_modules_are_kept() {
        let source = r#"
#[cfg(test)]
register_op!(TestOnly);
#[cfg(feature = "wgpu")]
#[allow(dead_code)]
register_op!(Gated);
#[cfg(any(test, debug_assertions))] fn helper() {}
register_op!(Plain);
#[cfg(not(test))]
mod live {
    #![cfg(unix)]
    register_op!(Nested);
}
"#;
        let found: Vec<(String, Vec<String>)> = scan(source)
            .calls
            .into_iter()
            .map(|c| (c.name, c.cfgs.iter().map(|at| format!("{}@{}", at.cfg, at.line)).collect()))
            .collect();
        assert_eq!(
            found,
            [
                ("TestOnly".to_owned(), vec!["test@2".to_owned()]),
                ("Gated".to_owned(), vec!["feature = \"wgpu\"@4".to_owned()]),
                ("Plain".to_owned(), vec![]),
                ("Nested".to_owned(), vec!["not(test)@9".to_owned(), "unix@11".to_owned()]),
            ]
        );
    }

    #[test]
    fn mod_declarations_carry_their_path_inline_modules_and_cfgs() {
        let source = r#"
#![cfg(not(test))]
mod ops;
#[cfg(test)]
mod tests;
#[path = "gen/extra.rs"]
pub(crate) mod extra;
mod outer { #[cfg(feature = "x")] mod inner; }
"#;
        let scan = scan(source);
        assert_eq!(scan.inner_cfgs.iter().map(|at| at.cfg.to_string()).collect::<Vec<_>>(), ["not(test)"]);
        let mods: Vec<String> = scan
            .mods
            .iter()
            .map(|m| {
                let cfgs: Vec<String> = m.cfgs.iter().map(|at| at.cfg.to_string()).collect();
                format!("{} path={:?} inline={:?} cfgs={cfgs:?}", m.name, m.path, m.inline)
            })
            .collect();
        assert_eq!(
            mods,
            [
                "ops path=None inline=[] cfgs=[]",
                "tests path=None inline=[] cfgs=[\"test\"]",
                "extra path=Some(\"gen/extra.rs\") inline=[] cfgs=[]",
                "inner path=None inline=[\"outer\"] cfgs=[\"feature = \\\"x\\\"\"]",
            ]
        );
    }

    #[test]
    fn the_link_function_or_macro_and_calls_to_other_crates_link_functions_are_found() {
        let source =
            "pub fn link() {\n    omuzaki_char::link();\n    omuzaki_anim::link()\n}\nfn other() { x.link(); }\n";
        let scan = scan(source);
        assert!(scan.defines_link);
        assert_eq!(scan.link_calls, ["omuzaki_char", "omuzaki_anim"]);
        let by_macro = super::scan("omuzaki_core::define_link! {\n    omuzaki_face::link();\n}\n");
        assert!(by_macro.defines_link);
        assert_eq!(by_macro.link_calls, ["omuzaki_face"]);
        assert!(super::scan("omuzaki_core::define_link!();").defines_link);
        assert!(!super::scan("mod m { pub fn link() {} }").defines_link, "a nested link() is not the crate's");
    }

    fn unsupported(source: &str) -> Vec<(Unsupported, u32)> {
        scan(source).unsupported.into_iter().map(|at| (at.form, at.line)).collect()
    }

    #[test]
    fn a_mod_in_a_body_is_refused_and_a_file_level_mod_is_followed() {
        let source = "fn f() {\n    mod hidden;\n}\nmod shown;\nconst _: () = { mod deep {} };\n";
        assert_eq!(unsupported(source), [(Unsupported::ModInBody, 2), (Unsupported::ModInBody, 5)]);
        let names: Vec<String> = scan(source).mods.into_iter().map(|m| m.name).collect();
        assert_eq!(names, ["shown"]);
    }

    #[test]
    fn include_of_a_file_is_refused_wherever_it_is_but_the_generated_out_dir_form_and_include_str_are_not() {
        let source = r#"include!("gen.rs");
fn f() { include!(concat!(env!("OUT_DIR"), "/x.rs")); }
include!(concat!(env!("OUT_DIR"), "/../x.rs"));
include!(concat!(env!("CARGO_MANIFEST_DIR"), "/x.rs"));
include!(concat!(env!("OUT_DIR"), "/x.rs") );
const A: &str = include_str!("a.txt");
const B: &[u8] = include_bytes!("b.bin");
"#;
        assert_eq!(
            unsupported(source),
            [(Unsupported::Include, 1), (Unsupported::Include, 3), (Unsupported::Include, 4)]
        );
    }

    #[test]
    fn a_mod_in_a_macro_matcher_is_input_not_a_declaration() {
        let source = "macro_rules! parse {\n    (mod $name:ident;) => { struct Parsed; };\n    ($name:ident) => {\n        mod $name;\n    };\n}\n";
        assert_eq!(unsupported(source), [(Unsupported::ModInBody, 4)], "only the transcriber's `mod` declares one");
        let matcher_only = "macro_rules! parse { (mod $name:ident;) => { struct Parsed; } }\n";
        assert_eq!(unsupported(matcher_only), []);
    }

    #[test]
    fn an_include_in_a_macro_matcher_is_input_but_one_in_a_transcriber_is_refused() {
        let source = "macro_rules! take {\n    (include!($file:literal)) => { include!($file); };\n}\n";
        assert_eq!(unsupported(source), [(Unsupported::Include, 2)]);
        assert_eq!(unsupported("macro_rules! take { (include!($file:literal)) => { struct A; } }\n"), []);
    }

    #[test]
    fn a_mod_named_by_a_macro_variable_in_a_macro_body_is_refused() {
        let source = "macro_rules! declare {\n    ($name:ident) => {\n        mod $name;\n    };\n}\n";
        assert_eq!(unsupported(source), [(Unsupported::ModInBody, 3)]);
    }

    #[test]
    fn a_path_meta_item_in_cfg_attr_is_refused_even_nested_but_a_predicate_named_path_is_not() {
        let source = r#"#[cfg_attr(path, allow(dead_code))]
mod predicate;
#[cfg_attr(unix, cfg_attr(x, path = "n.rs"))]
mod nested;
#[cfg_attr(unix, path = "u.rs")]
mod direct;
"#;
        assert_eq!(unsupported(source), [(Unsupported::CfgAttrPath, 4), (Unsupported::CfgAttrPath, 6)]);
        let names: Vec<String> = scan(source).mods.into_iter().map(|m| m.name).collect();
        assert_eq!(names, ["predicate", "nested", "direct"]);
    }

    #[test]
    fn a_cfg_attr_path_on_a_mod_is_refused_and_a_plain_path_is_followed() {
        let source = "#[cfg_attr(unix, path = \"unix.rs\")]\nmod os;\n#[path = \"plain.rs\"]\nmod plain;\n\
                      #[cfg_attr(unix, allow(dead_code))]\nmod allowed;\n";
        assert_eq!(unsupported(source), [(Unsupported::CfgAttrPath, 2)]);
        let paths: Vec<(String, Option<String>)> = scan(source).mods.into_iter().map(|m| (m.name, m.path)).collect();
        assert_eq!(
            paths,
            [("os".to_owned(), None), ("plain".to_owned(), Some("plain.rs".to_owned())), ("allowed".to_owned(), None),]
        );
    }

    #[test]
    fn a_cfg_attr_path_on_an_item_that_is_not_a_mod_does_not_reach_the_next_mod() {
        assert_eq!(unsupported("#[cfg_attr(unix, path = \"a.rs\")]\nconst X: u8 = 1;\nmod m;\n"), []);
        assert_eq!(unsupported("#[cfg_attr(unix, path = \"a.rs\")]\nfn f() {}\nmod m;\n"), []);
    }

    #[test]
    fn a_path_on_an_inline_mod_is_refused_and_an_inline_mod_without_one_is_followed() {
        let refused = "#[path = \"elsewhere\"]\nmod outer {\n    mod inner;\n}\n";
        assert_eq!(unsupported(refused), [(Unsupported::PathOnInlineMod, 2)]);
        let allowed = "mod outer {\n    #[path = \"elsewhere.rs\"]\n    mod inner;\n}\n";
        assert_eq!(unsupported(allowed), []);
        let inner: Vec<Vec<String>> = scan(allowed).mods.into_iter().map(|m| m.inline).collect();
        assert_eq!(inner, [vec!["outer".to_owned()]]);
    }
}
