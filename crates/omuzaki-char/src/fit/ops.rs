//! The fit ops: `fit.reference` (fit the character's body to reference silhouettes and report the proposal, the `IoU`
//! before and after and a difference heatmap per view, changing nothing) and `fit.apply` (the same fit, written into
//! the body part's `proportions` and `shape` as one undoable step).
//!
//! **Both are jobs** (`OpKind::Job`, design `ops-api.md` section 8). A fit builds and scores up to
//! [`MAX_EVALUATIONS`] bodies, seconds of work that must not run under the executor's one document lock:
//! - `plan` (under the lock) checks the arguments and the stored body, and hands `work` what it chose;
//! - `work` (off the lock) fits, reporting progress and stopping when the job is cancelled;
//! - `finish` (under the lock again) answers, or for `fit.apply` writes the result, refusing if the body changed while
//!   the fit ran. A dry run is the plan alone: it checks everything and fits nothing.
//!
//! **The fit starts from the stored body** (`solver::fit_from`), with its hands as they are. So the `IoU` before is the
//! character's own, a second fit after an apply starts where the first one ended, and an apply can never make the
//! outline cost more than it did.
//!
//! **Neither op writes the asset store.** `fit.reference` returns its heatmaps inline as base64 PNG, so a caller with
//! only `read` changes nothing anywhere, and nothing derived from a caller's reference is given a licence record.
//!
//! **Inputs** ([`input`](super::input)): each view's silhouette as text rows, a base64 PNG, or a PNG in the asset
//! store. **Parts**: the character's other parts (head, hair, accessories, garments) are drawn into the silhouette with
//! every candidate body, as built on the stored body (the solver's fixed parts, built once in the job), so a reference
//! of the whole character fits the body inside it; `parts` picks which (all enabled ones by default, `[]` for the body
//! alone). They stay where the stored body put them, as the head and hair do when the body changes today (the head
//! is placed from its own parameters, #825); the neck and anything else built on the body's joints is drawn as on the
//! stored body. The fit is deterministic, so `fit.apply` writes exactly what `fit.reference` proposed for the same
//! arguments and the same stored character.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use omuzaki_char_api::{BodyParams, BodyShape, CharId, CharacterDef, PartId, Proportions, paths};
use omuzaki_core::mesh::MeshData;
use omuzaki_doc_api::Change;
use omuzaki_face_api::Validate;
use omuzaki_ops_api::{CostHint, JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_store_api::Store;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::heatmap::heatmap;
use super::input::{ViewMask, from_fit, parse_views};
use super::solver::{FitInput, FitResult, MAX_EVALUATIONS, PARAM_ORDER, fit_from};
use super::{FitError, FitParams, FitView, Objective, Target};
use crate::ops::character::access;
use crate::ops::weights::build_character;

/// The kind id of the body part.
const BODY_KIND: &str = "body";
/// The time a fit is expected to take, shown before it starts: a release build on a loaded desktop fitted three
/// 384-row views of the body in 25 to 38 s each, four at once. The job manager's time cap is four times the estimate.
const FIT_SECONDS: f32 = 60.0;

/// Fits the character's body proportions and shape to reference silhouettes and reports the proposal without changing
/// anything: the parameter patch, the `IoU` and the cost before (the character as stored) and after (mean and per
/// view), and per view a difference heatmap as a base64 PNG (red where the fitted character is outside the reference,
/// blue where the reference is outside it, with the `IoU` printed in a legend strip). The other parts are drawn with
/// the body (`parts`). A job: the fit runs off the document lock, and a dry run only checks the arguments.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "views": [{"view": "front", "rows": [
            "..........", "...####...", "...####...", "..######..", "..######..", "...####...", "...#..#...", ".........."
        ]}], "heatmaps": false}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct FitReference {
    /// The character whose body is fitted.
    pub character: CharId,
    /// The reference silhouettes, one per view (`front`, `side`, `back`), 1 to 3.
    pub views: Vec<ViewMask>,
    /// The other parts drawn into the silhouette with the body, by id. Omitted (or `null`): every enabled part, in
    /// the character's order. `[]`: the body alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<PartId>>,
    /// Whether to return the heatmaps (default true). A caller that needs only the numbers leaves them out.
    #[serde(default = "with_heatmaps")]
    pub heatmaps: bool,
}

fn with_heatmaps() -> bool {
    true
}

/// Fits the character's body to reference silhouettes and writes the result into the body's `proportions` and
/// `shape`, as one undoable step. It is the fit `fit.reference` proposes for the same arguments, from the character as
/// stored; it is refused if the body changes while the fit runs. A job: a dry run only checks the arguments.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "views": [{"view": "front", "rows": [
            "..........", "...####...", "...####...", "..######..", "..######..", "...####...", "...#..#...", ".........."
        ]}]}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct FitApply {
    /// The character whose body is fitted.
    pub character: CharId,
    /// The reference silhouettes, one per view (`front`, `side`, `back`), 1 to 3.
    pub views: Vec<ViewMask>,
    /// The other parts drawn into the silhouette with the body, by id. Omitted (or `null`): every enabled part, in
    /// the character's order. `[]`: the body alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<PartId>>,
}

