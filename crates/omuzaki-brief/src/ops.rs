//! The brief's ops: `character.create_from_description` and `character.brief_vocabulary`.

use std::collections::BTreeMap;

use omuzaki_char_api::{CharId, paths};
use omuzaki_doc_api::{DocPath, DocRead, DocWrite, Patch, Scratch, Value};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::lexicon::{Choice, ColourTarget, Lexicon};
use crate::parse::{self, Read};
use crate::recipe::{self, COLOUR_KEYS, Named, Recipe};

/// The longest description read, in characters: a paragraph, not a document.
pub const MAX_DESCRIPTION: usize = 2000;
/// The most words of a name made from the description.
const NAME_WORDS: usize = 4;
/// The name of a character whose description gives none and says nothing that names it.
const FALLBACK_NAME: &str = "New character";

/// Creates a character from a written description, with the kit's own ops, and says every choice it made.
///
/// The description is read word by word against the brief's vocabulary (`character.brief_vocabulary` lists it): the
/// build (`chibi`, `tall`, `mature`), the body shape, the skin tone, the hair (style, length, bangs, colours, gradients
/// such as `blonde fading to pink`), the eyes (shape and colour), makeup, the expression, the outfit and its colour,
/// garments, and accessories (`round glasses`, `cat ears`, `horns`, `wings`, `earrings`...). Whatever it doesn't say
/// is drawn from the kit's priors by `seed`, so every result is a finished character, and two seeds give two takes.
///
/// It runs as one step (one undo): `character.create`, then each choice's ops, then the colours; the answer lists the
/// choices (with the words that made each), the ops it ran (to replay or adjust one by one), the words it set aside or
/// didn't understand, and a self-check that every choice holds in the finished character. An agent that understood the
/// description itself can name options directly in `facets`, which win over the words. Render the result with
/// `render.preview`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"description": "a sleepy librarian with silver twin tails and round glasses"},
        {"description": "a cheerful idol with pink hair fading to blonde, sparkly eyes and a red dress", "seed": 3},
        {"description": "a tall knight", "facets": {"outfit": ["cloak"], "hair_colour": ["#c0c8d8"]}, "id": "knight"}
    ]
))]
pub struct CharacterCreateFromDescription {
    /// What the character looks like, in plain words, at most 2000 characters.
    #[schemars(length(min = 1, max = 2000))]
    pub description: String,
    /// The new character's id. Left out: made from the name, with `-2`, `-3`... when it is taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<CharId>,
    /// The display name. Left out: the name the description gives (`named Mika`), or what it calls the character
    /// (`a sleepy librarian` is "Sleepy Librarian").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Draws what the description leaves open; the same seed and words give the same character. 0 when left out.
    #[serde(default)]
    pub seed: u32,
    /// Choices named directly, which win over the words: a facet's option ids (`{"hair_style": ["twintails"]}`;
    /// a list, since `accessories` takes several), or `hair_colour`, `eye_colour` and `outfit_colour` as colour names
    /// or `#rrggbb` (two or more make a gradient). `character.brief_vocabulary` lists every facet and option.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub facets: Named,
}

/// One planned step, as the answer lists it.
#[derive(Clone, Debug, Serialize)]
struct Ran {
    op: String,
    args: Value,
}

/// One line of the self-check.
#[derive(Clone, Debug, Serialize)]
struct Checked {
    facet: String,
    value: String,
    holds: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    problem: Option<String>,
    /// What a later choice refined (`its trousers gave way to skirt`).
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

/// The whole plan: the character's id and name, the combined patch, and the answer's parts.
struct Built {
    id: CharId,
    name: String,
    patch: Patch,
    ran: Vec<Ran>,
    checks: Vec<Checked>,
}

impl CharacterCreateFromDescription {
    /// The name: given, from the description, or what the description calls the character.
    fn name(&self, read: &Read) -> String {
        if let Some(name) = self.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            return name.to_owned();
        }
        if let Some(name) = &read.name {
            return name.clone();
        }
        let words: Vec<String> = read.what.iter().take(NAME_WORDS).map(|w| title_case(w)).collect();
        if words.is_empty() { FALLBACK_NAME.to_owned() } else { words.join(" ") }
    }

