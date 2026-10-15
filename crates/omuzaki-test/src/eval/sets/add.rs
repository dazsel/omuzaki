//! `eval sets add` (design 11.2): stores an item and writes its manifest entry, refusing what the rules refuse.
//!
//! The order matters: the file is hashed, then judged (provenance, the private list, by its path and by its hash), and only
//! then copied into the store. An item that is refused never enters the store. The manifest gets the item's hash and its
//! provenance record, and no machine path: the file's own path is used to check the private list and is not written
//! down, since the manifest is public. `--path` records a relative original path explicitly.

use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use omuzaki_store_api::ContentHash;

use super::MANIFEST_DIR;
use super::store::{HASH_PREFIX, SetStore, SetsError, hash_file};
use crate::eval::Discovery;
use crate::eval::provenance::{EvalSet, Item, PrivateList, Provenance, refusals};

/// A set to create with its first item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSet {
    /// The crate whose `evalsets/` folder gets the manifest.
    pub crate_name: String,
    /// `real` or `synthetic`.
    pub kind: String,
    /// `dev` or `holdout`.
    pub split: String,
    /// One line for people.
    pub summary: String,
}

/// What to add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The set's id.
    pub set: String,
    /// The file to store.
    pub file: PathBuf,
    /// The provenance record the manifest will carry.
    pub record: Provenance,
    /// A relative path to record as the item's origin, if any.
    pub recorded_path: Option<String>,
    /// Present when the set does not exist yet.
    pub new_set: Option<NewSet>,
}

/// What `add` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Added {
    /// The item's hash.
    pub hash: ContentHash,
    /// The manifest, relative to the workspace root.
    pub manifest: PathBuf,
    /// `false` when the manifest already named the item.
    pub new_item: bool,
    /// `true` when the set was created.
    pub created_set: bool,
}

fn invalid(text: impl Into<String>) -> SetsError {
    SetsError::Invalid(text.into())
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> SetsError + '_ {
    move |source| SetsError::Io { path: path.to_owned(), source }
}

/// A TOML basic string for `text`, on one line.
fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                // Writing to a `String` cannot fail.
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Whether `path` could name a place on one machine: absolute (either grammar) or climbing out with `..`.
fn is_machine_path(path: &str) -> bool {
    let normal = path.replace('\\', "/");
    let drive = normal.as_bytes().get(1) == Some(&b':');
    normal.starts_with('/') || drive || Path::new(&normal).components().any(|c| matches!(c, Component::ParentDir))
}

fn valid_set_id(id: &str) -> bool {
    !id.is_empty()
        && id.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '.'))
}

/// The manifest text of one item.
fn item_block(hash: &ContentHash, request: &Request) -> String {
    let record = &request.record;
    let mut out = format!("\n[[item]]\nhash = {}\n", quote(&format!("{HASH_PREFIX}{hash}")));
    let mut line = |text: String| {
        out.push_str(&text);
        out.push('\n');
    };
    if let Some(path) = &request.recorded_path {
        line(format!("path = {}", quote(path)));
    }
    line("[item.provenance]".to_owned());
    line(format!("source = {}", quote(record.source.word())));
    line(format!("commercial = {}", quote(&record.commercial)));
    if let Some(consent) = &record.consent {
        line(format!("consent = {}", quote(consent)));
    }
    if record.person {
        line("person = true".to_owned());
    }
    if let Some(licence) = &record.licence {
        line(format!("licence = {}", quote(licence)));
    }
    if let Some(generator) = &record.generator {
        line(format!("generator = {}", quote(generator)));
    }
    out
}

fn header(set: &str, new: &NewSet) -> String {
    format!(
        "id = {}\nkind = {}\nsplit = {}\nsummary = {}\n",
        quote(set),
        quote(&new.kind),
        quote(&new.split),
        quote(&new.summary)
    )
}

/// Where the manifest is and what it holds so far.
struct Target {
    relative: PathBuf,
    text: String,
    created_set: bool,
}

/// Stores the file and writes its manifest entry.
///
/// # Errors
/// [`SetsError::Invalid`] naming every reason the item is refused, or what is wrong with the request; [`SetsError::Io`] when
/// the file, the store or the manifest cannot be read or written.
pub fn add(
    root: &Path,
    discovery: &Discovery,
    private: &PrivateList,
    store: &SetStore,
    request: &Request,
) -> Result<Added, SetsError> {
    if !valid_set_id(&request.set) {
        return Err(invalid(format!("`{}` is not a set id: lowercase letters, digits, dashes and dots", request.set)));
    }
    if let Some(path) = &request.recorded_path
        && is_machine_path(path)
    {
        return Err(invalid(format!(
            "--path `{path}` would put a machine path in a public manifest; give a relative origin"
        )));
    }
    let hash = hash_file(&request.file)?;
    let text_hash = format!("{HASH_PREFIX}{hash}");
    refuse_if_private(private, request, &text_hash)?;

    let target = match find_target(root, discovery, request, &text_hash)? {
        Found::Already(manifest) => {
            store.put_file(&request.file)?;
            return Ok(Added { hash, manifest, new_item: false, created_set: false });
        }
        Found::Target(target) => target,
    };
    let Target { relative, mut text, created_set } = target;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&item_block(&hash, request));
    // The manifest we are about to write must read back as what we mean: the new item last, with this record.
    let parsed: EvalSet = toml::from_str(&text).map_err(|e| invalid(format!("the manifest would not parse: {e}")))?;
    let last = parsed.items.last().ok_or_else(|| invalid("the manifest would have no items"))?;
    if last.hash != text_hash || last.provenance.as_ref() != Some(&request.record) {
        return Err(invalid("the manifest would not read back as the item that was added"));
    }

    let stored = store.put_file(&request.file)?;
    if stored.hash != hash {
        return Err(invalid(format!(
            "the file changed while it was being added ({text_hash} became {HASH_PREFIX}{})",
            stored.hash
        )));
    }
    let path = root.join(&relative);
    if created_set {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(io(dir))?;
        }
        // `create_new`: a set created here never replaces a manifest someone else wrote.
        let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(io(&path))?;
        file.write_all(text.as_bytes()).map_err(io(&path))?;
    } else {
        let staging = path.with_extension("toml.tmp");
        fs::write(&staging, &text).map_err(io(&staging))?;
        fs::rename(&staging, &path).map_err(io(&path))?;
    }
    Ok(Added { hash, manifest: relative, new_item: true, created_set })
}

