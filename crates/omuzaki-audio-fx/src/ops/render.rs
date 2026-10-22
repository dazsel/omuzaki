//! `audio.fx.render` and `audio.fx.compare` (design section 6.2): jobs that put a clip through one or two presets
//! offline and measure what comes out, so an agent can tune a voice by numbers (median F0, formants, loudness,
//! spectral centroid, latency) and a person can listen to the WAV.
//!
//! The clip is the built-in synthetic voice, or a WAV file. A file is the person's own recording, so only a person may
//! render one (design section 7.2, rule 4): an agent never receives the person's voice unless the person sends it.

use omuzaki_audio_api::FxPreset;
use omuzaki_audio_api::graph::NodeSpec;
use omuzaki_doc_api::DocRead;
use omuzaki_ops_api::{
    Actor, CostHint, FsMode, JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op,
};
use omuzaki_store_api::{Commercial, Provenance, Source as StoreSource, Store};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{check_chain, find};
use crate::chain::Chain;
use crate::clip::{Synthetic, read_wav, wav};
use crate::measure::{Measurements, measure};

/// The longest file a render reads, in seconds.
const MAX_SECONDS: f64 = 30.0;

/// What to put through the preset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Clip {
    /// The built-in synthetic voice (a known F0 on one vowel).
    Synthetic(Synthetic),
    /// A 48 kHz WAV file, at most 30 s: the person's own recording, so person only.
    File(String),
}

impl Default for Clip {
    fn default() -> Self {
        Self::Synthetic(Synthetic::default())
    }
}

impl Clip {
    /// Checks the clip can be rendered by `actor`, before the job is queued.
    fn check(&self, ctx: &OpCtx<'_>) -> Result<(), OpError> {
        match self {
            Self::Synthetic(s) => s.problem().map_or(Ok(()), |p| Err(OpError::invalid("clip", p))),
            Self::File(path) => {
                if !matches!(ctx.actor, Actor::Person { .. }) {
                    return Err(OpError::person_only(
                        "rendering a recording: it may be the person's own voice, which an agent only gets when the \
                         person sends it",
                    ));
                }
                ctx.fs_access(path, FsMode::Read).map(|_| ())
            }
        }
    }

    /// The clip's samples.
    fn load(&self, job: &JobCtx<'_>) -> Result<Vec<f32>, OpError> {
        match self {
            Self::Synthetic(s) => Ok(s.render()),
            Self::File(path) => {
                if !matches!(job.actor(), Actor::Person { .. }) {
                    return Err(OpError::person_only("rendering a recording"));
                }
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut job.fs_access(path, FsMode::Read)?.open_read()?, &mut bytes)
                    .map_err(|e| OpError::invalid("clip", format!("couldn't read {path}: {e}")))?;
                let samples = read_wav(&bytes).map_err(|e| OpError::invalid("clip", format!("{path}: {e}")))?;
                #[allow(clippy::cast_precision_loss)]
                if samples.len() as f64 > MAX_SECONDS * 48_000.0 {
                    return Err(OpError::invalid("clip", format!("{path} is longer than {MAX_SECONDS} s"))
                        .hint("cut the part to compare into its own file"));
                }
                Ok(samples)
            }
        }
    }
}

/// The preset `name` (or `dry`, no effects).
fn resolve(doc: &dyn DocRead, character: Option<&str>, name: &str) -> Result<FxPreset, OpError> {
    if name == "dry" {
        return Ok(FxPreset { name: "dry".to_owned(), chain: Vec::new() });
    }
    Ok(find(doc, character, name)?.0)
}

/// What one render gives back.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Rendered {
    /// The preset's name.
    pub preset: String,
    /// The chain's declared latency in milliseconds (the output is shifted back by it before it is measured).
    pub latency_ms: f64,
    /// The measurements of the output.
    pub output: Measurements,
    /// The output as a WAV, in the asset store, when the app has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wav: Option<StoredWav>,
}

/// A WAV in the asset store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StoredWav {
    /// Its content hash.
    pub hash: String,
    /// Its size in bytes.
    pub bytes: u64,
}

/// Puts `clip` through `chain`, latency-compensated, and measures it; stores the WAV when there is a store.
fn render(job: &JobCtx<'_>, preset: &FxPreset, clip: &[f32]) -> Result<Rendered, OpError> {
    let mut chain = Chain::build(&preset.chain).map_err(|e| OpError::invalid("preset", e.to_string()))?;
    let latency = chain.latency() as usize;
    let mut audio = clip.to_vec();
    audio.extend(std::iter::repeat_n(0.0, latency));
    chain.process(&mut audio);
    let output = audio.split_off(latency);
    let wav = match job.service::<dyn Store>() {
        Ok(store) => {
            let bytes = wav(&output);
            let provenance = Provenance {
                source: StoreSource::Authored,
                author: None,
                licence: "LicenseRef-Omuzaki-User".to_owned(),
                attribution: None,
                commercial: Commercial::Unknown,
                consent: None,
                private_test: false,
            };
            let hash = store.put(&bytes, provenance).map_err(|e| OpError::internal(format!("storing the WAV: {e}")))?;
            Some(StoredWav { hash: hash.to_string(), bytes: bytes.len() as u64 })
        }
        Err(_) => None,
    };
    #[allow(clippy::cast_precision_loss)]
    let latency_ms = latency as f64 / 48.0;
    Ok(Rendered { preset: preset.name.clone(), latency_ms, output: measure(&output), wav })
}

