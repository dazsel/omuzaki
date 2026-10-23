//! The pure half of file access: reading a path string in a platform's grammar, with no file system involved.
//!
//! Path text from an op argument or a grant is untrusted, and the two platforms disagree about what it means: `C:x`
//! is "x on the current directory of drive C" on Windows and a file named `C:x` on Linux; `\x` is the root of the
//! current drive on Windows and a file named `\x` on Linux; `file.txt::$DATA` names a stream, and `CON` a device.
//! Guessing wrong turns a check on one path into an access to another, so this module accepts only the forms it can
//! compare exactly, and refuses everything ambiguous. Both grammars compile on every host (the host picks one with
//! [`Style::HOST`]), so a Linux CI run tests the Windows rules too.
//!
//! Nothing here touches the disk. A path is read into a [`Lexical`] place first and compared with the project folder
//! and the granted roots as a [`PathKey`]; only a place that passes is resolved on the disk
//! ([`resolve`](super::resolve)), and the resolved path is compared again.

use std::path::PathBuf;

/// The longest path text accepted, in bytes (Windows allows 32,767 characters; Linux 4,096).
const MAX_BYTES: usize = 32 * 1024;

/// The most names a place may have. A path this deep cannot exist on any file system we run on, and bounding it
/// bounds the work of looking for its longest existing prefix.
const MAX_NAMES: usize = 1024;

/// Which grammar to read a path in: the host's, except in tests that exercise the other one.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Style {
    Posix,
    Windows,
}

impl Style {
    /// The grammar of the machine running Omuzaki.
    pub(crate) const HOST: Self = if cfg!(windows) { Self::Windows } else { Self::Posix };
}

/// Why a path text is not accepted. Every case is a refusal, never a guess.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    Empty,
    Nul,
    /// A Windows-only form (`C:\x`, `\\server\share`, `\x`) on a POSIX host.
    WindowsForm,
    /// `\\.\`, `\\?\` followed by anything but a drive or `UNC`, and `//?/`.
    Device,
    /// `\x` or `/x` on Windows: the root of whichever drive happens to be current.
    RootRelative,
    /// `C:x` on Windows: relative to a per-drive current directory.
    DriveRelative,
    /// A UNC path without both a server and a share.
    IncompleteUnc,
    /// `.`, `..` or an empty name inside a `\\?\` path, where they are literal names and so never match anything real.
    VerbatimDots,
    /// A name Windows treats specially: reserved characters, a trailing dot or space, a device name.
    SpecialName,
    /// A `..` that climbs above the path's root (`/..`, `C:\..`, `\\server\share\..`).
    Escapes,
    /// A relative path, and no project folder to be relative to.
    NoBase,
    /// More text or more names than any real path has ([`MAX_BYTES`], [`MAX_NAMES`]): resolving one would cost more
    /// than any file access is worth, and it can only be an attack.
    TooLong,
}

impl Refusal {
    /// What is wrong with the text, for the error message.
    pub(crate) const fn reason(self) -> &'static str {
        match self {
            Self::Empty => "it is empty",
            Self::Nul => "it contains a NUL character",
            Self::WindowsForm => "it is a Windows path, and this machine uses POSIX paths",
            Self::Device => "it names a device or a raw volume",
            Self::RootRelative => "it is relative to the current drive's root",
            Self::DriveRelative => "it is relative to a drive's current directory",
            Self::IncompleteUnc => "a network path needs a server and a share",
            Self::VerbatimDots => "a `\\\\?\\` path may not contain `.`, `..` or empty names",
            Self::SpecialName => {
                "a name in it is reserved on Windows (a device name, a stream, or reserved characters)"
            }
            Self::Escapes => "it climbs above its root with `..`",
            Self::NoBase => "it is relative, and no project folder is open to resolve it against",
            Self::TooLong => "it is longer than any real path",
        }
    }

    /// How to write the path so it is accepted.
    pub(crate) const fn hint(self) -> &'static str {
        match self {
            Self::WindowsForm => "use the POSIX form of the path (under WSL, `C:\\x` is `/mnt/c/x`)",
            Self::RootRelative | Self::DriveRelative => {
                "write the full path with its drive letter, such as `C:\\dir\\file`"
            }
            Self::Empty | Self::Nul => "pass the path of a file or folder",
            Self::NoBase => "pass an absolute path",
            Self::TooLong => "pass a shorter path",
            Self::Device | Self::IncompleteUnc | Self::VerbatimDots | Self::SpecialName | Self::Escapes => {
                "pass an ordinary absolute path, or a path relative to the project folder"
            }
        }
    }
}

