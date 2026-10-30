//! Motion-graphics layers: the 2D half of a sequence. Text, shapes, bar charts, icons and speed lines, each with keyed
//! position, scale, stretch, rotation, skew, opacity, colour and a wipe-on reveal, parented to one another so a lower
//! third's band and its words move as one, and blurred along their motion when they whip. They are drawn behind the 3D
//! frame (`back`: a news set's logo wall) or over it (`front`: titles, stingers, lower thirds, charts).
//!
//! Sizes are in points: pixels on a frame 1080 wide, so a sequence renders the same at any size. Positions are
//! fractions of the frame (`x = 0.5, y = 0.5` is the centre).

use std::sync::OnceLock;

use ab_glyph::{Font, FontRef, OutlineCurve, PxScale, ScaleFont, VariableFont};
use omuzaki_core::colour::Colour;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tiny_skia::{
    FillRule, GradientStop, LinearGradient, Mask, Paint, Path, PathBuilder, Pixmap, Point, Rect, Shader, SpreadMode,
    Stroke as SkStroke, Transform,
};

use crate::curve::Track;

/// Points per frame width: sizes are pixels on a frame this wide.
pub const REFERENCE_WIDTH: f32 = 1080.0;
/// The most samples a motion-blurred layer is drawn with.
pub const MAX_BLUR_SAMPLES: u32 = 8;
/// The longest text of one layer, characters.
pub const MAX_TEXT: usize = 400;
/// The most bars in one chart.
pub const MAX_BARS: usize = 24;

/// The face text is set in: Bricolage Grotesque (SIL OFL 1.1), a variable font from 200 to 800.
static FACE: &[u8] = include_bytes!("../../omuzaki-short/assets/fonts/BricolageGrotesque.ttf");

fn face() -> Option<&'static FontRef<'static>> {
    static FONT: OnceLock<Option<FontRef<'static>>> = OnceLock::new();
    FONT.get_or_init(|| FontRef::try_from_slice(FACE).ok()).as_ref()
}

/// Whether a layer is drawn behind the shots or over them.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Depth {
    /// Behind every shot (a shot's backdrop must be `none` for it to show).
    Back,
    /// Over every shot. The default.
    #[default]
    Front,
}

/// Which edge a reveal wipes on from.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum From {
    /// From the left edge. The default.
    #[default]
    Left,
    /// From the right.
    Right,
    /// From the top.
    Top,
    /// From the bottom.
    Bottom,
    /// Out from the middle, both ways.
    Centre,
}

/// Text alignment about the layer's origin.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    /// The text starts at the origin.
    Left,
    /// Centred on it. The default.
    #[default]
    Centre,
    /// Ends at it.
    Right,
}

/// A line drawn along a shape's edge.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Stroke {
    /// Its colour.
    pub colour: Colour,
    /// Its width, points.
    pub width: f32,
}

/// The most stops in one gradient.
pub const MAX_STOPS: usize = 16;

/// A fill that fades through colours across the layer's box.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Gradient {
    /// The direction it runs, degrees clockwise from top to bottom: 0 runs down, 90 runs left to right.
    #[serde(default)]
    pub angle: f32,
    /// Its colours, 2 to 16, in order along the direction.
    pub stops: Vec<GradientAt>,
}

impl Gradient {
    /// Checks the angle and the stops, naming `at`.
    ///
    /// # Errors
    /// A sentence naming the first bad value.
    pub fn check(&self, at: &str) -> Result<(), String> {
        if !self.angle.is_finite() {
            return Err(format!("{at}.angle is a number of degrees"));
        }
        if !(2..=MAX_STOPS).contains(&self.stops.len()) {
            return Err(format!("{at}.stops: 2 to {MAX_STOPS} of them"));
        }
        let mut last = 0.0;
        for (i, stop) in self.stops.iter().enumerate() {
            if !(last..=1.0).contains(&stop.at) {
                return Err(format!("{at}.stops[{i}].at is {last} to 1, after the stop before it"));
            }
            last = stop.at;
        }
        Ok(())
    }
}

/// One colour of a gradient.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GradientAt {
    /// Where it sits along the gradient, 0 (start) to 1 (end), no earlier than the stop before it.
    pub at: f32,
    /// Its colour.
    pub colour: Colour,
}

/// What a layer draws.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Draw {
    /// A line of text.
    Text {
        /// The words.
        text: String,
        /// The type size, points.
        #[serde(default = "text_size")]
        size: f32,
        /// The weight, 200 (light) to 800 (extra bold).
        #[serde(default = "text_weight")]
        weight: f32,
        /// Extra space between letters, in ems (0.1 is airy).
        #[serde(default)]
        tracking: f32,
        /// Where the text sits about the origin.
        #[serde(default)]
        align: Align,
    },
    /// A rectangle, rounded if `radius` is over 0; give the layer `skew` for a news-style slant.
    Rect {
        /// Width, points.
        w: f32,
        /// Height, points.
        h: f32,
        /// Corner radius, points.
        #[serde(default)]
        radius: f32,
    },
    /// An ellipse.
    Ellipse {
        /// Width, points.
        w: f32,
        /// Height, points.
        h: f32,
    },
    /// A ring, or an arc of one (`sweep` below 360).
    Ring {
        /// Radius to the ring's middle, points.
        radius: f32,
        /// How thick, points.
        width: f32,
        /// Degrees of the ring drawn, clockwise from the top.
        #[serde(default = "full_turn")]
        sweep: Track<f32>,
    },
    /// A band whose top and bottom edges bow: the front of a curved desk, a stage lip, a road seen in perspective. Each
    /// edge is a parabola through its apex, so a band seen from one side of a curved set keeps its shape when `apex`
    /// moves off the middle.
    Band {
        /// Width, points.
        w: f32,
        /// Height at the apex, points.
        h: f32,
        /// How far the top edge's ends sit below its apex, points; negative lifts them (a desk seen from above bows
        /// down at its ends, one seen from below bows up).
        #[serde(default)]
        bow: f32,
        /// The bottom edge's bow, points; the same as `bow` when left out.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bow_bottom: Option<f32>,
        /// Where the apex sits across the band, 0 (left end) to 1 (right end); -2 to 3 puts it off the band, for a
        /// piece of a longer curve (a stripe across only part of a curved desk).
        #[serde(default = "middle_of")]
        apex: f32,
    },
    /// A closed polygon through points, in points from the layer's origin (y down).
    Polygon {
        /// The corners.
        points: Vec<[f32; 2]>,
    },
    /// A bar chart: one column per value, growing in one after another, with a label over each, a caption under it and
    /// an icon (an hourly forecast, a poll, a goal).
    Bars {
        /// The values.
        values: Vec<f32>,
        /// The top of the scale; the largest value when left out.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f32>,
        /// A label over each column.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        labels: Vec<String>,
        /// A caption under each column.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        captions: Vec<String>,
        /// An icon in each column (`sun`, `moon`, `cloud` ...; `icon` kinds).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        icons: Vec<String>,
        /// Each column's colour; the last repeats.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        colours: Vec<Colour>,
        /// The colour of the column's slot behind its bar.
        #[serde(default = "slot_colour")]
        slot: Colour,
        /// The chart's width, points.
        w: f32,
        /// Its height, points.
        h: f32,
        /// Space between columns, points.
        #[serde(default = "bar_gap")]
        gap: f32,
        /// How far the bars have grown, 0 to 1, one after another.
        #[serde(default = "full")]
        grow: Track<f32>,
    },
    /// A drawn icon.
    Icon {
        /// `sun`, `moon`, `cloud`, `bolt`, `drop`, `star`, `arrow` or `heart`.
        name: String,
        /// Its size, points.
        #[serde(default = "icon_size")]
        size: f32,
    },
    /// Speed lines: streaks across the frame that sell a whip or a stinger.
    Lines {
        /// How many.
        #[serde(default = "line_count")]
        count: u32,
        /// The longest streak, points.
        #[serde(default = "line_length")]
        length: f32,
        /// How thick, points.
        #[serde(default = "line_thickness")]
        thickness: f32,
        /// The band they fill, points high.
        #[serde(default = "line_band")]
        band: f32,
        /// How far they have travelled, 0 to 1, across a band twice the frame wide.
        #[serde(default = "full")]
        travel: Track<f32>,
    },
}

