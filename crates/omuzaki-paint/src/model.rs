//! A painting as data: layers of marks. This is what the document stores and what an agent writes, so every mark is
//! small, readable JSON, and the pixels are always rebuilt from it ([`crate::render`]).

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::blend::BlendMode;
use crate::brush::{Brush, BrushSettings, preset};
use crate::canvas::Canvas;
use crate::colour::Colour;
use crate::error::PaintError;
use crate::geom::{Fill, Geometry};
use crate::raster::FillRule;

/// The most layers a painting holds.
pub const MAX_LAYERS: usize = 1024;
/// The most marks a painting holds, over all its layers.
pub const MAX_MARKS: usize = 200_000;
/// The most points one stroke holds.
pub const MAX_POINTS: usize = 100_000;
/// The widest brush or line, in pixels.
pub const MAX_SIZE: f64 = 2048.0;
/// The widest feather, in pixels.
pub const MAX_FEATHER: f64 = 256.0;
/// The widest gap a bucket fill closes, in pixels.
pub const MAX_GAP: u32 = 32;
/// The furthest a bucket fill grows or shrinks, in pixels.
pub const MAX_GROW: i32 = 32;
/// The longest id or tag, in characters.
pub const MAX_ID: usize = 64;
/// The longest name, in characters.
pub const MAX_NAME: usize = 120;

fn yes() -> bool {
    true
}

fn one() -> f64 {
    1.0
}

fn ink() -> String {
    "ink".to_owned()
}

fn fine() -> String {
    "maru_pen".to_owned()
}

fn default_tolerance() -> f64 {
    24.0
}

fn default_grow() -> i32 {
    1
}

fn default_taper() -> f64 {
    0.35
}

/// A painting: a canvas size, a backdrop and layers, back to front.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Painting {
    /// What it is ("bust, painted", "left eye").
    pub name: String,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The backdrop under every layer. Default clear.
    #[serde(default)]
    pub background: Colour,
    /// Every layer's id, back to front. A group's children are the layers whose `parent` is the group, in this order.
    #[serde(default)]
    pub order: Vec<String>,
    /// The layers by id.
    #[serde(default)]
    pub layers: BTreeMap<String, Layer>,
}

/// What a layer is.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LayerKind {
    /// A layer of marks.
    #[default]
    Paint,
    /// A folder of layers, composited together.
    Group,
}

/// One layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    /// What it holds ("line art", "hair shadow").
    pub name: String,
    /// Paint or group. Default paint.
    #[serde(default)]
    pub kind: LayerKind,
    /// The group it is in, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Shown. Default true.
    #[serde(default = "yes")]
    pub visible: bool,
    /// Opacity, 0 to 1. Default 1.
    #[serde(default = "one")]
    pub opacity: f64,
    /// How it lands on what is under it. Default normal; a group without one passes its layers through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend: Option<BlendMode>,
    /// Clip to the layer under it: only shows where that layer has paint (shading over a base colour).
    #[serde(default)]
    pub clip: bool,
    /// What part of a character it is, for layered-art export to a puppet (`face`, `hair_front`, `iris`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// A mask: when set, the layer only shows where these marks paint (their alpha), as in a painter's layer mask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<Vec<Mark>>,
    /// The marks, first to last.
    #[serde(default)]
    pub marks: Vec<Mark>,
}

impl Layer {
    /// An empty, visible paint layer.
    pub fn paint(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: LayerKind::Paint,
            parent: None,
            visible: true,
            opacity: 1.0,
            blend: None,
            clip: false,
            role: None,
            mask: None,
            marks: Vec::new(),
        }
    }
}

/// One thing put down on a layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Mark {
    /// A brush stroke or an ink line through points.
    Stroke(Stroke),
    /// A filled shape (with an optional outline).
    Shape(ShapeMark),
    /// A bucket fill: floods the region around a point, bounded by line art.
    Bucket(Bucket),
    /// Hatching: parallel tapered lines across a region (blush, shading, texture).
    Hatch(Hatch),
}

