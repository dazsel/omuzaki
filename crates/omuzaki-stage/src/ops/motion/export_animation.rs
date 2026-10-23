//! `io.export_animation`: a library clip or an emote, on a character's own skeleton, written as a motion file (`vrma`).
//! A job; a dry run converts, runs the preflight and returns what would be written, and writes nothing.
//!
//! **What is gathered.** The character is loaded the way the stage loads it ([`load_body`]) for its skeleton, which
//! gives the rest tree the file carries (V4, V10). A clip comes from the pose library (`pose.list`, RG-06) and is
//! sampled with the character's own thumbs; an emote is an expression of the character's, eased in, held and eased
//! out, with an optional library pose held under it (V9). The conversion is `omuzaki-io-ops`'s
//! ([`clip_to_animation`], [`emote_to_animation`]).
//!
//! **Rights.** The file carries the motion's record, never the character's (D11). The library's clips and poses are the
//! project's own, so they carry its record: the project licence, commercial use allowed. The preflight runs the
//! licence gate on `publish`.
//!
//! **Notices.** What the export dialog shows before writing: always the proportions line (the file includes the
//! skeleton's proportions), then the licence and the attribution, and for a no-derivatives licence the "own use only"
//! line (V12).

use omuzaki_anim::hands::HandRest;
use omuzaki_anim::poses::{ClipLibrary, PoseClip};
use omuzaki_anim_api::{ExpressionId, HumanoidPose, Skeleton};
use omuzaki_char_api::{BuildServices, CharId, CharacterBuilder, Quality as BuildQuality};
use omuzaki_core::text::nearest;
use omuzaki_core::time::SystemClock;
use omuzaki_io_api::{
    AnimationSummary, ExportCtx, ExportOptions, FormatId, IoAnimation, Purpose, Report, animation_formats,
    find_animation_exporter, licence_forbids_derivatives, preflight_animation, run_export_animation,
};
use omuzaki_io_ops::convert::animation::{
    BoneSelection, ClipExportOptions, ClipSampler, ConvertError, EmoteMotionInput, EmoteTiming, FPS_RANGE,
    clip_to_animation, emote_to_animation,
};
use omuzaki_io_ops::error::from_io;
use omuzaki_io_ops::ops::export::{GENERATOR, WrittenFile, write};
use omuzaki_io_ops::ops::progress::JobProgress;
use omuzaki_ops_api::{CostHint, FsMode, JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_render_api::{Capabilities, MtoonSupport};
use omuzaki_store_api::{Commercial, Provenance, Source, Store};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::body::{Body, BodyServices, load_body};
use crate::ops::render::pose::PoseArg;

/// The licence the project's own library content carries, the same as a part from one of its own generators.
pub const LIBRARY_LICENCE: &str = "LicenseRef-Omuzaki-User";

/// The first notice, always: what of the character the file carries (V12).
pub const PROPORTIONS_NOTICE: &str = "This file includes your character's skeleton proportions (bone lengths and hips \
     height). VRM Animation needs them to scale hips motion between characters; it contains no mesh, texture or other \
     character data.";

/// The notice for a no-derivatives licence (V12).
pub const ND_NOTICE: &str = "For your own use only: the licence does not allow sharing a derivative.";

/// Writes a library clip or an emote, on a character's skeleton, as a motion file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "clip": "wave", "path": "exports/wave.vrma"},
        {"character": "kit", "emote": {"expression": "happy", "pose": {"id": "wave-right"}}, "path": "exports/hi.vrma"}
    ],
    "x-omuzaki-fixture" = "character-licensed"
))]
pub struct ExportAnimation {
    /// The character whose skeleton the motion is written for.
    pub character: CharId,
    /// A clip of the pose library, by id (`wave`, `walk`, `yawn`). Give this or `emote`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip: Option<String>,
    /// Play the clip reflected left to right. False by default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mirror: bool,
    /// An emote: an expression of the character's, eased in, held and eased out, with a pose under it. Give this or
    /// `clip`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emote: Option<EmoteArg>,
    /// The format (see `io.list_formats`, `animations`). `vrma` by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<FormatId>,
    /// Samples a second, 1 to 120. 30 by default; players interpolate between them.
    #[serde(default = "thirty")]
    pub fps: f32,
    /// Write every bone of the character, not only the bones the motion turns (for a full-body clip such as a walk).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub all_bones: bool,
    /// What the file is for: `private` (the default) for your own machine; `publish` for anything that leaves it, which
    /// the licence gate must pass.
    #[serde(default)]
    pub purpose: Purpose,
    /// Where to write the file, ending in the format's extension (`.vrma`). A relative path is inside the project
    /// folder; anywhere else needs the `fs` scope and a granted folder.
    pub path: String,
    /// The animation's name in the file. The clip's id or the emote's expression by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// An emote to export: the expression, the pose held under it and the timing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EmoteArg {
    /// The character's expression: a VRM preset (`happy`, `blink-left`) or one of its own (`smug`).
    pub expression: ExpressionId,
    /// A library pose held while the expression shows. None leaves the body at rest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pose: Option<PoseArg>,
    /// Seconds of each fade, in and out. 0.2 by default.
    #[serde(default = "fade")]
    pub fade_s: f32,
    /// Seconds at full weight. 1.6 by default.
    #[serde(default = "hold")]
    pub hold_s: f32,
    /// The expression's weight while held, 0 to 1. 1 by default.
    #[serde(default = "one")]
    pub intensity: f32,
}

