//! The rules one item must meet on its own: the id, where its file sits, the seed's first-party fields and private names.
//!
//! The version rules compare an item with its merge base and live in [`super::bump`].

use omuzaki_library_api::{FIRST_PARTY_PUBLISHER, ItemManifest, ItemRights};

use super::private::PrivateList;

/// The problems with one item, each a message without the file name. `path` is the file's workspace-relative path.
pub fn check(path: &str, seed: bool, manifest: &ItemManifest, list: &PrivateList) -> Vec<String> {
    let mut found = Vec::new();
    let (folder, file_name) = folder_and_file(path);
    match manifest.item_id() {
        Err(error) => found.push(format!("invalid id: {error}")),
        Ok(id) => {
            if seed && id.publisher() != FIRST_PARTY_PUBLISHER {
                found.push(format!("a seed id lives under `{FIRST_PARTY_PUBLISHER}/`, not `{}/`", id.publisher()));
            }
            if seed && file_name != format!("{}.toml", id.last_segment()) {
                found.push(format!("the file name must be `{}.toml`, the id's last segment", id.last_segment()));
            }
            found.extend(private_in_id(list, &manifest.section, &id.as_str().split('/').collect::<Vec<_>>()));
        }
    }
    if seed {
        if manifest.publisher.is_some() || manifest.signature.is_some() {
            found.push(
                "a seed item carries no `publisher` or `signature`: compiled-in items are first-party".to_owned(),
            );
        }
        if manifest.rights != ItemRights::first_party() {
            found.push("rights must be `ItemRights::first_party()`, the first-party rights".to_owned());
        }
        if folder != manifest.section {
            found.push(format!("the folder must be the section `{}`", manifest.section));
        }
    }
    found.extend(private_in_text(list, manifest));
    found
}

/// The parent folder's name and the file name (with `.toml`) of a workspace-relative path.
fn folder_and_file(path: &str) -> (&str, &str) {
    let mut parts = path.rsplit('/');
    let file_name = parts.next().unwrap_or_default();
    let folder = parts.next().unwrap_or_default();
    (folder, file_name)
}

/// Private names in the id: a name anywhere in it, or a listed face-profile id as a whole segment.
fn private_in_id(list: &PrivateList, section: &str, segments: &[&str]) -> Vec<String> {
    let id = segments.join("/");
    let mut found: Vec<String> = list
        .named_in(&id)
        .map(|name| format!("names private test material (`{name}`) in its id"))
        .into_iter()
        .collect();
    if let Some(name) = list.face_profile_named(section, segments) {
        found.push(format!(
            "`{name}` is the face profile fitted to private material; give the profile its geometric definition and a new id (library D15)"
        ));
    }
    found
}

