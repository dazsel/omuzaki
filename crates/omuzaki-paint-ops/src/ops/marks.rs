//! Marks: drawing them, replacing one, removing some, and a layer's mask.

use omuzaki_doc_api::{Patch, Value};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpMeta, Plan, register_op};
use omuzaki_paint::model::MAX_MARKS;
use omuzaki_paint::{LayerKind, Mark, Painting};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::edit;
use crate::doc::{from_paint, json, load, path_in};

/// The most marks one `paint.draw` lays down.
pub const MAX_BATCH: usize = 4096;

/// Checks `marks` can go on `layer` of `p` as they are (each mark, the layers they sample, and the totals).
fn check_marks(p: &Painting, layer: &str, marks: &[Mark], field: &str) -> Result<(), OpError> {
    let l = p.layer(layer).map_err(from_paint)?;
    if l.kind == LayerKind::Group {
        return Err(OpError::invalid("layer", format!("{layer:?} is a group")).hint("draw on a paint layer inside it"));
    }
    if marks.len() > MAX_BATCH {
        return Err(OpError::invalid(field, format!("{} marks at once; at most {MAX_BATCH}", marks.len())));
    }
    for (i, m) in marks.iter().enumerate() {
        let f = format!("{field}/{i}");
        m.check(&f).map_err(from_paint)?;
        for s in m.samples() {
            let sampled =
                p.layer(s).map_err(|_| OpError::invalid(format!("{f}/sample"), format!("there is no layer {s:?}")))?;
            if sampled.kind != LayerKind::Paint {
                return Err(OpError::invalid(format!("{f}/sample"), format!("{s:?} is a group")));
            }
        }
    }
    Ok(())
}

/// Checks the painting after a change: the total mark count and that no bucket fills sample each other in a loop.
fn check_after(p: &Painting) -> Result<(), OpError> {
    if p.mark_count() > MAX_MARKS {
        return Err(OpError::invalid("marks", format!("a painting holds at most {MAX_MARKS} marks")));
    }
    p.sample_order().map(|_| ()).map_err(from_paint)
}

/// Draw marks on a layer, in order, as one step: brush strokes and ink lines (`stroke`), filled shapes (`shape`),
/// bucket fills inside line art (`bucket`) and hatching (`hatch`). Coordinates are canvas pixels from the top left.
/// See `paint.brushes` for the brushes and blend modes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
    { "painting": "bust", "layer": "lines", "marks": [
        { "type": "stroke", "brush": "g_pen", "colour": "#1b1420", "size": 4,
          "points": [[400, 610, 0.3], [470, 590, 1], [540, 600, 0.6]], "tag": "upper lid, left" }
    ] },
    { "painting": "bust", "layer": "flats", "marks": [
        { "type": "bucket", "at": [520, 700], "fill": "#f3d9c8", "sample": ["lines"], "gap": 3, "tag": "skin" }
    ] },
    { "painting": "bust", "layer": "skin-shadow", "marks": [
        { "type": "shape", "geometry": { "kind": "spline", "points": [[430, 760], [600, 760], [560, 820], [470, 815]] },
          "fill": "#d9a5a0", "feather": 1.5, "tag": "neck shadow" },
        { "type": "hatch", "region": { "kind": "ellipse", "cx": 450, "cy": 690, "rx": 40, "ry": 16 },
          "colour": "#ff6f7f", "angle": 110, "spacing": 7, "width": 2, "opacity": 0.8, "tag": "blush, left" }
    ] }
],
    "x-omuzaki-fixture" = "painting"
))]
pub struct Draw {
    /// The painting.
    pub painting: String,
    /// The paint layer to draw on.
    pub layer: String,
    /// The marks, in the order they go down (at most 4,096).
    pub marks: Vec<Mark>,
}