fn middle_of() -> f32 {
    0.5
}

fn text_size() -> f32 {
    96.0
}

fn text_weight() -> f32 {
    700.0
}

fn full_turn() -> Track<f32> {
    Track::Hold(360.0)
}

fn slot_colour() -> Colour {
    Colour::rgba(255, 255, 255, 60)
}

fn bar_gap() -> f32 {
    10.0
}

fn full() -> Track<f32> {
    Track::Hold(1.0)
}

fn icon_size() -> f32 {
    64.0
}

fn line_count() -> u32 {
    24
}

fn line_length() -> f32 {
    600.0
}

fn line_thickness() -> f32 {
    6.0
}

fn line_band() -> f32 {
    400.0
}

impl Draw {
    /// Every kind's name, for listings.
    pub const KINDS: [&'static str; 9] =
        ["text", "rect", "ellipse", "ring", "band", "polygon", "bars", "icon", "lines"];

    /// This one's kind.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::Rect { .. } => "rect",
            Self::Ellipse { .. } => "ellipse",
            Self::Ring { .. } => "ring",
            Self::Band { .. } => "band",
            Self::Polygon { .. } => "polygon",
            Self::Bars { .. } => "bars",
            Self::Icon { .. } => "icon",
            Self::Lines { .. } => "lines",
        }
    }
}

/// One layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    /// Its name, unique in the sequence.
    pub id: String,
    /// What it draws.
    pub draw: Draw,
    /// Behind the shots or over them.
    #[serde(default)]
    pub depth: Depth,
    /// A layer drawn earlier whose position, scale, turn and opacity this one follows (a lower third's words on its
    /// band); this one's own `x` and `y` are then offsets in points from the parent's origin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// When it appears, seconds; its keys count from here.
    #[serde(default)]
    pub start: f32,
    /// When it goes; never when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<f32>,
    /// Across the frame, 0 (left) to 1 (right); points from the parent's origin with a parent.
    #[serde(default = "half")]
    pub x: Track<f32>,
    /// Down the frame, 0 (top) to 1 (bottom); points from the parent's origin with a parent.
    #[serde(default = "half")]
    pub y: Track<f32>,
    /// The point of the layer's own box at its position, `[0, 0]` its top left, `[1, 1]` its bottom right.
    #[serde(default = "middle")]
    pub anchor: [f32; 2],
    /// Uniform scale.
    #[serde(default = "full", skip_serializing_if = "is_full")]
    pub scale: Track<f32>,
    /// Scale per axis on top of `scale`: squash and stretch.
    #[serde(default = "unit", skip_serializing_if = "is_unit")]
    pub stretch: Track<[f32; 2]>,
    /// Clockwise turn, degrees.
    #[serde(default = "zero", skip_serializing_if = "is_zero")]
    pub rotation: Track<f32>,
    /// Horizontal slant, degrees (positive leans the top right, as italic and news bands do).
    #[serde(default = "zero", skip_serializing_if = "is_zero")]
    pub skew: Track<f32>,
    /// 0 to 1.
    #[serde(default = "full", skip_serializing_if = "is_full")]
    pub opacity: Track<f32>,
    /// How much is wiped on, 0 to 1, from `reveal_from`.
    #[serde(default = "full", skip_serializing_if = "is_full")]
    pub reveal: Track<f32>,
    /// The edge the reveal starts from.
    #[serde(default)]
    pub reveal_from: From,
    /// The fill colour.
    #[serde(default = "white")]
    pub fill: Track<Colour>,
    /// A second colour the fill fades to, top to bottom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_to: Option<Colour>,
    /// A gradient with several stops in any direction (a desk lit from one side, a sky with a horizon band), in place
    /// of `fill` and `fill_to`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gradient: Option<Gradient>,
    /// A line along the edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke: Option<Stroke>,
    /// Motion blur: the share of a frame the shutter is open, 0 (none) to 1.
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub motion_blur: f32,
}

fn half() -> Track<f32> {
    Track::Hold(0.5)
}

fn middle() -> [f32; 2] {
    [0.5, 0.5]
}

fn unit() -> Track<[f32; 2]> {
    Track::Hold([1.0, 1.0])
}

fn is_unit(t: &Track<[f32; 2]>) -> bool {
    *t == unit()
}

fn zero() -> Track<f32> {
    Track::Hold(0.0)
}

fn is_zero(t: &Track<f32>) -> bool {
    *t == zero()
}

#[allow(clippy::trivially_copy_pass_by_ref, reason = "serde's skip_serializing_if passes a reference")]
fn is_zero_f32(v: &f32) -> bool {
    *v == 0.0
}

fn is_full(t: &Track<f32>) -> bool {
    *t == full()
}

fn white() -> Track<Colour> {
    Track::Hold(Colour::WHITE)
}

/// The icon names [`Draw::Icon`] and [`Draw::Bars`] take.
pub const ICONS: [&str; 8] = ["sun", "moon", "cloud", "bolt", "drop", "star", "arrow", "heart"];

