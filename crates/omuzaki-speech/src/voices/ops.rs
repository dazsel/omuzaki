//! The `voice.*` ops (§3.4): which voice a character speaks in, what voices there are, and a preview.
//!
//! | Op | Kind | Scope | Undo |
//! |---|---|---|---|
//! | `voice.get` | query | `read` | none |
//! | `voice.list` | query | `read` | none |
//! | `voice.assign` | edit | `edit:character` (and `spend` for a hosted voice) | auto |
//! | `voice.preview` | edit | `edit:character` | none (it plays, the document doesn't change) |
//!
//! They reach the app through services: the [`VoiceCatalog`] (or, without one, the voices of the `dyn ModelRegistry`'s
//! models), the [`ConsentLookup`] (without one, [`NoConsentRecords`]: every cloned or imitating voice is refused) and,
//! for the preview, the [`VoicePreview`].

use omuzaki_ai_api::{ConsentLookup, ModelRegistry, ModelStatus, NoConsentRecords};
use omuzaki_char_api::ids::CharId;
use omuzaki_char_api::paths;
use omuzaki_doc_api::{Patch, Value};
use omuzaki_ops_api::{CostHint, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_performer_api::ids::{ConnectorId, VoiceId};
use omuzaki_performer_api::limits::CHUNK_MAX_CHARS;
use omuzaki_performer_api::speech::VoiceError;
use omuzaki_performer_api::voice::{
    AssignError, ConsentRef, PITCH_MAX_SEMITONES, PITCH_MIN_SEMITONES, SPEED_MAX, SPEED_MIN, TtsPrice, VoiceAssignment,
    VoiceCatalog, VoiceInfo, VoiceProvider,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::catalog::local_voices;
use super::figures::PublicFigures;
use super::preview::VoicePreview;
use super::{admit, assignment_path, read_assignment};

/// The line `voice.preview` speaks when it is given none.
pub const PREVIEW_LINE: &str = "Hi chat! This is how I sound. Thanks for hanging out with me today.";

/// Show which voice a character speaks in, with the voice's languages and whether it needs a consent record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "character": "mochi" }], "x-omuzaki-fixture" = "voiced"))]
pub struct VoiceGet {
    /// The character.
    pub character: CharId,
}

/// List the voices a provider offers: the local models' voices (installed or not), or a connected provider's.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, { "language": "en-GB" }, { "provider": "tts-cloud" }]))]
pub struct VoiceList {
    /// `local` (the default) or a connected provider's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Only voices that speak this language (`en`, `en-GB`); a voice for `en-GB` also matches `en`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// Give a character a voice: the voice, its speed and pitch, and for a cloned or imitating voice the consent record
/// of the person it sounds like. Kept in `characters/<id>/voice-assignment.toml`; undo puts the old voice back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "character": "mochi", "voice": "kokoro-82m/af_heart" },
    { "character": "mochi", "voice": "kokoro-82m/bf_emma", "speed": 1.1, "pitch_semitones": 2.0 },
    { "character": "mochi", "voice": "tts-cloud/Rachel", "provider": "tts-cloud" },
], "x-omuzaki-fixture" = "voiced"))]
pub struct VoiceAssign {
    /// The character.
    pub character: CharId,
    /// The voice, `<model or provider>/<voice>`, as `voice.list` names it.
    pub voice: String,
    /// `local` (the default) or a connected provider's id. A hosted voice needs the `spend` grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Tempo, 0.7 to 1.4. Default: the character's current speed, else 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f32>,
    /// Pitch shift in semitones, −4 to 4. Default: the character's current pitch, else 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitch_semitones: Option<f32>,
    /// For a cloned or imitating voice: the person whose consent record it uses. Default: the person the voice
    /// imitates, when their record exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent: Option<String>,
}

