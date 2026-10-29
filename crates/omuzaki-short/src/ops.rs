//! The `short.*` ops: `short.render` (a job: a short file to a finished video under the project), `short.plan` (a
//! query: the timeline and its measures against the reference bar, without rendering) and `short.draft` (a query: a
//! plain script to a directed short file). A short is given as a file of the project (`path`, a `*.short.toml`) or
//! inline (`short`), so an agent can iterate on one without writing files.

use std::io::Read as _;
use std::sync::Mutex;

use omuzaki_char_api::{BuildServices, CharId, CharacterBuilder, paths as char_paths};
use omuzaki_core::time::SystemClock;
use omuzaki_doc_api::DocRead;
use omuzaki_ops_api::{CostHint, FsMode, JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_puppet::autorig::autorig;
use omuzaki_puppet::bundle::{self, Bundle};
use omuzaki_puppet::layers::LayeredArt;
use omuzaki_render_api::{Capabilities, MtoonSupport, Renderer};
use omuzaki_stage::body::{Body, BodyServices, load_body};
use omuzaki_stage::ops::render::job::{RenderQuality, character_bytes, check_size, cost};
use omuzaki_stage::recipe::LightRig;
use omuzaki_stage::scene::alone::{ShotChoice, StagedScene, cast_alone, post_in_look, stage_alone};
use omuzaki_stage::scene::default::scene_for;
use omuzaki_stage_api::{Framing, SceneDef};
use omuzaki_store_api::Store;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::def::{ShortDef, VoiceDef};
use crate::draft;
use crate::encode::{ShortWriter, wav_bytes};
use crate::error::ShortError;
use crate::puppet::render_puppet_short;
use crate::render::{ShortScene, frame_count, render_short};
use crate::speech::{SpeechServices, TtsSpeaker};
use crate::timeline::{Measures, Timeline};
use crate::voice::{SPEECH_WPM, Synthesis, VoiceTrack, read_wav_from, voice_for};

/// The largest short file read, bytes.
const MAX_FILE_BYTES: u64 = 1 << 20;
/// Report progress every this many frames.
const PROGRESS_EVERY: u64 = 15;
/// The largest file of a puppet read, bytes: a texture atlas of 16384 pixels a side compresses well below it.
const MAX_PUPPET_FILE_BYTES: u64 = 512 << 20;

/// Where a short comes from: a file of the project, or inline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShortArg {
    /// A short file in the project (`shorts/intro.short.toml`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The short itself, as the file's fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short: Option<Box<ShortDef>>,
}

fn read_text(file: std::fs::File, path: &str) -> Result<String, OpError> {
    let mut text = String::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|e| OpError::invalid("path", format!("{path} doesn't read as text: {e}")))?;
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err(OpError::invalid("path", format!("{path} is over {MAX_FILE_BYTES} bytes: not a short file")));
    }
    Ok(text)
}

impl ShortArg {
    /// The short, read and checked.
    fn load(
        &self,
        fs: &dyn Fn(&str, FsMode) -> Result<omuzaki_ops_api::CanonicalPath, OpError>,
    ) -> Result<ShortDef, OpError> {
        let def = match (&self.path, &self.short) {
            (Some(path), None) => {
                let file = fs(path, FsMode::Read)?.open_read()?;
                ShortDef::from_toml(&read_text(file, path)?)?
            }
            (None, Some(def)) => (**def).clone(),
            _ => {
                return Err(OpError::invalid(
                    "path",
                    "give the short as `path` (a file of the project) or `short`, one of them",
                )
                .hint("short.draft writes a short from a plain script"));
            }
        };
        def.check()?;
        Ok(def)
    }

