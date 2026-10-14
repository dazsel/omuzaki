//! The loader: one TOML file per template in `library/scores/`, embedded, validated and parsed once.

use std::sync::OnceLock;

use omuzaki_anim_api::{Score, ScoreError};
use serde::Deserialize;

/// The built-in files, in the order they are listed. Adding a template is adding a file and a line here.
const BUILTIN: [(&str, &str); 9] = [
    ("intro-wave", include_str!("../../library/scores/intro-wave.toml")),
    ("outro", include_str!("../../library/scores/outro.toml")),
    ("kiss-mwah", include_str!("../../library/scores/kiss-mwah.toml")),
    ("laugh", include_str!("../../library/scores/laugh.toml")),
    ("gasp", include_str!("../../library/scores/gasp.toml")),
    ("sad", include_str!("../../library/scores/sad.toml")),
    ("smug", include_str!("../../library/scores/smug.toml")),
    ("shy", include_str!("../../library/scores/shy.toml")),
    ("angry", include_str!("../../library/scores/angry.toml")),
];

/// What every `origin` starts with: templates are our own designs (library design D15), never measured from a reference.
const ORIGIN_PREFIX: &str = "design target: ";

/// One template as written in `library/scores/<id>.toml`: a [`Score`] plus its header.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScoreTemplate {
    /// The name (`a-z`, `0-9`, `-`, `_`), unique in the library.
    pub id: String,
    /// What a person sees in a picker.
    pub title: String,
    /// Search words.
    pub tags: Vec<String>,
    /// The template's own version.
    pub version: String,
    /// Where the design came from: `design target: <one line>`.
    pub origin: String,
    /// The performance.
    pub score: Score,
}

/// Why a template file can't be loaded. Each says which file to fix.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ScoreTemplateError {
    /// The file is not valid TOML of the expected layout.
    #[error("score template {file}: {message}")]
    Parse {
        /// The label of the file.
        file: String,
        /// What the parser said.
        message: String,
    },
    /// A header field is empty or malformed.
    #[error("score template {file}: {message}")]
    Header {
        /// The label of the file.
        file: String,
        /// Which rule it breaks.
        message: String,
    },
    /// A key or cue lies outside `0..=duration`, or the duration is not positive.
    #[error("score template {file}: {what} is outside 0..={duration} s")]
    OutOfRange {
        /// The label of the file.
        file: String,
        /// The key or cue.
        what: String,
        /// The score's duration.
        duration: f64,
    },
    /// The score does not compile.
    #[error("score template {file}: {source}")]
    Score {
        /// The label of the file.
        file: String,
        /// What the compiler said.
        source: ScoreError,
    },
    /// Two files use the same id.
    #[error("score template id {0:?} is used twice")]
    DuplicateId(String),
}

/// A set of templates with unique ids, in a stable order.
#[derive(Clone, Debug, PartialEq)]
pub struct ScoreTemplates {
    templates: Vec<ScoreTemplate>,
}

/// The built-in set, or why it failed to load, parsed once.
static BUILTIN_TEMPLATES: OnceLock<Result<ScoreTemplates, ScoreTemplateError>> = OnceLock::new();

impl ScoreTemplates {
    /// Parses template files (TOML). `sources` pairs a label for error messages with the file's text.
    ///
    /// # Errors
    /// The first [`ScoreTemplateError`]: bad TOML or layout, a bad header, a key or cue outside the score, a score that
    /// does not compile, a repeated id.
    pub fn parse(sources: &[(&str, &str)]) -> Result<Self, ScoreTemplateError> {
        let mut templates: Vec<ScoreTemplate> = Vec::with_capacity(sources.len());
        for (label, text) in sources {
            let file = (*label).to_owned();
            let parse_error = |message: String| ScoreTemplateError::Parse { file: file.clone(), message };
            let value: toml::Value = toml::from_str(text).map_err(|e| parse_error(e.to_string()))?;
            check_score_keys(&value).map_err(parse_error)?;
            let template: ScoreTemplate = value.try_into().map_err(|e| parse_error(e.to_string()))?;
            validate(&file, &template)?;
            if templates.iter().any(|t| t.id == template.id) {
                return Err(ScoreTemplateError::DuplicateId(template.id));
            }
            templates.push(template);
        }
        Ok(Self { templates })
    }

