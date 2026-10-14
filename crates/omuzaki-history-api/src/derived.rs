//! Project paths that are regenerable caches or renders, which snapshots leave out (design section 7.1).

/// One declared derived path. Built by [`register_derived_path!`](crate::register_derived_path); never by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DerivedPath {
    /// The glob, relative to the project folder; see [`matches_glob`].
    pub glob: &'static str,
}

inventory::collect!(DerivedPath);

/// Every glob declared in this binary, in link order.
pub fn derived_paths() -> impl Iterator<Item = &'static str> {
    inventory::iter::<DerivedPath>.into_iter().map(|d| d.glob)
}

/// Whether `path` (relative to the project folder, `/`-separated) matches a declared derived path.
pub fn is_derived(path: &str) -> bool {
    derived_paths().any(|glob| matches_glob(glob, path))
}

/// Matches `path` against `glob`, both `/`-separated: `*` matches any run of characters inside one segment, `?` one
/// character inside a segment, and a whole segment `**` any number of segments (including none). Everything else
/// matches itself.
///
/// Repeated `*` in one segment backtracks, which is exponential in the worst case: this is for short patterns declared
/// in code, never for user input.
///
/// ```
/// use omuzaki_history_api::matches_glob;
///
/// assert!(matches_glob("renders/**", "renders/final/a.png"));
/// assert!(matches_glob("cache/*.bin", "cache/mesh.bin"));
/// assert!(!matches_glob("cache/*.bin", "cache/deep/mesh.bin"));
/// ```
pub fn matches_glob(glob: &str, path: &str) -> bool {
    let glob: Vec<&str> = glob.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    segments_match(&glob, &path)
}

fn segments_match(glob: &[&str], path: &[&str]) -> bool {
    match glob.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| segments_match(rest, &path[skip..])),
        Some((&segment, rest)) => match path.split_first() {
            Some((&name, tail)) => chars_match(segment.as_bytes(), name.as_bytes()) && segments_match(rest, tail),
            None => false,
        },
    }
}

/// Wildcard match inside one segment. Works on bytes: the wildcards are ASCII, and UTF-8 never contains an ASCII byte
/// inside a multi-byte character, so `*` can only split between characters. `?` takes one whole character.
fn chars_match(pattern: &[u8], name: &[u8]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some((b'*', rest)) => (0..=name.len()).any(|skip| chars_match(rest, &name[skip..])),
        Some((b'?', rest)) => {
            let width = name.first().map_or(0, |&b| utf8_width(b));
            width > 0 && chars_match(rest, &name[width.min(name.len())..])
        }
        Some((&literal, rest)) => name.first() == Some(&literal) && chars_match(rest, &name[1..]),
    }
}

/// Bytes in the UTF-8 character that starts with `lead`.
fn utf8_width(lead: u8) -> usize {
    match lead {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// Declares a project path that is a regenerable cache or render, which snapshots leave out:
/// `register_derived_path!("renders/**");`. Put it next to the code that writes the path. The glob is relative to the
/// project folder; see [`matches_glob`] for the syntax.
#[macro_export]
macro_rules! register_derived_path {
    ($glob:literal) => {
        $crate::__inventory::submit! { $crate::DerivedPath { glob: $glob } }
        $crate::__record_registration!("derived_path", $glob);
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn star_stays_inside_a_segment() {
        assert!(matches_glob("renders/*.png", "renders/a.png"));
        assert!(!matches_glob("renders/*.png", "renders/deep/a.png"));
        assert!(!matches_glob("renders/*.png", "renders/a.jpg"));
    }

    #[test]
    fn double_star_spans_segments_including_none() {
        assert!(matches_glob("cache/**", "cache"));
        assert!(matches_glob("cache/**", "cache/a/b.bin"));
        assert!(matches_glob("**/thumb.png", "thumb.png"));
        assert!(matches_glob("**/thumb.png", "a/b/thumb.png"));
        assert!(!matches_glob("cache/**", "cached/a"));
    }

    #[test]
    fn question_mark_takes_one_character() {
        assert!(matches_glob("a?.bin", "ab.bin"));
        assert!(matches_glob("a?.bin", "aé.bin"));
        assert!(!matches_glob("a?.bin", "a.bin"));
        assert!(!matches_glob("a?.bin", "abc.bin"));
    }

    #[test]
    fn literals_match_exactly() {
        assert!(matches_glob("library.lock", "library.lock"));
        assert!(!matches_glob("library.lock", "sub/library.lock"));
    }

    #[test]
    fn nothing_is_derived_until_something_registers() {
        assert!(!is_derived("renders/a.png"));
    }
}