impl Layer {
    /// Checks the layer, naming `at`.
    ///
    /// # Errors
    /// A sentence naming the first bad value.
    pub fn check(&self, at: &str) -> Result<(), String> {
        for (name, track) in [
            ("x", &self.x),
            ("y", &self.y),
            ("scale", &self.scale),
            ("rotation", &self.rotation),
            ("skew", &self.skew),
            ("opacity", &self.opacity),
            ("reveal", &self.reveal),
        ] {
            track.check(&format!("{at}.{name}"))?;
        }
        self.stretch.check(&format!("{at}.stretch"))?;
        self.fill.check(&format!("{at}.fill"))?;
        if !(0.0..=1.0).contains(&self.motion_blur) {
            return Err(format!("{at}.motion_blur is 0 to 1"));
        }
        if let Some(g) = &self.gradient {
            if self.fill_to.is_some() {
                return Err(format!("{at}: give a gradient or fill_to, not both"));
            }
            g.check(&format!("{at}.gradient"))?;
        }
        if self.end.is_some_and(|end| end <= self.start) {
            return Err(format!("{at}: end comes after start"));
        }
        let positive = |name: &str, v: f32| {
            if v.is_finite() && v > 0.0 && v <= 20_000.0 { Ok(()) } else { Err(format!("{at}.draw.{name} is over 0")) }
        };
        match &self.draw {
            Draw::Text { text, size, weight, .. } => {
                if text.chars().count() > MAX_TEXT {
                    return Err(format!("{at}.draw.text is at most {MAX_TEXT} characters"));
                }
                positive("size", *size)?;
                if !(200.0..=800.0).contains(weight) {
                    return Err(format!("{at}.draw.weight is 200 to 800"));
                }
            }
            Draw::Rect { w, h, radius } => {
                positive("w", *w)?;
                positive("h", *h)?;
                if *radius < 0.0 {
                    return Err(format!("{at}.draw.radius is 0 or more"));
                }
            }
            Draw::Ellipse { w, h } => {
                positive("w", *w)?;
                positive("h", *h)?;
            }
            Draw::Ring { radius, width, sweep } => {
                positive("radius", *radius)?;
                positive("width", *width)?;
                sweep.check(&format!("{at}.draw.sweep"))?;
            }
            Draw::Band { w, h, bow, bow_bottom, apex } => {
                positive("w", *w)?;
                positive("h", *h)?;
                for (name, v) in [("bow", Some(*bow)), ("bow_bottom", *bow_bottom)] {
                    if v.is_some_and(|v| !v.is_finite() || v.abs() > 20_000.0) {
                        return Err(format!("{at}.draw.{name} is -20000 to 20000 points"));
                    }
                }
                if !(-2.0..=3.0).contains(apex) {
                    return Err(format!("{at}.draw.apex is -2 to 3"));
                }
            }
            Draw::Polygon { points } => {
                if points.len() < 3 || points.len() > 256 {
                    return Err(format!("{at}.draw.points: 3 to 256 corners"));
                }
            }
            Draw::Bars { values, w, h, icons, grow, .. } => {
                if values.is_empty() || values.len() > MAX_BARS {
                    return Err(format!("{at}.draw.values: 1 to {MAX_BARS} of them"));
                }
                positive("w", *w)?;
                positive("h", *h)?;
                if let Some(bad) = icons.iter().find(|i| !ICONS.contains(&i.as_str())) {
                    return Err(format!("{at}.draw.icons: no icon '{bad}'; the icons are {}", ICONS.join(", ")));
                }
                grow.check(&format!("{at}.draw.grow"))?;
            }
            Draw::Icon { name, size } => {
                if !ICONS.contains(&name.as_str()) {
                    return Err(format!("{at}.draw.name: no icon '{name}'; the icons are {}", ICONS.join(", ")));
                }
                positive("size", *size)?;
            }
            Draw::Lines { count, length, thickness, band, travel } => {
                if !(1..=200).contains(count) {
                    return Err(format!("{at}.draw.count is 1 to 200"));
                }
                positive("length", *length)?;
                positive("thickness", *thickness)?;
                positive("band", *band)?;
                travel.check(&format!("{at}.draw.travel"))?;
            }
        }
        Ok(())
    }

    /// Whether the layer shows at `t`.
    pub fn shows_at(&self, t: f32) -> bool {
        t >= self.start && self.end.is_none_or(|end| t < end)
    }
}

/// A layer's place at one moment: where its origin is in the frame and how it is turned, and its opacity.
#[derive(Copy, Clone, Debug)]
struct Placed {
    transform: Transform,
    opacity: f32,
}

/// The layers of a sequence, drawn.
pub struct Painter<'a> {
    layers: &'a [Layer],
    size: [u32; 2],
}

impl<'a> Painter<'a> {
    /// A painter of `layers` onto frames of `size`.
    pub fn new(layers: &'a [Layer], size: [u32; 2]) -> Self {
        Self { layers, size }
    }

    #[allow(clippy::cast_precision_loss, reason = "frame sides of at most 4096")]
    fn units(&self) -> f32 {
        self.size[0] as f32 / REFERENCE_WIDTH
    }

    /// Where layer `index` is at `t`, its parents' places applied; `None` when it or a parent doesn't show.
    fn place(&self, index: usize, t: f32, depth: u32) -> Option<Placed> {
        let layer = self.layers.get(index)?;
        if !layer.shows_at(t) || depth > 16 {
            return None;
        }
        let local = t - layer.start;
        let u = self.units();
        #[allow(clippy::cast_precision_loss, reason = "frame sides of at most 4096")]
        let [w, h] = [self.size[0] as f32, self.size[1] as f32];
        let parent = match &layer.parent {
            Some(id) => {
                let at = self.layers.iter().position(|l| &l.id == id)?;
                Some(self.place(at, t, depth + 1)?)
            }
            None => None,
        };
        let (x, y) = match parent {
            Some(_) => (layer.x.at(local) * u, layer.y.at(local) * u),
            None => (layer.x.at(local) * w, layer.y.at(local) * h),
        };
        let scale = layer.scale.at(local);
        let [sx, sy] = layer.stretch.at(local);
        let skew = layer.skew.at(local).to_radians().tan();
        let own = Transform::from_translate(x, y)
            .pre_concat(Transform::from_rotate(layer.rotation.at(local)))
            .pre_concat(Transform::from_row(1.0, 0.0, -skew, 1.0, 0.0, 0.0))
            .pre_concat(Transform::from_scale(scale * sx * u, scale * sy * u));
        let opacity = layer.opacity.at(local).clamp(0.0, 1.0);
        Some(match parent {
            Some(p) => Placed { transform: p.transform.pre_concat(own), opacity: opacity * p.opacity },
            None => Placed { transform: own, opacity },
        })
    }

    /// Draws every layer at `depth` that shows at `t` onto `canvas`, in order.
    pub fn paint(&self, canvas: &mut Pixmap, depth: Depth, t: f32, fps: u32) {
        for (index, layer) in self.layers.iter().enumerate() {
            if layer.depth != depth || !layer.shows_at(t) {
                continue;
            }
            if layer.motion_blur > 0.0 {
                self.paint_blurred(canvas, index, t, fps);
            } else {
                self.paint_one(canvas, index, t, 1.0);
            }
        }
    }

    /// Draws layer `index` as the mean of several moments across the shutter, so a whip leaves a streak.
    fn paint_blurred(&self, canvas: &mut Pixmap, index: usize, t: f32, fps: u32) {
        let Some(layer) = self.layers.get(index) else { return };
        #[allow(clippy::cast_precision_loss, reason = "frame rates of at most 60")]
        let shutter = layer.motion_blur / fps.max(1) as f32;
        let samples = MAX_BLUR_SAMPLES;
        let Some(mut scratch) = Pixmap::new(canvas.width(), canvas.height()) else { return };
        let mut sum = vec![0.0f32; canvas.data().len()];
        for i in 0..samples {
            #[allow(clippy::cast_precision_loss, reason = "a handful of samples")]
            let at = t - shutter * (i as f32 / (samples - 1) as f32);
            scratch.fill(tiny_skia::Color::TRANSPARENT);
            self.paint_one(&mut scratch, index, at.max(layer.start), 1.0);
            for (s, v) in sum.iter_mut().zip(scratch.data()) {
                *s += f32::from(*v);
            }
        }
        #[allow(clippy::cast_precision_loss, reason = "a handful of samples")]
        let n = samples as f32;
        for (v, s) in scratch.data_mut().iter_mut().zip(&sum) {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a mean of bytes")]
            let byte = (s / n).round().clamp(0.0, 255.0) as u8;
            *v = byte;
        }
        canvas.draw_pixmap(0, 0, scratch.as_ref(), &tiny_skia::PixmapPaint::default(), Transform::identity(), None);
    }

