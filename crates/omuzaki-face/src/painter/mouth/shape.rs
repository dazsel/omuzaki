//! Mouth shapes: the prototype's `mouth()` up to the interior (`face.js` 290-336), as vector commands.
//!
//! One of three shapes is drawn, chosen the way the prototype chose it:
//!
//! - **Kiss**, when the lips are pushed together (`pucker > 0.35`) and the mouth is barely open (`open < 0.25`): a
//!   short wavy slit with a light rim and a faint upper-lip arc.
//! - **Closed line**, below an opening of [`CLOSED_BELOW`]: a thin line whose corners follow the smile of each side. It
//!   thins towards the switch, so it meets the open shape's rim.
//! - **Open shape**, otherwise: a lens that grows with the opening, rounder with `round`, filled with the interior
//!   gradient. The rim round it starts as the closed line and becomes the open mouth's over the openings
//!   [`CLOSED_BELOW`] to [`FADE_UNTIL`] ([`OpenMouth::fade`]). Its path is kept in [`PaintCtx::open_mouth`] so the interior, teeth and rim clip and stroke the
//!   same outline instead of recomputing it.
//!
//! Everything here is a pure function of the profile and the paint parameters: no clock, no state, and no allocation
//! once the draw list is warm (the point arrays live on the stack).

use omuzaki_core::glam::Vec2;
use omuzaki_face_api::{
    Colour, ColourExt, ColourRef, FaceLayerId, MouthParams, MouthStyle, Paint, PathRef, Stops, narrow,
};

use crate::painter::PaintCtx;

/// Below this opening (after the press has taken its share) the mouth is a closed line.
const CLOSED_BELOW: f32 = 0.06;
/// The open mouth's rim finishes becoming its own at this opening ([`OpenMouth::fade`]).
const FADE_UNTIL: f32 = 0.2;
/// The closed line is this much thinner at the switch than at rest, so the lens that replaces it keeps the same weight
/// (the line is 5.8 px thick at the switch and the lens 3.9 px deep, at 3600 px per metre).
const CLOSED_THIN: f32 = 0.2;
/// A pucker above this, with the mouth under [`KISS_OPEN_BELOW`], is a kiss.
const KISS_ABOVE: f32 = 0.35;
/// The kiss gives way to the open shape from this opening.
const KISS_OPEN_BELOW: f32 = 0.25;
/// How much of the opening a press cancels.
const PRESS_CLOSES: f32 = 0.5;
/// How much a press shortens the mouth.
const PRESS_SHORTENS: f32 = 0.25;
/// How much a press thins the closed line.
const PRESS_THINS: f32 = 0.3;
/// How much a pucker narrows the mouth.
const PUCKER_NARROWS: f32 = 0.18;
/// How much a pucker narrows an open mouth further.
const PUCKER_NARROWS_OPEN: f32 = 0.3;
/// The performance scale reaches its full effect at this opening, a third (`open x 3`), or at full pucker.
const GROW_OPEN_GAIN: f32 = 3.0;
/// An open mouth is this much wider per unit of opening.
const OPEN_WIDENS: f32 = 0.25;
/// A fully round mouth is this much narrower.
const ROUND_NARROWS: f32 = 0.35;
/// The opening height per unit of opening, at the prototype's mouth half-width.
const HEIGHT_PER_OPEN: f32 = 0.016;
/// The prototype's mouth half-width, which [`HEIGHT_PER_OPEN`] and the corner curves are tuned to.
const PROTOTYPE_HALF_WIDTH: f32 = 0.0105;
/// The upper edge of the open shape rises this fraction of its height at the centre.
const TOP_RISE: f32 = 0.25 * 0.5;
/// How much a round mouth flattens the upper edge.
const ROUND_FLATTENS_TOP: f32 = 0.6;
/// The lower edge's exponent is `BOTTOM_EXPONENT + ROUND_BOTTOM_EXPONENT x round`: higher is squarer.
const BOTTOM_EXPONENT: f32 = 0.5;
/// See [`BOTTOM_EXPONENT`].
const ROUND_BOTTOM_EXPONENT: f32 = 0.3;
/// The open shape's corners curve by this much per unit of smile, against the closed line's [`MouthStyle::corner_curve`]
/// of 0.0045: the prototype used 0.004 there, so the same ratio keeps both in step when a designer changes the curve.
const OPEN_CURVE_RATIO: f32 = 0.004 / 0.0045;
/// The lips' centre drops by this much per unit of the average smile.
const CENTRE_FOLLOWS_SMILE: f32 = 0.001;
/// Sideways travel of the whole mouth per unit of [`MouthParams::shift`], in metres.
const SHIFT_PER_UNIT: f32 = 0.006;
/// Samples along the closed line.
const CLOSED_SEGMENTS: usize = 16;
/// Samples along each edge of the open shape.
const OPEN_SEGMENTS: usize = 28;
/// Samples along the kiss.
const KISS_SEGMENTS: usize = 18;