    /// The video's path when none is given: the file's, with `.webm` for `.short.toml`; `shorts/<character>.webm` for
    /// an inline short.
    fn default_out(&self, def: &ShortDef) -> String {
        match &self.path {
            Some(path) => {
                let stem = path.strip_suffix(".short.toml").or_else(|| path.strip_suffix(".toml")).unwrap_or(path);
                format!("{stem}.webm")
            }
            None => format!("shorts/{}.webm", def.character),
        }
    }
}

/// The voice of `def`, its recording read through the project's file access, its spoken lines as `synthesis` says.
fn voice(
    def: &ShortDef,
    fs: &dyn Fn(&str, FsMode) -> Result<omuzaki_ops_api::CanonicalPath, OpError>,
    synthesis: Synthesis<'_>,
) -> Result<VoiceTrack, OpError> {
    let mut denied = None;
    let track = voice_for(
        def,
        |path| {
            let file = fs(path, FsMode::Read).and_then(|p| p.open_read()).map_err(|e| {
                let message = e.to_string();
                denied = Some(e);
                ShortError::Voice(message)
            })?;
            read_wav_from(std::io::BufReader::new(file), path)
        },
        synthesis,
    );
    match (track, denied) {
        (Ok(track), _) => Ok(track),
        (Err(_), Some(e)) => Err(e),
        (Err(e), None) => Err(e.into()),
    }
}

/// The puppet at `path` in the project, read through the op's file access: a bundle folder, or layered art (a
/// `*.toml` manifest) rigged now.
fn load_puppet(
    path: &str,
    fs: &dyn Fn(&str, FsMode) -> Result<omuzaki_ops_api::CanonicalPath, OpError>,
) -> Result<Bundle, OpError> {
    let read = |file: &str| -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        fs(file, FsMode::Read)
            .and_then(|p| p.open_read())
            .map_err(|e| e.to_string())?
            .take(MAX_PUPPET_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("{file}: {e}"))?;
        if bytes.len() as u64 > MAX_PUPPET_FILE_BYTES {
            return Err(format!("{file} is over {MAX_PUPPET_FILE_BYTES} bytes"));
        }
        Ok(bytes)
    };
    let invalid = |e: String| {
        OpError::invalid("puppet", e)
            .hint("a puppet is a bundle folder holding puppet.json, or a *.layers.toml manifest of layered art")
    };
    if std::path::Path::new(path).extension().is_some_and(|ext| ext.eq_ignore_ascii_case("toml")) {
        let dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
        let text = String::from_utf8(read(path).map_err(invalid)?).map_err(|e| invalid(format!("{path}: {e}")))?;
        let art = LayeredArt::from_toml_with(&text, |file| {
            read(&if dir.is_empty() { file.to_owned() } else { format!("{dir}/{file}") })
        })
        .map_err(|e| invalid(e.to_string()))?;
        let rigged = autorig(&art).map_err(|e| invalid(e.to_string()))?;
        Ok(Bundle::from_rigged(rigged))
    } else {
        let dir = path.trim_end_matches('/');
        bundle::load_with(|file| match read(&format!("{dir}/{file}")) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(_) if file == bundle::MAPPING_FILE => Ok(None),
            Err(e) => Err(e),
        })
        .map_err(|e| invalid(e.to_string()))
    }
}

/// The scene of `def`, the character cast alone in it, its look laid over.
fn scene_def(doc: &dyn DocRead, def: &ShortDef) -> Result<(SceneDef, omuzaki_stage_api::MemberId), OpError> {
    if doc.get(&char_paths::character(&def.character)).is_none() {
        return Err(OpError::not_found(format!("there is no character '{}'", def.character))
            .hint("check the id; characters live under characters/<id>"));
    }
    let (_, mut scene) = scene_for(doc, def.scene.as_ref())?;
    if def.look.light_rig != "scene" {
        let name = &def.look.light_rig;
        if !LightRig::names().any(|known| known == name) {
            let known: Vec<&str> = LightRig::names().collect();
            return Err(OpError::invalid("look.light_rig", format!("there is no light rig '{name}'"))
                .hint(format!("the light rigs are {}, or `scene` for the scene's own", known.join(", "))));
        }
        scene.lighting = LightRig::load(name)
            .and_then(|rig| rig.to_lighting())
            .map_err(|e| OpError::internal(format!("the light rig '{name}' doesn't load: {e}")))?;
    }
    if let Some(colour) = def.look.backdrop {
        scene.lighting.background = colour;
    }
    let member = cast_alone(&mut scene, &def.character, None)
        .map_err(|e| OpError::internal(format!("casting the character: {e}")))?;
    let mut choice = ShotChoice::default();
    choice.framing = Some(Framing::Face);
    choice.apply(&mut scene).map_err(|e| OpError::invalid("scene", e.rule.clone()))?;
    Ok((scene, member))
}