/// What `plan` chose and `work` fits: the character's only body part as stored when the job was planned. It is the
/// plan's output, so a dry run shows it and `work` reads it back (`JobCtx::plan_output`) without choosing again.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Chosen {
    character: CharId,
    part: PartId,
    template: String,
    body: BodyParams,
    views: Vec<String>,
    /// The other parts drawn with the body.
    parts: Vec<PartId>,
}

/// The character's only body part, as stored, with its parameters checked.
fn load(ctx: &OpCtx<'_>, character: &CharId) -> Result<(CharacterDef, PartId, BodyParams), OpError> {
    let def = access::load(ctx, character)?;
    let mut bodies = def.parts.iter().filter(|(_, p)| p.kind.as_str() == BODY_KIND);
    let (part, stored) = match (bodies.next(), bodies.next()) {
        (Some(only), None) => only,
        (None, _) => {
            return Err(access::not_found("character", format!("character `{character}` has no body part"))
                .hint("add a part of kind `body` with part.add, then retry"));
        }
        (Some(_), Some(_)) => {
            return Err(OpError::conflict(format!("character `{character}` has more than one body part"))
                .hint("keep one body part: the fit edits the only one"));
        }
    };
    let field = format!("parts/{part}/params");
    let params: BodyParams = serde_json::from_value(stored.params.clone()).map_err(|e| {
        OpError::invalid(field.clone(), format!("the body's parameters don't match the body format: {e}"))
    })?;
    // The fit starts from the stored body, so it must be one the body accepts.
    if let Err(errors) = params.validate()
        && let Some(first) = errors.0.first()
    {
        return Err(OpError::invalid(format!("{field}/{}", first.path), first.message.clone())
            .hint("repair the body's parameters with part.set_param, then retry"));
    }
    let part = part.clone();
    Ok((def, part, params))
}

/// The parts drawn with the body: `asked`, each checked to be one of the character's enabled parts other than the
/// body, or every enabled part other than the body when nothing is asked.
fn pick_parts(def: &CharacterDef, body: &PartId, asked: Option<&[PartId]>) -> Result<Vec<PartId>, OpError> {
    let Some(asked) = asked else {
        return Ok(def.parts.iter().filter(|(id, p)| *id != body && p.enabled).map(|(id, _)| id.clone()).collect());
    };
    let mut picked = Vec::with_capacity(asked.len());
    for (i, id) in asked.iter().enumerate() {
        let field = format!("parts/{i}");
        let Some(part) = def.parts.get(id) else {
            let known: Vec<String> = def.parts.keys().filter(|k| *k != body).map(ToString::to_string).collect();
            return Err(OpError::invalid(field, format!("there is no part `{id}`"))
                .hint(format!("parts of this character: {}", known.join(", "))));
        };
        if id == body {
            return Err(OpError::invalid(field, format!("`{id}` is the body, which the fit always draws"))
                .hint("list only the other parts to draw with it"));
        }
        if !part.enabled {
            return Err(OpError::invalid(field, format!("part `{id}` is disabled, so it is not built"))
                .hint("enable it with part.set_enabled, or leave it out"));
        }
        if !picked.contains(id) {
            picked.push(id.clone());
        }
    }
    Ok(picked)
}

/// The plan both ops share: the arguments and the stored character checked under the lock (each mask decoded, the
/// views distinct and not only specks, the template known, the parts the character's own), with no fitting.
fn plan_fit(
    ctx: &OpCtx<'_>,
    character: &CharId,
    views: &[ViewMask],
    parts: Option<&[PartId]>,
) -> Result<Plan, OpError> {
    let (def, part, body) = load(ctx, character)?;
    let targets = parse_views(views, ctx.service::<dyn Store>().ok())?;
    let template = def.template.to_string();
    Objective::new(&template, &targets, &[]).map_err(|e| from_fit(&e))?;
    let parts = pick_parts(&def, &part, parts)?;
    let chosen = Chosen {
        character: character.clone(),
        part,
        template,
        body,
        views: views.iter().map(|v| v.view.clone()).collect(),
        parts,
    };
    Ok(Plan {
        output: Some(to_value(&chosen)?),
        cost: Some(CostHint { seconds: Some(FIT_SECONDS), ..CostHint::default() }),
        ..Plan::default()
    })
}