    /// Draws layer `index` at `t` with its opacity times `alpha`.
    fn paint_one(&self, canvas: &mut Pixmap, index: usize, t: f32, alpha: f32) {
        let (Some(layer), Some(placed)) = (self.layers.get(index), self.place(index, t, 0)) else { return };
        let opacity = placed.opacity * alpha;
        if opacity <= 0.0 {
            return;
        }
        let local = t - layer.start;
        let shapes = shapes(&layer.draw, local);
        let Some(bounds) = bounds_of(&shapes) else { return };
        // The anchor point of the layer's box goes to the origin.
        let anchor = Transform::from_translate(
            -(bounds.left() + bounds.width() * layer.anchor[0]),
            -(bounds.top() + bounds.height() * layer.anchor[1]),
        );
        let transform = placed.transform.pre_concat(anchor);
        let mask = reveal_mask(canvas, layer, local, bounds, transform);
        if layer.reveal.at(local) <= 0.0 {
            return;
        }
        let fill = layer.fill.at(local);
        for shape in &shapes {
            let colour = shape.colour.unwrap_or(fill);
            let mut paint = Paint { anti_alias: true, ..Paint::default() };
            paint.shader = match (&layer.gradient, layer.fill_to, shape.colour) {
                (Some(g), _, None) => gradient_shader(g, bounds, opacity)
                    .unwrap_or_else(|| Shader::SolidColor(sk_colour(colour, opacity))),
                (None, Some(to), None) => LinearGradient::new(
                    Point::from_xy(0.0, bounds.top()),
                    Point::from_xy(0.0, bounds.bottom()),
                    vec![
                        GradientStop::new(0.0, sk_colour(colour, opacity)),
                        GradientStop::new(1.0, sk_colour(to, opacity)),
                    ],
                    SpreadMode::Pad,
                    Transform::identity(),
                )
                .unwrap_or_else(|| Shader::SolidColor(sk_colour(colour, opacity))),
                _ => Shader::SolidColor(sk_colour(colour, opacity)),
            };
            let rule = if shape.winding { FillRule::Winding } else { FillRule::EvenOdd };
            let stroke = layer.stroke.filter(|s| s.width > 0.0 && shape.colour.is_none());
            let mut line = Paint { anti_alias: true, ..Paint::default() };
            let mut sk = SkStroke { line_join: tiny_skia::LineJoin::Round, ..SkStroke::default() };
            if let Some(stroke) = stroke {
                line.set_color(sk_colour(stroke.colour, opacity));
                sk.width = stroke.width;
            }
            // Type is stroked under its fill at twice the width, as a sign painter outlines letters: a variable font's
            // outlines overlap inside a letter, and a line on top would draw those overlaps.
            if stroke.is_some() && shape.winding {
                let wide = SkStroke { width: sk.width * 2.0, ..sk.clone() };
                canvas.stroke_path(&shape.path, &line, &wide, transform, mask.as_ref());
            }
            canvas.fill_path(&shape.path, &paint, rule, transform, mask.as_ref());
            if stroke.is_some() && !shape.winding {
                canvas.stroke_path(&shape.path, &line, &sk, transform, mask.as_ref());
            }
        }
    }
}

/// A gradient's shader across `bounds`: its line runs through the box's middle at `angle`, long enough that the first
/// and last stops land on the box's two farthest corners along it.
fn gradient_shader(g: &Gradient, bounds: Rect, opacity: f32) -> Option<Shader<'static>> {
    let (sin, cos) = g.angle.to_radians().sin_cos();
    let (dx, dy) = (sin, cos);
    let reach = f32::midpoint(dx.abs() * bounds.width(), dy.abs() * bounds.height());
    let (cx, cy) = (bounds.left() + bounds.width() / 2.0, bounds.top() + bounds.height() / 2.0);
    let stops = g.stops.iter().map(|s| GradientStop::new(s.at, sk_colour(s.colour, opacity))).collect();
    LinearGradient::new(
        Point::from_xy(cx - dx * reach, cy - dy * reach),
        Point::from_xy(cx + dx * reach, cy + dy * reach),
        stops,
        SpreadMode::Pad,
        Transform::identity(),
    )
}

/// The mask of a partly revealed layer, or `None` when it shows whole.
fn reveal_mask(canvas: &Pixmap, layer: &Layer, local: f32, b: Rect, transform: Transform) -> Option<Mask> {
    let r = layer.reveal.at(local).clamp(0.0, 1.0);
    if r >= 1.0 {
        return None;
    }
    let (w, h) = (b.width(), b.height());
    let rect = match layer.reveal_from {
        From::Left => Rect::from_xywh(b.left(), b.top(), w * r, h),
        From::Right => Rect::from_xywh(b.right() - w * r, b.top(), w * r, h),
        From::Top => Rect::from_xywh(b.left(), b.top(), w, h * r),
        From::Bottom => Rect::from_xywh(b.left(), b.bottom() - h * r, w, h * r),
        From::Centre => Rect::from_xywh(b.left() + w * (1.0 - r) / 2.0, b.top(), w * r, h),
    }?;
    let mut mask = Mask::new(canvas.width(), canvas.height())?;
    mask.fill_path(&PathBuilder::from_rect(rect), FillRule::Winding, true, transform);
    Some(mask)
}

fn sk_colour(c: Colour, opacity: f32) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c.r, c.g, c.b, c.a).with_alpha_multiplied(opacity)
}

trait WithAlpha {
    fn with_alpha_multiplied(self, a: f32) -> Self;
}

impl WithAlpha for tiny_skia::Color {
    fn with_alpha_multiplied(mut self, a: f32) -> Self {
        self.apply_opacity(a);
        self
    }
}

/// One filled piece of a layer, in points; `colour` overrides the layer's fill (a chart's bars, its labels).
struct Shape {
    path: Path,
    colour: Option<Colour>,
    /// Filled by the non-zero rule: type, whose variable outlines overlap; everything else holes by even-odd.
    winding: bool,
}

fn shape(path: Option<Path>, colour: Option<Colour>) -> Option<Shape> {
    path.map(|path| Shape { path, colour, winding: false })
}

/// Type as a shape.
fn type_shape(path: Option<Path>, colour: Option<Colour>) -> Option<Shape> {
    path.map(|path| Shape { path, colour, winding: true })
}

fn bounds_of(shapes: &[Shape]) -> Option<Rect> {
    let mut it = shapes.iter().map(|s| s.path.bounds());
    let first = it.next()?;
    Some(it.fold(first, |a, b| {
        Rect::from_ltrb(
            a.left().min(b.left()),
            a.top().min(b.top()),
            a.right().max(b.right()),
            a.bottom().max(b.bottom()),
        )
        .unwrap_or(a)
    }))
}