/// Private names in the title, tags and origin.
fn private_in_text(list: &PrivateList, manifest: &ItemManifest) -> Vec<String> {
    let fields = std::iter::once(("title", manifest.title.as_str()))
        .chain(manifest.tags.iter().map(|tag| ("tag", tag.as_str())))
        .chain(manifest.origin.as_deref().map(|origin| ("origin", origin)));
    fields
        .filter_map(|(field, text)| {
            list.named_in(text).map(|name| format!("names private test material (`{name}`) in its {field}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &[&str] = &["zorblax"];

    fn list() -> PrivateList {
        PrivateList::new(LIST, &["gribble"])
    }

    fn manifest(text: &str) -> ItemManifest {
        ItemManifest::parse(text).expect("the test manifest parses")
    }

    const HAIR: &str = "schema = 1\nid = \"omuzaki/hair/twintails\"\nversion = \"1.0.0\"\nsection = \"hair\"\nkind = \"part\"\n\
        target = \"character\"\nslot = \"hairstyle\"\ntitle = \"Twintails\"\nsummary = \"Two tails.\"\ntags = [\"hair\"]\n\
        origin = \"generator defaults\"\n[rights]\nauthor = \"Omuzaki\"\nlicence = \"CC0-1.0\"\ncommercial = \"ok\"\nowned = true\n";

    #[test]
    fn a_seed_item_that_meets_every_rule_passes() {
        assert_eq!(
            check("crates/omuzaki-char/library/seed/hair/twintails.toml", true, &manifest(HAIR), &list()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_publisher_or_signature_is_refused_in_a_seed_file() {
        let signed = HAIR.replace("schema = 1", "schema = 1\npublisher = { name = \"Someone\", key = \"ab\" }");
        let found = check("crates/omuzaki-char/library/seed/hair/twintails.toml", true, &manifest(&signed), &list());
        assert!(found.iter().any(|m| m.contains("no `publisher` or `signature`")), "{found:?}");
    }

    #[test]
    fn rights_other_than_the_first_party_ones_are_refused() {
        let other = HAIR.replace("licence = \"CC0-1.0\"", "licence = \"CC-BY-4.0\"");
        let found = check("crates/omuzaki-char/library/seed/hair/twintails.toml", true, &manifest(&other), &list());
        assert!(found.iter().any(|m| m.starts_with("rights must be")), "{found:?}");
    }

    #[test]
    fn a_file_must_be_named_for_its_id_and_sit_in_its_section_folder() {
        let found = check("crates/omuzaki-char/library/seed/skirts/twintails.toml", true, &manifest(HAIR), &list());
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].starts_with("the folder must be the section `hair`"), "{found:?}");
        let renamed = check("crates/omuzaki-char/library/seed/hair/other.toml", true, &manifest(HAIR), &list());
        assert!(renamed[0].starts_with("the file name must be `twintails.toml`"), "{renamed:?}");
    }

    #[test]
    fn a_seed_id_must_be_under_omuzaki() {
        let other = HAIR.replace("omuzaki/hair/twintails", "acme/hair/twintails");
        let found = check("crates/omuzaki-char/library/seed/hair/twintails.toml", true, &manifest(&other), &list());
        assert!(found.iter().any(|m| m.starts_with("a seed id lives under `omuzaki/`")), "{found:?}");
    }

    #[test]
    fn a_private_name_is_refused_in_the_id_title_tag_or_origin() {
        let path = "crates/omuzaki-char/library/seed/hair/twintails.toml";
        let in_title = HAIR.replace("title = \"Twintails\"", "title = \"Zorblax tails\"");
        assert!(
            check(path, true, &manifest(&in_title), &list()).iter().any(|m| m.contains("(`zorblax`) in its title"))
        );
        let in_tag = HAIR.replace("tags = [\"hair\"]", "tags = [\"zorblax\"]");
        assert!(check(path, true, &manifest(&in_tag), &list()).iter().any(|m| m.contains("in its tag")));
        let in_origin = HAIR.replace("origin = \"generator defaults\"", "origin = \"copied from Zorblax\"");
        assert!(check(path, true, &manifest(&in_origin), &list()).iter().any(|m| m.contains("in its origin")));
        let in_id = HAIR.replace("omuzaki/hair/twintails", "omuzaki/hair/zorblax");
        assert!(
            check("crates/omuzaki-char/library/seed/hair/zorblax.toml", true, &manifest(&in_id), &list())
                .iter()
                .any(|m| m.contains("in its id"))
        );
    }

    #[test]
    fn a_face_profile_named_for_the_private_sheet_is_refused_only_in_its_section() {
        let face = "schema = 1\nid = \"omuzaki/face-profiles/gribble\"\nversion = \"1.0.0\"\nsection = \"face-profiles\"\n\
            kind = \"part\"\ntarget = \"character\"\ntitle = \"G\"\nsummary = \"G.\"\n[rights]\nauthor = \"Omuzaki\"\n\
            licence = \"CC0-1.0\"\ncommercial = \"ok\"\nowned = true\n";
        let found =
            check("crates/omuzaki-char/library/seed/face-profiles/gribble.toml", true, &manifest(face), &list());
        assert!(found.iter().any(|m| m.contains("`gribble` is the face profile")), "{found:?}");
    }
}