/// What the plan chose, read back in `work`.
fn chosen(job: &JobCtx<'_>) -> Result<Chosen, OpError> {
    let output = job.plan_output().ok_or_else(|| OpError::internal("the fit job was started without its plan"))?;
    serde_json::from_value(output.clone())
        .map_err(|e| OpError::internal(format!("the fit job's plan output is malformed: {e}")))
}

/// The meshes of the parts drawn with the body: the character as stored, built on the planned body, with every mesh of
/// `chosen.parts`. Empty when none is drawn.
fn parts_in_job(job: &JobCtx<'_>, chosen: &Chosen) -> Result<Vec<MeshData>, OpError> {
    if chosen.parts.is_empty() {
        return Ok(Vec::new());
    }
    let node = job.read(|doc| doc.get(&paths::character(&chosen.character)).cloned()).ok_or_else(|| {
        access::not_found("character", format!("character `{}` was removed while the fit ran", chosen.character))
    })?;
    let mut def = CharacterDef::from_node(&node)
        .map_err(|e| OpError::invalid("character", format!("character `{}` no longer loads: {e}", chosen.character)))?;
    // Built on the body the plan chose, so the parts sit where the fit starts.
    if let Some(body) = def.parts.get_mut(&chosen.part) {
        body.params = to_value(&chosen.body)?;
    }
    let built = build_character(&chosen.character, &def)?;
    Ok(built.meshes.into_iter().filter(|m| chosen.parts.contains(&m.part)).map(|m| m.mesh.geometry).collect())
}

/// Runs the fit of `chosen` against `views` in a job: progress after each evaluation, and a cancel stops it.
fn fit_in_job(
    job: &JobCtx<'_>,
    chosen: &Chosen,
    views: &[ViewMask],
) -> Result<(Vec<Target>, Vec<MeshData>, FitResult), OpError> {
    let targets = parse_views(views, job.service::<dyn Store>().ok())?;
    let parts = parts_in_job(job, chosen)?;
    let input = FitInput { template: &chosen.template, targets: &targets, fixed: &parts };
    #[allow(clippy::cast_precision_loss)] // counts up to MAX_EVALUATIONS, far inside f32's exact integers
    let mut watch = |done: usize| {
        job.progress("fitting", done as f32 / MAX_EVALUATIONS as f32);
        !job.is_cancelled()
    };
    let result = fit_from(&input, &chosen.body, &mut watch).map_err(|e| match e {
        FitError::Stopped => OpError::cancelled(job.id()),
        other => from_fit(&other),
    })?;
    Ok((targets, parts, result))
}

/// `{name: value}` of the parameters of `fit` that differ from `before`, in [`PARAM_ORDER`].
fn patch(before: &FitParams, fit: &FitParams) -> Value {
    let (mut proportions, mut shape) = (serde_json::Map::new(), serde_json::Map::new());
    for p in PARAM_ORDER {
        let (a, b) = (p.get(before), p.get(fit));
        if (a - b).abs() > 0.0 {
            let target = if p.range() == (-1.0, 1.0) { &mut shape } else { &mut proportions };
            target.insert(p.name().to_owned(), json!(b));
        }
    }
    json!({ "proportions": proportions, "shape": shape })
}

/// The answer both ops give: the patch against the stored body, the `IoU`s and the evaluations made.
fn summary(chosen: &Chosen, result: &FitResult) -> Value {
    let before = FitParams { proportions: chosen.body.proportions.clone(), shape: chosen.body.shape.clone() };
    let per_view: Vec<Value> = result
        .per_view
        .iter()
        .map(|v| json!({ "view": view_name(v.view), "iou_before": v.iou_before, "iou_after": v.iou_after }))
        .collect();
    json!({
        "character": chosen.character,
        "part": chosen.part,
        "parts": chosen.parts,
        "patch": patch(&before, &result.params),
        "iou_before": result.iou_before,
        "iou_after": result.iou_after,
        "cost_before": result.cost_before,
        "cost_after": result.cost_after,
        "per_view": per_view,
        "evaluations": result.evaluations,
    })
}

fn view_name(view: FitView) -> &'static str {
    match view {
        FitView::Front => "front",
        FitView::Side => "side",
        FitView::Back => "back",
    }
}

/// The heatmap of each view as a base64 PNG, in the targets' order: the fitted body with the stored hands and the
/// parts drawn with it, against the target as the fit saw it.
fn heatmaps(
    chosen: &Chosen,
    targets: &[Target],
    parts: &[MeshData],
    result: &FitResult,
) -> Result<Vec<String>, OpError> {
    let objective = Objective::new(&chosen.template, targets, parts).map_err(|e| from_fit(&e))?.with_body(&chosen.body);
    let masks = objective.masks(&result.params).map_err(|e| from_fit(&e))?;
    masks
        .iter()
        .map(|m| {
            let png = heatmap(&m.model, &m.target).and_then(|image| image.png()).map_err(|e| from_fit(&e))?;
            Ok(BASE64.encode(png))
        })
        .collect()
}