/// The kiss slit's outer shade: `rgba(150,70,80,0.6)`, 4 mm wide.
const KISS_SHADE: (Colour, f32) = (Colour::rgb(150, 70, 80), 0.6);
/// The kiss slit's light rim: `rgba(255,244,244,0.95)`, 2.6 mm wide.
const KISS_RIM: (Colour, f32) = (Colour::rgb(255, 244, 244), 0.95);
/// The kiss slit's dark core, `#7a2a35`, 1.1 mm wide.
const KISS_CORE: Colour = Colour::rgb(0x7a, 0x2a, 0x35);
/// The kiss's upper-lip arc: `rgba(220,130,135,0.45)`.
const UPPER_LIP: (Colour, f32) = (Colour::rgb(220, 130, 135), 0.45);
/// The faint line under a closed mouth on a flat (lip-less) face: `rgba(200,120,115,0.35)`.
const UNDER_LINE: (Colour, f32) = (Colour::rgb(200, 120, 115), 0.35);
/// Widths of the kiss strokes and the under line, in metres.
const KISS_SHADE_WIDTH: f32 = 0.0040;
const KISS_RIM_WIDTH: f32 = 0.0026;
const KISS_CORE_WIDTH: f32 = 0.0011;
const UPPER_LIP_WIDTH: f32 = 0.0012;
const UNDER_LINE_WIDTH: f32 = 0.0012;
/// Lip strength below which the under line is drawn (the sheet look has no lip rim to say where the lip is).
const UNDER_LINE_BELOW_LIPS: f32 = 0.5;

/// The open shape, kept for the pieces that clip or stroke along its outline (interior, teeth, rim, tongue).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct OpenMouth {
    /// The closed outline on the Base layer: the upper edge left to right, then the lower edge back.
    pub path: PathRef,
    /// The mouth centre in face-plane metres: the line the edges are measured from.
    pub centre: Vec2,
    /// Half the opening's width, in metres.
    pub half_width: f32,
    /// The opening's depth below [`centre`](Self::centre) at the middle, in metres, before the corner offsets.
    pub height: f32,
    /// The performance scale in force (`closedScale` to `openScale`), which teeth and fangs scale with too.
    pub grow: f32,
    /// How far the rim has become the open mouth's own, `0` at the switch to `1` from an opening of 0.2: until then
    /// the rim is the closed line, and the closed smile's fang gives way to the open ones.
    pub fade: f32,
    edges: Edges,
}

impl OpenMouth {
    /// Height of the upper edge above [`centre`](Self::centre) at `u` (`-1` left end to `1` right end), in metres.
    pub fn top(&self, u: f32) -> f32 {
        self.edges.top(self.height, u)
    }

    /// Height of the lower edge relative to [`centre`](Self::centre) at `u`; negative below it, in metres.
    pub fn bottom(&self, u: f32) -> f32 {
        self.edges.bottom(self.height, u)
    }

    /// The lens' height at the middle, from the upper edge to the lower, in metres.
    pub fn depth(&self) -> f32 {
        self.top(0.0) - self.bottom(0.0)
    }
}