/// The shapes `draw` makes at local time `t`.
fn shapes(draw: &Draw, t: f32) -> Vec<Shape> {
    match draw {
        Draw::Text { text, size, weight, tracking, align } => {
            type_shape(text_path(text, *size, *weight, *tracking, *align, 0.0, 0.0), None).into_iter().collect()
        }
        Draw::Rect { w, h, radius } => shape(rounded_rect(0.0, 0.0, *w, *h, *radius), None).into_iter().collect(),
        Draw::Ellipse { w, h } => {
            shape(Rect::from_xywh(0.0, 0.0, *w, *h).and_then(PathBuilder::from_oval), None).into_iter().collect()
        }
        Draw::Ring { radius, width, sweep } => shape(ring(*radius, *width, sweep.at(t)), None).into_iter().collect(),
        Draw::Band { w, h, bow, bow_bottom, apex } => {
            shape(band(*w, *h, *bow, bow_bottom.unwrap_or(*bow), *apex), None).into_iter().collect()
        }
        Draw::Polygon { points } => shape(polygon(points), None).into_iter().collect(),
        Draw::Bars { values, max, labels, captions, icons, colours, slot, w, h, gap, grow } => bars(
            &Bars { values, max: *max, labels, captions, icons, colours, slot: *slot, w: *w, h: *h, gap: *gap },
            grow.at(t),
        ),
        Draw::Icon { name, size } => icon(name, 0.0, 0.0, *size, None),
        Draw::Lines { count, length, thickness, band, travel } => {
            lines(*count, *length, *thickness, *band, travel.at(t)).into_iter().collect()
        }
    }
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, radius: f32) -> Option<Path> {
    let r = radius.min(w / 2.0).min(h / 2.0).max(0.0);
    if r <= 0.0 {
        return Rect::from_xywh(x, y, w, h).map(PathBuilder::from_rect);
    }
    // A quarter circle as a cubic: the control points sit 0.5523 of the radius along the tangents.
    let k = r * 0.552_284_8;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

/// A band `w` by `h` whose edges are parabolas through the apex at `apex` of the width, each end `bow` (top) and
/// `bow_bottom` points off it.
fn band(w: f32, h: f32, bow: f32, bow_bottom: f32, apex: f32) -> Option<Path> {
    const STEPS: u16 = 48;
    let centre = w * apex.clamp(-2.0, 3.0);
    // The farther end lands exactly on the bow; the nearer one partway along the same parabola.
    let reach = centre.abs().max((w - centre).abs()).max(f32::EPSILON);
    let at = |i: u16, bow: f32, base: f32| {
        let x = w * f32::from(i) / f32::from(STEPS);
        let d = (x - centre) / reach;
        (x, base + bow * d * d)
    };
    let mut pb = PathBuilder::new();
    let (x0, y0) = at(0, bow, 0.0);
    pb.move_to(x0, y0);
    for i in 1..=STEPS {
        let (x, y) = at(i, bow, 0.0);
        pb.line_to(x, y);
    }
    for i in (0..=STEPS).rev() {
        let (x, y) = at(i, bow_bottom, h);
        pb.line_to(x, y);
    }
    pb.close();
    pb.finish()
}

fn polygon(points: &[[f32; 2]]) -> Option<Path> {
    let mut pb = PathBuilder::new();
    let (first, rest) = points.split_first()?;
    pb.move_to(first[0], first[1]);
    for p in rest {
        pb.line_to(p[0], p[1]);
    }
    pb.close();
    pb.finish()
}

/// A ring's arc of `sweep` degrees clockwise from the top, as a closed band.
fn ring(radius: f32, width: f32, sweep: f32) -> Option<Path> {
    let sweep = sweep.clamp(0.0, 360.0);
    if sweep <= 0.0 {
        return None;
    }
    let (outer, inner) = (radius + width / 2.0, (radius - width / 2.0).max(0.0));
    let steps = 72;
    let at = |r: f32, i: u32| {
        #[allow(clippy::cast_precision_loss, reason = "a few steps")]
        let a = (sweep * i as f32 / steps as f32 - 90.0).to_radians();
        (radius + r * a.cos(), radius + r * a.sin())
    };
    let mut pb = PathBuilder::new();
    let (x, y) = at(outer, 0);
    pb.move_to(x, y);
    for i in 1..=steps {
        let (x, y) = at(outer, i);
        pb.line_to(x, y);
    }
    for i in (0..=steps).rev() {
        let (x, y) = at(inner, i);
        pb.line_to(x, y);
    }
    pb.close();
    pb.finish()
}

/// A chart's numbers, borrowed from its [`Draw::Bars`].
struct Bars<'a> {
    values: &'a [f32],
    max: Option<f32>,
    labels: &'a [String],
    captions: &'a [String],
    icons: &'a [String],
    colours: &'a [Colour],
    slot: Colour,
    w: f32,
    h: f32,
    gap: f32,
}

fn bars(chart: &Bars<'_>, grow: f32) -> Vec<Shape> {
    let n = chart.values.len().max(1);
    #[allow(clippy::cast_precision_loss, reason = "at most 24 bars")]
    let nf = n as f32;
    let column = ((chart.w - chart.gap * (nf - 1.0)) / nf).max(1.0);
    let top = chart.max.unwrap_or_else(|| chart.values.iter().copied().fold(f32::MIN, f32::max)).max(1e-6);
    let label = (column * 0.32).min(36.0);
    let icon_size = column * 0.62;
    let mut out = Vec::new();
    // Each bar grows over its own stretch of the track, one after another.
    let stagger = 0.35;
    for (i, value) in chart.values.iter().enumerate() {
        #[allow(clippy::cast_precision_loss, reason = "at most 24 bars")]
        let fi = i as f32;
        let x = fi * (column + chart.gap);
        let own = ((grow * (1.0 + stagger * (nf - 1.0)) - stagger * fi).clamp(0.0, 1.0)).powf(0.7);
        let colour = chart.colours.get(i).or(chart.colours.last()).copied().unwrap_or(Colour::WHITE);
        let label_h = if chart.labels.is_empty() { 0.0 } else { label * 1.5 };
        let icon_h = if chart.icons.is_empty() { 0.0 } else { icon_size * 1.3 };
        let slot_top = label_h;
        let slot_h = (chart.h - label_h).max(1.0);
        out.extend(shape(rounded_rect(x, slot_top, column, slot_h, column * 0.08), Some(chart.slot)));
        // The bar fills the slot from its foot to under the icon at the top of the scale, the caption printed on it.
        let pad = column * 0.08;
        let room = (slot_h - icon_h - pad).max(1.0);
        let bar_h = room * (value / top).clamp(0.0, 1.0) * own;
        if bar_h > 0.5 {
            let y = slot_top + slot_h - pad - bar_h;
            out.extend(shape(
                Rect::from_xywh(x + column * 0.08, y, column * 0.84, bar_h).map(PathBuilder::from_rect),
                Some(colour),
            ));
        }
        if let Some(text) = chart.labels.get(i) {
            out.extend(type_shape(
                text_path(text, label, 700.0, 0.0, Align::Centre, x + column / 2.0, label * 1.05),
                Some(Colour::rgb(20, 20, 30)),
            ));
        }
        if let Some(text) = chart.captions.get(i) {
            let y = slot_top + slot_h - pad - label * 0.3;
            out.extend(type_shape(
                text_path(text, label * 0.9, 600.0, 0.0, Align::Centre, x + column / 2.0, y),
                Some(Colour::WHITE),
            ));
        }
        if let Some(name) = chart.icons.get(i) {
            let y = slot_top + icon_size * 0.15;
            out.extend(icon(name, x + (column - icon_size) / 2.0, y, icon_size, Some(icon_colour(name))));
        }
    }
    out
}