/// A check of a short's timeline against the reference bar.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BarCheck {
    /// What is measured.
    pub measure: String,
    /// The short's value.
    pub value: f32,
    /// The bar: at least this.
    pub at_least: f32,
    /// Whether the short reaches it.
    pub ok: bool,
}

/// The reference bar for a talking-head short (`docs/quality/reference-shorts.md`), as checks of `m`.
#[must_use]
#[allow(clippy::cast_precision_loss, reason = "small counts")]
pub fn bar_checks(m: &Measures) -> Vec<BarCheck> {
    let check = |measure: &str, value: f32, at_least: f32| BarCheck {
        measure: measure.to_owned(),
        value,
        at_least,
        ok: value >= at_least,
    };
    vec![
        check("changes_per_minute", m.changes_per_minute, 40.0),
        check("distinct_faces", m.distinct_faces as f32, 12.0),
        check("marks", m.marks.len() as f32, 5.0),
        check("caption_coverage", m.caption_coverage, 0.95),
    ]
}

/// One line as planned.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlannedLine {
    /// What is said.
    pub text: String,
    /// When it starts and ends, seconds.
    pub span: [f32; 2],
    /// The expressions of its face.
    pub face: Vec<String>,
}

/// What `short.plan` answers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShortPlan {
    /// Seconds.
    pub duration_s: f32,
    /// Frames at the short's rate.
    pub frames: u64,
    /// The lines.
    pub lines: Vec<PlannedLine>,
    /// The timeline's measures.
    pub measures: Measures,
    /// The measures against the reference bar.
    pub bar: Vec<BarCheck>,
    /// How the timing was found when it is an estimate: a spoken short's lines are timed from their words, not
    /// synthesised; `short.render` says them and times them by their sound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<String>,
}

/// Plans a short: its timeline and its measures, without rendering.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"short": {"character": "kit", "line": [
            {"text": "Hi! {surprised @close+punch} Oh, you're here.", "face": "happy", "camera": "face"},
            {"text": "(I wasn't ready.)", "face": "flustered", "camera": "close+right", "caption": "aside"}
        ]}}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct PlanShort {
    /// The short.
    #[serde(flatten)]
    pub source: ShortArg,
}

