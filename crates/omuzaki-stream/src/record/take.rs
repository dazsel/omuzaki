//! What a take is on disk: its name, its files and its manifest (`docs/design/record.md` D6, D7, §6).
//!
//! A take is named `<UTC date>_<UTC time>[_<name>]`, for example `2026-10-21_23-55-01_intro`. Its video and its
//! audio tracks sit beside each other, `<stem>.webm` (or the folder `<stem>/` of PNG frames) and `<stem>_<track>.wav`,
//! with `<stem>.take.json` describing all of them, so an editor-side tool finds every part without guessing.
#![allow(
    clippy::disallowed_methods,
    reason = "the recorder writes only inside the take folder under `record.dir`, a Machine setting no agent may write; no op passes a path in"
)]

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use omuzaki_stream_api::error::StreamError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::settings::RecordFormat;

/// The longest take name, in characters.
pub const MAX_NAME_CHARS: usize = 64;

/// Checks a take name: one file-name component of letters, digits, spaces, `-`, `_` and `.`, not starting with `.`,
/// at most [`MAX_NAME_CHARS`] characters. Spaces become `_`.
///
/// # Errors
/// [`StreamError::Config`] naming the rule the name breaks.
pub fn check_name(name: &str) -> Result<String, StreamError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(StreamError::Config("the take name is empty".into()));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(StreamError::Config(format!("the take name is longer than {MAX_NAME_CHARS} characters")));
    }
    if name.starts_with('.') {
        return Err(StreamError::Config("the take name starts with a dot".into()));
    }
    if let Some(bad) = name.chars().find(|c| !(c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))) {
        return Err(StreamError::Config(format!("the take name holds {bad:?}; use letters, digits, spaces, - _ .")));
    }
    Ok(name.replace(' ', "_"))
}

/// The stem of a take started at `at`, with an optional checked name.
pub fn stem(at: SystemTime, name: Option<&str>) -> String {
    let (date, time) = date_time(at);
    match name {
        Some(name) => format!("{date}_{time}_{name}"),
        None => format!("{date}_{time}"),
    }
}

/// The UTC date and time of `at` as file-name parts: `2026-10-21` and `23-55-01`.
pub fn date_time(at: SystemTime) -> (String, String) {
    let secs = at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (y, mo, d) = civil_from_days(i64::try_from(secs / 86_400).unwrap_or(0));
    let rem = secs % 86_400;
    (format!("{y:04}-{mo:02}-{d:02}"), format!("{:02}-{:02}-{:02}", rem / 3600, rem / 60 % 60, rem % 60))
}

/// The default `record.name_template`: the UTC date and time, then the take's name when it has one.
pub const DEFAULT_TEMPLATE: &str = "{date}_{time}_{name}";
/// The placeholders a name template may hold.
pub const PLACEHOLDERS: [&str; 6] = ["date", "time", "scene", "take", "format", "name"];
/// The longest stem, in bytes.
pub const MAX_STEM_BYTES: usize = 120;
/// The longest path a take's stem may make, in characters: Windows' 260 with room for a suffix such as `_mic.wav`.
pub const MAX_PATH_CHARS: usize = 240;

/// Why a name template or a take's path was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum NameError {
    /// A placeholder that isn't one of [`PLACEHOLDERS`].
    #[error("{{{found}}} is not a placeholder; did you mean {{{nearest}}}?")]
    UnknownPlaceholder {
        /// What the template holds.
        found: String,
        /// The placeholder it is nearest to.
        nearest: &'static str,
    },
    /// A `{` with no `}` after it.
    #[error("a {{ without its }}")]
    Unclosed,
    /// The template makes an empty name.
    #[error("the template makes an empty name")]
    Empty,
    /// The take's path is longer than [`MAX_PATH_CHARS`].
    #[error("the take's path is {chars} characters, over {MAX_PATH_CHARS}; choose a shorter folder or template")]
    TooLong {
        /// Its length.
        chars: usize,
    },
}

/// What a name template's placeholders stand for.
#[derive(Clone, Copy, Debug)]
pub struct NameContext<'a> {
    /// `{date}`: the UTC date, `2026-10-21`.
    pub date: &'a str,
    /// `{time}`: the UTC time, `23-55-01`.
    pub time: &'a str,
    /// `{scene}`: the scene's name, empty when the caller has none.
    pub scene: &'a str,
    /// `{take}`: the take's number in its folder, written with at least three digits.
    pub take: u32,
    /// `{format}`: the format's short name (`webm`, `png`).
    pub format: RecordFormat,
    /// `{name}`: the take's checked name, if any.
    pub name: Option<&'a str>,
}