impl Op for FitReference {
    fn meta() -> OpMeta {
        OpMeta {
            name: "fit.reference",
            summary: "Fit the body proportions and shape to reference silhouettes without applying: the proposed patch, \
                      IoU before and after, and a difference heatmap per view",
            kind: OpKind::Job,
            scope: Scope::Read,
            // Its finish plans no change, so its empty patch is undone exactly and nothing enters the history.
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        plan_fit(ctx, &self.character, &self.views, self.parts.as_deref())
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let chosen = chosen(job)?;
        let (targets, parts, result) = fit_in_job(job, &chosen, &self.views)?;
        let mut out = summary(&chosen, &result);
        if self.heatmaps {
            let maps = heatmaps(&chosen, &targets, &parts, &result)?;
            if let Some(views) = out["per_view"].as_array_mut() {
                for (v, png) in views.iter_mut().zip(maps) {
                    v["heatmap_png"] = json!(png);
                }
            }
        }
        // A finish runs for work that returned even after a cancel request, so the cancel check comes last.
        job.checkpoint("done", 1.0)?;
        Ok(out)
    }
}

/// What `fit.apply`'s work hands its finish.
#[derive(Serialize, Deserialize)]
struct Fitted {
    /// The part, template and body the fit started from, to refuse a finish against a body that changed meanwhile.
    part: PartId,
    template: String,
    from: BodyParams,
    proportions: Proportions,
    shape: BodyShape,
    summary: Value,
}

/// Whether the fit made from `from` on `template` still describes `now`: the same part and template, and the same
/// values for everything the silhouette is built from (the skin's colours may change meanwhile).
fn still_fits(fitted: &Fitted, part: &PartId, template: &str, now: &BodyParams) -> bool {
    let from = &fitted.from;
    fitted.part == *part
        && fitted.template == template
        && (&from.proportions, &from.shape, &from.hands, &from.hand)
            == (&now.proportions, &now.shape, &now.hands, &now.hand)
}

impl Op for FitApply {
    fn meta() -> OpMeta {
        OpMeta {
            name: "fit.apply",
            summary: "Fit the body proportions and shape to reference silhouettes and write the result into the body \
                      part as one undoable step",
            kind: OpKind::Job,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        plan_fit(ctx, &self.character, &self.views, self.parts.as_deref())
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let chosen = chosen(job)?;
        let (_, _, result) = fit_in_job(job, &chosen, &self.views)?;
        let fitted = Fitted {
            summary: summary(&chosen, &result),
            proportions: result.params.proportions,
            shape: result.params.shape,
            part: chosen.part,
            template: chosen.template,
            from: chosen.body,
        };
        job.checkpoint("done", 1.0)?;
        to_value(&fitted)
    }

    fn finish(&self, ctx: &OpCtx<'_>, result: Value) -> Result<Plan, OpError> {
        let fitted: Fitted = serde_json::from_value(result)
            .map_err(|e| OpError::internal(format!("the fit's result is malformed: {e}")))?;
        let (def, part, stored) = load(ctx, &self.character)?;
        if !still_fits(&fitted, &part, def.template.as_str(), &stored) {
            return Err(OpError::conflict(format!(
                "the body of character `{}` changed while the fit ran",
                self.character
            ))
            .hint("run fit.apply again: it fits the body as it is now"));
        }
        // The stored hands, skin and shape preset stay; only the fitted proportions and shape axes are written.
        let after = BodyParams {
            proportions: fitted.proportions,
            shape: BodyShape { preset: stored.shape.preset.clone(), ..fitted.shape },
            ..stored.clone()
        };
        let base = paths::part_params(&self.character, &part);
        let mut fields = serde_json::Map::new();
        for (key, before, now) in [
            ("proportions", to_value(&stored.proportions)?, to_value(&after.proportions)?),
            ("shape", to_value(&stored.shape)?, to_value(&after.shape)?),
        ] {
            if before != now {
                fields.insert(key.to_owned(), now);
            }
        }
        // A body whose file has no `params` yet gets them created whole; otherwise only the two groups are written.
        let changes: Vec<Change> = if fields.is_empty() {
            Vec::new()
        } else if ctx.doc.get(&base).is_none() {
            vec![access::change(ctx, base, Some(Value::Object(fields)))]
        } else {
            fields.into_iter().map(|(key, now)| access::change(ctx, base.join(key), Some(now))).collect()
        };
        Ok(access::edit_plan(changes, fitted.summary))
    }
}

fn to_value<T: Serialize>(v: &T) -> Result<Value, OpError> {
    serde_json::to_value(v).map_err(|e| OpError::internal(format!("the fit's values do not serialise: {e}")))
}

register_op!(FitReference);
register_op!(FitApply);
