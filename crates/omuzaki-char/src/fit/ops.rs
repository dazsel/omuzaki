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
//! The reference silhouettes arrive inline as rows of text (`#` character, `.` background), the interim the task
//! allows until sheet images can be decoded here. The fit is deterministic, so `fit.apply` writes exactly what
//! `fit.reference` proposed for the same arguments and the same stored body. Only the body is fitted: the other parts
//! are not part of the silhouette.

use omuzaki_char_api::{BodyParams, BodyShape, CharId, PartId, Proportions, paths};
use omuzaki_doc_api::Change;
use omuzaki_face_api::Validate;
use omuzaki_ops_api::{CostHint, JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::heatmap::heatmap;
use super::solver::{FitInput, FitResult, MAX_EVALUATIONS, PARAM_ORDER, fit_from};
use super::{FitError, FitParams, FitView, MAX_SIDE, Objective, Silhouette, Target, base64, png};
use crate::ops::character::access;

/// The kind id of the body part.
const BODY_KIND: &str = "body";
/// The most views a fit takes: front, side and back.
const MAX_VIEWS: usize = 3;
/// The time a fit is expected to take, shown before it starts: a release build on a loaded desktop fitted three
/// 256-row views in 4 to 33 s and three 512-row views (the largest) in 28 to 57 s. The job manager's time cap is four
/// times the estimate.
const FIT_SECONDS: f32 = 60.0;

/// A reference silhouette.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewMask {
    /// The view: `front`, `side` or `back`.
    pub view: String,
    /// The mask, top row first: `#` is character, `.` is background. All rows are the same length, at most 512 wide
    /// and 512 rows, and at least one pixel is `#`.
    pub rows: Vec<String>,
}

/// Fits the character's body proportions and shape to reference silhouettes and reports the proposal without changing
/// anything: the parameter patch, the `IoU` before (the body as stored) and after (mean and per view), and per view a
/// difference heatmap as a base64 PNG (red where the fitted body is outside the reference, blue where the reference is
/// outside it, with the `IoU` printed in a legend strip). A job: the fit runs off the document lock, and a dry run
/// only checks the arguments.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FitReference {
    /// The character whose body is fitted.
    pub character: CharId,
    /// The reference silhouettes, one per view (`front`, `side`, `back`), 1 to 3.
    pub views: Vec<ViewMask>,
    /// Whether to return the heatmaps (default true). A caller that needs only the numbers leaves them out.
    #[serde(default = "with_heatmaps")]
    pub heatmaps: bool,
}

fn with_heatmaps() -> bool {
    true
}

/// Fits the character's body to reference silhouettes and writes the result into the body's `proportions` and
/// `shape`, as one undoable step. It is the fit `fit.reference` proposes for the same arguments, from the body as
/// stored; it is refused if the body changes while the fit runs. A job: a dry run only checks the arguments.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FitApply {
    /// The character whose body is fitted.
    pub character: CharId,
    /// The reference silhouettes, one per view (`front`, `side`, `back`), 1 to 3.
    pub views: Vec<ViewMask>,
}

fn parse_view(i: usize, v: &ViewMask) -> Result<Target, OpError> {
    let field = |what: &str| format!("views/{i}/{what}");
    let view = match v.view.as_str() {
        "front" => FitView::Front,
        "side" => FitView::Side,
        "back" => FitView::Back,
        other => {
            return Err(OpError::invalid(field("view"), format!("`{other}` is not a view"))
                .hint("valid views: front, side, back"));
        }
    };
    let height = v.rows.len();
    let width = v.rows.first().map_or(0, |r| r.chars().count());
    if height == 0 || height > MAX_SIDE || width == 0 || width > MAX_SIDE {
        return Err(OpError::invalid(
            field("rows"),
            format!("{width} x {height} is outside 1..={MAX_SIDE} on each side"),
        )
        .hint("downsize the silhouette first"));
    }
    let mut on = Vec::with_capacity(width * height);
    for (y, row) in v.rows.iter().enumerate() {
        if row.chars().count() != width {
            return Err(OpError::invalid(field("rows"), format!("row {y} is not {width} wide"))
                .hint("every row is as long as the first"));
        }
        for c in row.chars() {
            on.push(match c {
                '#' => true,
                '.' => false,
                other => {
                    return Err(OpError::invalid(field("rows"), format!("row {y} holds `{other}`"))
                        .hint("rows use `#` for character and `.` for background"));
                }
            });
        }
    }
    let mask = Silhouette::new(width, height, on).map_err(|e| from_fit(&e))?;
    Ok(Target { view, mask })
}