/// Judges the item before anything is stored: by the file's own path (never written down), by the recorded path, by its
/// hash and by its provenance record.
fn refuse_if_private(private: &PrivateList, request: &Request, text_hash: &str) -> Result<(), SetsError> {
    let judged =
        |path: String| Item { hash: text_hash.to_owned(), path: Some(path), provenance: Some(request.record.clone()) };
    let mut problems = refusals(&judged(request.file.display().to_string()), private);
    if let Some(recorded) = &request.recorded_path {
        problems.extend(refusals(&judged(recorded.clone()), private));
    }
    problems.sort();
    problems.dedup();
    if problems.is_empty() { Ok(()) } else { Err(invalid(format!("refused, and not stored: {}", problems.join("; ")))) }
}

/// Where the item goes.
enum Found {
    /// The manifest already names the item.
    Already(PathBuf),
    /// The manifest to write.
    Target(Target),
}

fn find_target(root: &Path, discovery: &Discovery, request: &Request, text_hash: &str) -> Result<Found, SetsError> {
    let existing = discovery.set_files.iter().find(|(_, set)| set.id == request.set);
    match (existing, &request.new_set) {
        (Some(_), Some(_)) => Err(invalid(format!("eval set `{}` exists already; leave out --new-in", request.set))),
        (None, None) => Err(invalid(format!(
            "no eval set `{}`: create it with --new-in <crate> --kind <real|synthetic> --split <dev|holdout> --summary <text>",
            request.set
        ))),
        (Some((path, set)), None) => {
            let bad = |e: &dyn std::fmt::Display| invalid(format!("{}: set `{}`: {e}", path.display(), set.id));
            set.kind_of().map_err(|e| bad(&e))?;
            set.is_holdout().map_err(|e| bad(&e))?;
            if let Some(item) = set.items.iter().find(|item| item.hash.eq_ignore_ascii_case(text_hash)) {
                // Said again with another record: not a no-op. The manifest's record stands until someone edits it.
                if item.provenance.as_ref() != Some(&request.record) {
                    return Err(invalid(format!(
                        "{text_hash} is already in set `{}` with a different provenance record; edit the manifest if the record is wrong",
                        set.id
                    )));
                }
                return Ok(Found::Already(path.clone()));
            }
            let file = root.join(path);
            let text = fs::read_to_string(&file).map_err(io(&file))?;
            Ok(Found::Target(Target { relative: path.clone(), text, created_set: false }))
        }
        (None, Some(new)) => {
            let draft = header(&request.set, new);
            let set: EvalSet = toml::from_str(&draft).map_err(|e| invalid(format!("the new set is not valid: {e}")))?;
            set.kind_of().map_err(|e| invalid(e.to_string()))?;
            set.is_holdout().map_err(|e| invalid(e.to_string()))?;
            let plain = !new.crate_name.is_empty()
                && new
                    .crate_name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'));
            if !plain {
                return Err(invalid(format!(
                    "`{}` is not a crate name: lowercase letters, digits, dashes and underscores",
                    new.crate_name
                )));
            }
            if !root.join("crates").join(&new.crate_name).is_dir() {
                return Err(invalid(format!("no crate `{}` under crates/", new.crate_name)));
            }
            let relative =
                PathBuf::from("crates").join(&new.crate_name).join(MANIFEST_DIR).join(format!("{}.toml", request.set));
            Ok(Found::Target(Target { relative, text: draft, created_set: true }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_paths_are_recognised_in_either_grammar() {
        for machine in
            ["/home/x/a.png", "C:\\Users\\x\\a.png", "c:/x/a.png", "..\\a.png", "a/../../b", "\\\\server\\share\\a"]
        {
            assert!(is_machine_path(machine), "{machine}");
        }
        for relative in ["sheets/a.png", "a.png", "a/b/c.png"] {
            assert!(!is_machine_path(relative), "{relative}");
        }
    }

    #[test]
    fn a_set_id_is_lowercase_words() {
        for good in ["webcam-real-holdout-1", "sheets.synthetic-1", "a"] {
            assert!(valid_set_id(good), "{good}");
        }
        for bad in ["", "-a", "A", "a b", "a/b", "../a"] {
            assert!(!valid_set_id(bad), "{bad}");
        }
    }

    #[test]
    fn strings_are_quoted_as_toml() {
        assert_eq!(quote("a \"b\"\n"), "\"a \\\"b\\\"\\n\"");
    }
}