impl Op for Draw {
    fn meta() -> OpMeta {
        edit(
            "paint.draw",
            "Draw marks on a painting's layer as one step: brush strokes and ink lines, filled shapes with colour or \
             gradient, bucket fills inside line art, and hatching.",
        )
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut p = load(ctx.doc, &self.painting)?;
        if self.marks.is_empty() {
            return Err(OpError::invalid("marks", "no marks to draw"));
        }
        check_marks(&p, &self.layer, &self.marks, "marks")?;
        let marks_path = path_in(&self.painting, &["layers", &self.layer, "marks"]);
        let stored = ctx.doc.get(&marks_path).cloned();
        let first = p.layer(&self.layer).map_err(from_paint)?.marks.len();
        if let Some(l) = p.layers.get_mut(&self.layer) {
            l.marks.extend(self.marks.iter().cloned());
        }
        check_after(&p)?;
        let mut patch = Patch::new();
        match stored {
            // Append each mark: undo pops them, and a concurrent edit of the same layer conflicts instead of merging.
            Some(Value::Array(_)) => {
                for (i, m) in self.marks.iter().enumerate() {
                    patch = patch.with(marks_path.join((first + i).to_string()), None, Some(json(m)?));
                }
            }
            before => patch = patch.with(marks_path, before, Some(json(&self.marks)?)),
        }
        let tags: Vec<Option<&str>> = self.marks.iter().map(Mark::tag).collect();
        let mut plan = Plan::edit(patch);
        plan.output = Some(json!({
            "layer": self.layer, "first": first, "count": self.marks.len(), "total": first + self.marks.len(), "tags": tags,
        }));
        Ok(plan)
    }
}
register_op!(Draw);

/// Replace one mark of a layer with another (fix a stroke without redrawing the rest).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{ "painting": "bust", "layer": "lines", "index": 0, "mark":
    { "type": "stroke", "brush": "g_pen", "colour": "#1b1420", "size": 5, "points": [[400, 612], [470, 588], [540, 600]] }
}],
    "x-omuzaki-fixture" = "painting"
))]
pub struct ReplaceMark {
    /// The painting.
    pub painting: String,
    /// The layer.
    pub layer: String,
    /// The mark's index (from `paint.get` or the `first` that `paint.draw` answered).
    pub index: usize,
    /// The new mark.
    pub mark: Mark,
}