/// Hear a voice: one line spoken to the monitor output only, never the stream. Either a character's own voice or a
/// voice being considered for it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "character": "mochi" },
    { "voice": "kokoro-82m/am_michael", "text": "Welcome back, everyone!" },
], "x-omuzaki-fixture" = "voiced"))]
pub struct VoicePreviewOp {
    /// Preview this character's voice (its speed and pitch too).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character: Option<CharId>,
    /// Preview this voice instead, at speed 1 and no pitch shift unless given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// `local` (the default) or a connected provider's id, for `voice`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Tempo for `voice`, 0.7 to 1.4.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f32>,
    /// Pitch for `voice`, −4 to 4 semitones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitch_semitones: Option<f32>,
    /// The line, at most 220 characters. Default: a short greeting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

register_op!(VoiceGet);
register_op!(VoiceList);
register_op!(VoiceAssign);
register_op!(VoicePreviewOp);

/// The catalog the app wired, or the registry's local voices.
fn catalog<'a>(ctx: &OpCtx<'a>) -> Result<CatalogRef<'a>, OpError> {
    if let Ok(catalog) = ctx.service::<dyn VoiceCatalog>() {
        return Ok(CatalogRef::Wired(catalog));
    }
    let registry = ctx.service::<dyn ModelRegistry>().map_err(|_| OpError::unavailable("dyn VoiceCatalog"))?;
    Ok(CatalogRef::Local(registry))
}

/// The catalog an op reads.
enum CatalogRef<'a> {
    Wired(&'a dyn VoiceCatalog),
    Local(&'a dyn ModelRegistry),
}

impl CatalogRef<'_> {
    fn voices(&self, provider: &VoiceProvider) -> Result<Vec<VoiceInfo>, VoiceError> {
        match self {
            CatalogRef::Wired(catalog) => catalog.voices(provider),
            CatalogRef::Local(registry) => match provider {
                VoiceProvider::Local => Ok(local_voices(*registry).into_iter().map(|v| v.info).collect()),
                other => Err(VoiceError::Provider(format!("{other:?} isn't connected: pick a local voice"))),
            },
        }
    }

    fn price(&self, provider: &VoiceProvider) -> Option<TtsPrice> {
        match self {
            CatalogRef::Wired(catalog) => catalog.price(provider),
            CatalogRef::Local(_) => None,
        }
    }
}

impl VoiceCatalog for CatalogRef<'_> {
    fn voices(&self, provider: &VoiceProvider) -> Result<Vec<VoiceInfo>, VoiceError> {
        CatalogRef::voices(self, provider)
    }

    fn price(&self, provider: &VoiceProvider) -> Option<TtsPrice> {
        CatalogRef::price(self, provider)
    }
}

/// The consent lookup the app wired, or none at all.
fn consent<'a>(ctx: &OpCtx<'a>) -> &'a dyn ConsentLookup {
    ctx.service::<dyn ConsentLookup>().unwrap_or(&NoConsentRecords)
}

/// `local` or a connector id.
fn provider(field: &str, text: Option<&str>) -> Result<VoiceProvider, OpError> {
    match text.map(str::trim) {
        None | Some("" | "local") => Ok(VoiceProvider::Local),
        Some(id) => ConnectorId::new(id)
            .map(VoiceProvider::Hosted)
            .map_err(|e| OpError::invalid(field, format!("{e}")).hint("`local`, or a connected provider's id")),
    }
}

fn voice_id(field: &str, text: &str) -> Result<VoiceId, OpError> {
    VoiceId::new(text.trim())
        .map_err(|e| OpError::invalid(field, e.to_string()).hint("voice.list names every voice as <model>/<voice>"))
}

/// The character must exist before it can be given a voice (a change's parent must exist).
fn require_character(ctx: &OpCtx<'_>, character: &CharId) -> Result<(), OpError> {
    if ctx.doc.get(&paths::character(character)).is_some() {
        return Ok(());
    }
    let mut error = OpError::not_found(format!("there is no character `{character}`"));
    error.field = Some("character".into());
    Err(error.hint("character.list lists the project's characters"))
}