impl Op for PlanShort {
    fn meta() -> OpMeta {
        OpMeta {
            name: "short.plan",
            summary: "Plan a short (a `*.short.toml` file of the project as `path`, or inline as `short`) without \
                      rendering: when each line is spoken, its face, and the measures against the talking-head bar \
                      (face changes per minute, distinct faces, anime marks, caption coverage). A spoken short \
                      (`voice.kind = \"speech\"`) is not synthesised here: its timing is estimated from the words.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let fs = |path: &str, mode| ctx.fs_access(path, mode);
        let def = self.source.load(&fs)?;
        let voice = voice(&def, &fs, Synthesis::Estimate)?;
        let timeline = Timeline::new(&def, &voice)?;
        let measures = timeline.measures();
        let lines = def
            .lines
            .iter()
            .zip(&timeline.lines)
            .map(|(line, span)| PlannedLine {
                text: line.text.clone(),
                span: [span.start, span.end],
                face: timeline.face_at(span.start + crate::timeline::FACE_LEAD_S).ids.clone(),
            })
            .collect();
        let plan = ShortPlan {
            duration_s: timeline.duration,
            frames: frame_count(timeline.duration, def.fps),
            lines,
            bar: bar_checks(&measures),
            measures,
            estimate: estimate_note(&def),
        };
        let value = serde_json::to_value(plan).map_err(|e| OpError::internal(format!("serialising the plan: {e}")))?;
        Ok(Plan::answer(value))
    }
}

register_op!(PlanShort);

/// What `short.plan` says about a spoken short's timing, which it estimates rather than synthesising.
fn estimate_note(def: &ShortDef) -> Option<String> {
    let VoiceDef::Speech { voice, speed } = &def.voice else { return None };
    Some(format!(
        "estimated: the lines are timed from their words at about {:.0} words a minute ({voice} at speed {speed}) and \
         the mouth from the text; short.render synthesises them and times every line by its sound",
        SPEECH_WPM * speed
    ))
}

/// Drafts a directed short from a plain script.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "script": "Okay so today I tried baking.\nIt was a DISASTER!!\n(don't tell anyone)"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct DraftShort {
    /// The character who performs it.
    pub character: CharId,
    /// The script: one line per line of text; a long line is split at its sentences.
    pub script: String,
    /// A voice recording in the project (a WAV) to time the lines and the mouth by; silent, timed from the words, when
    /// left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
}

impl Op for DraftShort {
    fn meta() -> OpMeta {
        OpMeta {
            name: "short.draft",
            summary: "Draft a directed short from a plain script: every line gets a face from its words and \
                      punctuation, a shot that keeps the frame moving (cuts, punch-ins, tilts, pushes) and a caption \
                      style. Returns the short file's TOML (save it as `*.short.toml`) and the short, ready for \
                      short.plan and short.render.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if self.script.trim().is_empty() {
            return Err(OpError::invalid("script", "the script is empty: give one line of text per line"));
        }
        let mut def = draft::draft(&self.script, self.character.clone());
        if let Some(path) = &self.voice {
            ctx.fs_access(path, FsMode::Read)?;
            def.voice = VoiceDef::File { path: path.clone() };
        }
        def.check()?;
        let toml = def.to_toml()?;
        Ok(Plan::answer(json!({ "toml": toml, "short": def })))
    }
}

register_op!(DraftShort);

fn default_true() -> bool {
    true
}

/// Renders a short to a video file in the project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"short": {"character": "kit", "line": [{"text": "Hello!", "face": "happy", "camera": "close+punch"}]}},
        {"short": {"character": "kit", "line": [{"text": "A quick draft."}]}, "out": "shorts/draft.webm",
         "size": [540, 960], "quality": "live"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct RenderShort {
    /// The short.
    #[serde(flatten)]
    pub source: ShortArg,
    /// The video to write, in the project; the short file's name with `.webm` when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out: Option<String>,
    /// Width and height in place of the short's own, for a quick draft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<[u32; 2]>,
    /// `live`, `high` or `offline` (the default: the same pixels on every run).
    #[serde(default)]
    pub quality: RenderQuality,
    /// Replace the video if it is there.
    #[serde(default = "default_true")]
    pub replace: bool,
}

/// What `short.render` gives back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RenderedShort {
    /// The video, in the project.
    pub path: String,
    /// Its bytes.
    pub bytes: u64,
    /// Frames, and the rate.
    pub frames: u64,
    /// Frames per second.
    pub fps: u32,
    /// Width and height.
    pub size: [u32; 2],
    /// Seconds.
    pub duration_s: f32,
    /// Whether the voice is in the video.
    pub audio: bool,
    /// The voice as a WAV beside the video, when it couldn't go in (no libopus on this machine).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_wav: Option<String>,
    /// Seconds rendering took per second of video.
    pub realtime_factor: f32,
    /// The timeline's measures.
    pub measures: Measures,
    /// The measures against the reference bar.
    pub bar: Vec<BarCheck>,
}

