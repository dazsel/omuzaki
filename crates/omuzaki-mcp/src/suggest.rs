//! "Did you mean ...?" for misspelled tool and argument names, so an agent can correct itself in one step.

use omuzaki_core::text::edit_distance;

const MAX_SUGGESTIONS: usize = 3;

/// The candidates closest to `wanted`, best first: names that contain it (or are contained in it), then names within a
/// small edit distance.
pub(crate) fn suggest<'a>(wanted: &str, candidates: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let wanted = wanted.to_lowercase();
    let allowed = (wanted.chars().count() / 3).max(2);
    let mut scored: Vec<(usize, &str)> = candidates
        .into_iter()
        .filter_map(|name| {
            let lower = name.to_lowercase();
            let related = wanted.len() >= 3 && (lower.contains(&wanted) || wanted.contains(&lower));
            let distance = if related { 0 } else { edit_distance(&wanted, &lower) };
            (distance <= allowed).then_some((distance, name))
        })
        .collect();
    scored.sort_unstable();
    scored.into_iter().take(MAX_SUGGESTIONS).map(|(_, name)| name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMES: [&str; 4] = ["face.set_profile", "face.eye_style", "hair.set_style", "render.preview"];

    #[test]
    fn finds_typos_and_missing_namespaces() {
        assert_eq!(suggest("face.set_profil", NAMES), ["face.set_profile"]);
        assert_eq!(suggest("set_profile", NAMES), ["face.set_profile"]);
        assert_eq!(suggest("render.previw", NAMES), ["render.preview"]);
    }

    #[test]
    fn unrelated_names_get_no_suggestion() {
        assert_eq!(suggest("export.vrm", NAMES), Vec::<&str>::new());
    }
}