/// The op error for an assignment `admit` refused, worded so an agent knows what to change.
fn refused(e: AssignError, voices: &[VoiceInfo]) -> OpError {
    let message = e.to_string();
    match e {
        AssignError::Speed { .. } => OpError::invalid("speed", message),
        AssignError::Pitch { .. } => OpError::invalid("pitch_semitones", message),
        AssignError::UnnamedClone { .. } | AssignError::ConsentForSomeoneElse { .. } => {
            OpError::invalid("consent", message)
        }
        AssignError::Voice(VoiceError::UnknownVoice { voice, .. }) => {
            let ids: Vec<&str> = voices.iter().map(|v| v.id.as_str()).collect();
            let mut error = OpError::not_found(message);
            error.field = Some("voice".into());
            match omuzaki_core::text::nearest(&voice, ids) {
                Some(near) => error.hint(format!("did you mean {near:?}? voice.list lists the voices")),
                None => error.hint("voice.list lists the voices"),
            }
        }
        AssignError::Voice(VoiceError::NoConsent { .. }) => OpError::denied(message).hint(
            "a cloned or imitating voice needs a consent record from the person it sounds like; record it with the \
             project's licence records, or pick a stock voice",
        ),
        AssignError::Voice(VoiceError::Provider(_)) => OpError::conflict(message),
        _ => OpError::invalid("voice", message),
    }
}

/// The impersonation warning for a voice and the character that speaks with it, if one applies (§3.5).
fn figure_warning(voice: &VoiceId, character_name: Option<&str>) -> Option<String> {
    let figures = PublicFigures::shipped();
    let (what, figure) = [("voice", Some(voice.voice())), ("character's name", character_name)]
        .into_iter()
        .find_map(|(what, text)| text.and_then(|t| figures.find(t)).map(|f| (what, f)))?;
    Some(format!(
        "the {what} matches a public figure ({}): using a real person's voice without their consent can be \
         impersonation",
        figure.name
    ))
}

fn character_name(ctx: &OpCtx<'_>, character: &CharId) -> Option<String> {
    ctx.doc.get(&paths::character(character).join("name")).and_then(Value::as_str).map(str::to_owned)
}

fn dollars(price: &TtsPrice) -> String {
    format!("≈ ${:.2} per hour at a 40 % talk share (price of {})", price.per_hour().0 as f64 / 1e6, price.as_of)
}

impl Op for VoiceGet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "voice.get",
            summary: "Show which voice a character speaks in.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        require_character(ctx, &self.character)?;
        let assignment = read_assignment(ctx.doc, &self.character)
            .map_err(|e| OpError::invalid("character", format!("its voice-assignment.toml doesn't read: {e}")))?;
        let Some(assignment) = assignment else {
            return Ok(Plan::answer(json!({
                "character": self.character,
                "assignment": null,
                "hint": "voice.list lists the voices; voice.assign gives the character one",
            })));
        };
        let info = catalog(ctx)?
            .voices(&assignment.provider)
            .ok()
            .and_then(|voices| voices.into_iter().find(|v| v.id == assignment.voice));
        Ok(Plan::answer(json!({
            "character": self.character,
            "assignment": assignment,
            "voice": info,
            "available": info.is_some(),
        })))
    }
}

