//! Who runs an op, why, and what they're allowed to do (spec §6: provenance and permissions).

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Scope;
use crate::file_access::{PathKey, Style};

/// Who is acting. Logged with every applied op.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Actor {
    /// A person at the UI or CLI.
    Person {
        /// Display name.
        name: String,
    },
    /// An AI agent (MCP client or the built-in assistant).
    Agent {
        /// Stable agent or session id.
        id: String,
        /// The model, when known (e.g. `claude-opus-5-5`).
        model: Option<String>,
    },
    /// A plugin.
    Plugin {
        /// Plugin id.
        id: String,
    },
    /// Omuzaki itself (migrations, automation rules).
    System,
}

/// Why an op ran: links history back to the prompt, message or rule that caused it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Cause {
    /// The kind of cause.
    pub kind: CauseKind,
    /// The prompt, message or rule text, when there is one.
    pub text: Option<String>,
}

/// The kinds of [`Cause`]. `#[non_exhaustive]` (ADR 0054 §4): a `match` outside this crate needs a wildcard arm, so a
/// new kind never breaks it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CauseKind {
    /// A direct UI or CLI action.
    Direct,
    /// An agent prompt.
    Prompt,
    /// A chat message or redeem.
    Chat,
    /// An automation rule.
    Rule,
    /// A script.
    Script,
    /// Replaying a log or recording.
    Replay,
    /// A review comment (ADR 0056, `docs/design/comments.md` §6): the cause's text is the comment's id only, never its
    /// words, so no comment text lands in History labels.
    Review,
    /// A plugin's entry call (ADR 0058): the cause's text is `<id>@<version> <entry>` (`example@0.1.0 init`), naming
    /// the entry and never its arguments. Its own kind because `Script` never queues and `Rule` is automation's.
    Plugin,
    /// A key, pad, MIDI or OSC binding the person set up (PF-05): the binding dispatcher runs its op as the person.
    Binding,
}

/// The scopes an actor holds, plus file-path grants for [`Scope::Fs`]. A person holds `fs` but no roots, so a path
/// outside the project folder needs a root added with [`Grants::with_fs_root`] first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Grants {
    scopes: BTreeSet<Scope>,
    fs_roots: Vec<String>,
}

impl Grants {
    /// Grants exactly these scopes.
    pub fn of(scopes: impl IntoIterator<Item = Scope>) -> Self {
        Self { scopes: scopes.into_iter().collect(), fs_roots: Vec::new() }
    }

    /// The default for people: everything (§6).
    pub fn person() -> Self {
        Self::of(Scope::ALL)
    }

    /// The default for agents: read, edit, render and review (comment and reply, design `comments.md` §3: a comment is
    /// a plain data node, and resolving, deleting and approving stay with people). `broadcast`, `publish` and `spend`
    /// need a per-session grant (§6).
    pub fn agent_default() -> Self {
        Self::of([
            Scope::Read,
            Scope::EditCharacter,
            Scope::EditScene,
            Scope::EditLibrary,
            Scope::EditSettings,
            Scope::Render,
            Scope::Review,
        ])
    }

    /// Adds a scope (a per-session grant).
    #[must_use]
    pub fn with(mut self, scope: Scope) -> Self {
        self.scopes.insert(scope);
        self
    }

    /// Allows file access under `root` (implies [`Scope::Fs`]). `root` must be an absolute path with no `..` and no
    /// symlinks in it, and not the top of a whole disk, `/` or `C:\` (ADR 0013); a network share's top is fine; anything else
    /// covers nothing. The checked path has its symlinks resolved, so a root that
    /// reaches its folder through a symlink never matches: grant the folder's real path.
    #[must_use]
    pub fn with_fs_root(mut self, root: impl Into<String>) -> Self {
        self.scopes.insert(Scope::Fs);
        self.fs_roots.push(root.into());
        self
    }

    /// What both `self` and `other` allow, never more than either (design §3.3): scopes intersect. A file root
    /// survives only where the other side also holds `fs` and, if that side names roots, one of them covers it (a
    /// person holds `fs` with no roots, so a session's roots pass through unchanged). Symmetric.
    #[must_use]
    pub fn intersect(&self, other: &Grants) -> Grants {
        let scopes: BTreeSet<Scope> = self.scopes.intersection(&other.scopes).copied().collect();
        let mut fs_roots = Vec::new();
        if scopes.contains(&Scope::Fs) {
            for root in self.roots_within(other).chain(other.roots_within(self)) {
                if !fs_roots.contains(&root) {
                    fs_roots.push(root);
                }
            }
        }
        Grants { scopes, fs_roots }
    }