/// One piece of a template.
enum Piece<'a> {
    Text(&'a str),
    Slot(&'static str),
}

/// Splits a template into text and placeholders.
fn pieces(template: &str) -> Result<Vec<Piece<'_>>, NameError> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        if open > 0 {
            out.push(Piece::Text(&rest[..open]));
        }
        let after = &rest[open + 1..];
        let close = after.find('}').ok_or(NameError::Unclosed)?;
        let found = &after[..close];
        let slot = PLACEHOLDERS
            .iter()
            .find(|p| **p == found)
            .ok_or_else(|| NameError::UnknownPlaceholder { found: found.to_owned(), nearest: nearest(found) })?;
        out.push(Piece::Slot(slot));
        rest = &after[close + 1..];
    }
    if !rest.is_empty() {
        out.push(Piece::Text(rest));
    }
    Ok(out)
}

/// The placeholder nearest to `found` by edit distance.
fn nearest(found: &str) -> &'static str {
    PLACEHOLDERS.iter().copied().min_by_key(|p| distance(found, p)).unwrap_or("name")
}

/// Edit distance over characters, a swap of two neighbours counting as one edit (optimal string alignment), so
/// `{tkae}` is nearest to `{take}`.
fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut rows = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in rows.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in rows[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let change = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (rows[i - 1][j] + 1).min(rows[i][j - 1] + 1).min(rows[i - 1][j - 1] + change);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(rows[i - 2][j - 2] + 1);
            }
            rows[i][j] = best;
        }
    }
    rows[a.len()][b.len()]
}

/// Checks a name template: only [`PLACEHOLDERS`], every `{` closed, and it names something.
///
/// # Errors
/// The [`NameError`] the template breaks.
pub fn check_template(template: &str) -> Result<(), NameError> {
    if pieces(template)?.is_empty() {
        return Err(NameError::Empty);
    }
    Ok(())
}

/// Characters that only separate parts of a name: an empty placeholder takes one of them with it.
fn separator(c: char) -> bool {
    matches!(c, '_' | '-' | '.' | ' ')
}

/// The stem `template` makes for `ctx`, as one safe file-name component. An empty placeholder (no scene, no name)
/// takes a separator with it, so `{date}_{time}_{name}` with no name is `2026-10-21_23-55-01`.
///
/// # Errors
/// The [`NameError`] the template breaks, or [`NameError::Empty`] when nothing is left.
pub fn file_stem(template: &str, ctx: &NameContext<'_>) -> Result<String, NameError> {
    let mut out = String::new();
    let mut eat_separator = false;
    for piece in pieces(template)? {
        let text = match piece {
            Piece::Text(t) => t.to_owned(),
            Piece::Slot("date") => ctx.date.to_owned(),
            Piece::Slot("time") => ctx.time.to_owned(),
            Piece::Slot("scene") => ctx.scene.to_owned(),
            Piece::Slot("take") => format!("{:03}", ctx.take),
            Piece::Slot("format") => format_name(ctx.format).to_owned(),
            Piece::Slot(_) => ctx.name.unwrap_or_default().to_owned(),
        };
        let text = if eat_separator { text.trim_start_matches(separator).to_owned() } else { text };
        eat_separator = false;
        if text.is_empty() && matches!(piece, Piece::Slot(_)) {
            if out.ends_with(separator) {
                out.pop();
            } else {
                eat_separator = true;
            }
            continue;
        }
        out.push_str(&text);
    }
    let stem = sanitise_component(out.trim_matches(separator));
    if stem.is_empty() { Err(NameError::Empty) } else { Ok(stem) }
}

/// The short name of a format in a file name.
fn format_name(format: RecordFormat) -> &'static str {
    match format {
        RecordFormat::Auto => "auto",
        RecordFormat::PngSequence => "png",
        RecordFormat::WebmVp9Alpha => "webm",
    }
}