/// Render a clip through a voice-effect preset offline and measure it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "preset": "Deeper" },
    { "preset": "Tiny", "clip": { "synthetic": { "f0_hz": 200.0, "vowel": "i" } } }
], "x-omuzaki-fixture" = "character"))]
pub struct Render {
    /// The character whose preset it is. Without one, a built-in preset.
    #[serde(default)]
    pub character: Option<String>,
    /// The preset, or `dry` for none. Give this or `chain`.
    #[serde(default)]
    pub preset: Option<String>,
    /// A chain to try without saving it. Give this or `preset`.
    #[serde(default)]
    pub chain: Option<Vec<NodeSpec>>,
    /// What to render. Default: the synthetic voice at 120 Hz saying /a/ for 2 s.
    #[serde(default)]
    pub clip: Clip,
}

impl Render {
    fn preset(&self, doc: &dyn DocRead) -> Result<FxPreset, OpError> {
        let preset = match (&self.preset, &self.chain) {
            (Some(name), None) => resolve(doc, self.character.as_deref(), name)?,
            (None, Some(chain)) => FxPreset { name: "chain".to_owned(), chain: chain.clone() },
            _ => return Err(OpError::invalid("preset", "give either `preset` or `chain`")),
        };
        check_chain(&preset.chain)?;
        Ok(preset)
    }
}

impl Op for Render {
    fn meta() -> OpMeta {
        OpMeta {
            name: "audio.fx.render",
            summary: "Render a clip (the synthetic voice, or a person's WAV) through a voice-effect preset offline and \
                      measure it: median F0, formants F1 to F3, loudness, spectral centroid, latency. The WAV goes to \
                      the asset store.",
            kind: OpKind::Job,
            scope: Scope::Render,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let preset = self.preset(ctx.doc)?;
        self.clip.check(ctx)?;
        Ok(Plan {
            output: Some(json!({ "renders": preset.name, "clip": self.clip })),
            cost: Some(CostHint { seconds: Some(1.0), gpu_mib: Some(0), usd: None }),
            ..Plan::default()
        })
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let preset = job.read(|doc| self.preset(doc))?;
        job.checkpoint("loading the clip", 0.0)?;
        let clip = self.clip.load(job)?;
        let input = measure(&clip);
        job.checkpoint("rendering", 0.3)?;
        let rendered = render(job, &preset, &clip)?;
        serde_json::to_value(json!({ "input": input, "rendered": rendered }))
            .map_err(|e| OpError::internal(e.to_string()))
    }
}

/// Render the same clip through two presets and give their measurements side by side, with the differences.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "a": "dry", "b": "Deeper" }], "x-omuzaki-fixture" = "character"))]
pub struct Compare {
    /// The character whose presets they are. Without one, built-in presets.
    #[serde(default)]
    pub character: Option<String>,
    /// The first preset, or `dry`.
    pub a: String,
    /// The second preset, or `dry`.
    pub b: String,
    /// What to render. Default: the synthetic voice at 120 Hz saying /a/ for 2 s.
    #[serde(default)]
    pub clip: Clip,
}

impl Compare {
    fn presets(&self, doc: &dyn DocRead) -> Result<(FxPreset, FxPreset), OpError> {
        let a = resolve(doc, self.character.as_deref(), &self.a).map_err(|mut e| {
            e.field = Some("a".to_owned());
            e
        })?;
        let b = resolve(doc, self.character.as_deref(), &self.b).map_err(|mut e| {
            e.field = Some("b".to_owned());
            e
        })?;
        check_chain(&a.chain)?;
        check_chain(&b.chain)?;
        Ok((a, b))
    }
}

/// `b / a`, when both are there.
fn ratio(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(a), Some(b)) if a > 0.0 => Some(b / a),
        _ => None,
    }
}

impl Op for Compare {
    fn meta() -> OpMeta {
        OpMeta {
            name: "audio.fx.compare",
            summary: "Render the same clip through two voice-effect presets (or `dry`) and compare them: F0 ratio, \
                      formant ratios, loudness and brightness differences, latency.",
            kind: OpKind::Job,
            scope: Scope::Render,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (a, b) = self.presets(ctx.doc)?;
        self.clip.check(ctx)?;
        Ok(Plan {
            output: Some(json!({ "compares": [a.name, b.name], "clip": self.clip })),
            cost: Some(CostHint { seconds: Some(2.0), gpu_mib: Some(0), usd: None }),
            ..Plan::default()
        })
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let (a, b) = job.read(|doc| self.presets(doc))?;
        job.checkpoint("loading the clip", 0.0)?;
        let clip = self.clip.load(job)?;
        job.checkpoint("rendering a", 0.2)?;
        let ra = render(job, &a, &clip)?;
        job.checkpoint("rendering b", 0.6)?;
        let rb = render(job, &b, &clip)?;
        let (fa, fb) = (ra.output.formants_hz, rb.output.formants_hz);
        let differences = json!({
            "f0_ratio": ratio(ra.output.median_f0_hz, rb.output.median_f0_hz),
            "f0_semitones": ratio(ra.output.median_f0_hz, rb.output.median_f0_hz).map(|r| 12.0 * r.log2()),
            "formant_ratios": fa.zip(fb).map(|(fa, fb)| [fb[0] / fa[0], fb[1] / fa[1], fb[2] / fa[2]]),
            "loudness_db": ra.output.loudness_lufs.zip(rb.output.loudness_lufs).map(|(la, lb)| lb - la),
            "centroid_ratio": ratio(Some(ra.output.spectral_centroid_hz), Some(rb.output.spectral_centroid_hz)),
            "latency_ms": rb.latency_ms - ra.latency_ms,
        });
        Ok(json!({ "a": ra, "b": rb, "b_against_a": differences }))
    }
}

register_op!(Render);
register_op!(Compare);
