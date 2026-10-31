//! Painting on a character's parts (#2575), `part.paint.*`: a part's **texture layer** holds a painting in the part's
//! UV space (canvas pixel `(x, y)` is UV `(x / width, y / height)`), drawn over the part's surface colour as a texture,
//! so a recolour still recolours what is under it and the paint survives every rebuild of the part.
//!
//! | Op | Does |
//! |---|---|
//! | `part.paint.new` | adds a texture layer to a part's material stack: an empty painting of a size, with paint layers |
//! | `part.paint.stroke` | brush strokes on it |
//! | `part.paint.draw` | any marks on it: strokes, shapes, bucket fills, hatching |
//! | `part.paint.fill` | fills all of it, a region, or the area around a point |
//! | `part.paint.erase` | erases along a path |
//! | `part.paint.opacity` | the texture layer's opacity, or one paint layer's |
//! | `part.paint.clear` | removes the marks of one paint layer, or of all |
//! | `part.paint.get` | the painting, its size and its layers' mark counts |
//!
//! Every edit is one undoable step. Marks are appended to the stored painting one by one, so undo pops them and a
//! long painting never rewrites the whole design for one stroke.

use omuzaki_char_api::design::{DesignDef, LayerKind as SurfaceKind, MaterialLayer};
use omuzaki_char_api::{CharId, CharacterDef, LayerId, PartId, paths};
use omuzaki_doc_api::{Change, DocPath, Patch, Value};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpMeta, Plan, register_op};
use omuzaki_paint::{Colour, Fill, Geometry, Layer, LayerKind, Mark, Painting, Stroke};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::marks::{check_after, check_marks};
use super::{edit, query};
use crate::doc::{from_paint, id, json};

/// The largest side of a part's painting, in pixels: a 4K texture.
pub const MAX_SIDE: u32 = 4096;

/// The layer id the ops use when none is given.
const DEFAULT_LAYER: &str = "paint";

/// An error that names the argument at fault.
fn not_found(field: &str, message: impl Into<String>) -> OpError {
    let mut e = OpError::not_found(message);
    e.field = Some(field.to_owned());
    e
}

/// `items` as a readable list for a hint.
fn list<T: std::fmt::Display>(items: impl IntoIterator<Item = T>) -> String {
    let joined = items.into_iter().map(|i| format!("`{i}`")).collect::<Vec<_>>().join(", ");
    if joined.is_empty() { "none".to_owned() } else { joined }
}

/// The layer id `layer` names, or `paint`.
fn layer_id(layer: Option<&LayerId>) -> Result<LayerId, OpError> {
    match layer {
        Some(l) => Ok(l.clone()),
        None => LayerId::new(DEFAULT_LAYER).map_err(|e| OpError::internal(format!("the default layer id: {e}"))),
    }
}

/// The character `character` and its design, with `part` checked to exist.
fn load_design(ctx: &OpCtx<'_>, character: &CharId, part: &PartId) -> Result<DesignDef, OpError> {
    let Some(node) = ctx.doc.get(&paths::character(character)) else {
        return Err(not_found("character", format!("there is no character `{character}`"))
            .hint("character.create makes one; character.list_parts lists a character's parts"));
    };
    let def = CharacterDef::from_node(node).map_err(|e| {
        OpError::invalid("character", format!("character `{character}` is stored in a shape the format rejects: {e}"))
    })?;
    if !def.parts.contains_key(part) {
        return Err(not_found("part", format!("character `{character}` has no part `{part}`"))
            .hint(format!("the parts of `{character}` are: {}", list(def.parts.keys()))));
    }
    Ok(*def.design)
}

/// A part's texture layer, read out of the design.
struct Target {
    design: DesignDef,
    /// Its place in the part's stack.
    index: usize,
    layer: LayerId,
    painting: Painting,
}