/// A brush stroke.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)] // independent switches an agent sets in the JSON, not a state machine
pub struct Stroke {
    /// The brush preset (see `paint.brushes`). Default `ink`.
    #[serde(default = "ink")]
    pub brush: String,
    /// Settings that override the preset's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<BrushSettings>,
    /// The colour.
    pub colour: Colour,
    /// The brush width in pixels at full pressure.
    pub size: f64,
    /// Opacity of the whole stroke, 0 to 1. Default 1.
    #[serde(default = "one")]
    pub opacity: f64,
    /// The points, `[x, y]` or `[x, y, pressure]` (pressure 0 to 1, default 1), in canvas pixels.
    pub points: Vec<Vec<f64>>,
    /// Smooth the line through the points (default) or join them straight.
    #[serde(default = "yes")]
    pub smooth: bool,
    /// Join the last point back to the first.
    #[serde(default)]
    pub closed: bool,
    /// The seed of the brush's jitter and grain.
    #[serde(default)]
    pub seed: u64,
    /// How it lands on the layer. Default normal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend: Option<BlendMode>,
    /// Erase instead of paint (the colour is then ignored).
    #[serde(default)]
    pub erase: bool,
    /// Paint only where the layer already has paint (lock transparency): shading inside flats on the same layer.
    #[serde(default)]
    pub alpha_lock: bool,
    /// Your name for it, so you can find it again ("upper lash, left").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

/// An outline drawn round a shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Outline {
    /// The line's colour.
    pub colour: Colour,
    /// The line's width in pixels.
    pub width: f64,
}

/// A filled shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShapeMark {
    /// The region.
    pub geometry: Geometry,
    /// A colour (`"#rrggbb"`) or a gradient.
    pub fill: Fill,
    /// Which parts of a self-overlapping path are inside. Default nonzero.
    #[serde(default)]
    pub fill_rule: FillRule,
    /// Soften the edge by this many pixels (a Gaussian blur of the shape). Default 0.
    #[serde(default)]
    pub feather: f64,
    /// An outline drawn over the fill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outline: Option<Outline>,
    /// Opacity, 0 to 1. Default 1.
    #[serde(default = "one")]
    pub opacity: f64,
    /// How it lands on the layer. Default normal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend: Option<BlendMode>,
    /// Erase the shape out of the layer instead.
    #[serde(default)]
    pub erase: bool,
    /// Paint only where the layer already has paint (lock transparency): shading inside flats on the same layer.
    #[serde(default)]
    pub alpha_lock: bool,
    /// Your name for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

/// A bucket fill.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Bucket {
    /// Where to start, `[x, y]`.
    pub at: [f64; 2],
    /// A colour or a gradient.
    pub fill: Fill,
    /// How different a pixel may be from the start's and still fill, in 8-bit levels (0 to 255). Default 24.
    #[serde(default = "default_tolerance")]
    pub tolerance: f64,
    /// Close gaps in the line art up to this many pixels wide. Default 0.
    #[serde(default)]
    pub gap: u32,
    /// Grow the fill this many pixels past its edge, under the line art (negative shrinks). Default 1.
    #[serde(default = "default_grow")]
    pub grow: i32,
    /// The layers whose paint bounds the fill (your line art), back to front. Empty: this layer as it is so far.
    #[serde(default)]
    pub sample: Vec<String>,
    /// Soften the fill's edge by this many pixels. Default 0.
    #[serde(default)]
    pub feather: f64,
    /// Opacity, 0 to 1. Default 1.
    #[serde(default = "one")]
    pub opacity: f64,
    /// How it lands on the layer. Default normal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend: Option<BlendMode>,
    /// Paint only where the layer already has paint (lock transparency): shading inside flats on the same layer.
    #[serde(default)]
    pub alpha_lock: bool,
    /// Your name for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