fn icon_colour(name: &str) -> Colour {
    match name {
        "sun" | "star" | "bolt" => Colour::rgb(255, 214, 40),
        "moon" => Colour::rgb(240, 244, 255),
        "drop" => Colour::rgb(120, 190, 255),
        "heart" => Colour::rgb(255, 80, 120),
        _ => Colour::WHITE,
    }
}

/// An icon's shapes in a `size` square at `(x, y)`, in `colour` (the layer's fill when `None`).
fn icon(name: &str, x: f32, y: f32, size: f32, colour: Option<Colour>) -> Vec<Shape> {
    let c = |dx: f32, dy: f32| (x + dx * size, y + dy * size);
    let circle = |cx: f32, cy: f32, r: f32| {
        let (px, py) = c(cx, cy);
        Rect::from_xywh(px - r * size, py - r * size, 2.0 * r * size, 2.0 * r * size).and_then(PathBuilder::from_oval)
    };
    let poly = |pts: &[(f32, f32)]| {
        let points: Vec<[f32; 2]> = pts
            .iter()
            .map(|(a, b)| {
                let (px, py) = c(*a, *b);
                [px, py]
            })
            .collect();
        polygon(&points)
    };
    let mut out = Vec::new();
    match name {
        "sun" => {
            out.extend(shape(circle(0.5, 0.5, 0.22), colour));
            for i in 0..8 {
                #[allow(clippy::cast_precision_loss, reason = "eight rays")]
                let a = (i as f32 * 45.0).to_radians();
                let (s, co) = a.sin_cos();
                let (r0, r1, half) = (0.3, 0.46, 0.045);
                let p = |r: f32, side: f32| (0.5 + co * r - s * side, 0.5 + s * r + co * side);
                out.extend(shape(poly(&[p(r0, -half), p(r1, 0.0), p(r0, half)]), colour));
            }
        }
        "moon" => {
            // A crescent: the disc minus a disc moved up and right (even-odd fill of both).
            let mut pb = PathBuilder::new();
            let (cx, cy) = c(0.5, 0.5);
            let (ox, oy) = c(0.66, 0.36);
            if let Some(r) = Rect::from_xywh(cx - 0.36 * size, cy - 0.36 * size, 0.72 * size, 0.72 * size) {
                pb.push_oval(r);
            }
            if let Some(r) = Rect::from_xywh(ox - 0.3 * size, oy - 0.3 * size, 0.6 * size, 0.6 * size) {
                pb.push_oval(r);
            }
            out.extend(shape(pb.finish(), colour));
        }
        "cloud" => {
            out.extend(shape(circle(0.35, 0.55, 0.2), colour));
            out.extend(shape(circle(0.58, 0.45, 0.26), colour));
            out.extend(shape(circle(0.76, 0.6, 0.16), colour));
            let (px, py) = c(0.2, 0.55);
            out.extend(shape(rounded_rect(px, py, 0.65 * size, 0.2 * size, 0.1 * size), colour));
        }
        "bolt" => out.extend(shape(
            poly(&[(0.55, 0.05), (0.2, 0.55), (0.45, 0.55), (0.38, 0.95), (0.8, 0.4), (0.55, 0.4), (0.68, 0.05)]),
            colour,
        )),
        "drop" => {
            let mut pb = PathBuilder::new();
            let (tx, ty) = c(0.5, 0.08);
            let (lx, ly) = c(0.2, 0.62);
            let (rx, ry) = c(0.8, 0.62);
            let (bx, by) = c(0.5, 0.92);
            pb.move_to(tx, ty);
            pb.quad_to(lx - 0.02 * size, ly - 0.15 * size, lx, ly);
            pb.quad_to(lx + 0.02 * size, by, bx, by);
            pb.quad_to(rx - 0.02 * size, by, rx, ry);
            pb.quad_to(rx + 0.02 * size, ry - 0.15 * size, tx, ty);
            pb.close();
            out.extend(shape(pb.finish(), colour));
        }
        "star" => {
            let pts: Vec<(f32, f32)> = (0..10)
                .map(|i| {
                    #[allow(clippy::cast_precision_loss, reason = "ten corners")]
                    let a = (i as f32 * 36.0 - 90.0).to_radians();
                    let r = if i % 2 == 0 { 0.46 } else { 0.19 };
                    (0.5 + a.cos() * r, 0.52 + a.sin() * r)
                })
                .collect();
            out.extend(shape(poly(&pts), colour));
        }
        "arrow" => out.extend(shape(
            poly(&[(0.1, 0.4), (0.55, 0.4), (0.55, 0.2), (0.92, 0.5), (0.55, 0.8), (0.55, 0.6), (0.1, 0.6)]),
            colour,
        )),
        "heart" => {
            out.extend(shape(circle(0.33, 0.38, 0.2), colour));
            out.extend(shape(circle(0.67, 0.38, 0.2), colour));
            out.extend(shape(poly(&[(0.14, 0.45), (0.86, 0.45), (0.5, 0.9)]), colour));
        }
        _ => {}
    }
    out
}

/// Speed lines: `count` streaks in a band `band` high, travelling with `travel` from off the left edge to off the
/// right (in points across a frame [`REFERENCE_WIDTH`] wide).
fn lines(count: u32, length: f32, thickness: f32, band: f32, travel: f32) -> Option<Shape> {
    let mut pb = PathBuilder::new();
    for i in 0..count {
        let r = |k: u32| {
            let mut h = i.wrapping_mul(0x8da6_b343) ^ k.wrapping_mul(0xd816_3841);
            h ^= h >> 15;
            h = h.wrapping_mul(0x846c_a68b);
            h ^= h >> 16;
            #[allow(clippy::cast_precision_loss, reason = "24 bits")]
            let u = (h >> 8) as f32 / 16_777_216.0;
            u
        };
        let len = length * (0.35 + 0.65 * r(1));
        let y = band * r(2);
        let speed = 0.7 + 0.6 * r(3);
        let x = -len + (REFERENCE_WIDTH * 2.0 + len) * (travel * speed + r(4) * 0.3) - REFERENCE_WIDTH * 0.3;
        let th = thickness * (0.5 + r(5));
        // A streak tapers to a point at its tail.
        pb.move_to(x, y);
        pb.line_to(x + len, y - th / 2.0);
        pb.line_to(x + len, y + th / 2.0);
        pb.close();
    }
    shape(pb.finish(), None)
}

