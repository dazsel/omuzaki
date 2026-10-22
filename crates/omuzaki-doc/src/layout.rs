//! Which part of the document lives in which file.
//!
//! The tree doesn't say whether an object is a folder or a `.toml` file, so the store keeps that decision here: a
//! map from the document path of every file to what is known about it. It is updated while patches are applied, which
//! is what lets `save` rewrite only the files that changed and lets `apply` refuse names that can't be files.
//!
//! The rules that decide it (file or folder, and what names are allowed) are in the [crate docs](crate).

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound::{Included, Unbounded};

use omuzaki_core::id::{Slug, SlugError};
use omuzaki_doc_api::{DocPath, PatchError, lookup};
use serde_json::{Map, Value};

/// A document path as plain segments (a `BTreeMap` key can borrow as a slice, so lookups don't allocate).
pub(crate) type Segs = Vec<String>;

/// What `save` still owes a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pending {
    /// The file on disk is what the document says.
    Clean,
    /// Part of the file changed: edit the text on disk in place.
    Edit,
    /// Write the file from scratch (new, or replacing one that was deleted and made again).
    Fresh,
}

/// What is known about one file.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Entry {
    /// Whether a file exists at its path right now.
    pub on_disk: bool,
    /// What `save` has to do.
    pub pending: Pending,
    /// Whether it has had content: such a file stays a file (rule 2 in the crate docs).
    pub sticky: bool,
}

/// Why a patch can't be laid out as files.
#[derive(Debug)]
pub(crate) enum LayoutError {
    /// A name that can't become a file or folder.
    BadName {
        /// The entry with the name.
        path: Segs,
        /// What is wrong with it.
        why: NameProblem,
    },
    /// A plain value was added to a folder of files, which would merge them into one file.
    Fold {
        /// The folder.
        folder: Segs,
        /// The key of the plain value.
        value: String,
        /// The files and folders in it that would be merged.
        members: Vec<String>,
    },
}

/// Why a key can't be a file or folder name.
#[derive(Debug)]
pub(crate) enum NameProblem {
    /// Not a slug.
    NotASlug(SlugError),
    /// A device name on Windows.
    Reserved,
}

/// The change to a [`Layout`] that one patch needs, worked out before anything is committed.
#[derive(Debug, Default)]
pub(crate) struct Edit {
    /// Files that no longer exist in the tree.
    drop: Vec<Segs>,
    /// Files that are new, each with whether it has content.
    add: Vec<(Segs, bool)>,
    /// Files that changed, each with whether it has content now.
    touch: Vec<(Segs, bool)>,
}

/// The map of files; see the module docs.
#[derive(Clone, Debug, Default)]
pub(crate) struct Layout {
    files: BTreeMap<Segs, Entry>,
    /// Files on disk whose node is gone; `save` deletes them after writing the new ones.
    deleted: BTreeSet<Segs>,
}

/// The paths a patch makes the layout look at.
#[derive(Default)]
struct Work {
    /// Subtrees to lay out again from scratch.
    roots: BTreeSet<Segs>,
    /// Existing files with a change inside them.
    edits: BTreeSet<Segs>,
    /// Folders that lost an entry and may now be empty.
    fixups: BTreeSet<Segs>,
    /// Folders that were given a plain value, with its key.
    folds: BTreeMap<Segs, String>,
}

impl Layout {
    /// The layout of a project just read from disk: `files` are the document paths of its files, each with whether
    /// it has content.
    pub(crate) fn from_disk(files: impl IntoIterator<Item = (Segs, bool)>) -> Self {
        let files = files
            .into_iter()
            .map(|(path, sticky)| (path, Entry { on_disk: true, pending: Pending::Clean, sticky }))
            .collect();
        Self { files, deleted: BTreeSet::new() }
    }

    /// True if `save` has anything to do.
    pub(crate) fn is_dirty(&self) -> bool {
        !self.deleted.is_empty() || self.files.values().any(|e| e.pending != Pending::Clean)
    }

    /// Every file in the layout, in path order.
    pub(crate) fn entries(&self) -> impl Iterator<Item = (&Segs, &Entry)> {
        self.files.iter()
    }

    /// The files `save` must write, in path order.
    pub(crate) fn pending(&self) -> Vec<(Segs, Entry)> {
        self.files.iter().filter(|(_, e)| e.pending != Pending::Clean).map(|(p, e)| (p.clone(), *e)).collect()
    }