/// The targets of `views`, checked for the count and each mask.
fn parse_views(views: &[ViewMask]) -> Result<Vec<Target>, OpError> {
    if views.is_empty() || views.len() > MAX_VIEWS {
        return Err(OpError::invalid("views", format!("{} views given", views.len()))
            .hint("give one silhouette per view: front, side, back"));
    }
    views.iter().enumerate().map(|(i, v)| parse_view(i, v)).collect()
}

/// The refusal for a fit error: the input is at fault, except when a candidate body cannot be built.
fn from_fit(e: &FitError) -> OpError {
    match e {
        FitError::NoTargets | FitError::DuplicateView(_) => {
            OpError::invalid("views", e.to_string()).hint("give one silhouette per view, at least one")
        }
        FitError::BadMask(_) => OpError::invalid("views", e.to_string()).hint("give a mask with some `#` pixels"),
        _ => OpError::internal(format!("the fit failed: {e}")),
    }
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
}

/// The character's only body part, as stored, with its parameters checked.
fn load(ctx: &OpCtx<'_>, character: &CharId) -> Result<(String, PartId, BodyParams), OpError> {
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
    Ok((def.template.to_string(), part.clone(), params))
}

/// The plan both ops share: the arguments and the stored body checked under the lock (each mask parsed, the views
/// distinct and not only specks, the template known), with no fitting.
fn plan_fit(ctx: &OpCtx<'_>, character: &CharId, views: &[ViewMask]) -> Result<Plan, OpError> {
    let (template, part, body) = load(ctx, character)?;
    let targets = parse_views(views)?;
    Objective::new(&template, &targets, &[]).map_err(|e| from_fit(&e))?;
    let chosen = Chosen {
        character: character.clone(),
        part,
        template,
        body,
        views: views.iter().map(|v| v.view.clone()).collect(),
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

/// Runs the fit of `chosen` against `views` in a job: progress after each evaluation, and a cancel stops it.
fn fit_in_job(job: &JobCtx<'_>, chosen: &Chosen, views: &[ViewMask]) -> Result<(Vec<Target>, FitResult), OpError> {
    let targets = parse_views(views)?;
    let input = FitInput { template: &chosen.template, targets: &targets, fixed: &[] };
    #[allow(clippy::cast_precision_loss)] // counts up to MAX_EVALUATIONS, far inside f32's exact integers
    let mut watch = |done: usize| {
        job.progress("fitting", done as f32 / MAX_EVALUATIONS as f32);
        !job.is_cancelled()
    };
    let result = fit_from(&input, &chosen.body, &mut watch).map_err(|e| match e {
        FitError::Stopped => OpError::cancelled(job.id()),
        other => from_fit(&other),
    })?;
    Ok((targets, result))
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
        "patch": patch(&before, &result.params),
        "iou_before": result.iou_before,
        "iou_after": result.iou_after,
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

/// The heatmap of each view as a base64 PNG, in the targets' order: the fitted body, with the stored hands, against
/// the target as the fit saw it.
fn heatmaps(chosen: &Chosen, targets: &[Target], result: &FitResult) -> Result<Vec<String>, OpError> {
    let objective = Objective::new(&chosen.template, targets, &[]).map_err(|e| from_fit(&e))?.with_body(&chosen.body);
    let masks = objective.masks(&result.params).map_err(|e| from_fit(&e))?;
    masks
        .iter()
        .map(|m| {
            heatmap(&m.model, &m.target).map(|image| base64::encode(&png::encode(&image))).map_err(|e| from_fit(&e))
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
        plan_fit(ctx, &self.character, &self.views)
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let chosen = chosen(job)?;
        let (targets, result) = fit_in_job(job, &chosen, &self.views)?;
        let mut out = summary(&chosen, &result);
        if self.heatmaps {
            let maps = heatmaps(&chosen, &targets, &result)?;
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
        plan_fit(ctx, &self.character, &self.views)
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let chosen = chosen(job)?;
        let (_, result) = fit_in_job(job, &chosen, &self.views)?;
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
        let (template, part, stored) = load(ctx, &self.character)?;
        if !still_fits(&fitted, &part, &template, &stored) {
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