impl RenderShort {
    fn sized(&self, mut def: ShortDef) -> Result<ShortDef, OpError> {
        if let Some(size) = self.size {
            def.size = size;
            def.check()?;
        }
        check_size(def.size, self.quality)?;
        Ok(def)
    }
}

/// Writes `bytes` at `path` in the project, replacing what is there.
fn write_file(job: &JobCtx<'_>, path: &str, bytes: &[u8]) -> Result<(), OpError> {
    use std::io::Write as _;
    let target = job.fs_access(path, FsMode::Write)?;
    let mut replacement = target.replace()?;
    replacement.write_all(bytes).map_err(|e| OpError::invalid("out", format!("couldn't write {path}: {e}")))?;
    replacement.commit()
}

/// Seconds per frame of a 1080x1920 render on a software GPU (lavapipe on four cores), by quality: the slowest machine a
/// short renders on, so the estimate (and the job's time cap, four times it) holds there. A real GPU is far faster.
const SOFTWARE_FRAME_S: [f32; 3] = [0.6, 1.5, 2.0];
/// Seconds per frame that don't scale with the size: the performer, captions and the encoder.
const FRAME_OVERHEAD_S: f32 = 0.1;

/// The estimate of a render of `frames` frames of `size` at `quality`.
#[allow(clippy::cast_precision_loss, reason = "frame and pixel counts")]
fn render_seconds(frames: u64, size: [u32; 2], quality: RenderQuality) -> f32 {
    let share = (size[0] as f32 * size[1] as f32) / (1080.0 * 1920.0);
    let per_frame = match quality {
        RenderQuality::Live => SOFTWARE_FRAME_S[0],
        RenderQuality::High => SOFTWARE_FRAME_S[1],
        RenderQuality::Offline => SOFTWARE_FRAME_S[2],
    };
    frames as f32 * (per_frame * share + FRAME_OVERHEAD_S)
}

/// Seconds per frame of a 1080x1920 puppet frame on the CPU renderer (four cores), with the performer, captions and
/// the encoder.
const PUPPET_FRAME_S: f32 = 0.25;

/// The estimate of a puppet render of `frames` frames of `size`.
#[allow(clippy::cast_precision_loss, reason = "frame and pixel counts")]
fn puppet_seconds(frames: u64, size: [u32; 2]) -> f32 {
    let share = (size[0] as f32 * size[1] as f32) / (1080.0 * 1920.0);
    frames as f32 * (PUPPET_FRAME_S * share + FRAME_OVERHEAD_S)
}