/// `text` set in the face at `size` points and `weight`, its baseline's `align` point at `(x, y)`.
fn text_path(text: &str, size: f32, weight: f32, tracking: f32, align: Align, x: f32, y: f32) -> Option<Path> {
    let mut font = face()?.clone();
    font.set_variation(b"wght", weight.clamp(200.0, 800.0));
    let scaled = font.as_scaled(PxScale::from(size));
    let (sx, sy) = (scaled.h_scale_factor(), scaled.v_scale_factor());
    let mut advance = 0.0;
    let mut placed = Vec::new();
    let mut last = None;
    for ch in text.chars() {
        let id = font.glyph_id(ch);
        if let Some(prev) = last {
            advance += scaled.kern(prev, id);
        }
        placed.push((id, advance));
        advance += scaled.h_advance(id) + tracking * size;
        last = Some(id);
    }
    let width = (advance - tracking * size).max(0.0);
    let start = match align {
        Align::Left => x,
        Align::Centre => x - width / 2.0,
        Align::Right => x - width,
    };
    let mut pb = PathBuilder::new();
    for (id, at) in placed {
        let Some(outline) = font.outline(id) else { continue };
        let map = |p: ab_glyph::Point| (start + at + p.x * sx, y - p.y * sy);
        let mut current: Option<(f32, f32)> = None;
        for curve in &outline.curves {
            let (from, to) = match curve {
                OutlineCurve::Line(a, b) | OutlineCurve::Quad(a, _, b) | OutlineCurve::Cubic(a, _, _, b) => {
                    (map(*a), map(*b))
                }
            };
            if current.is_none_or(|c| (c.0 - from.0).abs() > 1e-3 || (c.1 - from.1).abs() > 1e-3) {
                if current.is_some() {
                    pb.close();
                }
                pb.move_to(from.0, from.1);
            }
            match curve {
                OutlineCurve::Line(_, _) => pb.line_to(to.0, to.1),
                OutlineCurve::Quad(_, c1, _) => {
                    let c1 = map(*c1);
                    pb.quad_to(c1.0, c1.1, to.0, to.1);
                }
                OutlineCurve::Cubic(_, c1, c2, _) => {
                    let (c1, c2) = (map(*c1), map(*c2));
                    pb.cubic_to(c1.0, c1.1, c2.0, c2.1, to.0, to.1);
                }
            }
            current = Some(to);
        }
        if current.is_some() {
            pb.close();
        }
    }
    pb.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(json: serde_json::Value) -> Layer {
        serde_json::from_value(json).unwrap()
    }

    fn painted(layers: &[Layer], t: f32) -> Pixmap {
        let mut canvas = Pixmap::new(216, 384).unwrap();
        Painter::new(layers, [216, 384]).paint(&mut canvas, Depth::Front, t, 24);
        canvas
    }

    fn coverage(p: &Pixmap) -> usize {
        p.data().chunks(4).filter(|px| px[3] > 0).count()
    }

    #[test]
    fn text_draws_and_grows_with_its_size() {
        let small = layer(serde_json::json!({"id": "t", "draw": {"kind": "text", "text": "NEWS", "size": 60}}));
        let big = layer(serde_json::json!({"id": "t", "draw": {"kind": "text", "text": "NEWS", "size": 120}}));
        let ink = |p: &Pixmap| p.data().chunks(4).map(|px| u32::from(px[3])).sum::<u32>() / 255;
        let (a, b) = (ink(&painted(&[small], 0.0)), ink(&painted(&[big], 0.0)));
        assert!(a > 50, "the text drew {a} pixels");
        assert!(b > a * 3, "twice the size covers about four times the area: {a} then {b}");
    }

    #[test]
    fn keys_move_a_layer_across_the_frame() {
        let moving = layer(serde_json::json!({
            "id": "r", "draw": {"kind": "rect", "w": 100, "h": 100},
            "x": [{"t": 0.0, "v": 0.2}, {"t": 1.0, "v": 0.8, "ease": "linear"}]
        }));
        let centre_x = |p: &Pixmap| {
            let (mut sum, mut n) = (0usize, 0usize);
            for (i, px) in p.data().chunks(4).enumerate() {
                if px[3] > 128 {
                    sum += i % 216;
                    n += 1;
                }
            }
            sum as f32 / n as f32
        };
        let start = centre_x(&painted(std::slice::from_ref(&moving), 0.0));
        let end = centre_x(&painted(std::slice::from_ref(&moving), 1.0));
        assert!((start - 0.2 * 216.0).abs() < 2.0, "{start}");
        assert!((end - 0.8 * 216.0).abs() < 2.0, "{end}");
    }

    #[test]
    fn a_reveal_wipes_on_and_a_layer_hides_outside_its_time() {
        let wipe = |r: f32| {
            layer(
                serde_json::json!({"id": "r", "draw": {"kind": "rect", "w": 200, "h": 100}, "reveal": r, "start": 0.5}),
            )
        };
        let half = coverage(&painted(&[wipe(0.5)], 1.0));
        let whole = coverage(&painted(&[wipe(1.0)], 1.0));
        assert!((half as f32 / whole as f32 - 0.5).abs() < 0.05, "{half} of {whole}");
        assert_eq!(coverage(&painted(&[wipe(1.0)], 0.2)), 0, "before its start");
    }

    #[test]
    fn a_child_follows_its_parent() {
        let parent = layer(
            serde_json::json!({"id": "band", "draw": {"kind": "rect", "w": 300, "h": 60}, "x": [{"t": 0, "v": 0.3}, {"t": 1, "v": 0.7}], "opacity": 0.5}),
        );
        let child = layer(
            serde_json::json!({"id": "dot", "parent": "band", "draw": {"kind": "ellipse", "w": 20, "h": 20}, "x": 0, "y": 0}),
        );
        let painter_canvas = painted(&[parent.clone(), child.clone()], 1.0);
        let alone = painted(&[parent], 1.0);
        assert_ne!(painter_canvas.data(), alone.data(), "the child draws");
    }

    #[test]
    fn motion_blur_smears_a_fast_layer() {
        let fast = |blur: f32| {
            layer(serde_json::json!({
                "id": "r", "draw": {"kind": "rect", "w": 40, "h": 40}, "motion_blur": blur,
                "x": [{"t": 0.0, "v": 0.0}, {"t": 0.2, "v": 1.0, "ease": "linear"}]
            }))
        };
        let sharp = coverage(&painted(&[fast(0.0)], 0.1));
        let blurred = coverage(&painted(&[fast(1.0)], 0.1));
        assert!(blurred > sharp * 2, "the blur spreads it: {sharp} then {blurred}");
    }

    #[test]
    fn charts_and_icons_draw_and_bars_grow() {
        let chart = |grow: f32| {
            layer(serde_json::json!({"id": "c", "draw": {"kind": "bars", "values": [64, 72, 86, 91, 102],
                "labels": ["6AM", "8AM", "10AM", "12PM", "2PM"], "icons": ["moon", "sun", "sun", "sun", "sun"],
                "colours": ["#40c040", "#f0e040", "#f0a020", "#f06020", "#e02020"], "w": 900, "h": 500, "grow": grow}}))
        };
        let (empty, grown) = (painted(&[chart(0.0)], 0.0), painted(&[chart(1.0)], 0.0));
        let changed = empty.data().chunks(4).zip(grown.data().chunks(4)).filter(|(a, b)| a != b).count();
        assert!(changed > 2000, "bars fill their slots as they grow: {changed} pixels changed");
        for name in ICONS {
            let l = layer(serde_json::json!({"id": "i", "draw": {"kind": "icon", "name": name, "size": 120}}));
            l.check("i").unwrap();
            assert!(coverage(&painted(&[l], 0.0)) > 100, "{name} draws");
        }
    }

    #[test]
    fn a_band_bows_about_its_apex() {
        // Frame-wide, apex a quarter across, ends lifted 100 points on top and 40 below (a curved desk seen from its
        // left): at a fifth of the scale, 20 and 8 pixels at the far end.
        let band = layer(serde_json::json!({"id": "b", "draw": {"kind": "band", "w": 1080, "h": 200, "bow": -100,
            "bow_bottom": -40, "apex": 0.25}}));
        band.check("b").unwrap();
        let p = painted(&[band], 0.0);
        let column = |x: usize| {
            let rows: Vec<usize> = (0..384).filter(|y| p.data()[(y * 216 + x) * 4 + 3] > 128).collect();
            (rows[0] as f32, *rows.last().unwrap() as f32 + 1.0)
        };
        let (apex, near, far) = (column(54), column(1), column(214));
        assert!((apex.1 - apex.0 - 40.0).abs() <= 1.0, "200 points high at the apex: {apex:?}");
        assert!((apex.0 - far.0 - 20.0).abs() <= 1.5, "the far end's top lifts 20 pixels: {apex:?} {far:?}");
        assert!((apex.1 - far.1 - 8.0).abs() <= 1.5, "its bottom 8: {apex:?} {far:?}");
        assert!(apex.0 - near.0 < 3.5, "the near end lifts only a ninth as much: {apex:?} {near:?}");
    }

    #[test]
    fn a_gradient_runs_through_its_stops_in_its_direction() {
        let across = layer(serde_json::json!({"id": "g", "draw": {"kind": "rect", "w": 1080, "h": 1920},
            "gradient": {"angle": 90, "stops": [{"at": 0, "colour": "#ff0000"}, {"at": 0.5, "colour": "#00ff00"},
            {"at": 1, "colour": "#0000ff"}]}}));
        across.check("g").unwrap();
        let p = painted(&[across], 0.0);
        let at = |x: usize, y: usize| {
            let px = &p.data()[(y * 216 + x) * 4..][..3];
            [px[0], px[1], px[2]]
        };
        assert!(at(1, 100)[0] > 240 && at(214, 100)[2] > 240, "red at the left, blue at the right");
        assert!(at(108, 10)[1] > 240 && at(108, 370)[1] > 240, "green down the middle, top to bottom");
        let down = layer(serde_json::json!({"id": "g", "draw": {"kind": "rect", "w": 1080, "h": 1920},
            "gradient": {"stops": [{"at": 0, "colour": "#000000"}, {"at": 1, "colour": "#ffffff"}]}}));
        let p = painted(&[down], 0.0);
        let (top, bottom) = (p.data()[(2 * 216 + 108) * 4], p.data()[(381 * 216 + 108) * 4]);
        assert!(top < 10 && bottom > 245, "the default runs down: {top} then {bottom}");
    }

    #[test]
    fn bad_layers_are_refused() {
        for (gradient, says) in [
            (serde_json::json!({"stops": [{"at": 0, "colour": "#000000"}]}), "2 to 16"),
            (
                serde_json::json!({"stops": [{"at": 0.6, "colour": "#000000"}, {"at": 0.4, "colour": "#ffffff"}]}),
                "after",
            ),
        ] {
            let l =
                layer(serde_json::json!({"id": "g", "draw": {"kind": "rect", "w": 10, "h": 10}, "gradient": gradient}));
            assert!(l.check("l").unwrap_err().contains(says), "{says}");
        }
        let both =
            layer(serde_json::json!({"id": "g", "draw": {"kind": "rect", "w": 10, "h": 10}, "fill_to": "#000000",
            "gradient": {"stops": [{"at": 0, "colour": "#000000"}, {"at": 1, "colour": "#ffffff"}]}}));
        assert!(both.check("l").unwrap_err().contains("not both"));
        let l = layer(serde_json::json!({"id": "i", "draw": {"kind": "icon", "name": "unicorn"}}));
        assert!(l.check("l").unwrap_err().contains("no icon"));
        let l = layer(serde_json::json!({"id": "b", "draw": {"kind": "band", "w": 100, "h": 10, "apex": 3.5}}));
        assert!(l.check("l").unwrap_err().contains("apex is -2 to 3"));
    }
}