    /// The id: given (and free), or made from `name` and made free.
    fn id(&self, ctx: &OpCtx<'_>, name: &str) -> Result<CharId, OpError> {
        let taken = |id: &CharId| ctx.doc.get(&paths::character(id)).is_some();
        if let Some(id) = &self.id {
            if taken(id) {
                return Err(OpError::invalid("id", format!("a character `{id}` already exists"))
                    .hint("choose another id, or leave `id` out and one is made from the name"));
            }
            return Ok(id.clone());
        }
        let base = slug(name);
        let base = if base.is_empty() { "character".to_owned() } else { base };
        (1..=u32::MAX)
            .map(|n| if n == 1 { base.clone() } else { format!("{base}-{n}") })
            .filter_map(|candidate| CharId::new(&candidate).ok())
            .find(|id| !taken(id))
            .ok_or_else(|| OpError::internal("no free character id was found"))
    }

    fn build(&self, ctx: &OpCtx<'_>, lexicon: &Lexicon, read: &Read, recipe: &Recipe) -> Result<Built, OpError> {
        let name = self.name(read);
        let id = self.id(ctx, &name)?;
        let mut scratch = Scratch::of(ctx.doc);
        let mut patch = Patch::new();
        let mut create = json!({"id": id, "name": name, "template": recipe.template, "seed": self.seed});
        if let Some(style) = &recipe.style {
            create["style"] = json!(style);
        }
        let mut ran = vec![Ran { op: "character.create".into(), args: create.clone() }];
        let (_, plan) =
            ctx.plan_step("character.create", create, &scratch).map_err(|e| step_error(0, "character.create", &e))?;
        apply(&mut scratch, &mut patch, plan.patch)?;
        let mut parts = parts_of(&scratch, &id);
        let steps = recipe::steps(recipe, id.as_str(), &mut parts);
        let mut wrote = Vec::with_capacity(steps.len());
        for (i, (made, step)) in steps.iter().enumerate() {
            let (_, plan) =
                ctx.plan_step(&step.op, step.args.clone(), &scratch).map_err(|e| step_error(i + 1, &step.op, &e))?;
            let before = scratch.root().clone();
            apply(&mut scratch, &mut patch, plan.patch)?;
            let mut leaves = Vec::new();
            diff(&before, scratch.root(), String::new(), &mut leaves);
            wrote.push(Wrote { made: *made, leaves });
            ran.push(Ran { op: step.op.clone(), args: step.args.clone() });
        }
        let checks = check(lexicon, recipe, &wrote, &scratch, &id);
        Ok(Built { id, name, patch, ran, checks })
    }
}