    /// The templates that ship with Omuzaki, embedded and parsed once.
    ///
    /// # Errors
    /// Only if the embedded files are broken, which the crate's tests rule out.
    pub fn builtin() -> Result<&'static Self, ScoreTemplateError> {
        BUILTIN_TEMPLATES.get_or_init(|| Self::parse(&BUILTIN)).as_ref().map_err(Clone::clone)
    }

    /// Every template, in file order.
    pub fn all(&self) -> &[ScoreTemplate] {
        &self.templates
    }

    /// The template called `id`.
    pub fn get(&self, id: &str) -> Option<&ScoreTemplate> {
        self.templates.iter().find(|t| t.id == id)
    }
}

/// Keys a cue may carry besides `time` and `type`, per cue type.
const CUE_KEYS: [(&str, &[&str]); 3] = [
    ("clip", &["clip", "weight", "fade_s"]),
    ("expression", &["expression", "intensity", "hold_s", "fade_s"]),
    ("marker", &["name"]),
];

/// Refuses keys the score types would silently ignore. The `Score` types in `omuzaki-anim-api` do not deny unknown
/// fields, so a typo such as `ovre` for `over` would parse as a snap; the files are checked here instead.
fn check_score_keys(root: &toml::Value) -> Result<(), String> {
    fn table<'a>(v: &'a toml::Value, what: &str) -> Result<&'a toml::Table, String> {
        v.as_table().ok_or_else(|| format!("{what} must be a table"))
    }
    fn known(t: &toml::Table, allowed: &[&str], what: &str) -> Result<(), String> {
        match t.keys().find(|k| !allowed.contains(&k.as_str())) {
            Some(key) => Err(format!("unknown field `{key}` in {what}")),
            None => Ok(()),
        }
    }
    fn items<'a>(t: &'a toml::Table, key: &str) -> &'a [toml::Value] {
        t.get(key).and_then(toml::Value::as_array).map_or(&[], Vec::as_slice)
    }

    // A missing `score` is left to the typed parse, which names it.
    let Some(score) = root.get("score") else { return Ok(()) };
    let score = table(score, "score")?;
    known(score, &["duration", "tracks", "cues"], "score")?;
    for (i, track) in items(score, "tracks").iter().enumerate() {
        let track = table(track, "a track")?;
        known(track, &["channel", "initial", "keys"], &format!("track {i}"))?;
        for (j, key) in items(track, "keys").iter().enumerate() {
            known(table(key, "a key")?, &["time", "value", "over", "ease"], &format!("key {j} of track {i}"))?;
        }
    }
    for (i, cue) in items(score, "cues").iter().enumerate() {
        let cue = table(cue, "a cue")?;
        let kind = cue.get("type").and_then(toml::Value::as_str).unwrap_or_default();
        let Some((_, extra)) = CUE_KEYS.iter().find(|(name, _)| *name == kind) else { continue };
        let allowed: Vec<&str> = ["time", "type"].into_iter().chain(extra.iter().copied()).collect();
        known(cue, &allowed, &format!("cue {i}"))?;
    }
    Ok(())
}

fn validate(file: &str, t: &ScoreTemplate) -> Result<(), ScoreTemplateError> {
    let header = |message: String| ScoreTemplateError::Header { file: file.to_owned(), message };
    let id_ok = !t.id.is_empty()
        && t.id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if !id_ok {
        return Err(header(format!("id {:?} is not a valid name (a-z, 0-9, -, _)", t.id)));
    }
    if t.title.trim().is_empty() || t.version.trim().is_empty() {
        return Err(header("title and version must not be empty".into()));
    }
    if t.tags.iter().any(|tag| tag.trim().is_empty()) {
        return Err(header("a tag is empty".into()));
    }
    if t.origin.strip_prefix(ORIGIN_PREFIX).is_none_or(|rest| rest.trim().is_empty()) {
        return Err(header(format!("origin must read `{ORIGIN_PREFIX}<one line>`")));
    }

    let duration = t.score.duration.0;
    let out_of_range = |what: String| ScoreTemplateError::OutOfRange { file: file.to_owned(), what, duration };
    // A NaN time fails `contains`, so it is rejected below too; `compile` names any other non-finite number.
    let inside = |time: f64| (0.0..=duration).contains(&time);
    if duration.is_nan() || duration <= 0.0 {
        return Err(out_of_range("the duration".into()));
    }
    for track in &t.score.tracks {
        if let Some(i) = track.keys.iter().position(|k| !inside(k.time.0)) {
            return Err(out_of_range(format!("key {i} of {}", track.channel)));
        }
    }
    if let Some(i) = t.score.cues.iter().position(|c| !inside(c.time.0)) {
        return Err(out_of_range(format!("cue {i}")));
    }
    t.score.compile().map_err(|source| ScoreTemplateError::Score { file: file.to_owned(), source })?;
    Ok(())
}
