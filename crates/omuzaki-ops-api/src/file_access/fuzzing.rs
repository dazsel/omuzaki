//! The path parser's fuzzing entry (#1653), built only with the `fuzzing` feature: the fuzz target in `fuzz/` hands
//! it arbitrary text, and it reads the text in both grammars and checks what the parser promises, so a fuzzer finds
//! wrong answers as well as panics.

use super::form::{Lexical, Parsed, PathKey, Step, Style};

/// Reads `raw` as a POSIX and as a Windows path and checks, for each reading that names an absolute place:
/// - [`PathKey::absolute`] agrees with the place's key exactly when no `..` was collapsed;
/// - the place's spelling ([`Lexical::path`]) reads back as the same place, and on Windows so does its `\\?\` form;
/// - every prefix of the place covers the place, and the place is no deeper than it was spelt.
///
/// # Errors
/// A sentence naming the first promise `raw` breaks, for the fuzz target to panic with.
pub fn check_path(raw: &str) -> Result<(), String> {
    for style in [Style::Posix, Style::Windows] {
        let Ok(parsed) = Parsed::parse(raw, style) else { continue };
        let Ok(place) = parsed.lexical(None) else { continue };
        let key = place.key(style);
        let collapsed = parsed.steps().iter().any(|step| matches!(step, Step::Parent));
        if !collapsed && PathKey::absolute(raw, style).as_ref() != Some(&key) {
            return Err(format!("{style:?}: {raw:?} has a key that disagrees with its place"));
        }
        let spelt = place.path(place.depth());
        let spelt = spelt.to_str().ok_or_else(|| format!("{style:?}: {raw:?} spells as non-UTF-8"))?;
        if Lexical::absolute(spelt, style).map(|p| p.key(style)) != Some(key.clone()) {
            return Err(format!("{style:?}: {raw:?} spells as {spelt:?}, which reads as another place"));
        }
        let verbatim = place.verbatim(place.depth());
        if style == Style::Windows && Lexical::absolute(&verbatim, style).map(|p| p.key(style)) != Some(key.clone()) {
            return Err(format!("{style:?}: {raw:?} has the verbatim form {verbatim:?}, which reads as another place"));
        }
        for keep in 0..=place.depth() {
            if !place.truncated(keep).key(style).covers(&key) {
                return Err(format!("{style:?}: {raw:?}'s first {keep} names do not cover it"));
            }
        }
        if place.depth() > parsed.steps().len() {
            return Err(format!("{style:?}: {raw:?} is deeper than it was spelt"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_forms_the_parser_accepts_keep_its_promises() {
        for raw in [
            "/a/b/../c/./d",
            "relative/x",
            r"C:\Users\U\Art\sub\\x",
            r"\\?\UNC\srv\share\a\b",
            r"//Srv/Share/a/b",
            r"\\?\C:\",
            "/",
            "C:/x",
            "CON",
            "",
            "/a\0b",
        ] {
            assert_eq!(check_path(raw), Ok(()), "{raw:?}");
        }
    }
}