impl Target {
    fn load(ctx: &OpCtx<'_>, character: &CharId, part: &PartId, layer: Option<&LayerId>) -> Result<Self, OpError> {
        let design = load_design(ctx, character, part)?;
        let layer = layer_id(layer)?;
        let stack = design.layers_of(part);
        let Some(index) = stack.iter().position(|l| l.id == layer) else {
            let textures = stack.iter().filter(|l| matches!(l.kind, SurfaceKind::Texture { .. })).map(|l| &l.id);
            return Err(not_found("layer", format!("`{part}` has no layer `{layer}`"))
                .hint(format!("its texture layers are {}; part.paint.new adds one", list(textures))));
        };
        let SurfaceKind::Texture { painting } = &stack[index].kind else {
            return Err(OpError::invalid("layer", format!("`{layer}` on `{part}` isn't a texture layer"))
                .hint("paint on a layer part.paint.new made; part.layer.list lists the stack"));
        };
        let painting = Painting::deserialize(painting).map_err(|e| {
            OpError::invalid("layer", format!("the painting of `{layer}` on `{part}` doesn't read: {e}"))
        })?;
        Ok(Self { design, index, layer, painting })
    }

    /// The paint layer `paint_layer` names, or the topmost one.
    fn paint_layer(&self, paint_layer: Option<&str>) -> Result<String, OpError> {
        if let Some(l) = paint_layer {
            self.painting.layer(l).map_err(|_| {
                not_found("paint_layer", format!("the painting has no layer {l:?}"))
                    .hint(format!("its layers are {}", list(&self.painting.order)))
            })?;
            return Ok(l.to_owned());
        }
        self.painting
            .order
            .iter()
            .rev()
            .find(|l| self.painting.layers.get(*l).is_some_and(|x| x.kind == LayerKind::Paint))
            .cloned()
            .ok_or_else(|| OpError::invalid("paint_layer", "the painting has no paint layer to draw on"))
    }

    /// The path of the painting inside the character's design node.
    fn painting_path(&self, character: &CharId, part: &PartId) -> DocPath {
        paths::design(character)
            .join("layers")
            .join(part.as_str())
            .join(self.index.to_string())
            .join("kind")
            .join("painting")
    }

    /// The design with this painting written back.
    fn design_with_painting(&self, part: &PartId) -> Result<DesignDef, OpError> {
        let mut design = self.design.clone();
        let value = json(&self.painting)?;
        if let Some(l) = design.layers.get_mut(part).and_then(|s| s.get_mut(self.index)) {
            l.kind = SurfaceKind::Texture { painting: value };
        }
        Ok(design)
    }
}

/// The change that stores `design` as the character's design node (removing it when empty).
fn store(ctx: &OpCtx<'_>, character: &CharId, design: &DesignDef) -> Result<Vec<Change>, OpError> {
    design.check().map_err(|e| OpError::invalid("character", format!("the design would be invalid: {e}")))?;
    let after = if design.is_empty() {
        None
    } else {
        Some(serde_json::to_value(design).map_err(|e| OpError::internal(format!("can't store the design: {e}")))?)
    };
    let path = paths::design(character);
    let before = ctx.doc.get(&path).cloned();
    if before == after {
        return Ok(Vec::new());
    }
    Ok(vec![Change { path, before, after }])
}

/// The plan of an edit, with `output` as its result.
fn plan_of(changes: Vec<Change>, output: Value) -> Plan {
    let mut plan = Plan::edit(Patch { changes });
    plan.output = Some(output);
    plan
}

/// Appends `marks` to paint layer `paint_layer` of the target's painting, as one step.
fn append(
    ctx: &OpCtx<'_>,
    character: &CharId,
    part: &PartId,
    mut target: Target,
    paint_layer: &str,
    marks: &[Mark],
) -> Result<Plan, OpError> {
    if marks.is_empty() {
        return Err(OpError::invalid("marks", "no marks to draw"));
    }
    check_marks(&target.painting, paint_layer, marks, "marks")?;
    let first = target.painting.layer(paint_layer).map_err(from_paint)?.marks.len();
    if let Some(l) = target.painting.layers.get_mut(paint_layer) {
        l.marks.extend(marks.iter().cloned());
    }
    check_after(&target.painting)?;
    let marks_path = target.painting_path(character, part).join("layers").join(paint_layer).join("marks");
    let changes = match ctx.doc.get(&marks_path) {
        // Append each mark: undo pops them, and a concurrent edit of the same layer conflicts instead of merging.
        Some(Value::Array(stored)) if stored.len() == first => marks
            .iter()
            .enumerate()
            .map(|(i, m)| {
                Ok(Change { path: marks_path.join((first + i).to_string()), before: None, after: Some(json(m)?) })
            })
            .collect::<Result<Vec<_>, OpError>>()?,
        _ => store(ctx, character, &target.design_with_painting(part)?)?,
    };
    let tags: Vec<Option<&str>> = marks.iter().map(Mark::tag).collect();
    Ok(plan_of(
        changes,
        json!({
            "character": character, "part": part, "layer": target.layer, "paint_layer": paint_layer,
            "first": first, "count": marks.len(), "total": first + marks.len(), "tags": tags,
        }),
    ))
}