/// Where an absolute path starts. Compared exactly: `C:` and `c:` are the same drive, `\\?\C:` is `C:`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Anchor {
    /// `/` on POSIX.
    Root,
    /// A Windows drive, upper case.
    Drive(char),
    /// `\\server\share` (or `\\?\UNC\server\share`), lower case.
    Unc { server: String, share: String },
}

/// One step of a path below its anchor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Name(String),
    Parent,
}

/// A path text broken into an anchor (absolute paths only) and steps. `.` and repeated separators are gone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Parsed {
    anchor: Option<Anchor>,
    steps: Vec<Step>,
}

impl Parsed {
    /// Reads `raw` in `style`'s grammar.
    ///
    /// # Errors
    /// The [`Refusal`] for any form that cannot be compared exactly.
    pub(crate) fn parse(raw: &str, style: Style) -> Result<Self, Refusal> {
        if raw.is_empty() {
            return Err(Refusal::Empty);
        }
        if raw.len() > MAX_BYTES {
            return Err(Refusal::TooLong);
        }
        if raw.contains('\0') {
            return Err(Refusal::Nul);
        }
        match style {
            Style::Posix => parse_posix(raw),
            Style::Windows => parse_windows(raw),
        }
    }

    /// Where the text starts: its own anchor if it is absolute, else `here`. With [`steps`](Self::steps), this is how
    /// a link's target is followed one step at a time from the folder the link is in.
    pub(crate) fn start(&self, here: &Lexical) -> Lexical {
        match &self.anchor {
            Some(anchor) => Lexical { anchor: anchor.clone(), names: Vec::new() },
            None => here.clone(),
        }
    }

    /// The names and `..` steps after the start, in order (`.` and empty names are gone).
    pub(crate) fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// The place this text names, with `..` collapsed by name and no file system involved. A relative path starts
    /// from `base`. Collapsing is lexical on purpose: it decides what may be touched *before* anything is touched, and
    /// the later resolution on the disk is checked again, so a symlink in front of a `..` cannot widen access.
    ///
    /// # Errors
    /// [`Refusal::NoBase`] for a relative path with no `base`; [`Refusal::Escapes`] when `..` climbs above the root.
    pub(crate) fn lexical(&self, base: Option<&Lexical>) -> Result<Lexical, Refusal> {
        let mut place = match (&self.anchor, base) {
            (Some(anchor), _) => Lexical { anchor: anchor.clone(), names: Vec::new() },
            (None, Some(base)) => base.clone(),
            (None, None) => return Err(Refusal::NoBase),
        };
        for step in &self.steps {
            match step {
                Step::Name(name) => place.names.push(name.clone()),
                Step::Parent => {
                    place.names.pop().ok_or(Refusal::Escapes)?;
                }
            }
        }
        if place.names.len() > MAX_NAMES {
            return Err(Refusal::TooLong);
        }
        Ok(place)
    }
}

/// An absolute place in a path grammar: where it is anchored and the names below, in their original case, with no
/// `.` or `..`. It is what gets compared with the project folder and the roots ([`Lexical::key`]) and, once allowed,
/// what the disk is asked about ([`Lexical::path`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Lexical {
    anchor: Anchor,
    names: Vec<String>,
}

impl Lexical {
    /// The place named by an absolute `raw`, or `None` for anything else (relative, ambiguous, with a `..`).
    pub(crate) fn absolute(raw: &str, style: Style) -> Option<Self> {
        let parsed = Parsed::parse(raw, style).ok()?;
        parsed.steps.iter().all(|step| matches!(step, Step::Name(_))).then_some(())?;
        parsed.lexical(None).ok()
    }

