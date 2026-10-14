//! Text distance for "did you mean" suggestions: when an agent or a person misspells a name, the error names the closest
//! real one. Every crate that suggests a name uses these functions, so the suggestions agree across the workspace.

/// The Levenshtein distance between `a` and `b`: the fewest single-character insertions, deletions and substitutions
/// that turn one into the other. Distances count `char`s, not bytes, so `é` is one character.
///
/// Transposing two adjacent characters costs two edits here, not one.
///
/// Works one row of the table at a time, so the only allocation is that row, `b`'s length plus one.
///
/// # Examples
///
/// ```
/// assert_eq!(omuzaki_core::text::edit_distance("kitten", "sitting"), 3);
/// ```
#[must_use]
pub fn edit_distance(a: &str, b: &str) -> usize {
    let width = b.chars().count();
    // `row[j]` is the distance from the prefix of `a` read so far to the first `j` characters of `b`.
    let mut row: Vec<usize> = (0..=width).collect();
    for (i, ca) in a.chars().enumerate() {
        // `diagonal` is the previous row's cell to the left, which the substitution cost comes from.
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.chars().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(ca != cb)).min(above + 1).min(row[j] + 1);
            diagonal = above;
        }
    }
    row[width]
}

/// The candidate closest to `wanted` by edit distance. A tie goes to the first candidate, so the order the caller lists
/// its names in decides between equally close ones. `None` when there are no candidates.
///
/// # Examples
///
/// ```
/// assert_eq!(omuzaki_core::text::nearest("widht", ["height", "width"]), Some("width"));
/// ```
#[must_use]
pub fn nearest<'a>(wanted: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    candidates.into_iter().min_by_key(|candidate| edit_distance(wanted, candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_distance_counts_single_character_edits() {
        assert_eq!(edit_distance("", ""), 0);
        assert_eq!(edit_distance("width", "width"), 0);
        assert_eq!(edit_distance("widht", "width"), 2);
        assert_eq!(edit_distance("lash", "lsh"), 1, "one deletion");
        assert_eq!(edit_distance("hed", "head"), 1, "one insertion");
        assert_eq!(edit_distance("hair_hair_9", "hair_hair_1"), 1, "one substitution");
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("abc", ""), 3);
    }

    #[test]
    fn edit_distance_counts_chars_not_bytes() {
        assert_eq!(edit_distance("café", "cafe"), 1);
        assert_eq!(edit_distance("é", "e"), 1);
    }

    #[test]
    fn a_transposition_is_two_edits() {
        assert_eq!(edit_distance("ab", "ba"), 2);
    }

    #[test]
    fn the_nearest_name_wins_and_a_tie_goes_to_the_first() {
        assert_eq!(nearest("widht", ["height", "width", "drop"]), Some("width"));
        assert_eq!(nearest("ghost", ["hair", "ghosts", "gho"]), Some("ghosts"));
        assert_eq!(nearest("ab", ["xb", "ax"]), Some("xb"), "both are one edit away: the first wins");
        assert_eq!(nearest("ab", ["ba", "ac"]), Some("ac"));
        assert_eq!(nearest("ab", ["ax", "bx"]), Some("ax"));
    }

    #[test]
    fn nearest_of_no_names_is_none() {
        assert_eq!(nearest("ab", std::iter::empty()), None);
    }
}