    /// The roots of `self` that `limit` lets through: all of them when `limit` holds `fs` without roots, else those
    /// a root of `limit` covers. A root that is not a clean absolute path covers nothing, so it is dropped.
    fn roots_within<'a>(&'a self, limit: &'a Grants) -> impl Iterator<Item = String> + 'a {
        self.fs_roots
            .iter()
            .filter(move |root| {
                limit.allows(Scope::Fs)
                    && (limit.fs_roots.is_empty()
                        || PathKey::absolute(root, Style::HOST).is_some_and(|key| limit.covers(&key, Style::HOST)))
            })
            .cloned()
    }

    /// The scopes granted, in a fixed order.
    pub fn scopes(&self) -> impl Iterator<Item = Scope> + '_ {
        self.scopes.iter().copied()
    }

    /// The file roots granted (see [`with_fs_root`](Self::with_fs_root)).
    pub fn fs_roots(&self) -> &[String] {
        &self.fs_roots
    }

    /// These grants without `scope`. Removing `fs` removes the roots with it.
    #[must_use]
    pub fn without(mut self, scope: Scope) -> Self {
        self.scopes.remove(&scope);
        if scope == Scope::Fs {
            self.fs_roots.clear();
        }
        self
    }

    /// True if `scope` is granted.
    pub fn allows(&self, scope: Scope) -> bool {
        self.scopes.contains(&scope)
    }

    /// True if `path` lies under a granted file root. Paths are compared name by name, so `/a/bc` is not under `/a/b`,
    /// and on Windows `C:\A` and `c:/a` are the same place. Callers must pass canonical paths (symlinks resolved):
    /// a relative path, a `..`, or text this machine cannot read exactly (a Windows form on Linux, a device name) is
    /// never covered. Ops do not call this: [`OpCtx::fs_access`](crate::OpCtx::fs_access) resolves the path first.
    pub fn allows_path(&self, path: &str) -> bool {
        self.allows_path_as(path, Style::HOST)
    }

    /// [`allows_path`](Self::allows_path) in an explicit path grammar, so tests can check the other platform's.
    pub(crate) fn allows_path_as(&self, path: &str, style: Style) -> bool {
        PathKey::absolute(path, style).is_some_and(|path| self.covers(&path, style))
    }

    /// Whether `fs` is held and a root, read in `style`'s grammar, covers `path`. A root that is not a clean absolute
    /// path in that grammar covers nothing.
    pub(crate) fn covers(&self, path: &PathKey, style: Style) -> bool {
        self.roots(style).any(|root| root.covers(path))
    }

    /// The roots that count, read in `style`'s grammar: none without `fs`, and never one that is not a clean absolute
    /// path or is the top of a whole disk.
    pub(crate) fn roots(&self, style: Style) -> impl Iterator<Item = PathKey> + '_ {
        let fs = self.allows(Scope::Fs);
        self.fs_roots
            .iter()
            .filter(move |_| fs)
            .filter_map(move |root| PathKey::absolute(root, style))
            .filter(|root| !root.is_disk_root())
    }

    /// No scopes and no roots: what an [`OpCtx`](crate::OpCtx) holds until it is given the request's grants.
    pub(crate) const fn none() -> Self {
        Self { scopes: BTreeSet::new(), fs_roots: Vec::new() }
    }
}