    /// The files `save` must delete, in path order.
    pub(crate) fn deleted(&self) -> Vec<Segs> {
        self.deleted.iter().cloned().collect()
    }

    /// Records that `path` was written.
    pub(crate) fn mark_written(&mut self, path: &[String]) {
        if let Some(entry) = self.files.get_mut(path) {
            entry.on_disk = true;
            entry.pending = Pending::Clean;
        }
    }

    /// Takes the folder that replaced the file `file` on disk as that key's layout: the file's entry goes, and each of
    /// `files` (its files, each with whether it has content) is on disk and clean.
    pub(crate) fn adopt_folder(&mut self, file: &[String], files: impl IntoIterator<Item = (Segs, bool)>) {
        self.files.remove(file);
        self.deleted.remove(file);
        for (path, sticky) in files {
            self.deleted.remove(&path);
            self.files.insert(path, Entry { on_disk: true, pending: Pending::Clean, sticky });
        }
    }

    /// Records that the file for `path` was deleted.
    pub(crate) fn mark_deleted(&mut self, path: &[String]) {
        self.deleted.remove(path);
    }

    /// Works out how the layout must change after the tree changed at `touched`, without changing it.
    ///
    /// # Errors
    /// A new entry would need a file or folder name that can't be one, or a plain value would merge a folder of
    /// files into one file.
    pub(crate) fn plan(&self, root: &Value, touched: &[DocPath]) -> Result<Edit, LayoutError> {
        let mut work = Work::default();
        for path in touched {
            self.classify(root, path, &mut work);
        }
        let roots: BTreeSet<Segs> = work.roots.iter().filter(|r| !has_ancestor_in(&work.roots, r)).cloned().collect();
        let mut edit = Edit::default();
        for r in &roots {
            if let Some(value) = work.folds.get(r) {
                self.refuse_fold(root, r, value)?;
            }
            self.replan(root, r, &mut edit)?;
        }
        for q in work.edits.iter().filter(|q| !is_covered(&roots, q)) {
            edit.touch.push((q.clone(), has_content(lookup(root, &doc_path(q)))));
        }
        for a in work.fixups.iter().filter(|a| !is_covered(&roots, a)) {
            // A folder that lost its last entry would vanish on reopen, so it becomes an empty file.
            if matches!(lookup(root, &doc_path(a)), Some(Value::Object(m)) if m.is_empty()) {
                edit.add.push((a.clone(), false));
            }
        }
        Ok(edit)
    }

    /// Applies an [`Edit`] from [`plan`](Self::plan).
    pub(crate) fn commit(&mut self, edit: Edit) {
        for path in edit.drop {
            if self.files.remove(&path).is_some_and(|e| e.on_disk) {
                self.deleted.insert(path);
            }
        }
        for (path, sticky) in edit.add {
            let on_disk = self.deleted.remove(&path);
            self.files.insert(path, Entry { on_disk, pending: Pending::Fresh, sticky });
        }
        for (path, content) in edit.touch {
            if let Some(entry) = self.files.get_mut(&path) {
                if entry.pending == Pending::Clean {
                    entry.pending = Pending::Edit;
                }
                entry.sticky |= content;
            }
        }
    }

    /// The file that holds `path`, if `path` is a file or lies inside one. At most one can: files don't nest, so it
    /// is the last file at or before `path` in order, if it is a prefix.
    fn owner(&self, path: &[String]) -> Option<(&Segs, &Entry)> {
        self.files
            .range::<[String], _>((Unbounded, Included(path)))
            .next_back()
            .filter(|(file, _)| path.starts_with(file))
    }

    /// True if a file is at `path` or under it.
    fn exists(&self, path: &[String]) -> bool {
        self.files
            .range::<[String], _>((Included(path), Unbounded))
            .next()
            .is_some_and(|(file, _)| file.starts_with(path))
    }

    /// Decides what a change at `path` means for the layout.
    fn classify(&self, root: &Value, path: &DocPath, work: &mut Work) {
        let segs = path.segments();
        let parent: Segs = path.split_last().map(|(p, _)| p.segments().to_vec()).unwrap_or_default();
        let owner = self.owner(segs);
        if let Some((file, entry)) = owner.filter(|(file, _)| file.len() < segs.len()) {
            return Self::classify_in_file(root, file, *entry, work); // a change inside an existing file
        }
        match (lookup(root, path), owner) {
            (Some(Value::Object(_)), Some((file, entry))) => Self::classify_in_file(root, file, *entry, work),
            (Some(Value::Object(_)), None) => {
                work.roots.insert(segs.to_vec());
            }
            (None, _) => {
                work.roots.insert(segs.to_vec());
                if !parent.is_empty() {
                    work.fixups.insert(parent);
                }
            }
            // A number or list can only live in a file, so the parent has to be one.
            (Some(_), _) => {
                if let Some((_, key)) = path.split_last().filter(|_| !parent.is_empty()) {
                    work.folds.insert(parent.clone(), key.to_owned());
                    work.roots.insert(parent);
                }
            }
        }
    }