/// What shapes the two edges of the open mouth.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Edges {
    round: f32,
    /// Corner lift per side: `[left (u > 0), right (u <= 0)]`.
    smile: [f32; 2],
    /// Metres of corner travel per unit of smile.
    curve: f32,
    /// How far the lips' centre sits below the line, in metres.
    drop: f32,
}

impl Edges {
    fn corner(&self, u: f32) -> f32 {
        let smile = if u > 0.0 { self.smile[0] } else { self.smile[1] };
        smile * self.curve * u * u - self.drop
    }

    fn top(&self, h: f32, u: f32) -> f32 {
        h * TOP_RISE * (1.0 - self.round * ROUND_FLATTENS_TOP) * (1.0 - u * u) + self.corner(u)
    }

    fn bottom(&self, h: f32, u: f32) -> f32 {
        let exponent = BOTTOM_EXPONENT + ROUND_BOTTOM_EXPONENT * self.round;
        -h * (1.0 - u * u).max(0.0).powf(exponent) + self.corner(u)
    }
}

/// Which of the three shapes the mouth takes, chosen the way the prototype chose it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    /// Pushed-forward lips, barely open.
    Kiss,
    /// A thin line.
    Closed,
    /// A lens.
    Open,
}

/// The numbers every shape starts from: the prototype's first lines of `mouth()`. The pieces that paint on a closed
/// mouth or a kiss (gloss, the closed smile's fang) are built from it too.
pub(super) struct Frame {
    /// Opening after the press has closed some of it.
    pub(super) open: f32,
    pub(super) press: f32,
    pub(super) pucker: f32,
    /// The performance scale.
    pub(super) grow: f32,
    /// The mouth centre.
    pub(super) centre: Vec2,
    /// Half the closed width.
    pub(super) half_width: f32,
    /// Corner lift per side: `[left, right]`.
    pub(super) smile: [f32; 2],
}

impl Frame {
    /// The frame of the mouth this context paints.
    pub(super) fn of(ctx: &PaintCtx<'_>) -> Self {
        Self::new(&ctx.profile.mouth, &ctx.params.mouth, narrow(ctx.profile.layout.mouth_y))
    }

    fn new(style: &MouthStyle, mouth: &MouthParams, mouth_y: f32) -> Self {
        let press = mouth.press.clamp(0.0, 1.0);
        let pucker = mouth.pucker.clamp(0.0, 1.0);
        let open = (mouth.open - PRESS_CLOSES * press).max(0.0);
        let (closed, open_scale) = (narrow(style.closed_scale), narrow(style.open_scale));
        let grow = closed + (open_scale - closed) * (open * GROW_OPEN_GAIN + pucker).min(1.0);
        let half_width = narrow(style.half_width)
            * mouth.wide
            * grow
            * (1.0 - PUCKER_NARROWS * pucker)
            * (1.0 - PRESS_SHORTENS * press);
        let shift = mouth.shift * SHIFT_PER_UNIT;
        Self {
            open,
            press,
            pucker,
            grow,
            centre: Vec2::new(shift, mouth_y),
            half_width,
            smile: [mouth.smile_left, mouth.smile_right],
        }
    }

    /// The closed line's width: the style's, scaled with the performance scale and thinned by a press, and by the
    /// approach to the switch (a share of [`CLOSED_THIN`] growing to all of it at [`CLOSED_BELOW`]).
    pub(super) fn line_width(&self, style: &MouthStyle) -> f32 {
        let thin = CLOSED_THIN * smoothstep(self.open / CLOSED_BELOW);
        narrow(style.line_width) * self.grow.sqrt() * (1.0 - PRESS_THINS * self.press) * (1.0 - thin)
    }

    /// How far the open shape has taken over from the closed line, `0..=1` over the openings [`CLOSED_BELOW`] to
    /// [`FADE_UNTIL`].
    fn fade(&self) -> f32 {
        smoothstep((self.open - CLOSED_BELOW) / (FADE_UNTIL - CLOSED_BELOW))
    }