/// Windows' reserved device names, which no file may be called, with or without an extension.
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2",
    "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// `s` as one file-name component every system accepts: `<>:"/\|?*` and control characters become `_`, trailing dots
/// and spaces go, a Windows reserved name (`CON`, `com1.txt`) gets a `_` after its base, and it is cut to
/// [`MAX_STEM_BYTES`] on a character boundary.
pub fn sanitise_component(s: &str) -> String {
    let mut out: String =
        s.chars()
            .map(|c| {
                if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                    '_'
                } else {
                    c
                }
            })
            .collect();
    let base_len = out.find('.').unwrap_or(out.len());
    if RESERVED.iter().any(|r| out[..base_len].eq_ignore_ascii_case(r)) {
        out.insert(base_len, '_');
    }
    if out.len() > MAX_STEM_BYTES {
        let cut = (0..=MAX_STEM_BYTES).rev().find(|&i| out.is_char_boundary(i)).unwrap_or(0);
        out.truncate(cut);
    }
    out.trim_end_matches(['.', ' ']).to_owned()
}

/// Checks that a take named `stem` in `dir` stays under [`MAX_PATH_CHARS`].
///
/// # Errors
/// [`NameError::TooLong`] when it doesn't.
pub fn check_path(dir: &Path, stem: &str) -> Result<(), NameError> {
    let chars = dir.join(stem).to_string_lossy().chars().count();
    if chars > MAX_PATH_CHARS { Err(NameError::TooLong { chars }) } else { Ok(()) }
}

/// The number the next take in `dir` gets: one more than the takes it already holds (their manifests).
pub fn take_number(dir: &Path) -> u32 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 1 };
    let takes = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_str().is_some_and(|n| n.to_ascii_lowercase().ends_with(".take.json")))
        .count();
    u32::try_from(takes).unwrap_or(u32::MAX - 1).saturating_add(1)
}

/// The proleptic Gregorian date of a day count since 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// The most numbered stems [`unique_stem`] tries before it gives up and returns the last.
pub const MAX_STEM_TRIES: u32 = 10_000;

/// A stem not yet used in `dir`: `stem`, else `stem_2`, `stem_3`, … up to [`MAX_STEM_TRIES`].
pub fn unique_stem(dir: &Path, stem: &str) -> String {
    let taken = |s: &str| {
        dir.join(s).exists() || dir.join(format!("{s}.webm")).exists() || dir.join(format!("{s}.take.json")).exists()
    };
    if !taken(stem) {
        return stem.to_owned();
    }
    (2..=MAX_STEM_TRIES)
        .map(|n| format!("{stem}_{n}"))
        .find(|s| !taken(s))
        .unwrap_or_else(|| format!("{stem}_{MAX_STEM_TRIES}"))
}

/// Why a take ended.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StopCause {
    /// Someone stopped it.
    Requested,
    /// A write failed; what reached the disk is kept and readable.
    WriteFailed,
    /// The disk was about to fill (`record.min_free_mb`); the take was closed cleanly and is readable.
    DiskLow,
}

/// Why a stretch of an audio track is silence.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GapCause {
    /// Samples were lost on the way (the reader fell behind).
    Lost,
    /// The source was muted.
    Muted,
    /// The source was private (co-host only), so nothing of it is recorded.
    Private,
}

/// One silent stretch of a track.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Gap {
    /// Where it starts, in seconds from the start of the take.
    pub at_s: f64,
    /// How long it lasts, in seconds.
    pub len_s: f64,
    /// Why.
    pub cause: GapCause,
}

/// One audio track of a take.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TrackReport {
    /// The track's name (`mic`, `program`).
    pub name: String,
    /// Its files, one per part.
    pub files: Vec<PathBuf>,
    /// Sample frames written.
    pub frames: u64,
    /// Sample rate in Hz.
    pub rate: u32,
    /// Silent stretches and why.
    pub gaps: Vec<Gap>,
}

/// The take manifest, `<stem>.take.json`, and what `record.stop` answers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TakeReport {
    /// Always `"take"` (a saved replay says `"replay"`).
    pub kind: String,
    /// The take's stem.
    pub stem: String,
    /// The format written.
    pub format: RecordFormat,
    /// The video: the `.webm` file, or the folder of PNG frames.
    pub video: PathBuf,
    /// The encoder that made the video (`png`, `libvpx-vp9`).
    pub encoder: String,
    /// Frame size in pixels.
    pub size: [u32; 2],
    /// Frames per second as `[numerator, denominator]`.
    pub fps: [u32; 2],
    /// Frames in the take, repeats included.
    pub frames: u64,
    /// Frames that could not be encoded in time and repeat the frame before them.
    pub dropped_frames: u64,
    /// Seconds of video.
    pub duration_s: f64,
    /// Bytes on disk, video and audio.
    pub bytes: u64,
    /// The audio tracks.
    pub tracks: Vec<TrackReport>,
    /// Why it ended.
    pub stop_cause: StopCause,
    /// The manifest's own path.
    pub manifest: PathBuf,
}

impl TakeReport {
    /// Writes the manifest to [`TakeReport::manifest`] (to a temporary name first, so a crash never leaves half a
    /// file).
    ///
    /// # Errors
    /// [`StreamError::Io`] if it cannot be written.
    pub fn write(&self) -> Result<(), StreamError> {
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| StreamError::Io(format!("serialising the manifest: {e}")))?;
        let tmp = self.manifest.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| StreamError::Io(format!("could not write {}: {e}", tmp.display())))?;
        std::fs::rename(&tmp, &self.manifest)
            .map_err(|e| StreamError::Io(format!("could not write {}: {e}", self.manifest.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn context<'a>(scene: &'a str, name: Option<&'a str>) -> NameContext<'a> {
        NameContext { date: "2026-10-13", time: "21-04-05", scene, take: 7, format: RecordFormat::WebmVp9Alpha, name }
    }

    #[test]
    fn a_template_fills_its_placeholders_into_a_safe_stem() {
        let template = "{date}_{time}_{scene}_take{take}";
        assert_eq!(
            file_stem(template, &context("Main/Desk", None)).as_deref(),
            Ok("2026-10-13_21-04-05_Main_Desk_take007")
        );
        assert_eq!(file_stem("{format}-{take}", &context("", None)).as_deref(), Ok("webm-007"));
        // The default: an empty name takes its separator with it.
        assert_eq!(file_stem(DEFAULT_TEMPLATE, &context("", None)).as_deref(), Ok("2026-10-13_21-04-05"));
        assert_eq!(
            file_stem(DEFAULT_TEMPLATE, &context("", Some("intro"))).as_deref(),
            Ok("2026-10-13_21-04-05_intro")
        );
        assert_eq!(file_stem("{scene}_{date}", &context("", None)).as_deref(), Ok("2026-10-13"));
        assert_eq!(file_stem("{scene}{name}", &context("", None)), Err(NameError::Empty));
    }

    #[test]
    fn a_template_refuses_unknown_and_unclosed_placeholders() {
        assert_eq!(
            check_template("{date}_{scene2}"),
            Err(NameError::UnknownPlaceholder { found: "scene2".into(), nearest: "scene" })
        );
        assert_eq!(check_template("{date"), Err(NameError::Unclosed));
        assert_eq!(check_template(""), Err(NameError::Empty));
        assert_eq!(check_template(DEFAULT_TEMPLATE), Ok(()));
        let message = check_template("{tkae}").map_err(|e| e.to_string());
        assert_eq!(message, Err("{tkae} is not a placeholder; did you mean {take}?".into()));
    }

    #[test]
    fn components_are_sanitised_for_every_system() {
        assert_eq!(sanitise_component("CON.txt"), "CON_.txt");
        assert_eq!(sanitise_component("com7"), "com7_");
        assert_eq!(sanitise_component("console"), "console");
        assert_eq!(sanitise_component("name. "), "name");
        assert_eq!(sanitise_component("a<b>c:d\"e/f\\g|h?i*j\tk"), "a_b_c_d_e_f_g_h_i_j_k");
        let long = "シ".repeat(200);
        let cut = sanitise_component(&long);
        assert!(cut.len() <= MAX_STEM_BYTES && cut.chars().all(|c| c == 'シ'), "{cut}");
        assert_eq!(cut.len(), 120);
        assert_eq!(sanitise_component(&format!("{}x", "a".repeat(119))).len(), 120);
    }

    #[test]
    fn a_path_over_the_windows_limit_is_refused() {
        let dir = Path::new("/recordings");
        assert_eq!(check_path(dir, "take"), Ok(()));
        let chars = dir.join("x".repeat(240)).to_string_lossy().chars().count();
        assert_eq!(check_path(dir, &"x".repeat(240)), Err(NameError::TooLong { chars }));
    }

    #[test]
    fn stems_read_as_utc_date_and_time() {
        let at = UNIX_EPOCH + Duration::from_secs(1_792_626_901); // 2026-10-21 23:55:01 UTC
        assert_eq!(stem(at, None), "2026-10-21_23-55-01");
        assert_eq!(stem(at, Some("intro")), "2026-10-21_23-55-01_intro");
        assert_eq!(stem(UNIX_EPOCH, None), "1970-01-01_00-00-00");
        let leap = UNIX_EPOCH + Duration::from_hours(264_384); // 2000-02-29 00:00:00 UTC
        assert_eq!(stem(leap, None), "2000-02-29_00-00-00");
    }

    #[test]
    fn names_are_one_safe_component() {
        assert_eq!(check_name(" my take ").ok().as_deref(), Some("my_take"));
        assert!(check_name("").is_err());
        assert!(check_name("../up").is_err());
        assert!(check_name("a/b").is_err());
        assert!(check_name(".hidden").is_err());
        assert!(check_name(&"x".repeat(MAX_NAME_CHARS + 1)).is_err());
        assert_eq!(check_name("Szene-2_v1.0").ok().as_deref(), Some("Szene-2_v1.0"));
    }

    #[test]
    fn a_taken_stem_gets_a_number() -> Result<(), std::io::Error> {
        let dir = tempfile::tempdir()?;
        assert_eq!(unique_stem(dir.path(), "a"), "a");
        std::fs::write(dir.path().join("a.take.json"), "{}")?;
        assert_eq!(unique_stem(dir.path(), "a"), "a_2");
        std::fs::create_dir(dir.path().join("a_2"))?;
        assert_eq!(unique_stem(dir.path(), "a"), "a_3");
        Ok(())
    }
}