    /// How many names are below the anchor: zero for a filesystem root (`/`, `C:\`) or a share root.
    pub(crate) fn depth(&self) -> usize {
        self.names.len()
    }

    /// The comparable form.
    pub(crate) fn key(&self, style: Style) -> PathKey {
        let fold = |name: &String| if style == Style::Windows { name.to_ascii_lowercase() } else { name.clone() };
        PathKey { anchor: self.anchor.clone(), names: self.names.iter().map(fold).collect() }
    }

    /// The first `keep` names as a path (`keep` is clamped to the depth), spelt the way the anchor's platform does.
    pub(crate) fn path(&self, keep: usize) -> PathBuf {
        let names = &self.names[..keep.min(self.names.len())];
        let (root, separator) = match &self.anchor {
            Anchor::Root => ("/".to_owned(), "/"),
            Anchor::Drive(letter) => (format!("{letter}:\\"), "\\"),
            Anchor::Unc { server, share } => (format!("\\\\{server}\\{share}\\"), "\\"),
        };
        PathBuf::from(format!("{root}{}", names.join(separator)))
    }

    /// The names below the anchor, outermost first.
    pub(crate) fn names(&self) -> &[String] {
        &self.names
    }

    /// The same place with only its first `keep` names (`keep` is clamped to the depth).
    pub(crate) fn truncated(&self, keep: usize) -> Self {
        Self { anchor: self.anchor.clone(), names: self.names[..keep.min(self.names.len())].to_vec() }
    }

    /// The place one name further down.
    pub(crate) fn push(&mut self, name: String) {
        self.names.push(name);
    }

    /// Climbs one name up; `None` (and no change) at the anchor.
    pub(crate) fn pop(&mut self) -> Option<String> {
        self.names.pop()
    }

    /// This place with its first `depth` names replaced by `onto`: the same names below a different folder.
    pub(crate) fn rebased(&self, depth: usize, onto: &Self) -> Self {
        let mut names = onto.names.clone();
        names.extend_from_slice(&self.names[depth.min(self.names.len())..]);
        Self { anchor: onto.anchor.clone(), names }
    }

    /// The first `keep` names as a Windows `\\?\` path (`\\?\C:\a\b`, `\\?\UNC\server\share\a`), which Win32 takes
    /// literally: no `MAX_PATH`, no rewriting of names. The names were checked when the place was parsed (no `.`,
    /// `..`, reserved names or trailing dots), so the literal reading is the same as the ordinary one. The anchor's
    /// root keeps its trailing separator.
    #[cfg(any(windows, test, feature = "fuzzing"))]
    pub(crate) fn verbatim(&self, keep: usize) -> String {
        let names = &self.names[..keep.min(self.names.len())];
        match &self.anchor {
            // Not a Windows place: it has no verbatim form, and its ordinary form is literal already.
            Anchor::Root => format!("/{}", names.join("/")),
            Anchor::Drive(letter) => format!(r"\\?\{letter}:\{}", names.join("\\")),
            Anchor::Unc { server, share } => format!(r"\\?\UNC\{server}\{share}\{}", names.join("\\")),
        }
    }
}

/// An absolute path with no `..`, in a form two paths can be compared in: separators and prefixes unified, and on
/// Windows ASCII case folded. Non-ASCII names must match exactly: Windows folds case with its own table, and
/// guessing at it could call two different folders one. A mismatch only ever refuses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PathKey {
    anchor: Anchor,
    names: Vec<String>,
}