/// Add a texture layer to a part: an empty painting in the part's UV space that `part.paint.stroke`, `.draw`, `.fill`
/// and `.erase` draw on. Canvas pixel `(x, y)` lands at UV `(x / width, y / height)`; the paint is drawn over the
/// part's surface colour, above its other material layers, so a recolour still recolours what is under it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        { "character": "kit", "part": "shirt" },
        { "character": "kit", "part": "boots", "layer": "boot-art", "width": 512, "height": 512, "layers": ["base", "details"] }
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct PaintNew {
    /// The character.
    pub character: CharId,
    /// The part to paint on.
    pub part: PartId,
    /// The texture layer's id, unique in the part's stack. Default `paint`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<LayerId>,
    /// Width of the painting in pixels (1 to 4096). Default 1024.
    #[serde(default = "side")]
    pub width: u32,
    /// Height in pixels (1 to 4096). Default 1024.
    #[serde(default = "side")]
    pub height: u32,
    /// The painting's paint layers, back to front. Default one, `paint`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<String>,
    /// Where in the part's material stack, counted from the bottom. Left out: on top.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
}

const fn side() -> u32 {
    1024
}

impl Op for PaintNew {
    fn meta() -> OpMeta {
        edit(
            "part.paint.new",
            "Add a texture layer to a character's part: an empty painting in its UV space to paint on.",
        )
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut design = load_design(ctx, &self.character, &self.part)?;
        let layer = layer_id(self.layer.as_ref())?;
        for (field, v) in [("width", self.width), ("height", self.height)] {
            if v == 0 || v > MAX_SIDE {
                return Err(OpError::invalid(
                    field,
                    format!("{v} pixels; a part's painting is 1 to {MAX_SIDE} across"),
                ));
            }
        }
        let stack = design.layers.entry(self.part.clone()).or_default();
        if stack.iter().any(|l| l.id == layer) {
            return Err(OpError::invalid("layer", format!("`{}` already has a layer `{layer}`", self.part))
                .hint("give the texture layer another id, or paint on that one"));
        }
        if let Some(other) = stack.iter().find_map(|l| match &l.kind {
            SurfaceKind::Texture { painting } => Painting::deserialize(painting).ok(),
            _ => None,
        }) && (other.width, other.height) != (self.width, self.height)
        {
            return Err(OpError::invalid(
                "width",
                format!(
                    "the part's other texture layers are {}x{}: every texture layer of a part is one size",
                    other.width, other.height
                ),
            ));
        }
        let index = self.index.unwrap_or(stack.len());
        if index > stack.len() {
            return Err(OpError::invalid(
                "index",
                format!("the stack of `{}` has {} layer(s)", self.part, stack.len()),
            )
            .hint("leave index out to add the layer on top"));
        }
        let mut painting = Painting::new(format!("{} of {}", layer, self.part), self.width, self.height);
        painting.background = Colour::CLEAR;
        let names = if self.layers.is_empty() { vec![DEFAULT_LAYER.to_owned()] } else { self.layers.clone() };
        for name in &names {
            id("layers", name)?;
            if painting.layers.insert(name.clone(), Layer::paint(name.clone())).is_some() {
                return Err(OpError::invalid("layers", format!("{name:?} is listed twice")));
            }
            painting.order.push(name.clone());
        }
        painting.check().map_err(from_paint)?;
        stack.insert(index, MaterialLayer::new(layer.clone(), SurfaceKind::Texture { painting: json(&painting)? }));
        let changes = store(ctx, &self.character, &design)?;
        Ok(plan_of(
            changes,
            json!({
                "character": self.character, "part": self.part, "layer": layer, "index": index,
                "width": self.width, "height": self.height, "layers": names,
            }),
        ))
    }
}
register_op!(PaintNew);

