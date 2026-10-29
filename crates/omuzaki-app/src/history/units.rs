//! Units (`docs/design/branches.md` §7.1): what a diff is grouped by, and what a merge's "keep mine" or "take theirs"
//! applies to as a whole. Every document path maps to exactly one unit; the key is the path the unit starts at.

use omuzaki_doc_api::{DocPath, Value, lookup};
use serde::Serialize;

/// One unit of the document.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Unit {
    /// The path the unit starts at (`characters/kit/parts/hair`).
    pub key: String,
    /// What a person reads (`Kit: part hair`).
    pub title: String,
    /// The kind of unit (`part`, `face`, `scene`).
    pub kind: &'static str,
}

impl Unit {
    fn new(segments: &[String], count: usize, kind: &'static str, title: String) -> Self {
        Self { key: segments[..count.min(segments.len())].join("/"), title, kind }
    }
}

/// The library application that created `path`, if `trees` hold one under the same character whose `created` lists
/// it (or a part it lies under): such paths group with the application, not with their part.
fn application_of(segments: &[String], trees: &[&Value]) -> Option<String> {
    let [root, character, ..] = segments else { return None };
    if root != "characters" {
        return None;
    }
    let path = segments.join("/");
    for tree in trees {
        let applied = DocPath::parse(&format!("characters/{character}/applied")).ok()?;
        let Some(Value::Object(applications)) = lookup(tree, &applied) else { continue };
        for (id, application) in applications {
            let created = application.get("created").and_then(Value::as_array).into_iter().flatten();
            let source_rows = created.clone().filter_map(Value::as_str).filter_map(|made| {
                let part = made.strip_prefix(&format!("characters/{character}/parts/"))?;
                Some(format!("characters/{character}/sources/{}", part.split('/').next().unwrap_or(part)))
            });
            let listed = created.filter_map(Value::as_str).map(str::to_owned).chain(source_rows);
            if listed.into_iter().any(|made| path == made || path.starts_with(&format!("{made}/"))) {
                return Some(id.clone());
            }
        }
    }
    None
}

/// The unit `path` belongs to, with `trees` (the sides being compared) consulted for library applications.
pub fn unit_of(path: &DocPath, trees: &[&Value]) -> Unit {
    let s = path.segments();
    let seg = |i: usize| s.get(i).map_or("", String::as_str);
    match (seg(0), s.len()) {
        ("", _) => Unit { key: String::new(), title: "the whole project".to_owned(), kind: "project" },
        ("characters", 1) => Unit::new(s, 1, "characters", "every character".to_owned()),
        ("characters", 2) => Unit::new(s, 2, "character", format!("{}: the character", seg(1))),
        ("characters", _) => {
            let character = seg(1);
            if let Some(application) = application_of(s, trees) {
                let key = format!("characters/{character}/applied/{application}");
                return Unit { key, title: format!("{character}: library item {application}"), kind: "application" };
            }
            match seg(2) {
                "parts" | "sources" | "weight_brushes" if s.len() >= 4 => Unit {
                    key: format!("characters/{character}/parts/{}", seg(3)),
                    title: format!("{character}: part {}", seg(3)),
                    kind: "part",
                },
                "applied" if s.len() >= 4 => {
                    Unit::new(s, 4, "application", format!("{character}: library item {}", seg(3)))
                }
                "face" => Unit::new(s, 3, "face", format!("{character}: face")),
                "palette" | "skin_tone" => Unit {
                    key: format!("characters/{character}/palette"),
                    title: format!("{character}: palette"),
                    kind: "palette",
                },
                "rig" => Unit::new(s, 3, "rig", format!("{character}: rig")),
                "look" => Unit::new(s, 3, "look", format!("{character}: look")),
                kind @ ("variants" | "colourways" | "poses" | "clips" | "emotes") if s.len() >= 4 => {
                    let singular = &kind[..kind.len() - 1];
                    let kind = match kind {
                        "variants" => "variant",
                        "colourways" => "colourway",
                        "poses" => "pose",
                        "clips" => "clip",
                        _ => "emote",
                    };
                    Unit::new(s, 4, kind, format!("{character}: {singular} {}", seg(3)))
                }
                "licence" => Unit::new(s, 3, "licence", format!("{character}: licence record")),
                "name" | "template" | "seed" => Unit {
                    key: format!("characters/{character}/basics"),
                    title: format!("{character}: basics"),
                    kind: "basics",
                },
                other => Unit::new(s, 3, "other", format!("{character}: {other}")),
            }
        }
        ("scenes", n) if n >= 2 => Unit::new(s, 2, "scene", format!("scene {}", seg(1))),
        ("performances", n) if n >= 2 => Unit::new(s, 2, "performance", format!("performance {}", seg(1))),
        ("shows", n) if n >= 2 => Unit::new(s, 2, "show", format!("show {}", seg(1))),
        ("project", _) => Unit::new(s, 1, "project", "project settings".to_owned()),
        _ => Unit::new(s, 2, "other", s.iter().take(2).cloned().collect::<Vec<_>>().join(" ")),
    }
}