    pub(super) fn kind(&self) -> Kind {
        if self.pucker > KISS_ABOVE && self.open < KISS_OPEN_BELOW {
            Kind::Kiss
        } else if self.open < CLOSED_BELOW {
            Kind::Closed
        } else {
            Kind::Open
        }
    }
}

/// Smoothstep of `t` clamped to `0..=1`: flat at both ends, so an easing starts and stops without a kink.
pub(super) fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// `colour` at `alpha` as a solid paint.
pub(super) fn solid(colour: Colour, alpha: f32) -> Paint {
    Paint::Solid(colour.faded(alpha))
}

/// A style colour, or `fallback` for a palette slot nobody resolved (which a painter should never see).
pub(super) fn resolve(colour: &ColourRef, fallback: Colour) -> Colour {
    match colour {
        ColourRef::Fixed(c) => *c,
        ColourRef::Slot(_) => fallback,
    }
}

/// Position along a path of sample `i` out of `n`, from `-1` to `1`.
pub(super) fn along(i: usize, n: usize) -> f32 {
    // Sample counts are a few dozen at most, exact in f32.
    #[allow(clippy::cast_precision_loss)]
    let (i, n) = (i as f32, n as f32);
    -1.0 + 2.0 * i / n
}

/// Paints the mouth's shape (never the lower lip, interior, teeth, rim or tongue: those belong to the other mouth pieces).
pub(super) fn paint(ctx: &mut PaintCtx<'_>) {
    let frame = Frame::of(ctx);
    match frame.kind() {
        Kind::Kiss => kiss(ctx, &frame),
        Kind::Closed => closed_line(ctx, &frame),
        Kind::Open => open_shape(ctx, &frame),
    }
}

/// Records the open mouth's outline in [`PaintCtx::open_mouth`] without painting anything, for the pieces that clip
/// against it when the shape itself is not drawn. Does nothing for a closed mouth or a kiss.
pub(super) fn record_outline(ctx: &mut PaintCtx<'_>) {
    let frame = Frame::of(ctx);
    if frame.kind() == Kind::Open {
        open_outline(ctx, &frame);
    }
}

/// The closed mouth: a line whose corners follow the smile, and the faint line under it on a flat face.
fn closed_line(ctx: &mut PaintCtx<'_>, f: &Frame) {
    let style = &ctx.profile.mouth;
    let curve = narrow(style.corner_curve);
    let width = f.line_width(style);
    let colour = resolve(&style.line_colour, Colour::rgb(0xa8, 0x5a, 0x55));
    let mut points = [Vec2::ZERO; CLOSED_SEGMENTS + 1];
    for (i, p) in points.iter_mut().enumerate() {
        let u = along(i, CLOSED_SEGMENTS);
        let smile = if u > 0.0 { f.smile[0] } else { f.smile[1] };
        *p = f.centre + Vec2::new(u * f.half_width, smile * curve * u * u * f.grow);
    }
    let flat_face = narrow(style.lips.strength) < UNDER_LINE_BELOW_LIPS;
    let list = ctx.list(FaceLayerId::Base);
    let path = list.path().polyline(&points).finish();
    list.stroke(path, Paint::Solid(colour), width);
    if flat_face {
        let y = f.centre.y - 0.0038;
        let tick = list
            .path()
            .move_to(Vec2::new(f.centre.x - f.half_width * 0.35, y))
            .line_to(Vec2::new(f.centre.x + f.half_width * 0.35, y))
            .finish();
        list.stroke(tick, solid(UNDER_LINE.0, UNDER_LINE.1), UNDER_LINE_WIDTH);
    }
}