/// Hatching.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Hatch {
    /// The region the lines cross.
    pub region: Geometry,
    /// The lines' colour.
    pub colour: Colour,
    /// The lines' direction in degrees, clockwise from pointing right on screen.
    pub angle: f64,
    /// The gap between lines, pixels.
    pub spacing: f64,
    /// The lines' width, pixels.
    pub width: f64,
    /// The brush (default `maru_pen`).
    #[serde(default = "fine")]
    pub brush: String,
    /// How much of each line thins in at each end, 0 to 0.5. Default 0.35.
    #[serde(default = "default_taper")]
    pub taper: f64,
    /// How much to shorten each line inside the region, at each end, 0 to 0.45. Default 0.
    #[serde(default)]
    pub inset: f64,
    /// Random wobble of each line's gap, length and width, 0 to 1. Default 0.
    #[serde(default)]
    pub jitter: f64,
    /// The seed of the wobble.
    #[serde(default)]
    pub seed: u64,
    /// Opacity, 0 to 1. Default 1.
    #[serde(default = "one")]
    pub opacity: f64,
    /// How it lands on the layer. Default normal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend: Option<BlendMode>,
    /// Paint only where the layer already has paint (lock transparency): shading inside flats on the same layer.
    #[serde(default)]
    pub alpha_lock: bool,
    /// Your name for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

impl Mark {
    /// The mark's tag, if it has one.
    pub fn tag(&self) -> Option<&str> {
        match self {
            Self::Stroke(m) => m.tag.as_deref(),
            Self::Shape(m) => m.tag.as_deref(),
            Self::Bucket(m) => m.tag.as_deref(),
            Self::Hatch(m) => m.tag.as_deref(),
        }
    }

    /// Whether it paints only where the layer already has paint.
    pub fn alpha_lock(&self) -> bool {
        match self {
            Self::Stroke(m) => m.alpha_lock,
            Self::Shape(m) => m.alpha_lock,
            Self::Bucket(m) => m.alpha_lock,
            Self::Hatch(m) => m.alpha_lock,
        }
    }

    /// The layers this mark samples (a bucket fill's line art).
    pub fn samples(&self) -> &[String] {
        match self {
            Self::Bucket(b) => &b.sample,
            _ => &[],
        }
    }

    /// Checks the mark can be drawn on a `width` by `height` canvas.
    ///
    /// # Errors
    /// [`PaintError::Invalid`] naming the field at fault, under `field`.
    pub fn check(&self, field: &str) -> Result<(), PaintError> {
        if let Some(tag) = self.tag() {
            text(&format!("{field}/tag"), tag, MAX_ID)?;
        }
        match self {
            Self::Stroke(s) => {
                unit(&format!("{field}/opacity"), s.opacity)?;
                size(&format!("{field}/size"), s.size)?;
                s.brush(field)?.check(&format!("{field}/settings"))?;
                s.points(field).map(|_| ())
            }
            Self::Shape(s) => {
                unit(&format!("{field}/opacity"), s.opacity)?;
                range(&format!("{field}/feather"), s.feather, 0.0, MAX_FEATHER)?;
                s.geometry.to_path(&format!("{field}/geometry"))?;
                if let Some(o) = &s.outline {
                    size(&format!("{field}/outline/width"), o.width)?;
                }
                s.fill.check(&format!("{field}/fill"))
            }
            Self::Bucket(b) => {
                unit(&format!("{field}/opacity"), b.opacity)?;
                range(&format!("{field}/tolerance"), b.tolerance, 0.0, 255.0)?;
                range(&format!("{field}/feather"), b.feather, 0.0, MAX_FEATHER)?;
                if b.gap > MAX_GAP {
                    return Err(PaintError::invalid(format!("{field}/gap"), format!("at most {MAX_GAP} px")));
                }
                if b.grow.abs() > MAX_GROW {
                    return Err(PaintError::invalid(format!("{field}/grow"), format!("-{MAX_GROW} to {MAX_GROW} px")));
                }
                for v in b.at {
                    range(&format!("{field}/at"), v, -1e6, 1e6)?;
                }
                b.fill.check(&format!("{field}/fill"))
            }
            Self::Hatch(h) => {
                unit(&format!("{field}/opacity"), h.opacity)?;
                unit(&format!("{field}/jitter"), h.jitter)?;
                range(&format!("{field}/taper"), h.taper, 0.0, 0.5)?;
                range(&format!("{field}/inset"), h.inset, 0.0, 0.45)?;
                range(&format!("{field}/angle"), h.angle, -360.0, 360.0)?;
                range(&format!("{field}/spacing"), h.spacing, 0.5, MAX_SIZE)?;
                size(&format!("{field}/width"), h.width)?;
                preset(&h.brush).map_err(|e| PaintError::invalid(format!("{field}/brush"), e.to_string()))?;
                h.region.to_path(&format!("{field}/region")).map(|_| ())
            }
        }
    }
}

