//! `measure.face_outline`, `measure.feature_spacing` and `measure.likeness`: queries that read an image (or a target)
//! and change nothing, so an eval's read-only checker may call them.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Subject, outline_json, resolve, round};
use crate::face::likeness::{Compare, ToleranceOverrides, Tolerances, likeness};
use crate::face::spacing::layout_ratios;
use crate::face::{MEASURE_VERSION, Warning};
use crate::messages;

/// Measures a face's outline as a radial profile about the eye midpoint (85 samples, IOD), with the chin, the widths,
/// the aspect and the warnings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MeasureFaceOutline {
    /// The face: an image and its landmarks, or a likeness target.
    #[serde(flatten)]
    pub subject: Subject,
}

impl Op for MeasureFaceOutline {
    fn meta() -> OpMeta {
        OpMeta {
            name: "measure.face_outline",
            summary: "Measure a face's outline from a front image and its eye (and mouth) points: 85 radii about the \
                      eye midpoint in eye distances, the chin, the widths and the aspect. Use it after a shape edit on \
                      a front render (landmarks from render.preview), or on a reference sheet; changes nothing",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let resolved = resolve(ctx, &self.subject, "")?;
        Ok(Plan::answer(outline_json(&resolved)))
    }
}

register_op!(MeasureFaceOutline);

/// Measures a face's feature spacing in eye distances: eye line to chin and to mouth, mouth to chin, the widths at the
/// eyes and the mouth, the largest width, the aspect, and the layout ratios `face.layout.measure` takes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MeasureFeatureSpacing {
    /// The face: an image and its landmarks, or a likeness target.
    #[serde(flatten)]
    pub subject: Subject,
}

impl Op for MeasureFeatureSpacing {
    fn meta() -> OpMeta {
        OpMeta {
            name: "measure.feature_spacing",
            summary: "Measure a face's feature spacing from a front image and its eye and mouth points, in eye \
                      distances, with the layout ratios face.layout.measure compares (pass them as its reference); \
                      changes nothing",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let r = resolve(ctx, &self.subject, "")?;
        let s = &r.spacing;
        let mut out = json!({
            "measure_version": MEASURE_VERSION,
            "private": r.private,
            "spacing": {
                "eye_to_chin": round(s.eye_to_chin),
                "eye_to_mouth": s.eye_to_mouth.map(round),
                "mouth_to_chin": s.mouth_to_chin.map(round),
                "mouth": s.mouth.map(round),
                "nose": s.nose.map(round),
                "width_at_eyes": s.width_at_eyes.map(round),
                "width_at_mouth": s.width_at_mouth.map(round),
                "width_max": round(s.width_max),
                "aspect": round(s.aspect),
            },
            "unmeasured": s.unmeasured,
            "layout_ratios": layout_ratios(s),
            "warnings": r.warnings(),
        });
        if r.from_cache {
            out["from_cache"] = json!(true);
        }
        Ok(Plan::answer(out))
    }
}

register_op!(MeasureFeatureSpacing);

/// Compares a candidate face (a render) with a reference (a sheet, or a target): a weighted distance in units of
/// tolerance (0 identical, 1 at the tolerance, lower is better), a gate that every term is within its tolerance, the
/// worst sector, and a note per term outside.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MeasureLikeness {
    /// The reference: a face, or a likeness target (`{"target": {"character": "mira", "name": "front"}}`).
    pub reference: Subject,
    /// The candidate, usually a front render.
    pub candidate: Subject,
    /// `cross_look` (a drawing against a render; the default) or `same_look` (a render against a render).
    #[serde(default)]
    pub compare: Compare,
    /// Per-term tolerance overrides, IOD; absent terms keep the preset's value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tolerances: Option<ToleranceOverrides>,
}

impl Op for MeasureLikeness {
    fn meta() -> OpMeta {
        OpMeta {
            name: "measure.likeness",
            summary: "Compare a face with a reference (a sheet or a character's likeness target): the outline and the \
                      feature spacing as a score in units of tolerance (lower is better), within or not, the worst \
                      sector and notes. Use it after a shape or likeness edit; check `within` and the worst sector, \
                      and look at measure.face_overlay; changes nothing",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let reference = resolve(ctx, &self.reference, "reference")?;
        let candidate = resolve(ctx, &self.candidate, "candidate")?;
        let overrides = self.tolerances.unwrap_or_default();
        for (name, value) in [
            ("outline", overrides.outline),
            ("eye_to_chin", overrides.eye_to_chin),
            ("eye_to_mouth", overrides.eye_to_mouth),
            ("width_at_mouth", overrides.width_at_mouth),
            ("width_at_eyes", overrides.width_at_eyes),
        ] {
            if let Some(v) = value
                && !(v.is_finite() && v > 0.0)
            {
                return Err(OpError::invalid(format!("tolerances.{name}"), format!("{v} is not a positive number"))
                    .hint("a tolerance is in eye distances, such as 0.025"));
            }
        }
        let tolerances = Tolerances::preset(self.compare).with(&overrides);
        let l = likeness(reference.side(), candidate.side(), tolerances).map_err(|e| {
            OpError::invalid("candidate", messages::english(&messages::coverage(e.sectors.join(", "))))
                .hint("exclude less, or use a clearer front view")
        })?;
        let mut warnings: Vec<Warning> = Vec::new();
        if l.expression_differs {
            warnings.push(Warning {
                code: "expression",
                detail: "the two mouths differ in height by more than 2×: compare neutral faces".to_owned(),
            });
        }
        let tagged = |side: &str, list: &[Warning]| -> Vec<serde_json::Value> {
            list.iter().map(|w| json!({ "code": w.code, "detail": w.detail, "subject": side })).collect()
        };
        let mut all = tagged("reference", reference.warnings());
        all.extend(tagged("candidate", candidate.warnings()));
        all.extend(warnings.iter().map(|w| json!({ "code": w.code, "detail": w.detail })));
        let terms: Vec<serde_json::Value> = l
            .terms
            .iter()
            .map(|t| {
                let mut v = json!({
                    "id": t.id,
                    "value": round(t.value),
                    "tolerance": t.tolerance,
                    "weight": t.weight,
                    "within": t.within,
                });
                if let (Some(r), Some(c)) = (t.reference, t.candidate) {
                    v["reference"] = json!(round(r));
                    v["candidate"] = json!(round(c));
                    v["difference"] = json!(round(c - r));
                }
                v
            })
            .collect();
        let worst = l.worst.as_ref().map(|w| {
            json!({
                "from_deg": w.from_deg,
                "to_deg": w.to_deg,
                "mean": round(w.mean),
                "reference_points": w.reference_points.iter().map(|p| p.map(round)).collect::<Vec<_>>(),
                "candidate_points": w.candidate_points.iter().map(|p| p.map(round)).collect::<Vec<_>>(),
            })
        });
        Ok(Plan::answer(json!({
            "measure_version": MEASURE_VERSION,
            "private": reference.private || candidate.private,
            "compare": self.compare,
            "score": round(l.score),
            "within": l.within,
            "terms": terms,
            "outline": {
                "coverage": round(l.coverage),
                "mean_signed": round(l.mean_signed),
                "per_angle": l.per_angle.iter().map(|d| d.map(round)).collect::<Vec<_>>(),
                "worst": worst,
            },
            "notes": l.notes.iter().map(messages::english).collect::<Vec<_>>(),
            "from_cache": { "reference": reference.from_cache, "candidate": candidate.from_cache },
            "warnings": all,
        })))
    }
}

register_op!(MeasureLikeness);