/// Paint brush strokes on a part's texture layer, as one step. Points are `[x, y]` or `[x, y, pressure]` in the
/// painting's pixels (UV × size); see `paint.brushes` for the brushes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        { "character": "kit", "part": "shirt", "strokes": [
            { "brush": "marker", "colour": "#f4f0e8", "size": 24, "points": [[0, 512], [512, 500], [1024, 512]], "tag": "stripe" }
        ] }
    ],
    "x-omuzaki-fixture" = "character-painted"
))]
pub struct PaintStroke {
    /// The character.
    pub character: CharId,
    /// The part.
    pub part: PartId,
    /// The texture layer. Default `paint`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<LayerId>,
    /// The painting's layer to paint on. Default its top paint layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paint_layer: Option<String>,
    /// The strokes, in order.
    pub strokes: Vec<Stroke>,
}

impl Op for PaintStroke {
    fn meta() -> OpMeta {
        edit(
            "part.paint.stroke",
            "Paint brush strokes on a character part's texture layer, in its UV space, as one step.",
        )
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let target = Target::load(ctx, &self.character, &self.part, self.layer.as_ref())?;
        let paint_layer = target.paint_layer(self.paint_layer.as_deref())?;
        let marks: Vec<Mark> = self.strokes.iter().cloned().map(Mark::Stroke).collect();
        append(ctx, &self.character, &self.part, target, &paint_layer, &marks)
    }
}
register_op!(PaintStroke);

/// Draw any marks on a part's texture layer, as one step: strokes, filled shapes, bucket fills and hatching, exactly
/// as `paint.draw` takes them, in the painting's pixels (UV × size).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        { "character": "kit", "part": "shirt", "marks": [
            { "type": "shape", "geometry": { "kind": "ellipse", "cx": 512, "cy": 300, "rx": 90, "ry": 90 },
              "fill": "#c8324a", "outline": { "colour": "#2a1a20", "width": 6 }, "tag": "badge" },
            { "type": "hatch", "region": { "kind": "rect", "x": 0, "y": 700, "w": 1024, "h": 200 },
              "colour": "#30304080", "angle": 45, "spacing": 12, "width": 3, "tag": "shade" }
        ] }
    ],
    "x-omuzaki-fixture" = "character-painted"
))]
pub struct PaintDraw {
    /// The character.
    pub character: CharId,
    /// The part.
    pub part: PartId,
    /// The texture layer. Default `paint`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<LayerId>,
    /// The painting's layer to draw on. Default its top paint layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paint_layer: Option<String>,
    /// The marks, in order (at most 4,096).
    pub marks: Vec<Mark>,
}

impl Op for PaintDraw {
    fn meta() -> OpMeta {
        edit(
            "part.paint.draw",
            "Draw marks (strokes, shapes, bucket fills, hatching) on a character part's texture layer, as one step.",
        )
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let target = Target::load(ctx, &self.character, &self.part, self.layer.as_ref())?;
        let paint_layer = target.paint_layer(self.paint_layer.as_deref())?;
        append(ctx, &self.character, &self.part, target, &paint_layer, &self.marks)
    }
}
register_op!(PaintDraw);

/// Fill a part's texture layer: all of it, a `region` (a shape), or the area around a point `at` bounded by what is
/// painted (a bucket fill). The fill is a colour, a gradient or a freeform gradient, as `paint.draw` takes them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        { "character": "kit", "part": "shirt", "fill": "#ffffff30" },
        { "character": "kit", "part": "shirt", "region": { "kind": "rect", "x": 0, "y": 900, "w": 1024, "h": 124 },
          "fill": { "from": [0, 900], "to": [0, 1024], "stops": [{ "at": 0, "colour": "#20203000" }, { "at": 1, "colour": "#202030c0" }] } },
        { "character": "kit", "part": "shirt", "at": [512, 512], "fill": "#3a6ea5" }
    ],
    "x-omuzaki-fixture" = "character-painted"
))]
pub struct PaintFill {
    /// The character.
    pub character: CharId,
    /// The part.
    pub part: PartId,
    /// The texture layer. Default `paint`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<LayerId>,
    /// The painting's layer to fill on. Default its top paint layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paint_layer: Option<String>,
    /// The paint.
    pub fill: Fill,
    /// Fill only this shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<Geometry>,
    /// Fill the area around this point `[x, y]` that matches its colour (a bucket fill), sampling the layer itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
    /// How much shows, 0 to 1. Default 1.
    #[serde(default = "one")]
    pub opacity: f64,
}