    fn classify_in_file(root: &Value, file: &Segs, entry: Entry, work: &mut Work) {
        // A file that was empty has no layout of its own (rule 4 in the crate docs): once it holds only objects it
        // becomes a folder.
        if !entry.sticky && lookup(root, &doc_path(file)).is_some_and(wants_folder) {
            work.roots.insert(file.clone());
        } else {
            work.edits.insert(file.clone());
        }
    }

    /// Lays out the subtree at `at` again and records the difference from what is known.
    fn replan(&self, root: &Value, at: &Segs, edit: &mut Edit) -> Result<(), LayoutError> {
        let mut planned = BTreeMap::new();
        if let Some(Value::Object(map)) = lookup(root, &doc_path(at)) {
            self.check_new_name(at)?;
            self.plan_node(&mut at.clone(), map, &mut planned)?;
        }
        let under = self.files.range::<[String], _>((Included(at.as_slice()), Unbounded));
        for (path, _) in under.take_while(|(p, _)| p.starts_with(at)) {
            if !planned.contains_key(path) {
                edit.drop.push(path.clone());
            }
        }
        for (path, content) in planned {
            if self.files.contains_key(&path) {
                edit.touch.push((path, content));
            } else {
                edit.add.push((path, content));
            }
        }
        Ok(())
    }

    /// Refuses to make the folder of files at `folder` a single file because `value` was added next to them.
    ///
    /// Merging would rebuild the content of every file from the tree: their comments are lost, and so is the type of
    /// a value like a TOML date. Nothing is lost if the files are being removed by the same patch, so only files
    /// whose content is still in the tree count.
    fn refuse_fold(&self, root: &Value, folder: &Segs, value: &str) -> Result<(), LayoutError> {
        let under = self.files.range::<[String], _>((Included(folder.as_slice()), Unbounded));
        let members: BTreeSet<String> = under
            .take_while(|(file, _)| file.starts_with(folder))
            .filter(|(file, _)| lookup(root, &doc_path(file)).is_some())
            .filter_map(|(file, _)| file.get(folder.len()).cloned())
            .collect();
        if members.is_empty() {
            return Ok(());
        }
        Err(LayoutError::Fold {
            folder: folder.clone(),
            value: value.to_owned(),
            members: members.into_iter().collect(),
        })
    }

    /// A node that isn't in the project yet needs a name that works as a file name.
    fn check_new_name(&self, path: &[String]) -> Result<(), LayoutError> {
        let problem = path.last().and_then(|name| name_problem(name));
        match problem {
            Some(why) if !self.exists(path) => Err(LayoutError::BadName { path: path.to_vec(), why }),
            _ => Ok(()),
        }
    }

    /// Decides, for the object `map` at `path`, whether it is a file or a folder (rules 1 to 5 in the crate docs) and collects the
    /// files under it into `out`, each with whether it has content.
    fn plan_node(
        &self,
        path: &mut Segs,
        map: &Map<String, Value>,
        out: &mut BTreeMap<Segs, bool>,
    ) -> Result<(), LayoutError> {
        let must_be_file = map.is_empty() || map.values().any(|v| !v.is_object());
        let stays_file = self.files.get(path.as_slice()).is_some_and(|e| e.sticky);
        if must_be_file || stays_file {
            out.insert(path.clone(), !map.is_empty());
            return Ok(());
        }
        if let Some((name, why)) = map.keys().find_map(|k| self.unusable_name(path, k).map(|why| (k, why))) {
            if self.exists(path) {
                path.push(name.clone());
                return Err(LayoutError::BadName { path: path.clone(), why });
            }
            out.insert(path.clone(), true);
            return Ok(());
        }
        for (key, value) in map {
            if let Value::Object(child) = value {
                path.push(key.clone());
                self.plan_node(path, child, out)?;
                path.pop();
            }
        }
        Ok(())
    }