impl Op for CharacterCreateFromDescription {
    fn meta() -> OpMeta {
        OpMeta {
            name: "character.create_from_description",
            summary: "Create a character from a written description, and say every choice it made.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let description = self.description.trim();
        if description.is_empty() {
            return Err(OpError::invalid("description", "the description is empty")
                .hint("describe the character, for example \"a cheerful girl with pink twin tails and a hoodie\""));
        }
        if description.chars().count() > MAX_DESCRIPTION {
            return Err(OpError::invalid(
                "description",
                format!("the description is longer than {MAX_DESCRIPTION} characters"),
            )
            .hint("describe how the character looks in a paragraph; set the rest with the part and face ops after"));
        }
        let lexicon = Lexicon::builtin().map_err(OpError::internal)?;
        let read = parse::read(description, lexicon);
        let recipe = recipe::recipe(lexicon, &read, &self.facets, recipe::seed_for(&self.description, self.seed))
            .map_err(|bad| {
                OpError::invalid(format!("facets/{}", bad.key), bad.message)
                    .hint("character.brief_vocabulary lists every facet, option and colour")
            })?;
        let built = self.build(ctx, lexicon, &read, &recipe)?;
        let holds = built.checks.iter().all(|c| c.holds);
        let summary = json!({
            "character": built.id,
            "name": built.name,
            "template": recipe.template,
            "seed": self.seed,
            "choices": recipe.made,
            "set_aside": read.aside.iter().map(|a| json!({"words": a.words, "reason": a.reason})).collect::<Vec<_>>(),
            "not_understood": read.unknown,
            "self_check": {"holds": holds, "checks": built.checks},
            "ops": built.ran,
            "next": [{"op": "render.preview", "args": {"character": built.id}}],
        });
        Ok(Plan { output: Some(summary), ..Plan::edit(built.patch) })
    }
}

register_op!(CharacterCreateFromDescription);

/// Lists the words `character.create_from_description` knows: every facet with its options (their ids, what they
/// give and the phrases that choose them), the named colours, and the nouns a colour goes to. An agent can name these
/// ids in `facets` to say exactly what it means.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, {"facet": "hair_style"}]))]
pub struct CharacterBriefVocabulary {
    /// Only this facet (`hair_style`, `accessories`...), or `colours`. Left out: everything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facet: Option<String>,
}

impl Op for CharacterBriefVocabulary {
    fn meta() -> OpMeta {
        OpMeta {
            name: "character.brief_vocabulary",
            summary: "List the facets, options and colours a description can name.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let lexicon = Lexicon::builtin().map_err(OpError::internal)?;
        let wanted = self.facet.as_deref();
        if let Some(f) = wanted.filter(|f| *f != "colours" && lexicon.facet(f).is_none()) {
            let ids: Vec<&str> = lexicon.facet.iter().map(|f| f.id.as_str()).collect();
            return Err(OpError::invalid("facet", format!("there is no facet `{f}`"))
                .hint(format!("the facets are {}, and colours", ids.join(", "))));
        }
        let facets: Vec<Value> = lexicon
            .facet
            .iter()
            .filter(|f| wanted.is_none_or(|w| w == f.id))
            .map(|f| {
                let options: Vec<Value> = lexicon
                    .options(&f.id)
                    .map(|o| json!({"id": o.id, "summary": o.summary, "words": o.words}))
                    .collect();
                json!({"id": f.id, "summary": f.summary, "many": f.many, "options": options})
            })
            .collect();
        let mut answer = json!({"facets": facets});
        if wanted.is_none_or(|w| w == "colours") {
            let colours: Vec<Value> =
                lexicon.colour.iter().map(|c| json!({"id": c.id, "hex": c.hex, "words": c.words})).collect();
            let targets: Vec<Value> =
                lexicon.target.iter().map(|t| json!({"to": target_key(&t.target), "words": t.words})).collect();
            answer["colours"] = json!(colours);
            answer["colour_targets"] = json!(targets);
            answer["colour_facets"] = json!(COLOUR_KEYS.iter().map(|(k, _)| *k).collect::<Vec<_>>());
        }
        Ok(Plan::answer(answer))
    }
}

register_op!(CharacterBriefVocabulary);

fn target_key(target: &ColourTarget) -> String {
    match target {
        ColourTarget::Hair => "hair_colour".into(),
        ColourTarget::Eyes => "eye_colour".into(),
        ColourTarget::Outfit => "outfit_colour".into(),
        ColourTarget::Part(p) => p.clone(),
    }
}

/// Applies `next` to the scratch and keeps it in the combined patch.
fn apply(scratch: &mut Scratch, combined: &mut Patch, next: Patch) -> Result<(), OpError> {
    scratch.apply(&next).map_err(|e| OpError::internal(format!("a planned step doesn't apply: {e}")))?;
    combined.changes.extend(next.changes);
    Ok(())
}

/// The character's parts, as (id, kind).
fn parts_of(doc: &dyn DocRead, id: &CharId) -> Vec<(String, String)> {
    let Ok(path) = DocPath::parse(&format!("{}/parts", paths::character(id))) else { return Vec::new() };
    let Some(Value::Object(parts)) = doc.get(&path) else { return Vec::new() };
    parts
        .iter()
        .map(|(pid, part)| (pid.clone(), part.get("kind").and_then(Value::as_str).unwrap_or_default().to_owned()))
        .collect()
}

/// What one step changed: the choice it carries out (an index into [`Recipe::made`]) and the leaves it wrote, as JSON
/// pointers with their new values (`None` for a leaf it removed).
struct Wrote {
    made: usize,
    leaves: Vec<(String, Option<Value>)>,
}

/// Every leaf that differs between `before` and `after`, under `at`. An array that changed is one leaf.
fn diff(before: &Value, after: &Value, at: String, out: &mut Vec<(String, Option<Value>)>) {
    match (before, after) {
        (Value::Object(b), Value::Object(a)) => {
            for (key, value) in a {
                let path = format!("{at}/{}", key.replace('~', "~0").replace('/', "~1"));
                match b.get(key) {
                    Some(old) => diff(old, value, path, out),
                    None => diff(&Value::Null, value, path, out),
                }
            }
            for key in b.keys().filter(|k| !a.contains_key(*k)) {
                out.push((format!("{at}/{}", key.replace('~', "~0").replace('/', "~1")), None));
            }
        }
        (Value::Null, Value::Object(a)) => {
            for (key, value) in a {
                diff(&Value::Null, value, format!("{at}/{}", key.replace('~', "~0").replace('/', "~1")), out);
            }
        }
        _ if before != after => out.push((at, Some(after.clone()))),
        _ => {}
    }
}

/// Whether every choice holds in the finished character `done`.
///
/// - An option's parts are there with their kind, unless a later choice replaced one (`trousers` then `a skirt`:
///   the skirt refines the outfit, and the line says so); the parts it replaces are gone unless a choice brought one
///   back.
/// - Every leaf a choice's steps wrote still has that value, unless a later colour wrote it (a colour refines what it
///   colours) or a later choice of a facet that `refines` this one did (makeup's blush over the skin's cheeks). A leaf another later choice changed is
///   a conflict the line names.
fn check(lexicon: &Lexicon, recipe: &Recipe, wrote: &[Wrote], done: &Scratch, id: &CharId) -> Vec<Checked> {
    let parts = parts_of(done, id);
    let colours_from = recipe.options.len();
    recipe
        .made
        .iter()
        .enumerate()
        .map(|(index, made)| {
            let option = lexicon.option(&made.facet, &made.value);
            let mut note = None;
            let mut problem = option.and_then(|o| part_problem(o, index, &parts, recipe, &mut note));
            let by_parts = option.is_some_and(|o| !o.parts.is_empty());
            if problem.is_none() && !by_parts {
                problem = leaf_problem(lexicon, index, wrote, done, colours_from, recipe);
            }
            Checked { facet: made.facet.clone(), value: made.value.clone(), holds: problem.is_none(), problem, note }
        })
        .collect()
}

fn part_problem(
    option: &Choice,
    index: usize,
    parts: &[(String, String)],
    recipe: &Recipe,
    note: &mut Option<String>,
) -> Option<String> {
    for part in &option.parts {
        if parts.iter().any(|(id, kind)| *id == part.id && *kind == part.kind) {
            continue;
        }
        let later = recipe.options.iter().enumerate().skip(index + 1).find(|(_, o)| o.replaces.contains(&part.id));
        match later {
            Some((at, _)) => {
                let by = recipe.made.get(at).map_or("", |m| m.value.as_str());
                *note = Some(format!("its {} gave way to {by}", part.id));
            }
            None => return Some(format!("the {} part `{}` is not in the finished character", part.kind, part.id)),
        }
    }
    let brought_back = |gone: &str| recipe.options.iter().any(|o| o.parts.iter().any(|p| p.id == gone));
    option
        .replaces
        .iter()
        .find(|g| parts.iter().any(|(id, _)| id == *g) && !brought_back(g))
        .map(|gone| format!("the part `{gone}` it replaces is still there"))
}

/// The first leaf choice `index` wrote that the finished character no longer has, and which later choice changed
/// it, unless that was a colour.
fn leaf_problem(
    lexicon: &Lexicon,
    index: usize,
    wrote: &[Wrote],
    done: &Scratch,
    colours_from: usize,
    recipe: &Recipe,
) -> Option<String> {
    let root = done.root();
    for (at, step) in wrote.iter().enumerate().filter(|(_, w)| w.made == index) {
        for (pointer, value) in &step.leaves {
            if root.pointer(pointer) == value.as_ref() {
                continue;
            }
            let by = wrote[at + 1..]
                .iter()
                .filter(|w| w.made != index && w.leaves.iter().any(|(p, _)| p == pointer))
                .map(|w| w.made)
                .next_back();
            let refines = |by: usize| {
                let (Some(later), Some(mine)) = (recipe.made.get(by), recipe.made.get(index)) else { return false };
                lexicon.facet(&later.facet).is_some_and(|f| f.refines.contains(&mine.facet))
            };
            match by {
                Some(by) if (by >= colours_from && index < colours_from) || refines(by) => {}
                Some(by) => {
                    let name = recipe.made.get(by).map_or_else(String::new, |m| format!("{} {}", m.facet, m.value));
                    return Some(format!("{name} changed {pointer} after it"));
                }
                None => return Some(format!("{pointer} does not hold what it wrote")),
            }
        }
    }
    None
}

/// A step's refusal, naming the step.
fn step_error(index: usize, op: &str, e: &OpError) -> OpError {
    OpError::internal(format!("step {index} ({op}) was refused: {}", e.message))
}

fn title_case(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

/// `name` as an id: lower case, runs of anything else as one `-`, at most 48 characters.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(48);
    out.trim_end_matches('-').to_owned()
}