impl Stroke {
    /// The brush: the preset with the settings laid over it.
    ///
    /// # Errors
    /// [`PaintError::Invalid`] when there is no such preset.
    pub fn brush(&self, field: &str) -> Result<Brush, PaintError> {
        let base = preset(&self.brush).map_err(|e| PaintError::invalid(format!("{field}/brush"), e.to_string()))?;
        Ok(match &self.settings {
            Some(s) => base.with(s),
            None => base,
        })
    }

    /// The points as `[x, y, pressure]`.
    ///
    /// # Errors
    /// [`PaintError::Invalid`] for none, too many, a point that isn't 2 or 3 numbers, a pressure outside 0..1, or a
    /// number that isn't finite.
    pub fn points(&self, field: &str) -> Result<Vec<[f64; 3]>, PaintError> {
        let f = format!("{field}/points");
        if self.points.is_empty() || self.points.len() > MAX_POINTS {
            return Err(PaintError::invalid(f, format!("{} points; give 1 to {MAX_POINTS}", self.points.len())));
        }
        self.points
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let at = || format!("{f}/{i}");
                let ok = |v: f64| v.is_finite() && v.abs() <= 1e6;
                match p.as_slice() {
                    [x, y] if ok(*x) && ok(*y) => Ok([*x, *y, 1.0]),
                    [x, y, pr] if ok(*x) && ok(*y) && (0.0..=1.0).contains(pr) => Ok([*x, *y, *pr]),
                    _ => Err(PaintError::invalid(at(), "a point is [x, y] or [x, y, pressure] with pressure 0 to 1")),
                }
            })
            .collect()
    }
}

fn unit(field: &str, v: f64) -> Result<(), PaintError> {
    range(field, v, 0.0, 1.0)
}

fn size(field: &str, v: f64) -> Result<(), PaintError> {
    if v > 0.0 && v <= MAX_SIZE {
        Ok(())
    } else {
        Err(PaintError::invalid(field, format!("{v} is outside 0 to {MAX_SIZE} px")))
    }
}

fn range(field: &str, v: f64, lo: f64, hi: f64) -> Result<(), PaintError> {
    if (lo..=hi).contains(&v) {
        Ok(())
    } else {
        Err(PaintError::invalid(field, format!("{v} is outside {lo} to {hi}")))
    }
}

fn text(field: &str, s: &str, max: usize) -> Result<(), PaintError> {
    if s.chars().count() > max || s.chars().any(char::is_control) {
        return Err(PaintError::invalid(field, format!("at most {max} characters, no control characters")));
    }
    Ok(())
}

/// Checks an id: 1 to [`MAX_ID`] letters, digits, `-`, `_` or `.`.
///
/// # Errors
/// [`PaintError::Invalid`] under `field` when it isn't one.
pub fn check_id(field: &str, id: &str) -> Result<(), PaintError> {
    let ok = !id.is_empty()
        && id.len() <= MAX_ID
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(PaintError::invalid(field, format!("{id:?}: an id is 1 to {MAX_ID} letters, digits, '-', '_' or '.'")))
    }
}

impl Painting {
    /// An empty painting.
    pub fn new(name: impl Into<String>, width: u32, height: u32) -> Self {
        Self { name: name.into(), width, height, background: Colour::CLEAR, order: Vec::new(), layers: BTreeMap::new() }
    }

    /// The layer `id`.
    ///
    /// # Errors
    /// [`PaintError::NoLayer`] when there is none.
    pub fn layer(&self, id: &str) -> Result<&Layer, PaintError> {
        self.layers.get(id).ok_or_else(|| PaintError::NoLayer(id.to_owned()))
    }

