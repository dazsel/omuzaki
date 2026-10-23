//! The `performer.*` ops (`ai-performer.md` §3, §4, §7.3): the AI co-host started, stopped and steered the same way
//! by a person in the studio, a hotkey, a show's rules and an agent over MCP.
//!
//! | Op | Kind | Scope |
//! |---|---|---|
//! | `performer.start`, `performer.stop`, `performer.say`, `performer.intent`, `performer.respond` | live action | `perform` |
//! | `performer.status`, `performer.transcript` | query | `perform` |
//! | `performer.persona.get` | query | `read` |
//! | `performer.persona.set` | edit, undoable | `edit:character` |
//!
//! The live actions act on the running co-host ([`CohostService`]), never on the document; a dry run says what would
//! happen and starts nothing. The transcript is a query under `perform`, not `read`: it is what the person said, so
//! only a caller who may run the co-host may read it, and it never enters the op log. The persona is part of the
//! character (`characters/<id>/persona`, saved as `persona.toml`), so it travels, merges and undoes like the rest of it.

use omuzaki_char_api::{CharId, paths};
use omuzaki_doc_api::{DocPath, Patch, Value};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_performer::addressing::Respond;
use omuzaki_performer::persona::Persona;
use omuzaki_performer_api::intent::Intent;
use omuzaki_stage_api::ids::MemberId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::service::{Booking, CohostService, StartError, TRANSCRIPT_KEPT};

/// The longest line `performer.say` speaks, in characters.
pub const SAY_MAX_CHARS: usize = 2000;

/// The member a co-host performs when the project has no character to name it after.
const DEFAULT_MEMBER: &str = "cohost";

fn live(name: &'static str, summary: &'static str) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Edit, scope: Scope::Perform, undo: Undo::None }
}

fn query(name: &'static str, summary: &'static str, scope: Scope) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Query, scope, undo: Undo::None }
}

fn service<'a>(ctx: &OpCtx<'a>) -> Result<&'a CohostService, OpError> {
    ctx.service::<CohostService>()
}

fn not_running() -> OpError {
    OpError::conflict("the co-host is not running").hint("start it with performer.start")
}

fn start_error(error: StartError) -> OpError {
    match error {
        StartError::NotReady { what, fix } => OpError::unsupported(what).hint(fix),
        StartError::Failed(why) => OpError::internal(why),
    }
}