fn thirty() -> f32 {
    30.0
}

fn fade() -> f32 {
    EmoteTiming::default().fade_s
}

fn hold() -> f32 {
    EmoteTiming::default().hold_s
}

fn one() -> f32 {
    1.0
}

/// What `io.export_animation` returns; a dry run returns the same with the file it would write unwritten.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AnimationExportResult {
    /// Where the file goes.
    pub path: String,
    /// The file written: its type, size and hash. Absent in a dry run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<WrittenFile>,
    /// What the animation holds: its length, tracks and keys. Absent only from a dry run in a host that cannot load
    /// the character (one without a character builder); the job always converts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<AnimationSummary>,
    /// Every finding: the conversion's, the preflight's and the format's, errors first.
    pub report: Report,
    /// What the export dialog shows before writing: the proportions line first, then the licence lines.
    pub notices: Vec<String>,
}

/// A clip sampled on a rig whose thumbs rest as `rest`: what the converter samples.
struct LibraryClip<'a> {
    clip: &'a PoseClip,
    rest: HandRest,
}

impl ClipSampler for LibraryClip<'_> {
    fn duration(&self) -> f32 {
        self.clip.length()
    }

    fn looped(&self) -> bool {
        self.clip.looped()
    }

    fn sample(&self, t: f32) -> HumanoidPose {
        self.clip.sample(t, &self.rest)
    }
}

/// The record of the library item `item` (`wave@builtin`): the project's own, commercial use allowed.
fn library_record(item: String) -> Provenance {
    Provenance {
        source: Source::Library { item },
        author: None,
        licence: LIBRARY_LICENCE.to_owned(),
        attribution: None,
        commercial: Commercial::Ok,
        consent: None,
        private_test: false,
    }
}

/// The notices for an animation with record `provenance` (V12).
#[must_use]
pub fn notices(provenance: Option<&Provenance>) -> Vec<String> {
    let mut out = vec![PROPORTIONS_NOTICE.to_owned()];
    if let Some(p) = provenance {
        out.push(format!("Licence: {}", p.licence));
        if let Some(text) = p.attribution.as_deref().filter(|t| !t.trim().is_empty()) {
            out.push(format!("Attribution: {text}"));
        }
        if licence_forbids_derivatives(p).is_some() {
            out.push(ND_NOTICE.to_owned());
        }
    }
    out
}

/// The library clip `id`, or the refusal naming the nearest one and every clip there is.
fn library_clip(id: &str) -> Result<&'static PoseClip, OpError> {
    let library = ClipLibrary::builtin().map_err(|e| OpError::internal(format!("the clip library doesn't load: {e}")))?;
    library.get(id).ok_or_else(|| {
        let ids: Vec<&str> = library.iter().map(|c| c.id.as_str()).collect();
        let near = nearest(id, ids.iter().copied()).map(|n| format!("did you mean '{n}'? ")).unwrap_or_default();
        let mut error = OpError::not_found(format!("there is no clip '{id}' in the pose library"))
            .hint(format!("{near}the clips are {}", ids.join(", ")));
        error.field = Some("clip".to_owned());
        error
    })
}

/// A conversion's refusal, naming the argument at fault.
fn converted(error: &ConvertError) -> OpError {
    let field = match error {
        ConvertError::Fps(_) => "fps",
        ConvertError::Duration(_) => "clip",
        ConvertError::Timing(_) => "emote",
        _ => "character",
    };
    let hint = match error {
        ConvertError::NotHumanoid => "a VRM Animation drives a humanoid; give the character a humanoid skeleton",
        ConvertError::Fps(_) => "give 1 to 120; 30 is what players expect",
        _ => "io.check_export lists what a character needs to export",
    };
    OpError::invalid(field, format!("{error} ({})", error.rule())).hint(hint)
}

/// "animation formats: vrma", for hints.
fn animation_format_ids() -> String {
    let ids: Vec<&str> = animation_formats().iter().map(|f| f.id).collect();
    format!("animation formats: {}", ids.join(", "))
}