    /// How many marks the painting holds, over every layer and mask.
    pub fn mark_count(&self) -> usize {
        self.layers.values().map(|l| l.marks.len() + l.mask.as_ref().map_or(0, Vec::len)).sum()
    }

    /// The children of group `parent` (or the top level), back to front.
    pub fn children(&self, parent: Option<&str>) -> Vec<&str> {
        self.order
            .iter()
            .filter(|id| self.layers.get(id.as_str()).is_some_and(|l| l.parent.as_deref() == parent))
            .map(String::as_str)
            .collect()
    }

    /// Checks the whole painting: its size, its layers and their order, groups, every mark, and that no bucket fills
    /// sample each other in a loop.
    ///
    /// # Errors
    /// The first problem found, naming its field.
    pub fn check(&self) -> Result<(), PaintError> {
        Canvas::new(self.width, self.height).map(|_| ())?;
        text("name", &self.name, MAX_NAME)?;
        if self.layers.len() > MAX_LAYERS {
            return Err(PaintError::invalid("layers", format!("at most {MAX_LAYERS} layers")));
        }
        let listed: BTreeSet<&str> = self.order.iter().map(String::as_str).collect();
        if listed.len() != self.order.len() {
            return Err(PaintError::invalid("order", "a layer is listed twice"));
        }
        let held: BTreeSet<&str> = self.layers.keys().map(String::as_str).collect();
        if listed != held {
            return Err(PaintError::invalid("order", "order must list every layer exactly once"));
        }
        if self.mark_count() > MAX_MARKS {
            return Err(PaintError::invalid("layers", format!("at most {MAX_MARKS} marks in a painting")));
        }
        for (id, layer) in &self.layers {
            let f = format!("layers/{id}");
            check_id(&f, id)?;
            text(&format!("{f}/name"), &layer.name, MAX_NAME)?;
            unit(&format!("{f}/opacity"), layer.opacity)?;
            if let Some(role) = &layer.role {
                check_id(&format!("{f}/role"), role)?;
            }
            if let Some(parent) = &layer.parent {
                if self.layers.get(parent).map(|p| p.kind) != Some(LayerKind::Group) {
                    return Err(PaintError::invalid(format!("{f}/parent"), format!("{parent:?} is not a group")));
                }
                self.check_ancestry(id)?;
            }
            if layer.kind == LayerKind::Group && (!layer.marks.is_empty() || layer.mask.is_some()) {
                return Err(PaintError::invalid(f, "a group holds layers, not marks"));
            }
            for (i, m) in layer.marks.iter().enumerate() {
                m.check(&format!("{f}/marks/{i}"))?;
                for s in m.samples() {
                    let l = self
                        .layer(s)
                        .map_err(|_| PaintError::invalid(format!("{f}/marks/{i}/sample"), format!("no layer {s:?}")))?;
                    if l.kind != LayerKind::Paint {
                        return Err(PaintError::invalid(format!("{f}/marks/{i}/sample"), format!("{s:?} is a group")));
                    }
                }
            }
            for (i, m) in layer.mask.iter().flatten().enumerate() {
                m.check(&format!("{f}/mask/{i}"))?;
                if !m.samples().is_empty() {
                    return Err(PaintError::invalid(
                        format!("{f}/mask/{i}"),
                        "a mask's bucket fills sample the mask itself",
                    ));
                }
            }
        }
        self.sample_order().map(|_| ())
    }

    fn check_ancestry(&self, id: &str) -> Result<(), PaintError> {
        let mut seen = BTreeSet::new();
        let mut at = Some(id);
        while let Some(cur) = at {
            if !seen.insert(cur) {
                return Err(PaintError::invalid(format!("layers/{id}/parent"), "groups contain each other in a loop"));
            }
            at = self.layers.get(cur).and_then(|l| l.parent.as_deref());
        }
        Ok(())
    }