impl Op for VoiceList {
    fn meta() -> OpMeta {
        OpMeta {
            name: "voice.list",
            summary: "List the voices a provider offers (local models by default).",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let provider = provider("provider", self.provider.as_deref())?;
        let language = self.language.as_deref().map(str::to_ascii_lowercase);
        let speaks = |info: &VoiceInfo| {
            language.as_deref().is_none_or(|wanted| {
                info.languages.iter().any(|l| {
                    let l = l.as_str().to_ascii_lowercase();
                    l == wanted || l.split('-').next() == Some(wanted)
                })
            })
        };
        let catalog = catalog(ctx)?;
        let consent = consent(ctx);
        // Local voices carry their model's status, so a voice of a model not yet installed says so.
        let statuses: Vec<(VoiceId, String, ModelStatus)> = match (&catalog, &provider) {
            (CatalogRef::Local(registry), VoiceProvider::Local) => {
                local_voices(*registry).into_iter().map(|v| (v.info.id, v.model_name, v.status)).collect()
            }
            (CatalogRef::Wired(_), VoiceProvider::Local) => match ctx.service::<dyn ModelRegistry>() {
                Ok(registry) => {
                    local_voices(registry).into_iter().map(|v| (v.info.id, v.model_name, v.status)).collect()
                }
                Err(_) => Vec::new(),
            },
            _ => Vec::new(),
        };
        let voices = catalog.voices(&provider).map_err(|e| OpError::conflict(e.to_string()))?;
        let rows: Vec<Value> = voices
            .iter()
            .filter(|v| speaks(v))
            .map(|v| {
                let model = statuses.iter().find(|(id, ..)| id == &v.id);
                let consent_found = v
                    .imitates
                    .as_deref()
                    .filter(|_| v.needs_consent())
                    .map(|person| consent.find(person, omuzaki_ai_api::Likeness::VoiceClone).is_some());
                json!({
                    "id": v.id,
                    "languages": v.languages,
                    "imitates": v.imitates,
                    "cloned": v.cloned,
                    "needs_consent": v.needs_consent(),
                    "consent_found": consent_found,
                    "model": model.map(|(_, name, _)| name),
                    "installed": model.map(|(.., status)| *status == ModelStatus::Ready),
                    "status": model.map(|(.., status)| status),
                    "warning": figure_warning(&v.id, None),
                })
            })
            .collect();
        let price = catalog.price(&provider);
        Ok(Plan::answer(json!({
            "provider": provider,
            "voices": rows,
            "price": price.as_ref().map(dollars),
        })))
    }
}

impl VoiceAssign {
    /// The assignment this call asks for, starting from the character's current one.
    fn assignment(&self, current: Option<&VoiceAssignment>) -> Result<VoiceAssignment, OpError> {
        let provider = provider("provider", self.provider.as_deref())?;
        let voice = voice_id("voice", &self.voice)?;
        let speed = self.speed.or(current.map(|a| a.speed)).unwrap_or(1.0);
        let pitch = self.pitch_semitones.or(current.map(|a| a.pitch_semitones)).unwrap_or(0.0);
        let mut assignment = VoiceAssignment::new(self.character.clone(), voice, provider).tuned(speed, pitch);
        if let Some(person) = &self.consent {
            assignment = assignment.with_consent(ConsentRef::new(person.trim()));
        }
        Ok(assignment)
    }
}

impl Op for VoiceAssign {
    fn meta() -> OpMeta {
        OpMeta {
            name: "voice.assign",
            summary: "Give a character a voice, with its speed and pitch.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        require_character(ctx, &self.character)?;
        let current = read_assignment(ctx.doc, &self.character).ok().flatten();
        let mut assignment = self.assignment(current.as_ref())?;
        if !(SPEED_MIN..=SPEED_MAX).contains(&assignment.speed) {
            return Err(refused(AssignError::speed(assignment.speed), &[]));
        }
        if !(PITCH_MIN_SEMITONES..=PITCH_MAX_SEMITONES).contains(&assignment.pitch_semitones) {
            return Err(refused(AssignError::pitch(assignment.pitch_semitones), &[]));
        }
        let hosted = matches!(assignment.provider, VoiceProvider::Hosted(_));
        if hosted && !ctx.allows(Scope::Spend) {
            return Err(OpError::denied(
                "a hosted voice spends money on every line it speaks: it needs the spend grant",
            )
            .hint("ask the person for the spend grant, or pick a local voice (voice.list)"));
        }
        let catalog = catalog(ctx)?;
        let consent = consent(ctx);
        let voices = catalog.voices(&assignment.provider).unwrap_or_default();
        // Name the record of the person the voice imitates when the caller didn't and the record exists.
        if assignment.consent.is_none()
            && let Some(person) = voices
                .iter()
                .find(|v| v.id == assignment.voice)
                .and_then(|v| v.imitates.clone().filter(|_| v.needs_consent()))
            && consent.find(&person, omuzaki_ai_api::Likeness::VoiceClone).is_some()
        {
            assignment = assignment.with_consent(ConsentRef::new(person));
        }
        let info = admit(&assignment, &catalog, consent).map_err(|e| refused(e, &voices))?;

        let mut warnings = Vec::new();
        if let Some(warning) = figure_warning(&assignment.voice, character_name(ctx, &self.character).as_deref()) {
            warnings.push(warning);
        }
        if info.languages.is_empty() {
            warnings.push("the voice doesn't say which languages it speaks".to_owned());
        }
        let after = serde_json::to_value(&assignment).map_err(|e| OpError::internal(e.to_string()))?;
        let path = assignment_path(&self.character);
        let before = ctx.doc.get(&path).cloned();
        let mut plan = Plan::edit(Patch::new().with(path, before, Some(after)));
        let price = catalog.price(&assignment.provider).filter(|_| hosted);
        if let Some(price) = &price {
            plan.cost = Some(CostHint { usd: Some(price.per_hour().0 as f32 / 1e6), ..CostHint::default() });
        }
        plan.output = Some(json!({
            "character": self.character,
            "assignment": assignment,
            "previous": current.map(|a| a.voice),
            "warnings": warnings,
            "cost": price.as_ref().map(dollars),
        }));
        Ok(plan)
    }
}

impl Op for VoicePreviewOp {
    fn meta() -> OpMeta {
        OpMeta {
            name: "voice.preview",
            summary: "Hear a voice: one line spoken to the monitor output, never the stream.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let assignment = match (&self.character, &self.voice) {
            (_, Some(voice)) => {
                let character = self
                    .character
                    .clone()
                    .unwrap_or_else(|| CharId::new("preview").unwrap_or_else(|_| unreachable!("a literal id")));
                let provider = provider("provider", self.provider.as_deref())?;
                VoiceAssignment::new(character, voice_id("voice", voice)?, provider)
                    .tuned(self.speed.unwrap_or(1.0), self.pitch_semitones.unwrap_or(0.0))
            }
            (Some(character), None) => {
                require_character(ctx, character)?;
                read_assignment(ctx.doc, character)
                    .map_err(|e| OpError::invalid("character", format!("its voice-assignment.toml doesn't read: {e}")))?
                    .ok_or_else(|| {
                        OpError::not_found(format!("`{character}` has no voice yet"))
                            .hint("voice.assign gives it one, or preview a voice by naming it")
                    })?
            }
            (None, None) => return Err(OpError::invalid("voice", "name a voice, or a character to hear its voice")),
        };
        if matches!(assignment.provider, VoiceProvider::Hosted(_)) && !ctx.allows(Scope::Spend) {
            return Err(OpError::denied("previewing a hosted voice spends money: it needs the spend grant"));
        }
        let text = self.text.as_deref().unwrap_or(PREVIEW_LINE).trim().to_owned();
        if text.is_empty() || text.chars().count() > CHUNK_MAX_CHARS {
            return Err(OpError::invalid("text", format!("a line of 1 to {CHUNK_MAX_CHARS} characters")));
        }
        let catalog = catalog(ctx)?;
        let voices = catalog.voices(&assignment.provider).unwrap_or_default();
        let mut assignment = assignment;
        if assignment.consent.is_none()
            && let Some(person) = voices
                .iter()
                .find(|v| v.id == assignment.voice)
                .and_then(|v| v.imitates.clone().filter(|_| v.needs_consent()))
        {
            assignment = assignment.with_consent(ConsentRef::new(person));
        }
        admit(&assignment, &catalog, consent(ctx)).map_err(|e| refused(e, &voices))?;
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "voice": assignment.voice, "text": text, "played": false })));
        }
        let preview = ctx.service::<dyn VoicePreview>()?;
        let started = preview.preview(&assignment, &text).map_err(|e| OpError::conflict(e.to_string()))?;
        Ok(Plan::answer(json!({
            "voice": assignment.voice,
            "text": started.text,
            "seconds": (started.seconds * 10.0).round() / 10.0,
            "played": true,
            "to": "monitor_out",
        })))
    }
}