impl ExportAnimation {
    /// The format to write: `format`, or `vrma`.
    fn format(&self) -> &str {
        self.format.as_ref().map_or("vrma", FormatId::as_str)
    }

    /// The arguments that can be checked without the document: one motion, the format, its extension and the rate.
    fn check_arguments(&self) -> Result<(), OpError> {
        match (&self.clip, &self.emote) {
            (Some(_), Some(_)) => {
                return Err(OpError::invalid("emote", "give a clip or an emote, not both").hint("leave one out"));
            }
            (None, None) => {
                return Err(OpError::invalid("clip", "give a clip or an emote to export")
                    .hint("a clip of the pose library (wave, walk, yawn) or an emote of the character's"));
            }
            _ => {}
        }
        if self.mirror && self.clip.is_none() {
            return Err(OpError::invalid("mirror", "mirror reflects a clip; an emote's pose mirrors with its own flag")
                .hint("set emote.pose.mirror instead"));
        }
        if !(self.fps.is_finite() && FPS_RANGE.contains(&self.fps)) {
            return Err(converted(&ConvertError::Fps(self.fps)));
        }
        let format = animation_formats().into_iter().find(|f| f.id == self.format()).ok_or_else(|| {
            OpError::invalid("format", format!("no animation exporter is registered for format {}", self.format()))
                .hint(animation_format_ids())
        })?;
        let extension = std::path::Path::new(&self.path).extension().and_then(|e| e.to_str()).unwrap_or_default();
        if !format.extensions.iter().any(|e| e.eq_ignore_ascii_case(extension)) {
            let wanted = format.extensions.first().copied().unwrap_or_default();
            return Err(OpError::invalid(
                "path",
                format!("a {} file ends in .{wanted}, and '{}' doesn't", format.name, self.path),
            )
            .hint(format!("name the file <name>.{wanted}")));
        }
        if let Some(emote) = &self.emote
            && let Some(pose) = &emote.pose
        {
            pose.check("emote/pose")?;
        }
        if let Some(id) = &self.clip {
            library_clip(id)?;
        }
        Ok(())
    }

    fn options(&self) -> ClipExportOptions {
        let bones = if self.all_bones { BoneSelection::All } else { BoneSelection::Driven };
        ClipExportOptions::default().at_fps(self.fps).with_bones(bones)
    }

    fn export_options(&self) -> ExportOptions {
        ExportOptions::default().with_purpose(self.purpose).with_generator(GENERATOR)
    }

    /// The animation, with the conversion's findings.
    fn convert(&self, skeleton: &Skeleton) -> Result<(IoAnimation, Report), OpError> {
        let rest = HandRest::of(skeleton).unwrap_or_else(|_| HandRest::nominal());
        if let Some(id) = &self.clip {
            let library = library_clip(id)?;
            let mirrored;
            let clip = if self.mirror {
                mirrored = library.mirrored();
                &mirrored
            } else {
                library
            };
            let name = self.name.clone().unwrap_or_else(|| id.clone());
            let sampler = LibraryClip { clip, rest };
            return clip_to_animation(&sampler, skeleton, &name, self.record(), &self.options()).map_err(|e| converted(&e));
        }
        let Some(emote) = &self.emote else { return Err(OpError::internal("checked: a clip or an emote")) };
        let pose = emote.pose.as_ref().map(|p| p.resolve_on("emote/pose", skeleton)).transpose()?;
        let name = self.name.clone().unwrap_or_else(|| emote.expression.to_string());
        let mut input = EmoteMotionInput::new(name, emote.expression.clone(), pose);
        input.timing = EmoteTiming::new(emote.fade_s, emote.hold_s, emote.intensity);
        emote_to_animation(&input, skeleton, self.record(), &self.options()).map_err(|e| converted(&e))
    }

    /// The motion's record: the library item's (`wave@builtin`; an emote's pose, or the emote itself without one).
    fn record(&self) -> Provenance {
        let item = match (&self.clip, &self.emote) {
            (Some(id), _) => id.clone(),
            (None, Some(EmoteArg { pose: Some(pose), .. })) => pose.id.clone(),
            _ => "emote".to_owned(),
        };
        library_record(format!("{item}@builtin"))
    }

    /// The animation and every finding before writing; refuses on an error, as the export would.
    fn prepare(&self, skeleton: &Skeleton) -> Result<(IoAnimation, Report), OpError> {
        let (animation, mut report) = self.convert(skeleton)?;
        let exporter = find_animation_exporter(self.format())
            .ok_or_else(|| OpError::invalid("format", format!("no exporter for {}", self.format())))?;
        let options = self.export_options();
        let mut checked = preflight_animation(&animation, &options);
        checked.extend(exporter.check(&animation, &options));
        if checked.has_errors() {
            return Err(from_io(omuzaki_io_api::IoError::Rejected(checked), "clip"));
        }
        report.extend(checked);
        Ok((animation, report))
    }