impl PathKey {
    /// The key of `raw`, or `None` unless it is an absolute path that needs no resolving (no `..`, no relative form,
    /// nothing refused by [`Parsed::parse`]).
    pub(crate) fn absolute(raw: &str, style: Style) -> Option<Self> {
        let Parsed { anchor, steps } = Parsed::parse(raw, style).ok()?;
        let names = steps
            .into_iter()
            .map(|step| match step {
                Step::Name(name) => Some(if style == Style::Windows { name.to_ascii_lowercase() } else { name }),
                Step::Parent => None,
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self { anchor: anchor?, names })
    }

    /// Whether this is the top of a whole disk (`/`, `C:\`). A share's top (`\\server\share`) is not: a share is
    /// a thing a person can name and grant on purpose.
    pub(crate) fn is_disk_root(&self) -> bool {
        self.names.is_empty() && !matches!(self.anchor, Anchor::Unc { .. })
    }

    /// Whether `other` is this path or lies below it, name by name (`/a/b` does not cover `/a/bc`).
    pub(crate) fn covers(&self, other: &Self) -> bool {
        self.anchor == other.anchor && other.names.starts_with(&self.names)
    }

    /// How many names are below the anchor.
    pub(crate) fn depth(&self) -> usize {
        self.names.len()
    }

    /// The key one name further down, `name` folded the way `style` compares names.
    pub(crate) fn push(&mut self, name: &str, style: Style) {
        self.names.push(if style == Style::Windows { name.to_ascii_lowercase() } else { name.to_owned() });
    }

    /// Climbs one name up (nothing at the anchor).
    pub(crate) fn pop(&mut self) {
        self.names.pop();
    }
}

fn parse_posix(raw: &str) -> Result<Parsed, Refusal> {
    let bytes = raw.as_bytes();
    let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if drive || raw.starts_with('\\') {
        return Err(Refusal::WindowsForm);
    }
    let (anchor, rest) = raw.strip_prefix('/').map_or((None, raw), |rest| (Some(Anchor::Root), rest));
    let steps = rest
        .split('/')
        .filter(|piece| !piece.is_empty() && *piece != ".")
        .map(|piece| if piece == ".." { Step::Parent } else { Step::Name(piece.to_owned()) })
        .collect();
    Ok(Parsed { anchor, steps })
}

fn is_separator(byte: u8) -> bool {
    byte == b'\\' || byte == b'/'
}

fn parse_windows(raw: &str) -> Result<Parsed, Refusal> {
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 && is_separator(bytes[0]) && is_separator(bytes[1]) {
        return match bytes.get(2) {
            Some(b'?' | b'.') if raw.starts_with(r"\\?\") => parse_verbatim(&raw[4..]),
            Some(b'?' | b'.') => Err(Refusal::Device),
            _ => parse_unc(&raw[2..], false),
        };
    }
    if bytes.first().copied().is_some_and(is_separator) {
        return Err(Refusal::RootRelative);
    }
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        if bytes.len() < 3 || !is_separator(bytes[2]) {
            return Err(Refusal::DriveRelative);
        }
        let steps = parse_steps(&raw[3..], false)?;
        return Ok(Parsed { anchor: Some(Anchor::Drive(char::from(bytes[0].to_ascii_uppercase()))), steps });
    }
    Ok(Parsed { anchor: None, steps: parse_steps(raw, false)? })
}

/// What follows `\\?\`: `C:\...` or `UNC\server\share\...`. Everything else is a raw volume or a device.
fn parse_verbatim(rest: &str) -> Result<Parsed, Refusal> {
    let bytes = rest.as_bytes();
    if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\' {
        let steps = parse_steps(&rest[3..], true)?;
        return Ok(Parsed { anchor: Some(Anchor::Drive(char::from(bytes[0].to_ascii_uppercase()))), steps });
    }
    match rest.split_once('\\') {
        Some((unc, tail)) if unc.eq_ignore_ascii_case("UNC") => parse_unc(tail, true),
        _ => Err(Refusal::Device),
    }
}