/// The ids of the characters in the document.
fn characters(ctx: &OpCtx<'_>) -> Vec<String> {
    let any = CharId::new("x").ok();
    let table = any.map(|id| paths::character(&id)).and_then(|p| p.split_last().map(|(table, _)| table));
    match table.and_then(|t| ctx.doc.get(&t)) {
        Some(Value::Object(map)) => map.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

fn char_id(id: &str) -> Result<CharId, OpError> {
    CharId::new(id).map_err(|e| OpError::invalid("character", format!("`{id}` is not a character id: {e}")))
}

/// The character `id`'s node, or `not_found` naming the ones there are.
fn character<'a>(ctx: &OpCtx<'a>, id: &str) -> Result<(CharId, &'a Value), OpError> {
    let cid = char_id(id)?;
    ctx.doc.get(&paths::character(&cid)).map(|node| (cid, node)).ok_or_else(|| {
        let known = characters(ctx);
        let mut error = OpError::not_found(format!("there is no character `{id}`"));
        error.field = Some("character".into());
        if known.is_empty() {
            error.hint("the project has no characters yet: create one with character.create")
        } else {
            error.hint(format!("characters in this project: {}", known.join(", ")))
        }
    })
}

fn persona_path(id: &CharId) -> DocPath {
    paths::character(id).join("persona")
}

/// The persona stored with a character, if it has one.
fn stored_persona(ctx: &OpCtx<'_>, id: &CharId) -> Result<Option<Persona>, OpError> {
    let Some(value) = ctx.doc.get(&persona_path(id)) else { return Ok(None) };
    let persona: Persona = serde_json::from_value(value.clone()).map_err(|e| {
        OpError::invalid("persona", format!("the persona stored with `{id}` is not in the persona format: {e}"))
            .hint("repair it with performer.persona.set")
    })?;
    persona.check().map_err(|e| OpError::invalid("persona", e.to_string()))?;
    Ok(Some(persona))
}

/// The persona a character gets until someone writes one: its name, and a co-host's manner.
pub fn default_persona(name: &str) -> Persona {
    let mut persona = Persona::named(name);
    persona.personality = "Warm, curious and quick-witted; a co-host on the streamer's live show who enjoys the chat \
                           and the streamer's projects."
        .into();
    persona.style =
        "Spoken, not written: short sentences, one to three unless asked for more. No lists, no emoji.".into();
    persona.languages = vec!["en".into()];
    persona
}

// ------------------------------------------------------------------------------------------------ start and stop

/// Start the AI co-host: it listens to the microphone, answers aloud in its voice, and moves the character's face and
/// head with what it says. A running co-host is replaced.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, { "character": "mochi", "respond": "always" }]))]
pub struct Start {
    /// The character it performs and whose persona it takes (`characters/<id>`). Defaults to the project's only
    /// character.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character: Option<String>,
    /// A persona to use instead of the character's own, for this run only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<Persona>,
    /// When it answers: `addressed` (when named, the default), `always`, or `listen_only` (only `performer.say` makes
    /// it speak).
    #[serde(default)]
    pub respond: Respond,
}

impl Start {
    fn booking(&self, ctx: &OpCtx<'_>) -> Result<Booking, OpError> {
        let id = match &self.character {
            Some(id) => Some(id.clone()),
            None => match characters(ctx).as_slice() {
                [only] => Some(only.clone()),
                [] => None,
                many => {
                    return Err(OpError::invalid("character", "the project has more than one character")
                        .hint(format!("name one: {}", many.join(", "))));
                }
            },
        };
        let (member, stored, name) = match &id {
            Some(id) => {
                let (cid, node) = character(ctx, id)?;
                let name = node.get("name").and_then(Value::as_str).unwrap_or(id).to_owned();
                let member = MemberId::checked(id).map_err(|e| OpError::invalid("character", e.to_string()))?;
                (member, stored_persona(ctx, &cid)?, name)
            }
            None => {
                (MemberId::checked(DEFAULT_MEMBER).map_err(|e| OpError::internal(e.to_string()))?, None, "Mochi".into())
            }
        };
        let persona = match &self.persona {
            Some(persona) => {
                persona.check().map_err(|e| OpError::invalid("persona", e.to_string()))?;
                persona.clone()
            }
            None => stored.unwrap_or_else(|| default_persona(&name)),
        };
        let mut booking = Booking::new(member, persona).respond(self.respond);
        if let Some(id) = id {
            booking = booking.character(id);
        }
        Ok(booking)
    }
}

impl Op for Start {
    fn meta() -> OpMeta {
        live(
            "performer.start",
            "Start the AI co-host: it listens, answers aloud and moves the character's face and head with its words.",
        )
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        let booking = self.booking(ctx)?;
        if let Some(host) = service.factory().sends_to() {
            ctx.require_person(format!("the co-host sends what it hears to {host}, which is not this machine"))?;
        }
        if ctx.dry_run {
            let ready = service.factory().check();
            let mut answer = json!({
                "applied": false,
                "character": booking.character,
                "persona": booking.persona.name,
                "respond": booking.respond,
                "replaces_running": service.is_running(),
                "ready": ready.is_ok(),
            });
            if let Err(StartError::NotReady { what, fix }) = ready {
                answer["missing"] = json!(what);
                answer["hint"] = json!(fix);
            }
            if let Some(costs) = service.factory().costs() {
                answer["costs"] = costs;
            }
            return Ok(Plan::answer(answer));
        }
        let mut answer = service.start(booking).map_err(start_error)?;
        answer["applied"] = json!(true);
        Ok(Plan::answer(answer))
    }
}

/// Stop the AI co-host: it stops mid-word, lets go of its expressions and stops listening.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Stop {}

impl Op for Stop {
    fn meta() -> OpMeta {
        live("performer.stop", "Stop the AI co-host: it stops talking and listening and lets go of the character.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "applied": false, "running": service.is_running() })));
        }
        let was_running = service.stop();
        Ok(Plan::answer(json!({ "applied": true, "was_running": was_running })))
    }
}

// ------------------------------------------------------------------------------------------------ steering

/// Make the co-host say a line now, in its voice, with its face moving; inline tags such as `[emote happy]` work. A
/// line it is saying is cut.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "text": "[emote happy 0.8] Welcome back, everyone!" }]))]
pub struct Say {
    /// What to say, at most 2000 characters.
    pub text: String,
}

impl Op for Say {
    fn meta() -> OpMeta {
        live("performer.say", "Make the AI co-host say a line now, in its voice, with its face moving.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        if self.text.trim().is_empty() {
            return Err(OpError::invalid("text", "say something: the text is empty"));
        }
        let chars = self.text.chars().count();
        if chars > SAY_MAX_CHARS {
            return Err(OpError::invalid("text", format!("the text is {chars} characters, over {SAY_MAX_CHARS}"))
                .hint("split it into shorter lines"));
        }
        if !service.is_running() {
            return Err(not_running());
        }
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "applied": false, "chars": chars })));
        }
        service.say(&self.text).ok_or_else(not_running)?;
        Ok(Plan::answer(json!({ "applied": true, "chars": chars })))
    }
}

/// Make the co-host act now: an expression, a gesture, a look; checked, clamped and rate-limited like its own.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "intent": { "emote": { "expression": "happy", "intensity": 0.8, "hold_s": 2.0 } } }]))]
pub struct IntentOp {
    /// The action.
    pub intent: Intent,
}