    /// The paint layers in an order where each comes after the layers its bucket fills sample.
    ///
    /// # Errors
    /// [`PaintError::SampleLoop`] when layers sample each other in a loop.
    pub fn sample_order(&self) -> Result<Vec<&str>, PaintError> {
        fn visit<'a>(
            p: &'a Painting,
            id: &'a str,
            done: &mut BTreeSet<&'a str>,
            stack: &mut Vec<&'a str>,
            out: &mut Vec<&'a str>,
        ) -> Result<(), PaintError> {
            if done.contains(id) {
                return Ok(());
            }
            if stack.contains(&id) {
                stack.push(id);
                return Err(PaintError::SampleLoop(stack.join(" -> ")));
            }
            stack.push(id);
            if let Some(layer) = p.layers.get(id) {
                for m in &layer.marks {
                    for s in m.samples() {
                        visit(p, s, done, stack, out)?;
                    }
                }
            }
            stack.pop();
            done.insert(id);
            out.push(id);
            Ok(())
        }
        let mut done = BTreeSet::new();
        let mut out = Vec::new();
        for id in &self.order {
            if self.layers.get(id).is_some_and(|l| l.kind == LayerKind::Paint) {
                visit(self, id, &mut done, &mut Vec::new(), &mut out)?;
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(points: Vec<Vec<f64>>) -> Mark {
        Mark::Stroke(Stroke {
            brush: ink(),
            settings: None,
            colour: Colour::BLACK,
            size: 4.0,
            opacity: 1.0,
            points,
            smooth: true,
            closed: false,
            seed: 0,
            blend: None,
            erase: false,
            alpha_lock: false,
            tag: None,
        })
    }

    fn painting() -> Painting {
        let mut p = Painting::new("test", 64, 64);
        p.layers.insert("a".into(), Layer::paint("A"));
        p.order.push("a".into());
        p
    }

    #[test]
    fn a_mark_reads_from_the_json_an_agent_writes() {
        let m: Mark = serde_json::from_str(
            r##"{"type":"stroke","colour":"#1b1420","size":3,"points":[[10,10],[20,12,0.5]],"tag":"lash"}"##,
        )
        .unwrap();
        m.check("m").unwrap();
        let Mark::Stroke(s) = &m else { panic!("a stroke") };
        assert_eq!(s.points("m").unwrap()[1], [20.0, 12.0, 0.5]);
        let shape: Mark = serde_json::from_str(
            r##"{"type":"shape","geometry":{"kind":"ellipse","cx":5,"cy":5,"rx":3,"ry":2},"fill":"#ffffff"}"##,
        )
        .unwrap();
        shape.check("m").unwrap();
    }

    #[test]
    fn bad_points_are_refused_with_their_place() {
        let e = stroke(vec![vec![1.0, 2.0], vec![1.0]]).check("marks/0").unwrap_err();
        assert!(e.to_string().contains("marks/0/points/1"), "{e}");
        assert!(stroke(vec![vec![1.0, 2.0, 1.5]]).check("m").is_err());
        assert!(stroke(vec![]).check("m").is_err());
    }

    #[test]
    fn the_order_must_match_the_layers() {
        let mut p = painting();
        p.check().unwrap();
        p.order.push("ghost".into());
        assert!(p.check().is_err());
    }

    #[test]
    fn sampling_in_a_loop_is_refused() {
        let mut p = painting();
        p.layers.insert("b".into(), Layer::paint("B"));
        p.order.push("b".into());
        let bucket = |s: &str| {
            Mark::Bucket(Bucket {
                at: [1.0, 1.0],
                fill: Fill::Colour(Colour::WHITE),
                tolerance: 24.0,
                gap: 0,
                grow: 1,
                sample: vec![s.to_owned()],
                feather: 0.0,
                opacity: 1.0,
                blend: None,
                alpha_lock: false,
                tag: None,
            })
        };
        p.layers.get_mut("a").unwrap().marks.push(bucket("b"));
        p.check().unwrap();
        assert_eq!(p.sample_order().unwrap(), vec!["b", "a"]);
        p.layers.get_mut("b").unwrap().marks.push(bucket("a"));
        assert!(matches!(p.check(), Err(PaintError::SampleLoop(_))));
    }
}