    fn result(&self, animation: &IoAnimation, mut report: Report, file: Option<WrittenFile>) -> AnimationExportResult {
        // Errors first, then warnings, then notes (a stable sort keeps each group's order).
        report.findings.sort_by_key(|f| std::cmp::Reverse(f.severity));
        AnimationExportResult {
            path: self.path.clone(),
            file,
            summary: Some(animation.summary()),
            report,
            notices: notices(animation.provenance.as_ref()),
        }
    }
}

/// The skeleton of a loaded body.
fn skeleton(body: Body) -> Skeleton {
    match body {
        Body::Kit(built) => built.rig.skeleton,
        Body::Avatar(avatar) => avatar.skeleton,
    }
}

impl Op for ExportAnimation {
    fn meta() -> OpMeta {
        OpMeta {
            name: "io.export_animation",
            summary: "Export a pose-library clip or an emote, on a character's skeleton, as a VRM Animation (.vrma) that \
                      other VRM apps play. Returns what the file holds, every finding and the notices to show.",
            kind: OpKind::Job,
            // The file may leave the machine; the write itself also needs `fs` (checked on the path).
            scope: Scope::Publish,
            // It changes nothing in the document; the file is the result.
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        self.check_arguments()?;
        ctx.fs_access(&self.path, FsMode::Write)?;
        if ctx.doc.get(&omuzaki_char_api::paths::character(&self.character)).is_none() {
            return Err(OpError::not_found(format!("there is no character '{}'", self.character))
                .hint("check the id; characters live under characters/<id>"));
        }
        let planned = if let (Ok(builder), Ok(build), Ok(store)) =
            (ctx.service::<dyn CharacterBuilder>(), ctx.service::<BuildServices>(), ctx.service::<dyn Store>())
        {
            let mtoon = ctx.service::<Capabilities>().map_or(MtoonSupport::NONE, |c| c.mtoon);
            let services = BodyServices::new(builder, build, store, mtoon, ctx.clock);
            let skeleton = skeleton(load_body(ctx.doc, &services, &self.character, None, BuildQuality::Live)?);
            let (animation, report) = self.prepare(&skeleton)?;
            self.result(&animation, report, None)
        } else {
            // A host without a character builder: the arguments are checked; the job converts.
            AnimationExportResult {
                path: self.path.clone(),
                file: None,
                summary: None,
                report: Report::new(),
                notices: notices(Some(&self.record())),
            }
        };
        let output = serde_json::to_value(planned).map_err(|e| OpError::internal(format!("the planned export: {e}")))?;
        Ok(Plan { output: Some(output), cost: Some(CostHint { seconds: Some(2.0), gpu_mib: Some(0), usd: None }), ..Plan::default() })
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        self.check_arguments()?;
        job.checkpoint("loading the character", 0.0)?;
        let clock = SystemClock::default();
        let services = BodyServices::new(
            job.service::<dyn CharacterBuilder>()?,
            job.service::<BuildServices>()?,
            job.service::<dyn Store>()?,
            job.service::<Capabilities>().map_or(MtoonSupport::NONE, |c| c.mtoon),
            &clock,
        );
        let body = job.read(|doc| load_body(doc, &services, &self.character, None, BuildQuality::Live))?;
        let skeleton = skeleton(body);
        job.checkpoint("converting", 0.3)?;
        let (animation, report) = self.prepare(&skeleton)?;
        let exporter = find_animation_exporter(self.format())
            .ok_or_else(|| OpError::invalid("format", format!("no exporter for {}", self.format())))?;
        job.checkpoint("writing", 0.7)?;
        let options = self.export_options();
        let progress = JobProgress(job);
        let outcome = run_export_animation(exporter.as_ref(), &animation, &ExportCtx::new(&options).with_progress(&progress))
            .map_err(|e| from_io(e, "clip"))?;
        let Some(main) = outcome.files.first() else {
            return Err(OpError::internal("the animation exporter wrote no file"));
        };
        let file = write(job, &self.path, main)?;
        // The conversion's findings, then what the export said (the preflight's again, with the format's notes).
        let mut all = Report::new();
        all.findings.extend(report.findings.into_iter().filter(|f| f.rule.starts_with("anim_export.")));
        all.extend(outcome.report);
        job.checkpoint("done", 1.0)?;
        serde_json::to_value(self.result(&animation, all, Some(file)))
            .map_err(|e| OpError::internal(format!("the export's result: {e}")))
    }
}

register_op!(ExportAnimation);