impl Op for RenderShort {
    fn meta() -> OpMeta {
        OpMeta {
            name: "short.render",
            summary: "Render a short (a `*.short.toml` file of the project as `path`, or inline as `short`) to a \
                      finished WebM video in the project (VP9, with the voice as Opus): the character performs every \
                      line with its face, lip sync, blinks and life, the camera cuts, punches in, tilts and pushes as \
                      each line says, and captions are burned in. Frame-exact and headless; returns the file and the \
                      measures against the talking-head bar.",
            kind: OpKind::Job,
            scope: Scope::Render,
            // The video is a file; the document never changes.
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let fs = |path: &str, mode| ctx.fs_access(path, mode);
        let def = self.sized(self.source.load(&fs)?)?;
        let out = self.out.clone().unwrap_or_else(|| self.source.default_out(&def));
        ctx.fs_access(&out, FsMode::Write)?;
        if let Some(puppet) = &def.puppet {
            ctx.fs_access(puppet, FsMode::Read)?;
        } else {
            def.look.pose.check("look.pose")?;
            scene_def(ctx.doc, &def)?;
        }
        if let VoiceDef::Speech { voice, speed } = &def.voice {
            // Refused before the job starts when the voice can't be said here; the timing is still an estimate.
            TtsSpeaker::for_op(&SpeechServices::of_op(ctx), &def.character, voice, *speed)?;
        }
        let voice = voice(&def, &fs, Synthesis::Estimate)?;
        let timeline = Timeline::new(&def, &voice)?;
        let frames = frame_count(timeline.duration, def.fps);
        if def.puppet.is_some() {
            return Ok(Plan {
                cost: Some(CostHint { seconds: Some(puppet_seconds(frames, def.size)), ..CostHint::default() }),
                ..Plan::default()
            });
        }
        let character = character_bytes(ctx, &def.character, None, self.quality)?;
        let size = check_size(def.size, self.quality)?;
        let gpu = cost(size, 1, self.quality, character);
        Ok(Plan {
            cost: Some(CostHint { seconds: Some(render_seconds(frames, def.size, self.quality)), ..gpu }),
            ..Plan::default()
        })
    }

    #[allow(clippy::too_many_lines, reason = "one job's steps in order: load, stage, render, write, report")]
    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let started = std::time::Instant::now();
        let fs = |path: &str, mode| job.fs_access(path, mode);
        let def = self.sized(self.source.load(&fs)?)?;
        let out = self.out.clone().unwrap_or_else(|| self.source.default_out(&def));
        job.checkpoint("loading", 0.0)?;
        let voice = match &def.voice {
            VoiceDef::Speech { voice: name, speed } => {
                job.checkpoint("speaking", 0.01)?;
                let mut speaker = TtsSpeaker::for_op(&SpeechServices::of_job(job), &def.character, name, *speed)?;
                voice(&def, &fs, Synthesis::Speak(&mut speaker))?
            }
            _ => voice(&def, &fs, Synthesis::Estimate)?,
        };
        let timeline = Timeline::new(&def, &voice)?;
        if let Some(path) = &def.puppet {
            let puppet = load_puppet(path, &fs)?;
            return self.finish(job, started, &def, &out, &timeline, &voice, |writer, progress| {
                render_puppet_short(puppet, &def, &timeline, &voice, writer, progress)
            });
        }
        let (scene, member) = job.read(|doc| scene_def(doc, &def))?;
        let clock = SystemClock::default();
        let services = BodyServices::new(
            job.service::<dyn CharacterBuilder>()?,
            job.service::<BuildServices>()?,
            job.service::<dyn Store>()?,
            job.service::<Capabilities>().map_or(MtoonSupport::NONE, |c| c.mtoon),
            &clock,
        );
        let body = job.read(|doc| load_body(doc, &services, &def.character, None, self.quality.detail()))?;
        let landmarks = body.landmarks().map_err(|e| OpError::invalid("character", e.to_string()))?;
        let pose = def.look.pose.resolve("look.pose", &body)?;
        let [w, h] = def.size;
        #[allow(clippy::cast_precision_loss, reason = "sides are at most 4096")]
        let aspect = w as f32 / h as f32;
        let mut staged: StagedScene =
            stage_alone(&scene, &member, landmarks, aspect).map_err(|e| OpError::invalid("scene", e.to_string()))?;
        if staged.resolved.backdrop_image.is_some() {
            staged
                .resolved
                .load_backdrop(job.service::<dyn Store>()?)
                .map_err(|e| OpError::invalid("scene", e.to_string()))?;
        }
        if staged.resolved.environment.and_then(|e| e.hdri()).is_some() {
            staged
                .resolved
                .load_hdri(job.service::<dyn Store>()?)
                .map_err(|e| OpError::invalid("scene", e.to_string()))?;
        }
        if let Body::Kit(built) = &body {
            post_in_look(&mut staged.resolved.post, &built.look().scene);
        }

        let scene = ShortScene {
            def: &def,
            timeline: &timeline,
            voice: &voice,
            staged: &staged,
            landmarks,
            quality: self.quality.quality(),
            pose: Some(pose),
        };
        let renderer = job.service::<Mutex<Box<dyn Renderer>>>()?;
        self.finish(job, started, &def, &out, &timeline, &voice, |writer, progress| {
            let mut guard = renderer.lock().map_err(|_| {
                ShortError::Render("the renderer is unusable: an earlier render panicked while holding it".into())
            })?;
            render_short(&mut **guard, body, &scene, writer, progress)
        })
    }
}