const fn one() -> f64 {
    1.0
}

impl Op for PaintFill {
    fn meta() -> OpMeta {
        edit(
            "part.paint.fill",
            "Fill a character part's texture layer, a region of it, or the area around a point, with a colour or gradient.",
        )
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let target = Target::load(ctx, &self.character, &self.part, self.layer.as_ref())?;
        let paint_layer = target.paint_layer(self.paint_layer.as_deref())?;
        let fill = json(&self.fill)?;
        let mark = match (&self.region, self.at) {
            (Some(_), Some(_)) => {
                return Err(OpError::invalid("at", "give a region or a point to fill around, not both"));
            }
            (None, Some(at)) => {
                json!({ "type": "bucket", "at": at, "fill": fill, "opacity": self.opacity, "tag": "fill" })
            }
            (region, None) => {
                let (w, h) = (f64::from(target.painting.width), f64::from(target.painting.height));
                let geometry = match region {
                    Some(g) => json(g)?,
                    None => json!({ "kind": "rect", "x": 0, "y": 0, "w": w, "h": h }),
                };
                json!({ "type": "shape", "geometry": geometry, "fill": fill, "opacity": self.opacity, "tag": "fill" })
            }
        };
        let mark: Mark =
            serde_json::from_value(mark).map_err(|e| OpError::internal(format!("building the fill mark: {e}")))?;
        append(ctx, &self.character, &self.part, target, &paint_layer, &[mark])
    }
}
register_op!(PaintFill);

/// Erase along a path on a part's texture layer: what is under it goes clear and the surface shows through.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        { "character": "kit", "part": "shirt", "points": [[100, 500], [300, 520]], "size": 40 },
        { "character": "kit", "part": "shirt", "brush": "soft_eraser", "points": [[600, 500], [700, 480, 0.5]], "size": 60, "opacity": 0.5 }
    ],
    "x-omuzaki-fixture" = "character-painted"
))]
pub struct PaintErase {
    /// The character.
    pub character: CharId,
    /// The part.
    pub part: PartId,
    /// The texture layer. Default `paint`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<LayerId>,
    /// The painting's layer to erase on. Default its top paint layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paint_layer: Option<String>,
    /// The path, `[x, y]` or `[x, y, pressure]`, in the painting's pixels.
    pub points: Vec<Vec<f64>>,
    /// The eraser's width in pixels.
    pub size: f64,
    /// `eraser` (hard, the default) or `soft_eraser`, or any brush (its shape erases).
    #[serde(default = "eraser")]
    pub brush: String,
    /// How much it erases, 0 to 1. Default 1.
    #[serde(default = "one")]
    pub opacity: f64,
}

fn eraser() -> String {
    "eraser".to_owned()
}

impl Op for PaintErase {
    fn meta() -> OpMeta {
        edit(
            "part.paint.erase",
            "Erase along a path on a character part's texture layer, so the surface shows through.",
        )
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let target = Target::load(ctx, &self.character, &self.part, self.layer.as_ref())?;
        let paint_layer = target.paint_layer(self.paint_layer.as_deref())?;
        let mark = json!({
            "type": "stroke", "brush": self.brush, "colour": "#000000", "size": self.size, "points": self.points,
            "opacity": self.opacity, "erase": true, "tag": "erase",
        });
        let mark: Mark = serde_json::from_value(mark)
            .map_err(|e| OpError::invalid("points", format!("not an erase stroke: {e}")))?;
        append(ctx, &self.character, &self.part, target, &paint_layer, &[mark])
    }
}
register_op!(PaintErase);

/// Set how much a part's texture layer shows (0 to 1), or, with `paint_layer`, one of its painting's layers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        { "character": "kit", "part": "shirt", "opacity": 0.6 },
        { "character": "kit", "part": "shirt", "paint_layer": "paint", "opacity": 0.8 }
    ],
    "x-omuzaki-fixture" = "character-painted"
))]
pub struct PaintOpacity {
    /// The character.
    pub character: CharId,
    /// The part.
    pub part: PartId,
    /// The texture layer. Default `paint`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<LayerId>,
    /// A layer of the painting, to set its opacity instead of the whole texture layer's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paint_layer: Option<String>,
    /// 0 (hidden) to 1.
    pub opacity: f64,
}