/// The grants of an [`OpCtx`](crate::OpCtx) that was never given any: nothing is allowed.
pub(crate) static NO_GRANTS: Grants = Grants::none();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_need_explicit_grants_for_risky_scopes() {
        let g = Grants::agent_default();
        assert!(g.allows(Scope::EditCharacter) && g.allows(Scope::EditSettings) && g.allows(Scope::Render));
        assert!(!g.allows(Scope::Publish) && !g.allows(Scope::Spend) && !g.allows(Scope::Broadcast));
        assert!(g.allows(Scope::Review), "agents may comment and reply by default (ADR 0056)");
        assert!(Grants::person().allows(Scope::Review) && !Grants::of([Scope::Read]).allows(Scope::Review));
        assert!(g.with(Scope::Publish).allows(Scope::Publish));
        assert!(Grants::person().allows(Scope::Native));
    }

    // The path grammar is named in each test (`allows_path_as`), so the Windows rules run on every host.
    const POSIX: Style = Style::Posix;
    const WINDOWS: Style = Style::Windows;

    #[test]
    fn fs_roots_match_whole_components() {
        let g = Grants::default().with_fs_root("/home/u/art/");
        assert!(g.allows_path_as("/home/u/art", POSIX) && g.allows_path_as("/home/u/art/x.png", POSIX));
        assert!(!g.allows_path_as("/home/u/artwork/x.png", POSIX) && !g.allows_path_as("/etc/passwd", POSIX));
        assert!(!Grants::default().allows_path_as("/home/u/art/x.png", POSIX));
        assert_eq!(g.allows_path("/home/u/art/x.png"), cfg!(unix), "the host's grammar decides for allows_path");
    }

    #[test]
    fn a_path_that_is_not_canonical_is_never_covered() {
        let g = Grants::default().with_fs_root("/home/u/art");
        for path in ["/home/u/art/../../etc/passwd", "/home/u/art/..", "home/u/art/x", "art/x", "", "/home/u/art/x\0"] {
            assert!(!g.allows_path_as(path, POSIX), "{path:?}");
        }
        // A root that is not a clean absolute path grants nothing, rather than something wider.
        for root in ["", "art", "/home/u/art/..", "/home/u/art/../.."] {
            let g = Grants::default().with_fs_root(root);
            assert!(!g.allows_path_as("/home/u/art/x", POSIX) && !g.allows_path_as("/etc/passwd", POSIX), "{root:?}");
        }
    }

    #[test]
    fn windows_roots_and_paths_compare_whatever_their_spelling() {
        let g = Grants::default().with_fs_root(r"C:\Users\U\Art");
        assert!(g.allows_path_as(r"C:\Users\U\Art\x.png", WINDOWS));
        assert!(g.allows_path_as("c:/users/u/art/x.png", WINDOWS));
        assert!(g.allows_path_as(r"\\?\C:\Users\u\ART\sub\x.png", WINDOWS), "what canonicalize returns on Windows");
        for outside in [
            r"C:\Users\U\Artwork\x.png",
            r"D:\Users\U\Art\x.png",
            r"C:\Users\U\Art\..\Other\x.png",
            r"C:Users\U\Art\x.png",
            r"\\Users\U\Art\x.png",
            r"\\.\C:\Users\U\Art\x.png",
            r"C:\Users\U\Art\CON",
            r"C:\Users\U\Art\x.png::$DATA",
        ] {
            assert!(!g.allows_path_as(outside, WINDOWS), "{outside}");
        }
        assert!(!Grants::of([Scope::Read]).allows_path_as(r"C:\Users\U\Art\x.png", WINDOWS), "no fs scope");
    }

    #[test]
    fn a_root_on_a_whole_disk_covers_nothing() {
        for (root, path, style) in [
            ("/", "/etc/passwd", POSIX),
            ("//", "/etc/passwd", POSIX),
            (r"C:\", r"C:\Users\u\x", WINDOWS),
            (r"\\?\C:\", r"C:\x", WINDOWS),
            (r"\\NAS\Models", r"\\nas\models\x", WINDOWS),
        ] {
            let covered = Grants::default().with_fs_root(root).allows_path_as(path, style);
            assert_eq!(covered, root.contains("Models"), "{root}");
        }
    }

    #[test]
    fn a_unc_root_covers_its_share_only() {
        let g = Grants::default().with_fs_root(r"\\NAS\Models");
        assert!(g.allows_path_as(r"\\nas\models\a\b.glb", WINDOWS));
        assert!(g.allows_path_as(r"\\?\UNC\NAS\Models\b.glb", WINDOWS));
        assert!(!g.allows_path_as(r"\\nas\modelsX\b.glb", WINDOWS));
        assert!(!g.allows_path_as(r"\\nas\other\b.glb", WINDOWS));
    }

    #[test]
    fn a_windows_root_grants_nothing_on_a_posix_host() {
        let g = Grants::default().with_fs_root(r"C:\Users\u");
        assert!(!g.allows_path_as(r"C:\Users\u\x", POSIX) && !g.allows_path_as("/mnt/c/Users/u/x", POSIX));
    }
}