/// The kiss: a wavy slit in three strokes (shade, rim, core) and a faint arc for the upper lip.
fn kiss(ctx: &mut PaintCtx<'_>, f: &Frame) {
    let mut points = [Vec2::ZERO; KISS_SEGMENTS + 1];
    for (i, p) in points.iter_mut().enumerate() {
        let u = along(i, KISS_SEGMENTS);
        let rise = 0.0016 * (u * std::f32::consts::PI * 1.15).sin() + 0.0009 * (1.0 - u * u);
        *p = f.centre + Vec2::new(u * f.half_width * 0.85, rise);
    }
    let list = ctx.list(FaceLayerId::Base);
    let slit = list.path().polyline(&points).finish();
    list.stroke(slit, solid(KISS_SHADE.0, KISS_SHADE.1), KISS_SHADE_WIDTH);
    list.stroke(slit, solid(KISS_RIM.0, KISS_RIM.1), KISS_RIM_WIDTH);
    list.stroke(slit, Paint::Solid(KISS_CORE), KISS_CORE_WIDTH);
    let arc = list
        .path()
        .move_to(f.centre + Vec2::new(-f.half_width * 0.7, 0.0022))
        .quad_to(f.centre + Vec2::new(0.0, 0.0048), f.centre + Vec2::new(f.half_width * 0.7, 0.0022))
        .finish();
    list.stroke(arc, solid(UPPER_LIP.0, UPPER_LIP.1), UPPER_LIP_WIDTH);
}

/// The open mouth: a lens filled with the interior gradient, kept for the pieces that draw into it.
fn open_shape(ctx: &mut PaintCtx<'_>, f: &Frame) {
    let open = open_outline(ctx, f);
    let interior = &ctx.profile.mouth.interior;
    let top = resolve(&interior.top, Colour::rgb(0x4a, 0x16, 0x22));
    let bottom = resolve(&interior.bottom, Colour::rgb(0x6e, 0x24, 0x33));
    let fill = Paint::Linear {
        from: f.centre,
        to: f.centre - Vec2::new(0.0, open.height),
        stops: Stops::new(&[(0.0, top), (1.0, bottom)]),
    };
    ctx.list(FaceLayerId::Base).fill(open.path, fill);
}

/// The open mouth's outline, built once per frame: a piece that needs it before the shape paints (the lower-lip gloss)
/// records it, and the shape then reuses it instead of adding the path again.
fn open_outline(ctx: &mut PaintCtx<'_>, f: &Frame) -> OpenMouth {
    if let Some(open) = ctx.open_mouth {
        return open;
    }
    let style = &ctx.profile.mouth;
    let scale = narrow(style.half_width) / PROTOTYPE_HALF_WIDTH;
    let height = HEIGHT_PER_OPEN * scale * f.open * f.grow;
    let half_width = f.half_width
        * (1.0 + OPEN_WIDENS * f.open)
        * (1.0 - ROUND_NARROWS * ctx.params.mouth.round)
        * (1.0 - PUCKER_NARROWS_OPEN * f.pucker);
    let edges = Edges {
        round: ctx.params.mouth.round,
        smile: f.smile,
        curve: narrow(style.corner_curve) * OPEN_CURVE_RATIO,
        drop: CENTRE_FOLLOWS_SMILE * f32::midpoint(f.smile[0], f.smile[1]),
    };

    let at = |u: f32, v: f32| f.centre + Vec2::new(u * half_width, v);
    let list = ctx.list(FaceLayerId::Base);
    let mut outline = list.path();
    for i in 0..=OPEN_SEGMENTS {
        let u = along(i, OPEN_SEGMENTS);
        let p = at(u, edges.top(height, u));
        outline = if i == 0 { outline.move_to(p) } else { outline.line_to(p) };
    }
    for i in (0..=OPEN_SEGMENTS).rev() {
        let u = along(i, OPEN_SEGMENTS);
        outline = outline.line_to(at(u, edges.bottom(height, u)));
    }
    let path = outline.close().finish();
    let open = OpenMouth { path, centre: f.centre, half_width, height, grow: f.grow, fade: f.fade(), edges };
    ctx.open_mouth = Some(open);
    open
}

#[cfg(test)]
mod tests {
    use omuzaki_core::time::Seconds;
    use omuzaki_face_api::{
        Cmd, DEFAULT_PX_PER_M, FaceDrawing, FaceInput, FaceLayerSpec, FaceMapping, FaceProfile, FaceProjection,
        FaceRect, FaceView, PaintParams,
    };