impl Op for PaintOpacity {
    fn meta() -> OpMeta {
        edit("part.paint.opacity", "Set how much a character part's texture layer, or one of its paint layers, shows.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if !(0.0..=1.0).contains(&self.opacity) {
            return Err(OpError::invalid("opacity", format!("{} is outside 0 to 1", self.opacity)));
        }
        let mut target = Target::load(ctx, &self.character, &self.part, self.layer.as_ref())?;
        let design = match &self.paint_layer {
            None => {
                let mut design = target.design.clone();
                if let Some(l) = design.layers.get_mut(&self.part).and_then(|s| s.get_mut(target.index)) {
                    #[allow(clippy::cast_possible_truncation)] // checked to 0..=1 above
                    let opacity = self.opacity as f32;
                    l.opacity = opacity;
                }
                design
            }
            Some(name) => {
                let name = target.paint_layer(Some(name))?;
                if let Some(l) = target.painting.layers.get_mut(&name) {
                    l.opacity = self.opacity;
                }
                target.design_with_painting(&self.part)?
            }
        };
        let changes = store(ctx, &self.character, &design)?;
        Ok(plan_of(
            changes,
            json!({
                "character": self.character, "part": self.part, "layer": target.layer,
                "paint_layer": self.paint_layer, "opacity": self.opacity,
            }),
        ))
    }
}
register_op!(PaintOpacity);

/// Remove every mark from a part's texture layer (or from one of its paint layers), leaving the layers in place.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        { "character": "kit", "part": "shirt" },
        { "character": "kit", "part": "shirt", "paint_layer": "paint" }
    ],
    "x-omuzaki-fixture" = "character-painted"
))]
pub struct PaintClear {
    /// The character.
    pub character: CharId,
    /// The part.
    pub part: PartId,
    /// The texture layer. Default `paint`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<LayerId>,
    /// Clear only this layer of the painting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paint_layer: Option<String>,
}

impl Op for PaintClear {
    fn meta() -> OpMeta {
        edit(
            "part.paint.clear",
            "Remove every mark from a character part's texture layer, or from one of its paint layers.",
        )
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut target = Target::load(ctx, &self.character, &self.part, self.layer.as_ref())?;
        let names: Vec<String> = match &self.paint_layer {
            Some(name) => vec![target.paint_layer(Some(name))?],
            None => target.painting.layers.keys().cloned().collect(),
        };
        let mut removed = 0;
        for name in &names {
            if let Some(l) = target.painting.layers.get_mut(name) {
                removed += l.marks.len();
                l.marks.clear();
            }
        }
        let changes = store(ctx, &self.character, &target.design_with_painting(&self.part)?)?;
        Ok(plan_of(
            changes,
            json!({ "character": self.character, "part": self.part, "layer": target.layer, "removed": removed }),
        ))
    }
}
register_op!(PaintClear);

/// Read a part's texture layer: its painting (every layer and mark), its size, and how many marks each layer holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{ "character": "kit", "part": "shirt" }],
    "x-omuzaki-fixture" = "character-painted"
))]
pub struct PaintGet {
    /// The character.
    pub character: CharId,
    /// The part.
    pub part: PartId,
    /// The texture layer. Default `paint`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<LayerId>,
}

impl Op for PaintGet {
    fn meta() -> OpMeta {
        query("part.paint.get", "Read a character part's texture layer: its painting, its size and its layers' marks.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let target = Target::load(ctx, &self.character, &self.part, self.layer.as_ref())?;
        let opacity = target.design.layers_of(&self.part).get(target.index).map_or(1.0, |l| l.opacity);
        let layers: Vec<Value> = target
            .painting
            .order
            .iter()
            .filter_map(|id| target.painting.layers.get(id).map(|l| (id, l)))
            .map(|(id, l)| json!({ "id": id, "marks": l.marks.len(), "opacity": l.opacity, "visible": l.visible }))
            .collect();
        Ok(Plan::answer(json!({
            "character": self.character, "part": self.part, "layer": target.layer, "opacity": opacity,
            "width": target.painting.width, "height": target.painting.height, "layers": layers,
            "painting": json(&target.painting)?,
        })))
    }
}
register_op!(PaintGet);