    /// Why `name` can't be an entry of the folder at `path`, unless it is there already.
    fn unusable_name(&self, path: &[String], name: &str) -> Option<NameProblem> {
        let why = name_problem(name)?;
        let mut child = path.to_vec();
        child.push(name.to_owned());
        (!self.exists(&child)).then_some(why)
    }
}

impl LayoutError {
    /// The patch error to report.
    pub(crate) fn into_patch_error(self) -> PatchError {
        let path = match &self {
            Self::BadName { path, .. } => doc_path(path),
            Self::Fold { folder, value, .. } => doc_path(folder).join(value.as_str()),
        };
        PatchError::Unreachable { path, reason: self.to_string() }
    }
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadName { path, why } => {
                let name = path.last().map_or("", String::as_str);
                write!(f, "'{name}' would become a file or folder name, but {why}; rename it")
            }
            Self::Fold { folder, value, members } => write!(
                f,
                "'{}' is a folder of files ({}), so the plain value '{value}' can't go next to them: merging them into \
                 one file would lose their comments and formatting. Put the value inside one of those files, or make \
                 it an object",
                folder.join("/"),
                members.join(", ")
            ),
        }
    }
}

impl std::fmt::Display for NameProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotASlug(why) => write!(f, "file and folder names must be slugs ({why})"),
            Self::Reserved => f.write_str("Windows reserves it for devices (con, prn, aux, nul, com1-9, lpt1-9)"),
        }
    }
}

/// Why `name` can't be the name of a file or folder the store creates, if it can't.
///
/// Slugs are the rule; reserved device names are checked here too, first, so this layer holds on its own whatever
/// `Slug` accepts (the project is opened on Windows).
fn name_problem(name: &str) -> Option<NameProblem> {
    if is_windows_reserved(name) {
        return Some(NameProblem::Reserved);
    }
    Slug::new(name).err().map(NameProblem::NotASlug)
}

/// True for the names Windows treats as devices, in any case and with any extension (`con`, `NUL.txt`, `com1.toml`).
fn is_windows_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").trim_end().to_ascii_lowercase();
    let numbered = |prefix: &str| stem.strip_prefix(prefix).is_some_and(|n| matches!(n.as_bytes(), [b'1'..=b'9']));
    matches!(stem.as_str(), "con" | "prn" | "aux" | "nul") || numbered("com") || numbered("lpt")
}

/// The [`DocPath`] for plain segments.
pub(crate) fn doc_path(segs: &[String]) -> DocPath {
    segs.iter().fold(DocPath::root(), |path, seg| path.join(seg.as_str()))
}

/// A non-empty object whose entries are all objects: the only kind of node that can be a folder.
fn wants_folder(value: &Value) -> bool {
    matches!(value, Value::Object(m) if !m.is_empty() && m.values().all(Value::is_object))
}

/// True if `value` is an object with entries.
fn has_content(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::Object(m)) if !m.is_empty())
}

/// True if a strict prefix of `path` is in `set`.
fn has_ancestor_in(set: &BTreeSet<Segs>, path: &[String]) -> bool {
    (1..path.len()).any(|n| set.contains(&path[..n]))
}

