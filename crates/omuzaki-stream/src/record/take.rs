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
    let secs = at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (y, mo, d) = civil_from_days(i64::try_from(secs / 86_400).unwrap_or(0));
    let rem = secs % 86_400;
    let date = format!("{y:04}-{mo:02}-{d:02}_{:02}-{:02}-{:02}", rem / 3600, rem / 60 % 60, rem % 60);
    match name {
        Some(name) => format!("{date}_{name}"),
        None => date,
    }
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