impl Op for ReplaceMark {
    fn meta() -> OpMeta {
        edit("paint.replace_mark", "Replace one mark of a layer with another.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut p = load(ctx.doc, &self.painting)?;
        check_marks(&p, &self.layer, std::slice::from_ref(&self.mark), "mark")?;
        let count = p.layer(&self.layer).map_err(from_paint)?.marks.len();
        if self.index >= count {
            return Err(OpError::invalid(
                "index",
                format!("the layer has {count} marks; index {} is past them", self.index),
            ));
        }
        let old = p.layers.get_mut(&self.layer).map(|l| std::mem::replace(&mut l.marks[self.index], self.mark.clone()));
        check_after(&p)?;
        let path = path_in(&self.painting, &["layers", &self.layer, "marks", &self.index.to_string()]);
        let before = ctx.doc.get(&path).cloned().or(old.map(|m| json(&m)).transpose()?);
        let mut plan = Plan::edit(Patch::new().with(path, before, Some(json(&self.mark)?)));
        plan.output = Some(json!({ "layer": self.layer, "index": self.index }));
        Ok(plan)
    }
}
register_op!(ReplaceMark);

/// Remove marks from a layer: `count` marks from `from`, or every mark with a tag.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
    { "painting": "bust", "layer": "lines", "from": 1, "count": 2 },
    { "painting": "bust", "layer": "skin-shadow", "tag": "blush, left" }
],
    "x-omuzaki-fixture" = "painting"
))]
pub struct RemoveMarks {
    /// The painting.
    pub painting: String,
    /// The layer.
    pub layer: String,
    /// The first mark to remove.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<usize>,
    /// How many to remove from `from`. Default 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    /// Remove every mark with this tag instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

impl Op for RemoveMarks {
    fn meta() -> OpMeta {
        edit("paint.remove_marks", "Remove marks from a layer: a run of them by index, or every mark with a tag.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut p = load(ctx.doc, &self.painting)?;
        let layer = p.layer(&self.layer).map_err(from_paint)?;
        let total = layer.marks.len();
        let keep: Vec<bool> = match (&self.tag, self.from) {
            (Some(_), Some(_)) => return Err(OpError::invalid("tag", "give from or tag, not both")),
            (Some(tag), None) => layer.marks.iter().map(|m| m.tag() != Some(tag.as_str())).collect(),
            (None, Some(from)) => {
                let count = self.count.unwrap_or(1);
                if count == 0 || from.checked_add(count).is_none_or(|end| end > total) {
                    return Err(OpError::invalid(
                        "count",
                        format!("the layer has {total} marks; {from} + {count} is past them"),
                    ));
                }
                (0..total).map(|i| i < from || i >= from + count).collect()
            }
            (None, None) => return Err(OpError::invalid("from", "say which marks: from (and count), or tag")),
        };
        let removed = keep.iter().filter(|k| !**k).count();
        if removed == 0 {
            return Err(
                OpError::not_found("no mark has that tag").hint("paint.get with the layer lists its marks and tags")
            );
        }
        let path = path_in(&self.painting, &["layers", &self.layer, "marks"]);
        let before = ctx.doc.get(&path).cloned();
        if let Some(l) = p.layers.get_mut(&self.layer) {
            let mut k = keep.iter();
            l.marks.retain(|_| k.next().copied().unwrap_or(true));
        }
        check_after(&p)?;
        let after = json(&p.layer(&self.layer).map_err(from_paint)?.marks)?;
        let mut plan = Plan::edit(Patch::new().with(path, before, Some(after)));
        plan.output = Some(json!({ "layer": self.layer, "removed": removed, "left": total - removed }));
        Ok(plan)
    }
}
register_op!(RemoveMarks);

/// Set or clear a layer's mask: when set, the layer shows only where the mask's marks paint (their alpha; erase marks
/// cut holes), like a painter's layer mask. The marks are the same kinds as `paint.draw`'s, without bucket sampling.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
    { "painting": "bust", "layer": "hair-glow", "marks": [
        { "type": "shape", "geometry": { "kind": "ellipse", "cx": 540, "cy": 420, "rx": 300, "ry": 120 },
          "fill": "#ffffff", "feather": 30 }
    ] },
    { "painting": "bust", "layer": "hair-glow", "marks": null }
],
    "x-omuzaki-fixture" = "painting"
))]
pub struct SetMask {
    /// The painting.
    pub painting: String,
    /// The paint layer.
    pub layer: String,
    /// The mask's marks; `null` removes the mask.
    pub marks: Option<Vec<Mark>>,
}

impl Op for SetMask {
    fn meta() -> OpMeta {
        edit("paint.set_mask", "Set a layer's mask from marks (the layer shows only where they paint), or remove it.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut p = load(ctx.doc, &self.painting)?;
        if let Some(marks) = &self.marks {
            check_marks(&p, &self.layer, marks, "marks")?;
            if marks.iter().any(|m| !m.samples().is_empty()) {
                return Err(OpError::invalid("marks", "a mask's bucket fills can't sample other layers"));
            }
        } else {
            p.layer(&self.layer).map_err(from_paint)?;
        }
        if let Some(l) = p.layers.get_mut(&self.layer) {
            l.mask.clone_from(&self.marks);
        }
        check_after(&p)?;
        let path = path_in(&self.painting, &["layers", &self.layer, "mask"]);
        let before = ctx.doc.get(&path).cloned();
        let after = self.marks.as_ref().map(json).transpose()?;
        let mut plan = Plan::edit(Patch::new().with(path, before, after));
        plan.output = Some(json!({ "layer": self.layer, "masked": self.marks.is_some() }));
        Ok(plan)
    }
}
register_op!(SetMask);
