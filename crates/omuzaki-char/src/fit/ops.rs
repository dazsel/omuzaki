//! The fit ops: `fit.reference` (a query: fit the character's body to reference silhouettes and report the proposal,
//! the `IoU` before and after and a difference heatmap per view, without changing anything) and `fit.apply` (an edit:
//! the same fit, written into the body part's `proportions` and `shape` as one transaction).
//!
//! The reference silhouettes arrive inline as rows of text (`#` character, `.` background), the interim the task
//! allows until sheet images can be decoded here. The fit is deterministic, so `fit.apply` writes exactly what
//! `fit.reference` proposed for the same arguments. Only the body is fitted: the other parts are not part of the
//! silhouette.

use omuzaki_char_api::{BodyParams, BodyShape, CharId, CharacterDef, PartId, paths};
use omuzaki_doc_api::Change;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_store_api::{Commercial, ContentHash, Provenance, Source, Store};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::heatmap::heatmap;
use super::png;
use super::solver::{FitInput, FitResult, PARAM_ORDER, fit};
use super::{FitError, FitParams, FitView, MAX_SIDE, Silhouette, Target, view_masks};
use crate::ops::character::access;

/// The kind id of the body part.
const BODY_KIND: &str = "body";
/// The most views a fit takes: front, side and back.
const MAX_VIEWS: usize = 3;

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
/// anything: the parameter patch, the `IoU` before and after (mean and per view), and a difference heatmap per view
/// stored as a PNG asset (red where the fitted body is outside the reference, blue where the reference is outside it).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FitReference {
    /// The character whose body is fitted.
    pub character: CharId,
    /// The reference silhouettes, one per view (`front`, `side`, `back`), 1 to 3.
    pub views: Vec<ViewMask>,
}

/// Fits the character's body to reference silhouettes and writes the result into the body's `proportions` and
/// `shape`, as one undoable edit. It is the same fit `fit.reference` proposes for the same arguments.
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

/// The character's only body part, as stored.
struct Stored {
    template: String,
    part: PartId,
    params: BodyParams,
}

fn load(ctx: &OpCtx<'_>, character: &CharId) -> Result<(CharacterDef, Stored), OpError> {
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
    let params = serde_json::from_value(stored.params.clone()).map_err(|e| {
        OpError::invalid(
            format!("parts/{part}/params"),
            format!("the body's parameters don't match the body format: {e}"),
        )
    })?;
    let found = Stored { template: def.template.to_string(), part: part.clone(), params };
    Ok((def, found))
}

/// The fit of the stored body to `views`.
fn run(ctx: &OpCtx<'_>, character: &CharId, views: &[ViewMask]) -> Result<(Stored, Vec<Target>, FitResult), OpError> {
    if views.is_empty() || views.len() > MAX_VIEWS {
        return Err(OpError::invalid("views", format!("{} views given", views.len()))
            .hint("give one silhouette per view: front, side, back"));
    }
    let (_, stored) = load(ctx, character)?;
    let targets = views.iter().enumerate().map(|(i, v)| parse_view(i, v)).collect::<Result<Vec<_>, _>>()?;
    let result =
        fit(&FitInput { template: &stored.template, targets: &targets, fixed: &[] }).map_err(|e| from_fit(&e))?;
    Ok((stored, targets, result))
}

/// The body parameters with the fitted proportions and shape axes (the stored hands, skin and shape preset stay).
fn fitted(stored: &BodyParams, params: &FitParams) -> BodyParams {
    let mut out = stored.clone();
    out.proportions = params.proportions.clone();
    out.shape = BodyShape { preset: stored.shape.preset.clone(), ..params.shape.clone() };
    out
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

fn summary(character: &CharId, stored: &Stored, result: &FitResult) -> Value {
    let before = FitParams { proportions: stored.params.proportions.clone(), shape: stored.params.shape.clone() };
    let per_view: Vec<Value> = result
        .per_view
        .iter()
        .map(|v| json!({ "view": format!("{:?}", v.view).to_lowercase(), "iou_before": v.iou_before, "iou_after": v.iou_after }))
        .collect();
    json!({
        "character": character,
        "part": stored.part,
        "patch": patch(&before, &result.params),
        "iou_before": result.iou_before,
        "iou_after": result.iou_after,
        "per_view": per_view,
        "evaluations": result.evaluations,
    })
}

fn authored() -> Provenance {
    Provenance {
        source: Source::Authored,
        author: None,
        licence: "CC0-1.0".to_owned(),
        attribution: None,
        commercial: Commercial::Ok,
        consent: None,
        private_test: false,
    }
}

/// Stores one heatmap PNG per view; returns the hashes in view order.
fn store_heatmaps(
    ctx: &OpCtx<'_>,
    stored: &Stored,
    targets: &[Target],
    result: &FitResult,
) -> Result<Vec<ContentHash>, OpError> {
    let store = ctx.service::<dyn Store>()?;
    let masks = view_masks(&stored.template, targets, &[], &result.params).map_err(|e| from_fit(&e))?;
    masks
        .iter()
        .map(|m| {
            let image = heatmap(&m.model, &m.target).map_err(|e| from_fit(&e))?;
            store
                .put(&png::encode(&image), authored())
                .map_err(|e| OpError::internal(format!("storing a heatmap failed: {e}")))
        })
        .collect()
}

impl Op for FitReference {
    fn meta() -> OpMeta {
        OpMeta {
            name: "fit.reference",
            summary: "Fit the body proportions and shape to reference silhouettes without applying: the proposed patch, \
                      IoU before and after, and a difference heatmap per view",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (stored, targets, result) = run(ctx, &self.character, &self.views)?;
        let hashes = store_heatmaps(ctx, &stored, &targets, &result)?;
        let mut out = summary(&self.character, &stored, &result);
        if let Some(views) = out["per_view"].as_array_mut() {
            for (v, hash) in views.iter_mut().zip(hashes) {
                v["heatmap"] = json!(hash);
            }
        }
        Ok(Plan::answer(out))
    }
}

impl Op for FitApply {
    fn meta() -> OpMeta {
        OpMeta {
            name: "fit.apply",
            summary: "Fit the body proportions and shape to reference silhouettes and write the result into the body \
                      part as one undoable edit",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (stored, _, result) = run(ctx, &self.character, &self.views)?;
        let after = fitted(&stored.params, &result.params);
        let base = paths::part_params(&self.character, &stored.part);
        let mut fields = serde_json::Map::new();
        for (key, before, now) in [
            ("proportions", to_value(&stored.params.proportions)?, to_value(&after.proportions)?),
            ("shape", to_value(&stored.params.shape)?, to_value(&after.shape)?),
        ] {
            if before != now {
                fields.insert(key.to_owned(), now);
            }
        }
        // A body whose file has no `params` yet gets them created whole; otherwise only the two groups are written.
        let changes: Vec<Change> = if ctx.doc.get(&base).is_none() {
            vec![access::change(ctx, base, Some(Value::Object(fields)))]
        } else {
            fields.into_iter().map(|(key, now)| access::change(ctx, base.join(key), Some(now))).collect()
        };
        Ok(access::edit_plan(changes, summary(&self.character, &stored, &result)))
    }
}

fn to_value<T: Serialize>(v: &T) -> Result<Value, OpError> {
    serde_json::to_value(v).map_err(|e| OpError::internal(format!("body parameters do not serialise: {e}")))
}

register_op!(FitReference);
register_op!(FitApply);