/// Whether `path` is bookkeeping a diff leaves out (`merges`, design 7.3).
pub fn is_bookkeeping(path: &DocPath) -> bool {
    path.segments().first().is_some_and(|first| first == "merges")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn unit(path: &str) -> (String, &'static str) {
        let u = unit_of(&DocPath::parse(path).unwrap_or_else(|_| DocPath::root()), &[]);
        (u.key, u.kind)
    }

    #[test]
    fn every_row_of_the_table_maps_as_the_design_says() {
        let cases = [
            ("characters/kit/parts/hair/len", "characters/kit/parts/hair", "part"),
            ("characters/kit/sources/hair", "characters/kit/parts/hair", "part"),
            ("characters/kit/weight_brushes/hair", "characters/kit/parts/hair", "part"),
            ("characters/kit/applied/cat-ears", "characters/kit/applied/cat-ears", "application"),
            ("characters/kit/face/eye/size", "characters/kit/face", "face"),
            ("characters/kit/palette/skin", "characters/kit/palette", "palette"),
            ("characters/kit/skin_tone", "characters/kit/palette", "palette"),
            ("characters/kit/rig/spring", "characters/kit/rig", "rig"),
            ("characters/kit/look/outline", "characters/kit/look", "look"),
            ("characters/kit/variants/winter", "characters/kit/variants/winter", "variant"),
            ("characters/kit/colourways/red/a", "characters/kit/colourways/red", "colourway"),
            ("characters/kit/poses/wave", "characters/kit/poses/wave", "pose"),
            ("characters/kit/emotes/hi", "characters/kit/emotes/hi", "emote"),
            ("characters/kit/licence", "characters/kit/licence", "licence"),
            ("characters/kit/name", "characters/kit/basics", "basics"),
            ("characters/kit/seed", "characters/kit/basics", "basics"),
            ("characters/kit", "characters/kit", "character"),
            ("scenes/main/lights/key", "scenes/main", "scene"),
            ("performances/p1/x", "performances/p1", "performance"),
            ("shows/s1/graph", "shows/s1", "show"),
            ("project/fps", "project", "project"),
            ("other/thing/deep", "other/thing", "other"),
        ];
        for (path, key, kind) in cases {
            assert_eq!(unit(path), (key.to_owned(), kind), "{path}");
        }
    }

    #[test]
    fn a_part_a_library_item_created_groups_with_the_item() {
        let tree = json!({"characters": {"kit": {"applied": {"cat-ears": {
            "created": ["characters/kit/parts/ears"]
        }}}}});
        for path in ["characters/kit/parts/ears/size", "characters/kit/sources/ears"] {
            let u = unit_of(&DocPath::parse(path).unwrap_or_else(|_| DocPath::root()), &[&tree]);
            assert_eq!((u.key.as_str(), u.kind), ("characters/kit/applied/cat-ears", "application"), "{path}");
        }
        let other =
            unit_of(&DocPath::parse("characters/kit/parts/hair/len").unwrap_or_else(|_| DocPath::root()), &[&tree]);
        assert_eq!(other.kind, "part");
    }

    #[test]
    fn merges_is_bookkeeping() {
        assert!(is_bookkeeping(&DocPath::parse("merges/abc").unwrap_or_else(|_| DocPath::root())));
        assert!(!is_bookkeeping(&DocPath::parse("characters/kit").unwrap_or_else(|_| DocPath::root())));
    }
}