/// True if `path` or a prefix of it is in `set`.
fn is_covered(set: &BTreeSet<Segs>, path: &[String]) -> bool {
    (1..=path.len()).any(|n| set.contains(&path[..n]))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn p(s: &str) -> DocPath {
        DocPath::parse(s).unwrap()
    }

    fn segs(s: &str) -> Segs {
        s.split('/').map(str::to_owned).collect()
    }

    /// A layout for a project whose files are `files` (with content), plus `doc` as the tree.
    fn setup(files: &[&str]) -> Layout {
        Layout::from_disk(files.iter().map(|f| (segs(f), true)))
    }

    fn apply(layout: &mut Layout, root: &Value, touched: &[&str]) -> Result<(), LayoutError> {
        let touched: Vec<DocPath> = touched.iter().map(|t| p(t)).collect();
        let edit = layout.plan(root, &touched)?;
        layout.commit(edit);
        Ok(())
    }

    fn files(layout: &Layout) -> Vec<String> {
        layout.files.keys().map(|k| k.join("/")).collect()
    }

    fn pending(layout: &Layout) -> Vec<(String, Pending)> {
        layout.pending().into_iter().map(|(k, e)| (k.join("/"), e.pending)).collect()
    }

    #[test]
    fn an_edit_inside_a_file_dirties_only_that_file() {
        let mut l = setup(&["project", "characters/d/face", "characters/d/rig"]);
        let doc = json!({"project": {}, "characters": {"d": {"face": {"eyes": {"size": 2}}, "rig": {"a": 1}}}});
        apply(&mut l, &doc, &["characters/d/face/eyes/size"]).unwrap();
        assert_eq!(pending(&l), [("characters/d/face".to_owned(), Pending::Edit)]);
    }

    #[test]
    fn a_new_entity_becomes_a_folder_of_files() {
        let mut l = setup(&["project"]);
        let doc = json!({"project": {}, "characters": {"d": {"face": {"profile": "round"}, "look": {"outline": 1}}}});
        apply(&mut l, &doc, &["characters"]).unwrap();
        assert_eq!(files(&l), ["characters/d/face", "characters/d/look", "project"]);
        assert!(pending(&l).iter().all(|(_, s)| *s == Pending::Fresh));
    }

    #[test]
    fn removing_a_file_deletes_it_and_an_emptied_folder_becomes_an_empty_file() {
        let mut l = setup(&["characters/d/face"]);
        let doc = json!({"characters": {"d": {}}});
        apply(&mut l, &doc, &["characters/d/face"]).unwrap();
        assert_eq!(files(&l), ["characters/d"]);
        assert_eq!(l.deleted(), [segs("characters/d/face")]);
        assert!(!l.files[&segs("characters/d")].sticky);
    }

    #[test]
    fn an_empty_file_turns_into_a_folder_when_it_gains_object_entries() {
        let mut l = Layout::from_disk([(segs("variants"), false)]);
        let doc = json!({"variants": {"game": {"scale": 2}}});
        apply(&mut l, &doc, &["variants/game"]).unwrap();
        assert_eq!(files(&l), ["variants/game"]);
        assert_eq!(l.deleted(), [segs("variants")]);
    }

    #[test]
    fn a_file_with_content_stays_a_file_even_if_it_holds_only_tables() {
        let mut l = setup(&["scenes/s1"]);
        let doc = json!({"scenes": {"s1": {"stage": {"w": 1}, "cam": {"fov": 2}}}});
        apply(&mut l, &doc, &["scenes/s1/cam"]).unwrap();
        assert_eq!(files(&l), ["scenes/s1"]);
        assert_eq!(l.deleted(), Vec::<Segs>::new());
    }

    #[test]
    fn a_plain_value_next_to_the_files_of_a_folder_is_refused_not_merged() {
        let mut l = setup(&["characters/d/face", "characters/d/rig"]);
        let doc = json!({"characters": {"d": {"face": {"a": 1}, "rig": {"b": 1}, "nickname": "D"}}});
        let err = apply(&mut l, &doc, &["characters/d/nickname"]).unwrap_err();
        assert!(matches!(&err, LayoutError::Fold { folder, value, members }
            if folder == &segs("characters/d") && value == "nickname" && members == &["face", "rig"]));
        let PatchError::Unreachable { path, reason } = err.into_patch_error() else { panic!("wrong error") };
        assert_eq!(path.to_string(), "characters/d/nickname");
        assert!(reason.contains("face, rig") && reason.contains("inside one of those files"), "{reason}");
        assert_eq!(files(&l), ["characters/d/face", "characters/d/rig"], "nothing was committed");
    }

    #[test]
    fn a_plain_value_is_fine_when_the_same_patch_removes_the_files_it_would_merge() {
        let mut l = setup(&["characters/d/face", "characters/d/rig"]);
        let doc = json!({"characters": {"d": {"nickname": "D"}}});
        apply(&mut l, &doc, &["characters/d/face", "characters/d/rig", "characters/d/nickname"]).unwrap();
        assert_eq!(files(&l), ["characters/d"]);
        assert_eq!(l.deleted().len(), 2);
    }

    #[test]
    fn a_new_node_with_a_plain_value_is_a_file_and_needs_no_merge() {
        let mut l = setup(&["characters/d/face"]);
        let doc = json!({"characters": {"d": {"face": {"a": 1}}, "e": {"face": {"a": 1}, "nickname": "E"}}});
        apply(&mut l, &doc, &["characters/e"]).unwrap();
        assert_eq!(files(&l), ["characters/d/face", "characters/e"]);
    }

    #[test]
    fn a_file_that_gained_content_stays_a_file_after_it_loses_its_plain_values() {
        // Starts empty, so it has no layout of its own (sticky = false); a plain value gives it one.
        let mut l = Layout::from_disk([(segs("a"), false)]);
        apply(&mut l, &json!({"a": {"x": 1}}), &["a/x"]).unwrap();
        assert!(l.files[&segs("a")].sticky);
        // Now it holds only tables; it must not be turned into a folder behind the user's back.
        apply(&mut l, &json!({"a": {"t": {"u": 1}}}), &["a/x", "a/t"]).unwrap();
        assert_eq!(files(&l), ["a"]);
        assert_eq!(l.deleted(), Vec::<Segs>::new());
    }

    #[test]
    fn device_names_are_refused_as_file_and_folder_names() {
        for name in
            ["con", "CON", "aux", "nul", "prn", "com1", "COM9", "lpt1", "lpt9", "con.toml", "nul.txt", "aux ", "Com5.x"]
        {
            assert!(is_windows_reserved(name), "{name}");
        }
        for name in ["console", "com0", "com10", "lpt", "lpt0", "communicate", "mira", "a.con", ""] {
            assert!(!is_windows_reserved(name), "{name}");
        }
        let mut l = setup(&["characters/d/face"]);
        let doc = json!({"characters": {"d": {"face": {"a": 1}}, "con": {"face": {"a": 1}}}});
        let err = apply(&mut l, &doc, &["characters/con"]).unwrap_err();
        assert!(err.to_string().contains("Windows reserves it"), "{err}");

        // Inside a new file the same word is just a key; as an entry of an existing folder it is refused.
        let doc = json!({"characters": {"d": {"face": {"a": 1}}, "e": {"con": {"a": {"b": 1}}}}});
        apply(&mut l, &doc, &["characters/e"]).unwrap();
        assert_eq!(files(&l), ["characters/d/face", "characters/e"]);
        let doc = json!({"characters": {"d": {"face": {"a": 1}, "aux": {"a": 1}}}});
        assert!(apply(&mut l, &doc, &["characters/d/aux"]).is_err());
    }

    #[test]
    fn bad_names_are_refused_for_new_entries_in_a_folder_but_make_new_nodes_files() {
        let mut l = setup(&["characters/d/face"]);
        let doc = json!({"characters": {"d": {"face": {"a": 1}}, "Big Boss": {"face": {"a": 1}}}});
        assert!(apply(&mut l, &doc, &["characters/Big Boss"]).is_err());

        let doc = json!({"characters": {"d": {"face": {"a": 1}, "My Face": {"a": 1}}}});
        assert!(apply(&mut l, &doc, &["characters/d/My Face"]).is_err());

        // A new node with such a key is simply a file.
        let doc = json!({"characters": {"d": {"face": {"a": 1}}, "e": {"My Face": {"a": 1}}}});
        apply(&mut l, &doc, &["characters/e"]).unwrap();
        assert_eq!(files(&l), ["characters/d/face", "characters/e"]);
    }

    #[test]
    fn loaded_names_are_kept_even_if_they_are_not_slugs() {
        let mut l = setup(&["characters/Old Name/face", "characters/d/face"]);
        let doc = json!({"characters": {"Old Name": {"face": {"a": 2}}, "d": {"face": {"a": 1}}}});
        apply(&mut l, &doc, &["characters/Old Name/face/a"]).unwrap();
        apply(&mut l, &doc, &["characters"]).unwrap();
        assert_eq!(files(&l), ["characters/Old Name/face", "characters/d/face"]);
        assert_eq!(l.deleted(), Vec::<Segs>::new());
    }

    #[test]
    fn a_recreated_file_is_written_from_scratch_over_the_old_one() {
        let mut l = setup(&["a/x"]);
        apply(&mut l, &json!({"a": {}}), &["a/x"]).unwrap(); // a/x gone; a becomes an empty file
        apply(&mut l, &json!({"a": {"x": {"k": 1}}}), &["a/x"]).unwrap(); // a/x is back
        assert_eq!(files(&l), ["a/x"]);
        let entry = l.files[&segs("a/x")];
        assert!(entry.on_disk && entry.pending == Pending::Fresh);
        assert_eq!(l.deleted(), Vec::<Segs>::new());
    }

    #[test]
    fn the_file_for_a_path_is_found_with_one_lookup() {
        let l = setup(&["a/b", "a/c/d", "z"]);
        assert_eq!(l.owner(&segs("a/c/d/e/f")).map(|(f, _)| f.join("/")), Some("a/c/d".to_owned()));
        assert!(l.owner(&segs("a/c")).is_none());
        assert!(l.owner(&segs("a/bb")).is_none());
        assert!(l.exists(&segs("a/c")) && !l.exists(&segs("b")));
    }
}