/// `server\share\rest...` after the two leading separators.
fn parse_unc(rest: &str, verbatim: bool) -> Result<Parsed, Refusal> {
    let is_sep = |c: char| if verbatim { c == '\\' } else { c == '\\' || c == '/' };
    let mut pieces = rest.splitn(3, is_sep);
    let (Some(server), Some(share)) = (pieces.next(), pieces.next()) else {
        return Err(Refusal::IncompleteUnc);
    };
    if server.is_empty() || share.is_empty() {
        return Err(Refusal::IncompleteUnc);
    }
    // `\\?\UNC\.x\share` is spelt `\\.x\share` in the plain form, which reads as the device namespace (`\\.\`), so a
    // server whose name starts with a dot has no form that round-trips; no host name starts with one (#2246).
    if server.starts_with('.') {
        return Err(Refusal::Device);
    }
    check_windows_name(server)?;
    check_windows_name(share)?;
    let steps = parse_steps(pieces.next().unwrap_or(""), verbatim)?;
    let anchor = Anchor::Unc { server: server.to_ascii_lowercase(), share: share.to_ascii_lowercase() };
    Ok(Parsed { anchor: Some(anchor), steps })
}

/// The names below an anchor. In a `\\?\` path nothing is interpreted: `/` is not a separator and `.`, `..` and empty
/// names are not steps, so they are refused rather than guessed at.
fn parse_steps(rest: &str, verbatim: bool) -> Result<Vec<Step>, Refusal> {
    let is_sep = |c: char| c == '\\' || (!verbatim && c == '/');
    let rest = rest.strip_suffix(is_sep).unwrap_or(rest);
    if rest.is_empty() {
        return Ok(Vec::new());
    }
    let mut steps = Vec::new();
    for piece in rest.split(is_sep) {
        match piece {
            "" | "." | ".." if verbatim => return Err(Refusal::VerbatimDots),
            "" | "." => {}
            ".." => steps.push(Step::Parent),
            name => {
                check_windows_name(name)?;
                steps.push(Step::Name(name.to_owned()));
            }
        }
    }
    Ok(steps)
}

/// Device names Windows opens whatever the folder or extension: `CON.txt` is the console.
const DEVICE_NAMES: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];