impl Op for IntentOp {
    fn meta() -> OpMeta {
        live("performer.intent", "Make the AI co-host act now: an expression, a gesture or a look.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        if matches!(self.intent, Intent::Speak { .. }) {
            return Err(OpError::invalid("intent", "speech is not an intent").hint("make it speak with performer.say"));
        }
        if !service.is_running() {
            return Err(not_running());
        }
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "applied": false, "intent": self.intent })));
        }
        service.intent(&self.intent).ok_or_else(not_running)?;
        Ok(Plan::answer(json!({ "applied": true, "intent": self.intent })))
    }
}

/// Change when the running co-host answers: `addressed` (when named), `always`, or `listen_only`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "respond": "always" }]))]
pub struct RespondOp {
    /// When it answers.
    pub respond: Respond,
}

impl Op for RespondOp {
    fn meta() -> OpMeta {
        live("performer.respond", "Change when the AI co-host answers: when named, always, or only when told to.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        if !service.is_running() {
            return Err(not_running());
        }
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "applied": false, "respond": self.respond })));
        }
        service.set_respond(self.respond).ok_or_else(not_running)?;
        Ok(Plan::answer(json!({ "applied": true, "respond": self.respond })))
    }
}

// ------------------------------------------------------------------------------------------------ reading

/// Whether the AI co-host runs, for which character, on which models, and where its turn is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Status {}

impl Op for Status {
    fn meta() -> OpMeta {
        query("performer.status", "Whether the AI co-host runs, on what, and where its turn is.", Scope::Perform)
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(service(ctx)?.status()))
    }
}

/// What the co-host heard, thought and said, oldest first: pass the answer's `next` as `after` to read only what is
/// new. Private turns carry no text. Kept in memory only, never in the project or the op log.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, { "after": 12, "limit": 50 }]))]
pub struct Transcript {
    /// Only lines after this sequence number (0, the default, for every kept line).
    #[serde(default)]
    pub after: u64,
    /// At most this many lines (default 100, at most 512).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

impl Op for Transcript {
    fn meta() -> OpMeta {
        query("performer.transcript", "What the AI co-host heard and said, oldest first.", Scope::Perform)
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let limit = self.limit.map_or(100, |l| l as usize);
        if limit == 0 || limit > TRANSCRIPT_KEPT {
            return Err(OpError::invalid("limit", format!("the limit is 1 to {TRANSCRIPT_KEPT}")));
        }
        Ok(Plan::answer(service(ctx)?.transcript(self.after, limit)))
    }
}

// ------------------------------------------------------------------------------------------------ persona

/// Read a character's co-host persona (`characters/<id>/persona.toml`): its name, aliases, personality, style and
/// boundaries. A character without one answers the persona it would get.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "character": "mochi" }]))]
pub struct PersonaGet {
    /// The character.
    pub character: String,
}

impl Op for PersonaGet {
    fn meta() -> OpMeta {
        query("performer.persona.get", "Read a character's AI co-host persona.", Scope::Read)
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (id, node) = character(ctx, &self.character)?;
        let stored = stored_persona(ctx, &id)?;
        let is_stored = stored.is_some();
        let persona = stored
            .unwrap_or_else(|| default_persona(node.get("name").and_then(Value::as_str).unwrap_or(&self.character)));
        Ok(Plan::answer(json!({
            "character": self.character,
            "stored": is_stored,
            "persona": persona,
            "revision": persona.revision().to_string(),
            "toml": persona.to_toml().unwrap_or_default(),
        })))
    }
}

/// Write a character's co-host persona (`characters/<id>/persona.toml`); undoable. A running co-host takes it at its
/// next start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{
    "character": "mochi",
    "persona": {
        "name": "Mochi",
        "aliases": ["Mochie", "Mocchi"],
        "personality": "Bubbly and curious; teases the streamer gently.",
        "style": "Short spoken sentences."
    }
}]))]
pub struct PersonaSet {
    /// The character.
    pub character: String,
    /// The persona; its texts are at most 1,000 characters, its lists at most 16 items.
    pub persona: Persona,
}

impl Op for PersonaSet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "performer.persona.set",
            summary: "Write a character's AI co-host persona: name, aliases, personality, style and boundaries.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (id, _) = character(ctx, &self.character)?;
        self.persona.check().map_err(|e| OpError::invalid("persona", e.to_string()))?;
        let path = persona_path(&id);
        let before = ctx.doc.get(&path).cloned();
        let after = serde_json::to_value(&self.persona).map_err(|e| OpError::internal(e.to_string()))?;
        let mut plan = Plan::edit(Patch::new().with(path, before, Some(after)));
        let running = ctx.service::<CohostService>().is_ok_and(CohostService::is_running);
        plan.output = Some(json!({
            "character": self.character,
            "revision": self.persona.revision().to_string(),
            "running_cohost_restarts_to_use_it": running,
        }));
        Ok(plan)
    }
}

register_op!(Start);
register_op!(Stop);
register_op!(Say);
register_op!(IntentOp);
register_op!(RespondOp);
register_op!(Status);
register_op!(Transcript);
register_op!(PersonaGet);
register_op!(PersonaSet);