impl RenderShort {
    /// Opens the video at `out`, runs `render` into it with a progress callback that reports to `job`, finishes the
    /// file (the voice beside it when it couldn't go in) and reports.
    #[allow(clippy::too_many_arguments, reason = "the render's parts, each used once")]
    fn finish(
        &self,
        job: &JobCtx<'_>,
        started: std::time::Instant,
        def: &ShortDef,
        out: &str,
        timeline: &Timeline,
        voice: &VoiceTrack,
        render: impl FnOnce(
            &mut ShortWriter,
            &mut dyn FnMut(f32) -> Result<(), ShortError>,
        ) -> Result<crate::render::LoopReport, ShortError>,
    ) -> Result<Value, OpError> {
        let target = job.fs_access(out, FsMode::Write)?;
        if let Some(folder) = target.parent() {
            folder.create_dir_all()?;
        }
        if !self.replace && target.open_read().is_ok() {
            return Err(OpError::invalid("out", format!("{out} is already there"))
                .hint("pass `replace: true`, or another `out`"));
        }
        let mut replacement = target.replace()?;
        let file =
            replacement.file().try_clone().map_err(|e| OpError::internal(format!("the video file's handle: {e}")))?;
        let has_voice = matches!(def.voice, VoiceDef::File { .. } | VoiceDef::Speech { .. });
        let mut writer = ShortWriter::create(
            file,
            std::path::Path::new(out),
            def.size,
            def.fps,
            has_voice.then_some(voice.pcm.as_slice()),
        )?;
        let frames = frame_count(timeline.duration, def.fps);
        let mut stopped: Option<OpError> = None;
        let mut done = 0u64;
        let mut progress = |share: f32| {
            done += 1;
            if !done.is_multiple_of(PROGRESS_EVERY) {
                return Ok(());
            }
            job.checkpoint("rendering", 0.05 + 0.9 * share).map_err(|e| {
                stopped = Some(e);
                ShortError::Render("stopped".into())
            })
        };
        let outcome = render(&mut writer, &mut progress);
        if let Some(e) = stopped {
            return Err(e);
        }
        let report = outcome?;
        job.checkpoint("finishing", 0.97)?;
        let written = writer.finish()?;
        replacement.commit()?;
        let mut voice_wav = None;
        if has_voice && !written.audio_in_video {
            let wav = out.strip_suffix(".webm").map_or_else(|| format!("{out}.wav"), |stem| format!("{stem}.wav"));
            write_file(job, &wav, &wav_bytes(&voice.pcm)?)?;
            voice_wav = Some(wav);
        }
        let measures = timeline.measures();
        let elapsed = started.elapsed().as_secs_f32();
        tracing::info!(
            frames = report.frames,
            render_s = report.render_s,
            encode_s = report.encode_s,
            "short rendered"
        );
        let result = RenderedShort {
            path: out.to_owned(),
            bytes: written.bytes,
            frames,
            fps: def.fps,
            size: def.size,
            duration_s: timeline.duration,
            audio: written.audio_in_video,
            voice_wav,
            realtime_factor: elapsed / timeline.duration.max(0.001),
            bar: bar_checks(&measures),
            measures,
        };
        job.checkpoint("done", 1.0)?;
        serde_json::to_value(result).map_err(|e| OpError::internal(format!("serialising the result: {e}")))
    }
}

register_op!(RenderShort);