/// Refuses a name that Win32 would not take literally: reserved characters (including `:`, which starts a stream),
/// a trailing dot or space (stripped, so `x.` is `x`), and device names with or without an extension.
fn check_windows_name(name: &str) -> Result<(), Refusal> {
    let reserved_char = |c: char| c.is_control() || "<>:\"|?*/\\".contains(c);
    if name.chars().any(reserved_char) || name.ends_with(['.', ' ']) {
        return Err(Refusal::SpecialName);
    }
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    // `COM1` to `COM9`, `LPT1` to `LPT9`, and the superscript digits Windows also counts.
    let numbered = |prefix: &str| {
        let head = stem.get(..prefix.len()).is_some_and(|head| head.eq_ignore_ascii_case(prefix));
        let mut digit = stem.get(prefix.len()..).unwrap_or("").chars();
        let single = matches!((digit.next(), digit.next()), (Some(d), None) if d.is_ascii_digit() || "\u{b9}\u{b2}\u{b3}".contains(d));
        head && single
    };
    if DEVICE_NAMES.iter().any(|device| stem.eq_ignore_ascii_case(device)) || numbered("COM") || numbered("LPT") {
        return Err(Refusal::SpecialName);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(raw: &str) -> Option<PathKey> {
        PathKey::absolute(raw, Style::Windows)
    }

    fn posix(raw: &str) -> Option<PathKey> {
        PathKey::absolute(raw, Style::Posix)
    }

    #[test]
    fn posix_paths_compare_by_whole_names() {
        let root = posix("/home/u/art/").unwrap();
        assert!(root.covers(&posix("/home/u/art").unwrap()));
        assert!(root.covers(&posix("/home/u/art/sub//./x.png").unwrap()));
        assert!(!root.covers(&posix("/home/u/artwork/x.png").unwrap()));
        assert!(!root.covers(&posix("/home/u").unwrap()));
        assert!(posix("/").unwrap().covers(&posix("/etc/passwd").unwrap()));
        assert!(
            !posix("/home/U/art").unwrap().covers(&posix("/home/u/art").unwrap()),
            "POSIX names are case-sensitive"
        );
    }

    #[test]
    fn posix_keys_are_only_made_from_clean_absolute_paths() {
        for refused in
            ["", "relative/x", "/a/../b", "/a/b/..", "/a\0b", "C:\\x", "C:/x", "c:x", "\\\\server\\share", "\\x"]
        {
            assert!(posix(refused).is_none(), "{refused:?}");
        }
        assert!(posix("/a:b/c\\d").is_some(), "a colon or backslash past the first name is an ordinary POSIX name");
        assert!(posix("/c:/x").is_some());
    }

    #[test]
    fn a_windows_form_on_a_posix_host_is_refused_with_a_hint() {
        for raw in ["C:\\Users\\x", "c:/x", "C:x", "\\\\server\\share\\f", "\\\\?\\C:\\x", "\\x"] {
            assert_eq!(Parsed::parse(raw, Style::Posix), Err(Refusal::WindowsForm), "{raw}");
        }
        assert!(Refusal::WindowsForm.hint().contains("/mnt/c"));
    }

    #[test]
    fn windows_drive_paths_ignore_separator_style_and_ascii_case() {
        let root = win(r"C:\Users\U\Art").unwrap();
        assert!(root.covers(&win("c:/users/u/art/x.PNG").unwrap()));
        assert!(root.covers(&win(r"C:\Users\U\Art\sub\\x").unwrap()));
        assert!(!root.covers(&win(r"C:\Users\U\Artwork").unwrap()));
        assert!(!root.covers(&win(r"D:\Users\U\Art").unwrap()), "another drive");
        assert!(win(r"C:\").unwrap().covers(&win(r"C:\Windows").unwrap()));
        assert!(!win(r"C:\").unwrap().covers(&win(r"D:\Windows").unwrap()));
    }

    #[test]
    fn non_ascii_names_must_match_exactly() {
        let kelvin = "\u{212a}"; // Kelvin sign: lower-cases to `k`, yet NTFS keeps it apart from `k`.
        assert!(!win(r"C:\k").unwrap().covers(&win(&format!(r"C:\{kelvin}\x")).unwrap()));
        assert!(win(r"C:\Küche").unwrap().covers(&win(r"C:\Küche\x").unwrap()));
        assert!(!win(r"C:\Küche").unwrap().covers(&win(r"C:\KÜCHE\x").unwrap()), "folding stays ASCII");
    }

    #[test]
    fn verbatim_and_unc_forms_match_their_plain_twins() {
        let plain = win(r"C:\Users\u").unwrap();
        assert!(plain.covers(&win(r"\\?\C:\Users\u\a.txt").unwrap()));
        assert!(win(r"\\?\c:\users").unwrap().covers(&win(r"C:\Users\u").unwrap()));
        let share = win(r"\\Server\Share\dir").unwrap();
        assert!(share.covers(&win(r"//server/share/dir/f").unwrap()));
        assert!(share.covers(&win(r"\\?\UNC\SERVER\share\dir\f").unwrap()));
        assert!(!share.covers(&win(r"\\server\other\dir\f").unwrap()));
        assert!(!share.covers(&win(r"C:\server\share\dir\f").unwrap()));
    }

    #[test]
    fn ambiguous_windows_forms_are_refused() {
        let cases = [
            (r"\\.\C:\x", Refusal::Device),
            (r"\\.\pipe\x", Refusal::Device),
            (r"\\?\Volume{1234}\x", Refusal::Device),
            (r"\\?\GLOBALROOT\Device\x", Refusal::Device),
            (r"\\?\C:", Refusal::Device),
            ("//?/C:/x", Refusal::Device),
            (r"\x", Refusal::RootRelative),
            ("/x", Refusal::RootRelative),
            ("C:x", Refusal::DriveRelative),
            ("C:", Refusal::DriveRelative),
            (r"\\server", Refusal::IncompleteUnc),
            (r"\\server\", Refusal::IncompleteUnc),
            (r"\\\share\x", Refusal::IncompleteUnc),
            (r"\\?\C:\a\..\b", Refusal::VerbatimDots),
            (r"\\?\C:\a\\b", Refusal::VerbatimDots),
            (r"\\?\C:\a/b", Refusal::SpecialName),
        ];
        for (raw, expected) in cases {
            assert_eq!(Parsed::parse(raw, Style::Windows), Err(expected), "{raw}");
        }
    }

    #[test]
    fn windows_names_that_win32_does_not_take_literally_are_refused() {
        for name in [
            "file.txt::$DATA",
            "file:stream",
            "CON",
            "nul.txt",
            "Aux",
            "COM1",
            "lpt9.log",
            "COM\u{b9}",
            "CONOUT$",
            "trailing.",
            "trailing ",
            "what?",
            "a*b",
            "a|b",
            "q\"q",
        ] {
            let raw = format!(r"C:\proj\{name}");
            assert_eq!(Parsed::parse(&raw, Style::Windows), Err(Refusal::SpecialName), "{name}");
        }
        for fine in ["COM", "COM10", "console", "com.txt.d", "PROGRA~1", "a b", ".hidden", "x.tar.gz"] {
            assert!(win(&format!(r"C:\proj\{fine}")).is_some(), "{fine}");
        }
    }

    #[test]
    fn windows_keys_are_only_made_from_clean_absolute_paths() {
        assert!(win(r"C:\a\..\b").is_none());
        assert!(win(r"relative\x").is_none());
        assert!(win("").is_none());
        assert!(win("C:\\a\0b").is_none());
        assert!(win(r"C:\a\.\b").is_some(), "`.` is dropped");
    }

    #[test]
    fn relative_paths_parse_but_have_no_key() {
        let parsed = Parsed::parse(r"assets\..\m.vrm", Style::Windows).unwrap();
        assert_eq!(parsed.lexical(None), Err(Refusal::NoBase));
        let base = Lexical::absolute(r"C:\proj", Style::Windows).unwrap();
        assert_eq!(
            parsed.lexical(Some(&base)).unwrap().key(Style::Windows),
            PathKey::absolute(r"c:\proj\m.vrm", Style::Windows).unwrap()
        );
    }

    #[test]
    fn dot_dot_collapses_by_name_and_may_not_climb_above_the_root() {
        let lex = |raw: &str, style, base: Option<&Lexical>| Parsed::parse(raw, style).unwrap().lexical(base);
        let key = |l: Lexical, style| l.key(style);
        assert_eq!(key(lex("/a/b/../c/./d", Style::Posix, None).unwrap(), Style::Posix), posix("/a/c/d").unwrap());
        assert_eq!(lex("/..", Style::Posix, None), Err(Refusal::Escapes));
        assert_eq!(lex("/a/../..", Style::Posix, None), Err(Refusal::Escapes));
        assert_eq!(lex(r"C:\..", Style::Windows, None), Err(Refusal::Escapes));
        assert_eq!(lex(r"\\srv\share\..\x", Style::Windows, None), Err(Refusal::Escapes));
        let base = Lexical::absolute("/p/q", Style::Posix).unwrap();
        assert_eq!(key(lex("../../x", Style::Posix, Some(&base)).unwrap(), Style::Posix), posix("/x").unwrap());
        assert_eq!(lex("../../../x", Style::Posix, Some(&base)), Err(Refusal::Escapes));
        // An absolute path ignores the base; a relative one needs it.
        assert_eq!(key(lex("/z", Style::Posix, Some(&base)).unwrap(), Style::Posix), posix("/z").unwrap());
        assert_eq!(lex("z", Style::Posix, None), Err(Refusal::NoBase));
    }

    #[test]
    fn a_place_is_spelt_the_way_its_platform_spells_it() {
        let spell = |raw: &str| Lexical::absolute(raw, Style::Windows).unwrap().path(usize::MAX);
        assert_eq!(spell(r"c:/Users//U/Art/"), PathBuf::from(r"C:\Users\U\Art"));
        assert_eq!(spell(r"\\?\C:\Users"), PathBuf::from(r"C:\Users"));
        assert_eq!(spell(r"//Srv/Share/a/b"), PathBuf::from(r"\\srv\share\a\b"));
        assert_eq!(spell(r"C:\"), PathBuf::from(r"C:\"));
        let place = Lexical::absolute("/a/b/c", Style::Posix).unwrap();
        assert_eq!((place.path(0), place.path(2), place.path(9)), ("/".into(), "/a/b".into(), "/a/b/c".into()));
    }

    /// A verbatim network path whose server starts with a dot (the fuzzer's find, #2246) is refused: its plain
    /// spelling `\\.x\...` is the device namespace. The other prefixes that change meaning without the `\\?\` are
    /// refused in both forms already.
    #[test]
    fn a_server_whose_plain_spelling_reads_as_another_place_is_refused() {
        for raw in [
            r"\\?\UNC\.Qv^saerha\H\b",
            r"\\?\UNC\.\share\a",
            r"\\?\UNC\..\share",
            r"\\?\UNC\.srv\share",
            r"\\.srv\share",
            r"//.srv/share",
            r"\\?\UNC\?\share",
            r"\\?\UNC\srv.\share",
            r"\\?\UNC\srv \share",
            r"\\?\UNC\con\share",
            r"\\?\UNC\srv\nul.txt",
            r"\\?\UNC\srv\share.\a",
        ] {
            assert!(Lexical::absolute(raw, Style::Windows).is_none(), "{raw:?} is refused");
            assert_eq!(crate::file_access::fuzzing::check_path(raw), Ok(()), "{raw:?}");
        }
        // A dot inside or after the first character is an ordinary server name.
        for raw in [r"\\?\UNC\srv.lan\share\a", r"\\srv.lan\share\a", r"\\?\UNC\s.\x"] {
            assert_eq!(crate::file_access::fuzzing::check_path(raw), Ok(()), "{raw:?}");
        }
        assert!(Lexical::absolute(r"\\?\UNC\srv.lan\share\a", Style::Windows).is_some());
    }

    #[test]
    fn a_place_is_spelt_verbatim_for_win32() {
        let verbatim = |raw: &str, keep| Lexical::absolute(raw, Style::Windows).unwrap().verbatim(keep);
        assert_eq!(verbatim(r"c:/Users//U/Art/", usize::MAX), r"\\?\C:\Users\U\Art");
        assert_eq!(verbatim(r"C:\Users\U", 1), r"\\?\C:\Users");
        assert_eq!(verbatim(r"C:\Users", 0), r"\\?\C:\", "a drive's root keeps its separator");
        assert_eq!(verbatim(r"//Srv/Share/a/b", usize::MAX), r"\\?\UNC\srv\share\a\b");
        assert_eq!(verbatim(r"\\?\UNC\srv\share", 0), r"\\?\UNC\srv\share\");
        let posix = Lexical::absolute("/a/b", Style::Posix).unwrap();
        assert_eq!((posix.verbatim(usize::MAX), posix.verbatim(0)), ("/a/b".to_owned(), "/".to_owned()));
    }

    #[test]
    fn absurdly_long_paths_are_refused_before_anything_is_built_from_them() {
        let long_text = format!("/{}", "a".repeat(MAX_BYTES));
        assert_eq!(Parsed::parse(&long_text, Style::Posix), Err(Refusal::TooLong));
        let deep = format!("/{}", vec!["a"; MAX_NAMES + 1].join("/"));
        assert_eq!(Parsed::parse(&deep, Style::Posix).unwrap().lexical(None), Err(Refusal::TooLong));
        let just_fits = format!("/{}", vec!["a"; MAX_NAMES].join("/"));
        assert!(Parsed::parse(&just_fits, Style::Posix).unwrap().lexical(None).is_ok());
        // Names pushed past the limit and popped back do not count: only the place at the end does.
        let wobbly = format!("/{}", vec!["a/.."; MAX_BYTES / 8].join("/"));
        assert_eq!(Parsed::parse(&wobbly, Style::Posix).unwrap().lexical(None).map(|p| p.depth()), Ok(0));
    }
}