    use super::*;

    /// Paints the shape and returns what it left in the context, with the drawing.
    fn painted(edit: impl FnOnce(&mut MouthParams)) -> (Option<OpenMouth>, FaceDrawing) {
        let projection =
            FaceProjection { rect: FaceRect { x0: -0.1, x1: 0.1, y0: -0.2, y1: -0.1 }, front_fade: [0.0, 0.05] };
        let specs = FaceLayerId::ALL.map(|id| FaceLayerSpec::for_layer(id, &projection, DEFAULT_PX_PER_M));
        let profile = FaceProfile::default();
        let mut params: PaintParams = FaceMapping::default().rest_params();
        edit(&mut params.mouth);
        let input = FaceInput { profile: &profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
        let mut drawing = FaceDrawing::new();
        let mut ctx = PaintCtx::new(&input, &specs, &mut drawing, None);
        paint(&mut ctx);
        let open = ctx.open_mouth;
        (open, drawing)
    }

    #[test]
    fn an_open_mouth_leaves_the_path_it_filled_for_the_pieces_after_it() {
        let (open, drawing) = painted(|m| m.open = 0.6);
        let open = open.expect("an open mouth is recorded");
        let list = &drawing[FaceLayerId::Base];
        assert_eq!(list.cmds().first().map(|c| matches!(c, Cmd::Fill { path, .. } if *path == open.path)), Some(true));
        assert_eq!(open.centre.y, -0.15);
        assert!((open.height - 0.016 * 0.6).abs() < 1e-6);
        assert!((open.half_width - 0.0105 * 1.15).abs() < 1e-6);
    }

    #[test]
    fn the_edges_it_exposes_are_the_ones_it_drew() {
        let (open, drawing) = painted(|m| (m.open, m.smile_left, m.smile_right, m.round) = (0.6, 0.8, -0.3, 0.5));
        let open = open.expect("recorded");
        let list = &drawing[FaceLayerId::Base];
        let pts: Vec<Vec2> = list
            .elements(open.path)
            .iter()
            .filter_map(|e| match e {
                omuzaki_face_api::PathEl::Move(p) | omuzaki_face_api::PathEl::Line(p) => Some(*p),
                _ => None,
            })
            .collect();
        for (i, p) in pts[..=OPEN_SEGMENTS].iter().enumerate() {
            let u = along(i, OPEN_SEGMENTS);
            assert!((p.y - (open.centre.y + open.top(u))).abs() < 1e-6, "top edge at {u}");
            assert!((p.x - (open.centre.x + u * open.half_width)).abs() < 1e-6, "x at {u}");
        }
        for (i, p) in pts[OPEN_SEGMENTS + 1..].iter().enumerate() {
            let u = along(OPEN_SEGMENTS - i, OPEN_SEGMENTS);
            assert!((p.y - (open.centre.y + open.bottom(u))).abs() < 1e-6, "bottom edge at {u}");
        }
    }

    #[test]
    fn the_fade_runs_from_zero_at_the_switch_to_one_at_an_opening_of_a_fifth() {
        let fade = |open: f32| painted(|m| m.open = open).0.expect("open").fade;
        assert!(fade(0.0601) < 1e-3, "at the switch");
        assert!((fade(0.13) - 0.5).abs() < 1e-3, "halfway, where the easing is symmetric");
        assert!((fade(0.2) - 1.0).abs() < 1e-6 && (fade(0.8) - 1.0).abs() < 1e-6, "from 0.2");
        let steps: Vec<f32> = (0..=14_u8).map(|i| fade(0.06 + 0.01 * f32::from(i))).collect();
        assert!(steps.windows(2).all(|w| w[1] >= w[0]), "monotone: {steps:?}");
    }

    #[test]
    fn a_closed_mouth_and_a_kiss_leave_nothing_to_clip_against() {
        assert!(painted(|_| {}).0.is_none());
        assert!(painted(|m| m.pucker = 0.9).0.is_none());
    }
}