#[cfg(test)]
mod look {
    use super::*;

    #[test]
    #[ignore = "writes a picture to look at"]
    fn dump() {
        let layers: Vec<Layer> = serde_json::from_value(serde_json::json!([
            {"id": "bg", "draw": {"kind": "rect", "w": 1200, "h": 2000}, "fill": "#3f6fe0", "fill_to": "#b4b8f4"},
            {"id": "t", "draw": {"kind": "text", "text": "OMU NEWS", "size": 200, "weight": 800}, "y": 0.15, "skew": 30.0, "fill": "#ffffff", "stroke": {"colour": "#2c3fa0", "width": 6.0}},
            {"id": "t4", "draw": {"kind": "text", "text": "NEWS WEATHER", "size": 120, "weight": 400}, "y": 0.25, "fill": "#ffffff"},
            {"id": "t6", "draw": {"kind": "text", "text": "NEWS WEATHER", "size": 120, "weight": 600}, "y": 0.31, "fill": "#ffffff"},
            {"id": "t8", "draw": {"kind": "text", "text": "NEWS WEATHER", "size": 120, "weight": 800}, "y": 0.37, "fill": "#ffffff"},
            {"id": "band", "draw": {"kind": "rect", "w": 560, "h": 120}, "y": 0.35, "skew": -16.0, "fill": "#ff3d8b", "fill_to": "#d01770"},
            {"id": "w", "parent": "band", "draw": {"kind": "text", "text": "WEATHER", "size": 84, "weight": 800}, "x": 0.0, "y": 0.0, "skew": -16.0, "fill": "#ffffff"},
            {"id": "ring", "draw": {"kind": "ring", "radius": 200, "width": 5}, "y": 0.5, "fill": "#ffffff80"},
            {"id": "lines", "draw": {"kind": "lines", "count": 22, "length": 700, "thickness": 5, "band": 520, "travel": 0.5}, "y": 0.5, "fill": "#ffffff70"},
            {"id": "panel", "draw": {"kind": "rect", "w": 1010, "h": 360, "radius": 6}, "y": 0.865, "fill": "#ffffff"},
            {"id": "bars", "y": 0.89, "fill": "#ffffff", "draw": {"kind": "bars", "values": [64, 72, 86, 91, 102, 104, 97, 86], "max": 110,
              "labels": ["6AM", "8AM", "10AM", "12PM", "2PM", "4PM", "6PM", "8PM"], "captions": ["64°F", "72°F", "86°F", "91°F", "102°F", "104°F", "97°F", "86°F"],
              "icons": ["moon", "sun", "sun", "sun", "sun", "sun", "sun", "moon"], "colours": ["#3fc94a", "#f2d21b", "#f5a524", "#f5841f", "#ea3a24", "#e3241c", "#f57c22", "#f5a524"],
              "slot": "#2a63de", "w": 960, "h": 250, "gap": 12, "grow": 0.8}}
        ])).unwrap();
        let mut canvas = Pixmap::new(540, 960).unwrap();
        Painter::new(&layers, [540, 960]).paint(&mut canvas, Depth::Front, 0.0, 24);
        let img = image::RgbaImage::from_raw(540, 960, canvas.data().to_vec()).unwrap();
        img.save(std::env::var("LAYER_DUMP").unwrap_or_else(|_| "layers.png".into())).unwrap();
    }
}
